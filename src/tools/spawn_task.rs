// src/tools/spawn_task.rs
//
// SpawnTaskTool — lets the LLM orchestrate sub-agents.
//
// ── Two modes ─────────────────────────────────────────────────────────────────
//
// SINGLE-TASK mode  (only "description" is provided):
//
//   { "description": "Write a Fibonacci function in src/lib.rs" }
//
//   Creates one CoderAgent, runs it with a fresh message history built from the
//   agent's system prompt plus the description as the first user message.
//   Returns the agent's `final_message` as the tool output.
//
// MULTI-TASK mode  (a "tasks" array is provided):
//
//   {
//     "tasks": [
//       { "id": "write-lib",   "description": "...", "depends_on": [] },
//       { "id": "write-tests", "description": "...", "depends_on": ["write-lib"] }
//     ],
//     "parallel": true,
//     "max_concurrent": 4
//   }
//
//   Builds a TaskGraph from the provided list, runs a TaskExecutor that honours
//   the dependency ordering, and returns a JSON-like text summary of the results.
//
// ── Nesting guard ─────────────────────────────────────────────────────────────
//
// Sub-agents receive a clone of the parent's `ToolContext` with `nesting_depth`
// incremented by 1.  When `nesting_depth >= 3` the tool refuses to spawn so the
// call stack cannot grow without bound.
//
// ─────────────────────────────────────────────────────────────────────────────

use crate::agent::coder::CoderAgent;
use crate::agent::Agent;
use crate::config::AgentConfig;
use crate::llm::Message;
use crate::orchestrator::executor::TaskExecutor;
use crate::orchestrator::graph::{TaskGraph, TaskNode};
use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

// ─── SpawnTaskTool ────────────────────────────────────────────────────────────

/// Tool that lets the LLM delegate work to one or more sub-agents.
///
/// This is the core of the "完全安全托管" (fully autonomous delegation) design:
/// the top-level LLM can hand off independent sub-tasks, run them in parallel,
/// collect their results, and incorporate the output into its own response —
/// all without human involvement.
///
/// The tool is stateless; all dynamic state comes from `ToolContext`.
pub struct SpawnTaskTool;

#[async_trait]
impl Tool for SpawnTaskTool {
    fn name(&self) -> &str {
        "spawn_task"
    }

    fn description(&self) -> &str {
        "Delegate work to one or more sub-agent(s). \
         In single-task mode, provide 'description' to run a CoderAgent on that task \
         and get back its final message. \
         In multi-task mode, provide a 'tasks' array with id/description/depends_on \
         fields to run tasks in parallel with dependency ordering and get back a \
         summary report. Nesting depth is capped at 3 levels."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Single-task mode: the task description to give to a CoderAgent."
                },
                "tasks": {
                    "type": "array",
                    "description": "Multi-task mode: list of tasks with dependency ordering.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for this task (e.g. 'write-tests')."
                            },
                            "description": {
                                "type": "string",
                                "description": "What this sub-agent should do."
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "IDs of tasks that must complete before this one starts.",
                                "default": []
                            }
                        },
                        "required": ["id", "description"]
                    }
                },
                "parallel": {
                    "type": "boolean",
                    "description": "Run independent tasks in parallel (default: true). \
                                    Set false to force serial execution.",
                    "default": true
                },
                "max_concurrent": {
                    "type": "integer",
                    "description": "Maximum tasks running at the same time (default: 4).",
                    "default": 4
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // ── 1. Nesting guard ──────────────────────────────────────────────────
        //
        // Each call to spawn_task increments nesting_depth in the child context.
        // We refuse to go deeper than 3 so we can never have unbounded recursion
        // (LLM A spawns LLM B which spawns LLM C which would try to spawn LLM D…).
        if ctx.nesting_depth >= 3 {
            return Ok(ToolResult {
                output: format!(
                    "spawn_task refused: maximum nesting depth (3) reached. \
                     Current depth: {}. Cannot spawn further sub-agents.",
                    ctx.nesting_depth
                ),
                is_error: true,
            });
        }

        // ── 2. Build child context ────────────────────────────────────────────
        //
        // Clone the full context and bump the depth counter.  The child agents
        // inherit the same working directory, LLM provider, tool registry, and
        // I/O channel, which lets them operate on the same project files.
        let mut sub_ctx = ctx.clone();
        sub_ctx.nesting_depth += 1;

        // ── 3. Dispatch to the right mode ─────────────────────────────────────
        let tasks_val = args.get("tasks");
        let description_val = args.get("description").and_then(|v| v.as_str());

        match (tasks_val, description_val) {
            // ─────────────────────────────────────────────────────────────────
            // MULTI-TASK MODE: caller provided a "tasks" array
            // ─────────────────────────────────────────────────────────────────
            (Some(tasks_arr), _) => {
                let tasks = match tasks_arr.as_array() {
                    Some(arr) => arr,
                    None => {
                        return Ok(ToolResult {
                            output: "spawn_task: 'tasks' must be a JSON array, not a scalar."
                                .to_string(),
                            is_error: true,
                        });
                    }
                };

                // Empty array is technically valid — nothing to do.
                if tasks.is_empty() {
                    return Ok(ToolResult {
                        output: "spawn_task: 'tasks' array is empty — nothing to execute."
                            .to_string(),
                        is_error: false,
                    });
                }

                // Parse optional concurrency controls.
                let parallel = args
                    .get("parallel")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let max_concurrent = args
                    .get("max_concurrent")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(4) as usize;

                // Build the TaskGraph from the caller-supplied task list.
                // TaskGraph validates all dependency IDs and rejects cycles.
                let mut graph = TaskGraph::new();
                for task_val in tasks {
                    let id = match task_val.get("id").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(ToolResult {
                                output: "spawn_task: each task must have an 'id' string field."
                                    .to_string(),
                                is_error: true,
                            });
                        }
                    };

                    let desc = match task_val.get("description").and_then(|v| v.as_str()) {
                        Some(s) => s,
                        None => {
                            return Ok(ToolResult {
                                output: format!(
                                    "spawn_task: task '{}' must have a 'description' string field.",
                                    id
                                ),
                                is_error: true,
                            });
                        }
                    };

                    // Build the node, chaining on any declared dependencies.
                    let mut node = TaskNode::new(id, desc);
                    if let Some(deps) = task_val.get("depends_on").and_then(|v| v.as_array()) {
                        for dep_val in deps {
                            if let Some(dep_id) = dep_val.as_str() {
                                node = node.with_dependency(dep_id);
                            }
                        }
                    }

                    // add_task validates the dependency IDs against what is
                    // already in the graph, so tasks must be listed in
                    // topological order (dependencies before dependents).
                    graph.add_task(node)?;
                }

                // If parallel=false, override concurrency to 1 (serial execution).
                let concurrency = if parallel { max_concurrent } else { 1 };

                // Run the executor.  It takes ownership of the graph, the shared
                // Arc handles for LLM + tools, a clone of sub_ctx for each task,
                // and the I/O channel for progress output.
                let executor = TaskExecutor::new(graph).with_max_concurrent(concurrency);

                let report = executor
                    .run(
                        Arc::clone(&ctx.llm),
                        Arc::clone(&ctx.tools),
                        sub_ctx,
                        Arc::clone(&ctx.io),
                    )
                    .await?;

                // ── Format the report ─────────────────────────────────────────
                let completed_count = report.task_results.len();
                let failed_count = report.failed.len();
                let cancelled_count = report.cancelled.len();

                let mut output = format!(
                    "spawn_task completed: {} succeeded, {} failed, {} cancelled\n\
                     Duration: {:.1}s\n\n",
                    completed_count,
                    failed_count,
                    cancelled_count,
                    report.total_duration.as_secs_f64(),
                );

                if !report.task_results.is_empty() {
                    output.push_str("## Completed tasks\n");
                    // Sort by task ID for deterministic output.
                    let mut results: Vec<_> = report.task_results.iter().collect();
                    results.sort_by_key(|(id, _)| id.as_str());
                    for (id, result) in results {
                        // Truncate very long final messages to keep the tool
                        // output at a reasonable size.
                        let preview: String = result.final_message.chars().take(200).collect();
                        let ellipsis = if result.final_message.len() > 200 {
                            "…"
                        } else {
                            ""
                        };
                        output.push_str(&format!(
                            "- **{}**: {}{} (iters={}, tools={})\n",
                            id, preview, ellipsis, result.iterations, result.tool_calls_total,
                        ));
                    }
                    output.push('\n');
                }

                if !report.failed.is_empty() {
                    output.push_str(&format!(
                        "## Failed tasks\n{}\n\n",
                        report.failed.join(", ")
                    ));
                }

                if !report.cancelled.is_empty() {
                    output.push_str(&format!(
                        "## Cancelled tasks\n{}\n\n",
                        report.cancelled.join(", ")
                    ));
                }

                // Mark the result as an error when any tasks failed, so the
                // calling LLM knows it should investigate the failures.
                Ok(ToolResult {
                    output,
                    is_error: !report.failed.is_empty(),
                })
            }

            // ─────────────────────────────────────────────────────────────────
            // SINGLE-TASK MODE: caller provided only a "description" string
            // ─────────────────────────────────────────────────────────────────
            (None, Some(description)) => {
                // Create a fresh CoderAgent with default configuration.
                // AgentConfig::default() gives 25 max iterations and
                // reasonable tool-call limits — tuned for autonomous coding tasks.
                let agent = CoderAgent::new(AgentConfig::default());

                // Build the initial message history:
                //   [0] system  — agent's hard-coded system prompt
                //   [1] user    — the task description from the caller
                let mut messages = vec![
                    Message::system(agent.system_prompt().as_str()),
                    Message::user(description),
                ];

                // Run the agent to completion.  It will loop until it emits
                // [TASK_COMPLETE], hits max_iterations, or encounters an error.
                let result = agent
                    .run(
                        &mut messages,
                        ctx.tools.as_ref(), // &ToolRegistry
                        ctx.llm.as_ref(),   // &dyn LlmProvider
                        &sub_ctx,
                    )
                    .await?;

                Ok(ToolResult {
                    output: result.final_message,
                    is_error: false,
                })
            }

            // ─────────────────────────────────────────────────────────────────
            // USAGE ERROR: neither "description" nor "tasks" was provided
            // ─────────────────────────────────────────────────────────────────
            (None, None) => Ok(ToolResult {
                output: "spawn_task: provide either 'description' (single-task mode) \
                         or a 'tasks' array (multi-task mode)."
                    .to_string(),
                is_error: true,
            }),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;
    use crate::llm::NullLlmProvider;
    use std::path::PathBuf;
    use tokio::sync::Mutex;

    /// Minimal ToolContext for unit-testing spawn_task itself.
    ///
    /// Uses `NullLlmProvider` (returns empty responses) and an empty
    /// `ToolRegistry` so agent runs inside these tests terminate quickly
    /// (they hit max_iterations rather than [TASK_COMPLETE]).
    fn make_ctx() -> ToolContext {
        let llm = Arc::new(NullLlmProvider);
        let tools = Arc::new(crate::tools::ToolRegistry::new());
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io: Arc::new(NullIO),
            compact_mode: false,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm,
            tools,
        }
    }

    // ── Metadata tests ────────────────────────────────────────────────────────

    #[test]
    fn test_spawn_task_name() {
        assert_eq!(SpawnTaskTool.name(), "spawn_task");
    }

    #[test]
    fn test_spawn_task_description_non_empty() {
        assert!(!SpawnTaskTool.description().is_empty());
    }

    #[test]
    fn test_spawn_task_schema_has_required_properties() {
        let schema = SpawnTaskTool.parameters_schema();
        let props = &schema["properties"];
        // All four parameters must be documented.
        assert!(
            props["description"].is_object(),
            "schema missing 'description'"
        );
        assert!(props["tasks"].is_object(), "schema missing 'tasks'");
        assert!(props["parallel"].is_object(), "schema missing 'parallel'");
        assert!(
            props["max_concurrent"].is_object(),
            "schema missing 'max_concurrent'"
        );
    }

    // ── Nesting guard ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_nesting_depth_at_limit_is_refused() {
        // When nesting_depth == 3, spawn_task must refuse.
        let mut ctx = make_ctx();
        ctx.nesting_depth = 3;
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "description": "test" }), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "should be an error at depth 3");
        assert!(
            result.output.contains("maximum nesting depth"),
            "error message should mention nesting depth: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_nesting_depth_below_limit_is_allowed_to_attempt() {
        // depth 2 is below the limit → the tool should proceed (and may fail
        // for other reasons — NullLlmProvider returns empty responses —
        // but it should NOT refuse with a nesting error).
        let mut ctx = make_ctx();
        ctx.nesting_depth = 2;
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "tasks": [] }), &ctx)
            .await
            .unwrap();
        // Empty tasks array → succeeds with a "nothing to execute" message.
        assert!(!result.is_error);
        assert!(result.output.contains("empty"));
    }

    // ── Usage / argument errors ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_no_args_returns_usage_error() {
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "missing args should be an error");
        // The error message should mention both modes so the caller knows what to do.
        let msg = result.output.to_lowercase();
        assert!(
            msg.contains("description") || msg.contains("tasks"),
            "error message should mention required fields: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_empty_tasks_array_succeeds() {
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "tasks": [] }), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn test_tasks_not_array_returns_error() {
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "tasks": "not-an-array" }), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "non-array tasks should be an error");
    }

    #[tokio::test]
    async fn test_task_missing_id_returns_error() {
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(
                serde_json::json!({ "tasks": [{ "description": "no id here" }] }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error, "task without 'id' should be an error");
    }

    #[tokio::test]
    async fn test_task_missing_description_returns_error() {
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "tasks": [{ "id": "t1" }] }), &ctx)
            .await
            .unwrap();
        assert!(
            result.is_error,
            "task without 'description' should be an error"
        );
    }

    // ── Graph construction / validation ───────────────────────────────────────

    #[tokio::test]
    async fn test_unknown_dependency_returns_error() {
        // The graph validator should reject a dependency on a non-existent task ID.
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(
                serde_json::json!({
                    "tasks": [
                        {
                            "id": "t1",
                            "description": "first task",
                            "depends_on": ["does-not-exist"]
                        }
                    ]
                }),
                &ctx,
            )
            .await;
        // add_task propagates an error for unknown dep IDs.
        assert!(
            result.is_err() || result.unwrap().is_error,
            "dependency on unknown task should fail"
        );
    }

    // ── parallel / max_concurrent ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_parallel_false_accepted_as_serial() {
        // When parallel=false, we clamp concurrency to 1.
        // The test just checks the executor runs without crashing on empty input.
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(serde_json::json!({ "tasks": [], "parallel": false }), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_max_concurrent_respected() {
        // max_concurrent=1 is equivalent to serial=true; just ensure no crash.
        let ctx = make_ctx();
        let result = SpawnTaskTool
            .execute(
                serde_json::json!({ "tasks": [], "max_concurrent": 1 }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
    }
}
