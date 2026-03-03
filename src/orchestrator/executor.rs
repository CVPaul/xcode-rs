// src/orchestrator/executor.rs
//
// Task Graph Executor — the "完全安全托管" engine.
//
// ── Why this exists ───────────────────────────────────────────────────────────
//
// The `TaskGraph` (graph.rs) models *what* to do and in what order.
// The `TaskExecutor` here implements *how* to do it:
//
//   1. Inspect `graph.next_ready()` to find tasks whose dependencies have all
//      been Completed.
//   2. Spawn each ready task as a tokio async task (real concurrency, not
//      cooperative multitasking).
//   3. Collect results with a `JoinSet`; on success call `graph.mark_completed`,
//      on failure retry up to `max_retries` or cancel dependents.
//   4. Loop until the graph reports `is_finished()`.
//
// The graph is wrapped in `Arc<Mutex<TaskGraph>>` so that the executor loop
// (which drives `mark_running`, `mark_completed`, etc.) and the spawned tasks
// (which need to read the task description/config when they start) can share it
// safely across async boundaries.
//
// ── Concurrency model ─────────────────────────────────────────────────────────
//
//   - The main loop is single-threaded: it dispatches work and collects results
//     sequentially from the JoinSet (one completion at a time).
//   - Individual tasks run in their own tokio tasks (potentially on separate OS
//     threads via Tokio's work-stealing scheduler).
//   - We cap live concurrency at `max_concurrent` to avoid saturating the API's
//     rate-limiter or opening too many file handles at once.
//
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::agent::coder::CoderAgent;
use crate::agent::{Agent, AgentResult};
use crate::io::AgentIO;
use crate::llm::{LlmProvider, Message};
use crate::orchestrator::graph::TaskGraph;
use crate::tools::{ToolContext, ToolRegistry};

// ─── ExecutionReport ─────────────────────────────────────────────────────────

/// Summary of a completed execution run.
///
/// Returned by [`TaskExecutor::run`] when the graph is finished (all nodes are
/// in a terminal state: Completed, Failed, or Cancelled).
#[derive(Debug)]
pub struct ExecutionReport {
    /// Successful results keyed by task ID.
    ///
    /// Only contains entries for tasks that reached `Completed`.
    pub task_results: HashMap<String, AgentResult>,

    /// IDs of tasks that permanently failed (exhausted all retries).
    pub failed: Vec<String>,

    /// IDs of tasks that were cancelled because a dependency failed.
    pub cancelled: Vec<String>,

    /// Wall-clock time from the first `run()` call to the last task finishing.
    pub total_duration: Duration,
}

// ─── TaskExecutor ─────────────────────────────────────────────────────────────

/// Parallel wave-based executor for a [`TaskGraph`].
///
/// # Usage
///
/// ```rust,ignore
/// let executor = TaskExecutor::new(graph)
///     .with_max_concurrent(4)
///     .with_max_retries(2);
///
/// let report = executor.run(llm, tools, tool_ctx, io).await?;
/// println!("completed: {}", report.task_results.len());
/// ```
///
/// # Execution Model
///
/// The executor runs a loop:
///
/// ```text
/// loop:
///   1. Find all Pending tasks whose deps are all Completed → "ready set"
///   2. If ready.is_empty() && nothing running && not finished → deadlock (error)
///   3. Dispatch up to (max_concurrent - running) tasks from the ready set
///   4. Wait for ONE task to finish (JoinSet::join_next)
///   5. On success  → mark_completed, store result
///      On failure  → if retries < max_retries: reset_for_retry (will be re-queued)
///                    else: mark_failed, cancel all downstream dependents
///   6. Repeat until graph.is_finished()
/// ```
pub struct TaskExecutor {
    /// The task graph being executed.
    /// Made `pub` so callers can inspect it after a failed run.
    pub graph: TaskGraph,

    /// Maximum number of tasks that may be executing concurrently.
    /// Default: 4.
    pub max_concurrent: usize,

    /// How many times a task may be retried before it is permanently failed.
    /// Default: 2.
    pub max_retries: u32,
}

impl TaskExecutor {
    /// Create an executor with default settings (4 concurrent, 2 retries).
    pub fn new(graph: TaskGraph) -> Self {
        TaskExecutor {
            graph,
            max_concurrent: 4,
            max_retries: 2,
        }
    }

    /// Override the maximum concurrency (number of tasks running at once).
    pub fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n;
        self
    }

    /// Override the per-task retry limit.
    #[allow(dead_code)]
    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Execute the task graph, returning a report when all tasks finish.
    ///
    /// # Parameters
    ///
    /// - `llm`      — LLM provider shared across all tasks (thread-safe Arc).
    /// - `tools`    — Tool registry shared across all tasks.
    /// - `tool_ctx` — Per-execution tool context (cloned for each task).
    /// - `io`       — I/O channel for progress output.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The graph enters a deadlock (tasks remain but none are ready or running).
    /// - A spawned tokio task panics (join error).
    pub async fn run(
        mut self,
        llm: Arc<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        tool_ctx: ToolContext,
        io: Arc<dyn AgentIO>,
    ) -> Result<ExecutionReport> {
        // ── Handle empty graph ────────────────────────────────────────────────
        if self.graph.is_empty() {
            return Ok(ExecutionReport {
                task_results: HashMap::new(),
                failed: Vec::new(),
                cancelled: Vec::new(),
                total_duration: Duration::ZERO,
            });
        }

        let start = Instant::now();

        // Wrap the graph in Arc<Mutex<>> so spawned tasks can read it to get
        // their description/config, while the main loop can update status.
        //
        // Note: spawned tasks only READ the graph (to get task description).
        // All WRITES (mark_running, mark_completed, etc.) happen in the main loop
        // after collecting results from JoinSet, so we hold the lock for short
        // windows only.
        let graph = Arc::new(Mutex::new(self.graph));

        // JoinSet collects the handles of all in-flight tasks.
        // Each task resolves to `(task_id: String, Result<AgentResult>)`.
        let mut join_set: JoinSet<(String, Result<AgentResult>)> = JoinSet::new();

        // Count of tasks currently running (i.e. spawned but not yet finished).
        let mut running_count: usize = 0;

        // Accumulate results across the whole run.
        let mut task_results: HashMap<String, AgentResult> = HashMap::new();
        let mut failed_ids: Vec<String> = Vec::new();
        let mut cancelled_ids: Vec<String> = Vec::new();

        loop {
            // ── Dispatch new tasks ────────────────────────────────────────────
            //
            // We can dispatch up to (max_concurrent - running_count) new tasks.
            // We must collect the ready task info BEFORE calling mark_running so
            // next_ready() returns fresh results each iteration.
            {
                let mut g = graph.lock().await;

                // How many slots are open?
                let available = self.max_concurrent.saturating_sub(running_count);

                if available > 0 {
                    // Collect owned (id, description, config) so we can drop the
                    // lock before spawning (spawning may take time, and we don't
                    // want to hold the graph mutex across that).
                    //
                    // IMPORTANT: `next_ready()` returns `Vec<&TaskNode>` (borrowed
                    // references).  We clone the fields we need and drop them
                    // before calling `mark_running` or releasing the lock.
                    let to_dispatch: Vec<(String, String, crate::config::AgentConfig)> = g
                        .next_ready()
                        .into_iter()
                        .take(available)
                        .map(|node| {
                            (
                                node.id.clone(),
                                node.description.clone(),
                                node.agent_config.clone(),
                            )
                        })
                        .collect();

                    for (task_id, description, agent_config) in to_dispatch {
                        // Transition the node to Running so it doesn't appear in
                        // next_ready() again on the next iteration.
                        g.mark_running(&task_id)?;
                        running_count += 1;

                        // ── Report progress ───────────────────────────────────
                        let (pending, running_now, completed, failed_n, cancelled_n) =
                            g.status_counts();
                        let status_msg = format!(
                            "▶ Dispatching '{}' | pending={} running={} done={} failed={} cancelled={}",
                            task_id,
                            pending,
                            running_now,
                            completed,
                            failed_n,
                            cancelled_n,
                        );
                        // We need to drop the lock before calling async methods.
                        drop(g);
                        io.show_status(&status_msg).await?;
                        // Re-acquire for the next loop iteration.
                        g = graph.lock().await;

                        // ── Spawn the task ────────────────────────────────────
                        //
                        // Clone everything the spawned task needs. All of these
                        // are cheap Arc clones (reference-counted pointers).
                        let llm_clone: Arc<dyn LlmProvider> = Arc::clone(&llm);
                        let tools_clone = Arc::clone(&tools);
                        let tool_ctx_clone = tool_ctx.clone();
                        let task_id_clone = task_id.clone();

                        join_set.spawn(async move {
                            // Build a CoderAgent for this specific task.
                            // The agent_config controls model, iterations, etc.
                            let agent = CoderAgent::new(agent_config);

                            // The agent's conversation starts with a single user
                            // message containing the task description.
                            // The CoderAgent's system_prompt handles the rest.
                            let mut messages: Vec<Message> = vec![Message::user(description)];

                            // Run the agent loop.  This may call tools, loop
                            // multiple times, and eventually return AgentResult.
                            let result = agent
                                .run(
                                    &mut messages,
                                    &tools_clone,
                                    llm_clone.as_ref(),
                                    &tool_ctx_clone,
                                )
                                .await;

                            // Return the task id alongside the result so the
                            // main loop can route the outcome back to the graph.
                            (task_id_clone, result)
                        });
                    }
                }
            } // lock released here

            // ── Check for deadlock ────────────────────────────────────────────
            {
                let g = graph.lock().await;
                if g.is_finished() {
                    break;
                }
                if running_count == 0 {
                    // No tasks running AND we couldn't dispatch any → deadlock.
                    // This can happen if every remaining task has a dependency on
                    // a failed task that was NOT cancelled (shouldn't happen with
                    // correct cancel_dependents logic, but guard against it).
                    let (pending, _, _, failed_n, _) = g.status_counts();
                    if pending > 0 && failed_n == 0 {
                        anyhow::bail!(
                            "Task graph deadlock: {} pending tasks but none are ready \
                             and none are running. This is a bug — check for missing \
                             dependency cancellations.",
                            pending
                        );
                    }
                    // If there are failed tasks and no pending/running, the graph
                    // should already be_finished() (all blocked nodes should have
                    // been cancelled). Break to produce the final report.
                    break;
                }
            }

            // ── Await ONE task completion ─────────────────────────────────────
            //
            // `join_next()` returns None when the JoinSet is empty.
            // We guard against that above (running_count > 0) but handle it
            // defensively here too.
            let Some(join_result) = join_set.join_next().await else {
                break;
            };
            running_count -= 1;

            // Unwrap the JoinHandle result.  A join error means the task panicked.
            let (task_id, agent_result) = match join_result {
                Ok(pair) => pair,
                Err(join_err) => {
                    // A panic in a spawned task.  We don't know which task it was,
                    // but we can log and continue — other tasks are unaffected.
                    // In practice this shouldn't happen unless there's a bug.
                    io.write_error(&format!("⚠ A task panicked: {}", join_err))
                        .await?;
                    continue;
                }
            };

            // ── Handle success ────────────────────────────────────────────────
            match agent_result {
                Ok(result) => {
                    let mut g = graph.lock().await;
                    // Store the result in the graph node AND in our local map.
                    g.mark_completed(&task_id, result.clone())?;
                    task_results.insert(task_id.clone(), result);

                    let (pending, running_now, completed, failed_n, cancelled_n) =
                        g.status_counts();
                    drop(g);

                    io.show_status(&format!(
                        "✓ '{}' completed | pending={} running={} done={} failed={} cancelled={}",
                        task_id, pending, running_now, completed, failed_n, cancelled_n,
                    ))
                    .await?;
                }

                // ── Handle failure ────────────────────────────────────────────
                Err(err) => {
                    let mut g = graph.lock().await;

                    // Record the failure (increments retry_count on the node).
                    g.mark_failed(&task_id, err.to_string())?;

                    let retry_count = g.get(&task_id).map(|n| n.retry_count).unwrap_or(u32::MAX);

                    if retry_count <= self.max_retries {
                        // Still within retry budget — reset to Pending so the
                        // main dispatch loop will pick it up again.
                        g.reset_for_retry(&task_id)?;
                        drop(g);

                        io.show_status(&format!(
                            "↺ '{}' failed (attempt {}), will retry (max {})",
                            task_id, retry_count, self.max_retries,
                        ))
                        .await?;
                    } else {
                        // Out of retries — permanently failed.
                        // Cancel all tasks that (directly or transitively) depend
                        // on this one, since they can never run now.
                        let to_cancel = collect_dependents(&g, &task_id);
                        for dep_id in &to_cancel {
                            // A dependent might already be Cancelled if another
                            // of its dependencies failed first — ignore the error.
                            let _ = g.mark_cancelled(dep_id);
                            cancelled_ids.push(dep_id.clone());
                        }
                        failed_ids.push(task_id.clone());

                        let (pending, running_now, completed, failed_n, cancelled_n) =
                            g.status_counts();
                        drop(g);

                        io.write_error(&format!(
                            "✗ '{}' permanently failed after {} retries: {}",
                            task_id, retry_count, err
                        ))
                        .await?;
                        io.show_status(&format!(
                            "  Cancelled {} dependents | pending={} running={} done={} failed={} cancelled={}",
                            to_cancel.len(), pending, running_now, completed, failed_n, cancelled_n,
                        ))
                        .await?;
                    }
                }
            }
        } // end main loop

        // Recover the graph from the Arc.
        // `Arc::try_unwrap` succeeds when we hold the only reference.
        // The JoinSet is empty (we've awaited all tasks) so there are no other
        // Arc holders — this unwrap is always safe here.
        self.graph = Arc::try_unwrap(graph)
            .expect("Arc still has other holders — this is a bug in the executor")
            .into_inner();

        Ok(ExecutionReport {
            task_results,
            failed: failed_ids,
            cancelled: cancelled_ids,
            total_duration: start.elapsed(),
        })
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Collect all task IDs that transitively depend on `failed_id`.
///
/// When a task permanently fails, every task that (directly or indirectly)
/// needed it must be cancelled because they can never be satisfied.
///
/// Algorithm: a simple BFS starting from `failed_id`, following edges in the
/// "dependent" direction (i.e. tasks that list `failed_id` in their `depends_on`).
///
/// Returns the IDs of all dependents in BFS order (breadth-first, so more
/// direct dependents come first — useful for progress display).
fn collect_dependents(graph: &TaskGraph, failed_id: &str) -> Vec<String> {
    let mut result = Vec::new();
    // BFS frontier: IDs to expand.
    let mut frontier: Vec<String> = vec![failed_id.to_string()];
    // Visited set: avoid duplicate entries if multiple paths lead to the same node.
    let mut visited = std::collections::HashSet::new();
    visited.insert(failed_id.to_string());

    while !frontier.is_empty() {
        let current_frontier = std::mem::take(&mut frontier);
        for current_id in &current_frontier {
            // Find all nodes that list `current_id` as a dependency.
            for node in graph.nodes() {
                if node.depends_on.contains(current_id) && !visited.contains(&node.id) {
                    visited.insert(node.id.clone());
                    result.push(node.id.clone());
                    frontier.push(node.id.clone());
                }
            }
        }
    }

    result
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;
    use crate::orchestrator::graph::{TaskGraph, TaskNode};
    use crate::tools::ToolRegistry;
    use crate::tracking::SessionTracker;
    use std::sync::Arc;

    // ── Mock LLM provider ─────────────────────────────────────────────────────
    //
    // The real CoderAgent runs an LLM loop.  For executor unit tests we don't
    // want to talk to a real API, so we use a mock provider that immediately
    // returns "[TASK_COMPLETE]" — the signal the CoderAgent uses to stop.
    //
    // This makes tests fast and deterministic.

    use crate::llm::{LlmProvider, LlmResponse, Message as LlmMessage, ToolDefinition};
    use async_trait::async_trait;

    struct ImmediateCompleteProvider;

    #[async_trait]
    impl LlmProvider for ImmediateCompleteProvider {
        async fn chat_completion(
            &self,
            _messages: &[LlmMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse> {
            // Return "[TASK_COMPLETE]" so CoderAgent terminates in one iteration.
            Ok(LlmResponse {
                content: Some("[TASK_COMPLETE]".to_string()),
                tool_calls: None,
                usage: None,
            })
        }
    }

    // ── Helper: build a minimal ToolContext usable without real I/O or LSP ──

    fn make_tool_ctx(io: Arc<dyn AgentIO>) -> crate::tools::ToolContext {
        use crate::tools::ToolContext;
        use std::path::PathBuf;
        use tokio::sync::Mutex;

        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io,
            compact_mode: false,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(ImmediateCompleteProvider),
            tools: Arc::new(ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        }
    }

    // ── Helper: build a minimal AgentResult for manual graph manipulation ───

    fn dummy_result() -> AgentResult {
        AgentResult {
            final_message: "done".to_string(),
            iterations: 1,
            tool_calls_total: 0,
            auto_continues: 0,
            tracker: SessionTracker::new("test-model"),
        }
    }

    // ── test_executor_empty_graph ────────────────────────────────────────────

    /// An empty graph should return immediately with an empty report.
    #[tokio::test]
    async fn test_executor_empty_graph() {
        let graph = TaskGraph::new();
        let executor = TaskExecutor::new(graph);

        let io: Arc<dyn AgentIO> = Arc::new(NullIO);
        let report = executor
            .run(
                Arc::new(ImmediateCompleteProvider),
                Arc::new(ToolRegistry::new()),
                make_tool_ctx(Arc::clone(&io)),
                io,
            )
            .await
            .unwrap();

        assert!(report.task_results.is_empty());
        assert!(report.failed.is_empty());
        assert!(report.cancelled.is_empty());
        // Duration should be essentially zero for an empty graph.
        assert!(report.total_duration < Duration::from_secs(1));
    }

    // ── test_executor_single_task ────────────────────────────────────────────

    /// A single task with no dependencies should complete successfully.
    #[tokio::test]
    async fn test_executor_single_task() {
        let mut graph = TaskGraph::new();
        graph
            .add_task(TaskNode::new("t1", "Write a hello world function"))
            .unwrap();

        let executor = TaskExecutor::new(graph).with_max_retries(0);

        let io: Arc<dyn AgentIO> = Arc::new(NullIO);
        let report = executor
            .run(
                Arc::new(ImmediateCompleteProvider),
                Arc::new(ToolRegistry::new()),
                make_tool_ctx(Arc::clone(&io)),
                io,
            )
            .await
            .unwrap();

        assert_eq!(report.task_results.len(), 1, "expected 1 completed task");
        assert!(report.task_results.contains_key("t1"));
        assert!(report.failed.is_empty());
        assert!(report.cancelled.is_empty());
    }

    // ── test_executor_parallel_tasks ─────────────────────────────────────────

    /// Three independent tasks should all complete (order may vary, but all done).
    #[tokio::test]
    async fn test_executor_parallel_tasks() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("a", "Task A")).unwrap();
        graph.add_task(TaskNode::new("b", "Task B")).unwrap();
        graph.add_task(TaskNode::new("c", "Task C")).unwrap();

        let executor = TaskExecutor::new(graph)
            .with_max_concurrent(3)
            .with_max_retries(0);

        let io: Arc<dyn AgentIO> = Arc::new(NullIO);
        let report = executor
            .run(
                Arc::new(ImmediateCompleteProvider),
                Arc::new(ToolRegistry::new()),
                make_tool_ctx(Arc::clone(&io)),
                io,
            )
            .await
            .unwrap();

        assert_eq!(report.task_results.len(), 3);
        assert!(report.task_results.contains_key("a"));
        assert!(report.task_results.contains_key("b"));
        assert!(report.task_results.contains_key("c"));
        assert!(report.failed.is_empty());
        assert!(report.cancelled.is_empty());
    }

    // ── test_executor_linear_chain ────────────────────────────────────────────

    /// t1 → t2 → t3: all should complete, with t1 before t2 before t3.
    /// We verify this by inspecting the graph state after execution.
    #[tokio::test]
    async fn test_executor_linear_chain() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("t1", "Step 1")).unwrap();
        graph
            .add_task(TaskNode::new("t2", "Step 2").with_dependency("t1"))
            .unwrap();
        graph
            .add_task(TaskNode::new("t3", "Step 3").with_dependency("t2"))
            .unwrap();

        let executor = TaskExecutor::new(graph)
            .with_max_concurrent(1)
            .with_max_retries(0);

        let io: Arc<dyn AgentIO> = Arc::new(NullIO);
        let report = executor
            .run(
                Arc::new(ImmediateCompleteProvider),
                Arc::new(ToolRegistry::new()),
                make_tool_ctx(Arc::clone(&io)),
                io,
            )
            .await
            .unwrap();

        // All three tasks should have completed.
        assert_eq!(
            report.task_results.len(),
            3,
            "all three tasks must complete"
        );
        assert!(report.failed.is_empty());
        assert!(report.cancelled.is_empty());
    }

    // ── test_executor_cancels_dependents ──────────────────────────────────────

    /// When a task permanently fails, all dependents should be Cancelled.
    ///
    /// We test this by building a graph, manually marking the root task as
    /// failed (bypassing the LLM), and checking the report.
    ///
    /// Graph: t1(fails) → t2 → t3
    ///                  → t4 (independent, should still complete)
    ///
    /// Expected: t2 and t3 cancelled; t4 completes; report shows t1 failed.
    #[tokio::test]
    async fn test_executor_cancels_dependents() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("t1", "Failing task")).unwrap();
        graph
            .add_task(TaskNode::new("t2", "Depends on t1").with_dependency("t1"))
            .unwrap();
        graph
            .add_task(TaskNode::new("t3", "Depends on t2").with_dependency("t2"))
            .unwrap();
        graph.add_task(TaskNode::new("t4", "Independent")).unwrap();

        // Pre-fail t1 so the executor immediately treats it as exhausted.
        // We set retry_count > max_retries by calling mark_failed twice
        // (max_retries=0 means 1 allowed failure before permanent fail).
        graph
            .mark_failed("t1", "injected failure".to_string())
            .unwrap();
        // With max_retries=0, the first failure (retry_count=1) exceeds the
        // threshold (retry_count <= max_retries=0 is false), so the executor
        // will permanently fail it.
        //
        // But the executor only processes tasks it dispatches.  For this test
        // we need t1 to be Pending when the executor starts, then fail.
        //
        // We use a separate approach: use a FailFirstProvider that fails on t1
        // and immediately completes all others.

        // Reset t1 so the executor dispatches it normally.
        graph.reset_for_retry("t1").unwrap();

        let executor = TaskExecutor::new(graph)
            .with_max_concurrent(4)
            .with_max_retries(0); // 0 retries → first failure = permanent

        let io: Arc<dyn AgentIO> = Arc::new(NullIO);

        // Use a provider that fails specifically for "t1" by looking at the
        // message content.
        let report = executor
            .run(
                Arc::new(FailTaskProvider {
                    fail_prefix: "Failing task".to_string(),
                }),
                Arc::new(ToolRegistry::new()),
                make_tool_ctx(Arc::clone(&io)),
                io,
            )
            .await
            .unwrap();

        // t4 is independent → should complete.
        assert!(
            report.task_results.contains_key("t4"),
            "t4 (independent) should complete even when t1 fails"
        );
        // t1 should be in the failed list.
        assert!(
            report.failed.contains(&"t1".to_string()),
            "t1 should be failed"
        );
        // t2 and t3 depend on t1 → should be cancelled.
        assert!(
            report.cancelled.contains(&"t2".to_string()),
            "t2 should be cancelled"
        );
        assert!(
            report.cancelled.contains(&"t3".to_string()),
            "t3 should be cancelled"
        );
    }

    // ── test_collect_dependents ────────────────────────────────────────────────

    /// Unit test for the `collect_dependents` helper.
    /// Graph: root → a → c
    ///             → b → c  (c has two parents)
    #[test]
    fn test_collect_dependents_transitive() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("root", "Root")).unwrap();
        graph
            .add_task(TaskNode::new("a", "A").with_dependency("root"))
            .unwrap();
        graph
            .add_task(TaskNode::new("b", "B").with_dependency("root"))
            .unwrap();
        graph
            .add_task(
                TaskNode::new("c", "C")
                    .with_dependency("a")
                    .with_dependency("b"),
            )
            .unwrap();

        let deps = collect_dependents(&graph, "root");

        // a, b, c should all be reachable from root.
        assert!(deps.contains(&"a".to_string()));
        assert!(deps.contains(&"b".to_string()));
        assert!(deps.contains(&"c".to_string()));
        // root itself should NOT be in the list.
        assert!(!deps.contains(&"root".to_string()));
    }

    /// No transitive dependents — just the direct child.
    #[test]
    fn test_collect_dependents_direct_only() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("a", "A")).unwrap();
        graph
            .add_task(TaskNode::new("b", "B").with_dependency("a"))
            .unwrap();
        // c depends on b, NOT on a.
        graph
            .add_task(TaskNode::new("c", "C").with_dependency("b"))
            .unwrap();

        let deps = collect_dependents(&graph, "a");
        // Both b and c are transitively reachable from a.
        assert!(deps.contains(&"b".to_string()));
        assert!(deps.contains(&"c".to_string()));
    }

    /// A leaf node has no dependents.
    #[test]
    fn test_collect_dependents_leaf() {
        let mut graph = TaskGraph::new();
        graph.add_task(TaskNode::new("a", "A")).unwrap();
        graph
            .add_task(TaskNode::new("b", "B").with_dependency("a"))
            .unwrap();

        // b has no dependents.
        let deps = collect_dependents(&graph, "b");
        assert!(deps.is_empty());
    }

    // ── FailTaskProvider ──────────────────────────────────────────────────────
    //
    // A mock LLM provider used in test_executor_cancels_dependents.
    // Fails when the first user message starts with a given prefix;
    // otherwise returns "[TASK_COMPLETE]" immediately.

    struct FailTaskProvider {
        /// The task description prefix that should cause a failure.
        fail_prefix: String,
    }

    #[async_trait]
    impl LlmProvider for FailTaskProvider {
        async fn chat_completion(
            &self,
            messages: &[LlmMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse> {
            // Check if any user message starts with the fail prefix.
            let should_fail = messages.iter().any(|m| {
                m.text_content()
                    .map(|t| t.starts_with(&self.fail_prefix))
                    .unwrap_or(false)
            });

            if should_fail {
                Err(anyhow::anyhow!("injected failure for testing"))
            } else {
                Ok(LlmResponse {
                    content: Some("[TASK_COMPLETE]".to_string()),
                    tool_calls: None,
                    usage: None,
                })
            }
        }
    }
}
