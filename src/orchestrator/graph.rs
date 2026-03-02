// Allow dead_code: all types here are public API for the task graph executor
// (Task 26).  They will be wired in once the executor is implemented.
#![allow(dead_code)]

/// Task graph data structures and algorithms.
///
/// # Key types
///
/// - [`TaskStatus`]  — lifecycle state of a single task
/// - [`TaskNode`]    — one unit of work with dependencies, config, and result storage
/// - [`TaskGraph`]   — directed acyclic graph (DAG) of `TaskNode`s; validates on insert
///
/// # Algorithms
///
/// All graph algorithms operate on a validated DAG (cycles are rejected at insert time).
///
/// | Method | Description |
/// |---|---|
/// | [`TaskGraph::topological_sort`] | Kahn's BFS algorithm → linearised order |
/// | [`TaskGraph::compute_waves`]    | Groups independent tasks into parallel batches |
/// | [`TaskGraph::next_ready`]       | Tasks whose every dependency is `Completed` |
///
/// # Thread-safety
///
/// `TaskGraph` is **not** `Send + Sync` by itself because `AgentResult` is not `Sync`.
/// Wrap it in `Arc<Mutex<TaskGraph>>` when sharing across tokio tasks.
use crate::agent::AgentResult;
use crate::config::AgentConfig;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

// ─── TaskStatus ───────────────────────────────────────────────────────────────

/// The lifecycle state of a single task node.
///
/// Transitions follow a strict FSM:
/// ```text
/// Pending → Running → Completed
///                  ↘ Failed { error, retries }
/// Any → Cancelled
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not yet started.  All dependencies are still pending/running.
    Pending,

    /// Currently being executed by a `CoderAgent`.
    Running,

    /// Successfully finished.  `result` on the `TaskNode` is populated.
    Completed,

    /// Execution failed.  Contains the error message and how many times we
    /// have already retried.  The executor increments `retries` before each
    /// retry attempt.
    Failed {
        /// Human-readable error description (from `anyhow` chain).
        error: String,
        /// Number of retry attempts so far (0 = failed on first try).
        retries: u32,
    },

    /// Explicitly cancelled (e.g. because a dependency failed and the
    /// executor is configured to cancel downstream tasks on failure).
    Cancelled,
}

impl TaskStatus {
    /// Returns `true` when this task is in a terminal state (no further
    /// transitions are expected from the executor's perspective).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed { .. }
        )
    }

    /// Returns `true` only when the task completed successfully.
    pub fn is_completed(&self) -> bool {
        matches!(self, TaskStatus::Completed)
    }

    /// Returns `true` when the task failed (may still be retried).
    pub fn is_failed(&self) -> bool {
        matches!(self, TaskStatus::Failed { .. })
    }
}

// ─── TaskNode ─────────────────────────────────────────────────────────────────

/// A single unit of work in the orchestration graph.
///
/// Each node carries:
/// - A stable string `id` (used in dependency references).
/// - A human-readable `description` for the agent's system context.
/// - The `AgentConfig` that the executor will use when spinning up a `CoderAgent`.
/// - A list of `depends_on` IDs — edges in the DAG.
/// - A mutable `status` updated by the executor as the task progresses.
/// - An optional `result` (populated on success).
///
/// `AgentResult` is skipped in serialisation because it contains non-serialisable
/// futures/handles.  Persist results separately if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNode {
    /// Stable identifier.  Must be unique within a `TaskGraph`.
    /// Use lowercase-kebab-case by convention, e.g. `"write-tests"`.
    pub id: String,

    /// Human-readable description shown to the agent and in progress displays.
    pub description: String,

    /// Current lifecycle state.
    pub status: TaskStatus,

    /// Agent configuration for this specific task (can differ per task —
    /// e.g. a "generate docs" task might use a smaller model or more iterations).
    pub agent_config: AgentConfig,

    /// IDs of tasks that must reach `Completed` before this task can start.
    /// Validated as existing IDs when the node is added to a `TaskGraph`.
    pub depends_on: Vec<String>,

    /// Cumulative retry counter.  Increments every time `mark_failed` is called
    /// and persists across `reset_for_retry` calls so we can enforce a
    /// per-task retry limit in the executor.
    pub retry_count: u32,

    /// Execution result populated by the executor upon success.
    /// Not serialised — reconstruct from session store if needed.
    #[serde(skip)]
    pub result: Option<AgentResult>,
}

impl TaskNode {
    /// Create a minimal `TaskNode` with no dependencies and default `AgentConfig`.
    ///
    /// Use the builder methods (`with_dependency`, `with_config`, `with_description`)
    /// to customise before calling [`TaskGraph::add_task`].
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        TaskNode {
            id: id.into(),
            description: description.into(),
            status: TaskStatus::Pending,
            agent_config: AgentConfig::default(),
            depends_on: Vec::new(),
            retry_count: 0,
            result: None,
        }
    }

    /// Add a single dependency by task ID.
    ///
    /// The referenced task must exist in the `TaskGraph` *by the time this node
    /// is added to the graph* — see [`TaskGraph::add_task`].
    pub fn with_dependency(mut self, dep_id: impl Into<String>) -> Self {
        self.depends_on.push(dep_id.into());
        self
    }

    /// Replace the default `AgentConfig` with a custom one.
    pub fn with_config(mut self, config: AgentConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Replace the description (useful when building nodes programmatically).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

// ─── TaskGraph ────────────────────────────────────────────────────────────────

/// A directed acyclic graph (DAG) of [`TaskNode`]s.
///
/// # Invariants (enforced by [`add_task`](TaskGraph::add_task))
///
/// 1. Every node ID is unique.
/// 2. Every referenced dependency ID must already exist in the graph.
/// 3. No cycles — checked by running Kahn's algorithm after every insert.
///
/// # Storage
///
/// Nodes are stored in a `Vec` (preserving insertion order) together with an
/// ID→index map for O(1) lookups.  The dependency lists are the adjacency
/// representation; we build a reverse map on demand for algorithms that need it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TaskGraph {
    /// All nodes in insertion order.
    nodes: Vec<TaskNode>,

    /// Maps task ID → index into `nodes`.  Kept in sync with `nodes`.
    #[serde(skip)]
    id_to_index: HashMap<String, usize>,
}

impl TaskGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        TaskGraph::default()
    }

    // ── Mutation ──────────────────────────────────────────────────────────────

    /// Add a new task node to the graph.
    ///
    /// # Errors
    ///
    /// - Duplicate ID
    /// - Unknown dependency ID (must be added before the dependent node)
    /// - Cycle detection (added after all edges are registered)
    pub fn add_task(&mut self, node: TaskNode) -> Result<()> {
        // 1. Reject duplicate IDs immediately.
        if self.id_to_index.contains_key(&node.id) {
            bail!("Duplicate task ID: '{}'", node.id);
        }

        // 2. Validate that every referenced dependency already exists.
        for dep_id in &node.depends_on {
            if !self.id_to_index.contains_key(dep_id) {
                bail!(
                    "Task '{}' depends on '{}' which has not been added yet.  \
                     Add dependencies before dependents.",
                    node.id,
                    dep_id
                );
            }
        }

        // 3. Register the node before cycle check so Kahn's algorithm can see it.
        let idx = self.nodes.len();
        self.id_to_index.insert(node.id.clone(), idx);
        self.nodes.push(node);

        // 4. Cycle check — re-runs Kahn on the entire graph.
        //    This is O(V+E) per insertion but graphs are small (<100 tasks).
        self.check_no_cycles()?;

        Ok(())
    }

    /// Mark a task as `Running`.
    ///
    /// # Errors
    /// - Task ID not found
    /// - Task is not in `Pending` state (can only start from Pending)
    pub fn mark_running(&mut self, id: &str) -> Result<()> {
        let node = self.get_mut(id)?;
        if node.status != TaskStatus::Pending {
            bail!(
                "Cannot mark '{}' as Running — current state is {:?}",
                id,
                node.status
            );
        }
        node.status = TaskStatus::Running;
        Ok(())
    }

    /// Mark a task as successfully `Completed` and store its result.
    ///
    /// # Errors
    /// - Task ID not found
    pub fn mark_completed(&mut self, id: &str, result: AgentResult) -> Result<()> {
        let node = self.get_mut(id)?;
        node.status = TaskStatus::Completed;
        node.result = Some(result);
        Ok(())
    }

    /// Mark a task as `Failed`.
    ///
    /// The `retries` counter in the `Failed` variant is initialised to 0 if the
    /// task was previously `Pending`/`Running`, or incremented if it was already
    /// `Failed` (i.e. this is a retry that also failed).
    ///
    /// # Errors
    /// - Task ID not found
    pub fn mark_failed(&mut self, id: &str, error: String) -> Result<()> {
        let node = self.get_mut(id)?;
        // Increment the node's cumulative retry counter so we can enforce a
        // per-task retry limit in the executor even across reset_for_retry cycles.
        node.retry_count += 1;
        // The `retries` field in the Failed variant mirrors the cumulative count
        // so callers can inspect it from the status alone.
        let retries = node.retry_count - 1; // 0-based: first failure = 0 retries
        node.status = TaskStatus::Failed { error, retries };
        Ok(())
    }

    /// Mark a task as `Cancelled`.
    ///
    /// # Errors
    /// - Task ID not found
    pub fn mark_cancelled(&mut self, id: &str) -> Result<()> {
        let node = self.get_mut(id)?;
        node.status = TaskStatus::Cancelled;
        Ok(())
    }

    /// Reset a failed task back to `Pending` so it can be retried.
    ///
    /// The executor calls this before re-queueing a `Failed` task.
    ///
    /// # Errors
    /// - Task ID not found
    /// - Task is not in `Failed` state
    pub fn reset_for_retry(&mut self, id: &str) -> Result<()> {
        let node = self.get_mut(id)?;
        if !node.status.is_failed() {
            bail!("Cannot reset '{}' for retry — not in Failed state", id);
        }
        node.status = TaskStatus::Pending;
        Ok(())
    }

    // ── Read-only access ──────────────────────────────────────────────────────

    /// Returns an iterator over all nodes in insertion order.
    pub fn nodes(&self) -> impl Iterator<Item = &TaskNode> {
        self.nodes.iter()
    }

    /// Returns the number of nodes in the graph.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` if the graph has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Look up a node by ID.  Returns `None` if not found.
    pub fn get(&self, id: &str) -> Option<&TaskNode> {
        self.id_to_index.get(id).map(|&i| &self.nodes[i])
    }

    // ── Graph algorithms ──────────────────────────────────────────────────────

    /// Topological sort using **Kahn's BFS algorithm**.
    ///
    /// Returns a linearised ordering of all task IDs such that every task
    /// appears after all of its dependencies.  If multiple orderings are valid
    /// (i.e. there are independent tasks), the result is deterministic but
    /// arbitrary — use [`compute_waves`](TaskGraph::compute_waves) to get
    /// explicit parallel batches.
    ///
    /// # Errors
    /// Returns an error if a cycle is detected (should not happen if nodes were
    /// added via [`add_task`](TaskGraph::add_task), which validates on insert).
    pub fn topological_sort(&self) -> Result<Vec<String>> {
        // in_degree[i] = number of unprocessed dependencies of nodes[i]
        let mut in_degree = vec![0usize; self.nodes.len()];

        // Build adjacency in the "dep → dependent" direction so we can
        // efficiently decrement in-degrees after processing a node.
        // adj[i] = list of node indices that have nodes[i] as a dependency.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];

        for (idx, node) in self.nodes.iter().enumerate() {
            for dep_id in &node.depends_on {
                // We know dep_id exists (validated at insert time).
                let dep_idx = self.id_to_index[dep_id];
                adj[dep_idx].push(idx); // dep → dependent
                in_degree[idx] += 1;
            }
        }

        // Start with all nodes that have no dependencies (in_degree == 0).
        let mut queue: VecDeque<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        let mut result = Vec::with_capacity(self.nodes.len());

        while let Some(idx) = queue.pop_front() {
            result.push(self.nodes[idx].id.clone());

            // "Remove" the processed node by decrementing its dependents' in-degrees.
            for &dep_idx in &adj[idx] {
                in_degree[dep_idx] -= 1;
                if in_degree[dep_idx] == 0 {
                    queue.push_back(dep_idx);
                }
            }
        }

        if result.len() != self.nodes.len() {
            // Kahn's algorithm leaves nodes with non-zero in-degree when a cycle
            // exists.  This is a belt-and-suspenders check (add_task already
            // validates), but we keep it for safety.
            bail!("Cycle detected in task graph — this is a bug in add_task validation");
        }

        Ok(result)
    }

    /// Compute **parallel execution waves**.
    ///
    /// A "wave" is a group of tasks that:
    /// 1. Have all dependencies in *earlier* waves (so they can run immediately
    ///    once the previous wave finishes), AND
    /// 2. Are independent of each other within the wave (no edge between them).
    ///
    /// This is the critical function for "完全安全托管" — the executor uses these
    /// waves to maximise concurrency: it runs all tasks in wave 0 in parallel,
    /// waits for all to complete, then runs wave 1 in parallel, etc.
    ///
    /// **Algorithm**: assign each node a "wave number" = 1 + max(wave numbers of
    /// its dependencies).  Nodes with no dependencies get wave 0.  This is a
    /// standard "longest path in a DAG" computation.
    ///
    /// # Returns
    ///
    /// A `Vec<Vec<String>>` where:
    /// - `result[0]` = IDs of tasks that can run immediately (no deps)
    /// - `result[1]` = IDs of tasks whose deps are all in `result[0]`
    /// - …and so on
    ///
    /// # Errors
    /// Propagates any cycle error from [`topological_sort`](TaskGraph::topological_sort).
    pub fn compute_waves(&self) -> Result<Vec<Vec<String>>> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }

        // wave_num[i] = which wave node i belongs to
        let mut wave_num = vec![0usize; self.nodes.len()];

        // Process in topological order so all deps are processed before their dependents.
        let topo_ids = self.topological_sort()?;

        for id in &topo_ids {
            let idx = self.id_to_index[id];
            let max_dep_wave = self.nodes[idx]
                .depends_on
                .iter()
                .map(|dep_id| wave_num[self.id_to_index[dep_id]])
                .max()
                .unwrap_or(0);

            // This node goes one wave after its latest dependency.
            wave_num[idx] = if self.nodes[idx].depends_on.is_empty() {
                0
            } else {
                max_dep_wave + 1
            };
        }

        // Group node IDs by wave number.
        let max_wave = *wave_num.iter().max().unwrap_or(&0);
        let mut waves: Vec<Vec<String>> = vec![Vec::new(); max_wave + 1];
        for (idx, node) in self.nodes.iter().enumerate() {
            waves[wave_num[idx]].push(node.id.clone());
        }

        // Preserve topological order within each wave (stable, deterministic).
        // Build a position map from the topological sort.
        let topo_pos: HashMap<&str, usize> = topo_ids
            .iter()
            .enumerate()
            .map(|(pos, id)| (id.as_str(), pos))
            .collect();
        for wave in &mut waves {
            wave.sort_by_key(|id| topo_pos[id.as_str()]);
        }

        Ok(waves)
    }

    /// Returns all tasks that are currently eligible to run.
    ///
    /// A task is "ready" when:
    /// - Its status is `Pending`, AND
    /// - Every task in `depends_on` has status `Completed`.
    ///
    /// The executor calls this after each task completes to find new tasks to
    /// dispatch.
    pub fn next_ready(&self) -> Vec<&TaskNode> {
        self.nodes
            .iter()
            .filter(|node| {
                node.status == TaskStatus::Pending
                    && node.depends_on.iter().all(|dep_id| {
                        self.get(dep_id)
                            .map(|dep| dep.status.is_completed())
                            .unwrap_or(false)
                    })
            })
            .collect()
    }

    /// Returns `true` when every node in the graph is in a terminal state.
    ///
    /// The executor uses this as its loop termination condition.
    pub fn is_finished(&self) -> bool {
        self.nodes.iter().all(|n| n.status.is_terminal())
    }

    /// Returns `true` when every node has status `Completed`.
    pub fn is_all_completed(&self) -> bool {
        self.nodes.iter().all(|n| n.status.is_completed())
    }

    /// Counts tasks by status category (for progress display).
    ///
    /// Returns `(pending, running, completed, failed, cancelled)`.
    pub fn status_counts(&self) -> (usize, usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0, 0);
        for node in &self.nodes {
            match &node.status {
                TaskStatus::Pending => counts.0 += 1,
                TaskStatus::Running => counts.1 += 1,
                TaskStatus::Completed => counts.2 += 1,
                TaskStatus::Failed { .. } => counts.3 += 1,
                TaskStatus::Cancelled => counts.4 += 1,
            }
        }
        counts
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Mutable node lookup — used by the mark_* methods.
    fn get_mut(&mut self, id: &str) -> Result<&mut TaskNode> {
        match self.id_to_index.get(id) {
            Some(&i) => Ok(&mut self.nodes[i]),
            None => bail!("Task ID '{}' not found in graph", id),
        }
    }

    /// Cycle check via Kahn's algorithm.
    ///
    /// Called after every `add_task` to maintain the DAG invariant.
    /// Returns `Ok(())` when no cycle exists, `Err` otherwise.
    fn check_no_cycles(&self) -> Result<()> {
        // Re-use topological_sort_internal for cycle detection.
        // If it can process all nodes, the graph is acyclic.
        let sorted = self.topological_sort_internal();
        if sorted.len() != self.nodes.len() {
            bail!(
                "Cycle detected after adding task '{}' — remove the circular dependency",
                self.nodes.last().map(|n| n.id.as_str()).unwrap_or("?")
            );
        }
        Ok(())
    }

    /// Internal Kahn's sort that returns a partial result (for cycle detection).
    ///
    /// Unlike [`topological_sort`](TaskGraph::topological_sort) this does NOT
    /// return an error — a short result means a cycle was detected.
    fn topological_sort_internal(&self) -> Vec<usize> {
        // in_degree[i] = how many of nodes[i]'s dependencies are unprocessed
        let mut in_degree = vec![0usize; self.nodes.len()];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];

        for (idx, node) in self.nodes.iter().enumerate() {
            for dep_id in &node.depends_on {
                let dep_idx = self.id_to_index[dep_id];
                adj[dep_idx].push(idx);
                in_degree[idx] += 1;
            }
        }

        let mut queue: VecDeque<usize> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        let mut result = Vec::with_capacity(self.nodes.len());
        while let Some(idx) = queue.pop_front() {
            result.push(idx);
            for &dep_idx in &adj[idx] {
                in_degree[dep_idx] -= 1;
                if in_degree[dep_idx] == 0 {
                    queue.push_back(dep_idx);
                }
            }
        }

        result
    }
}

// ─── Serialisation helpers ────────────────────────────────────────────────────

impl TaskGraph {
    /// Rebuild the `id_to_index` map after deserialisation.
    ///
    /// `serde` skips `id_to_index` (marked `#[serde(skip)]`) so we must
    /// rebuild it when loading a saved graph.  Call this immediately after
    /// deserialising:
    ///
    /// ```ignore
    /// let mut graph: TaskGraph = serde_json::from_str(&json)?;
    /// graph.rebuild_index();
    /// ```
    pub fn rebuild_index(&mut self) {
        self.id_to_index.clear();
        for (i, node) in self.nodes.iter().enumerate() {
            self.id_to_index.insert(node.id.clone(), i);
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Helper: build a minimal AgentResult for marking tasks complete
    fn dummy_result() -> AgentResult {
        use crate::tracking::SessionTracker;
        AgentResult {
            final_message: "done".to_string(),
            iterations: 1,
            tool_calls_total: 0,
            auto_continues: 0,
            tracker: SessionTracker::new("test-model"),
        }
    }

    // ── Basic construction ──────────────────────────────────────────────────

    #[test]
    fn test_empty_graph() {
        let g = TaskGraph::new();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
    }

    #[test]
    fn test_add_single_task() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Do something")).unwrap();
        assert_eq!(g.len(), 1);
        let node = g.get("t1").unwrap();
        assert_eq!(node.id, "t1");
        assert_eq!(node.description, "Do something");
        assert_eq!(node.status, TaskStatus::Pending);
    }

    #[test]
    fn test_duplicate_id_rejected() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "First")).unwrap();
        let err = g.add_task(TaskNode::new("t1", "Duplicate")).unwrap_err();
        assert!(err.to_string().contains("Duplicate task ID"));
    }

    #[test]
    fn test_unknown_dependency_rejected() {
        let mut g = TaskGraph::new();
        let err = g
            .add_task(TaskNode::new("t2", "Second").with_dependency("t1"))
            .unwrap_err();
        assert!(err.to_string().contains("has not been added yet"));
    }

    // ── Topological sort ────────────────────────────────────────────────────

    #[test]
    fn test_topological_sort_linear_chain() {
        // t1 → t2 → t3
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Step 1")).unwrap();
        g.add_task(TaskNode::new("t2", "Step 2").with_dependency("t1"))
            .unwrap();
        g.add_task(TaskNode::new("t3", "Step 3").with_dependency("t2"))
            .unwrap();

        let order = g.topological_sort().unwrap();
        // t1 must come before t2, t2 before t3
        let pos: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        assert!(pos["t1"] < pos["t2"]);
        assert!(pos["t2"] < pos["t3"]);
    }

    #[test]
    fn test_topological_sort_diamond() {
        // t1 → t2 → t4
        //   → t3 ↗
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Root")).unwrap();
        g.add_task(TaskNode::new("t2", "Left").with_dependency("t1"))
            .unwrap();
        g.add_task(TaskNode::new("t3", "Right").with_dependency("t1"))
            .unwrap();
        g.add_task(
            TaskNode::new("t4", "Merge")
                .with_dependency("t2")
                .with_dependency("t3"),
        )
        .unwrap();

        let order = g.topological_sort().unwrap();
        assert_eq!(order.len(), 4);
        let pos: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        assert!(pos["t1"] < pos["t2"]);
        assert!(pos["t1"] < pos["t3"]);
        assert!(pos["t2"] < pos["t4"]);
        assert!(pos["t3"] < pos["t4"]);
    }

    #[test]
    fn test_topological_sort_empty() {
        let g = TaskGraph::new();
        let order = g.topological_sort().unwrap();
        assert!(order.is_empty());
    }

    // ── Compute waves ───────────────────────────────────────────────────────

    #[test]
    fn test_compute_waves_no_deps() {
        // All independent — should be one wave
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("a", "A")).unwrap();
        g.add_task(TaskNode::new("b", "B")).unwrap();
        g.add_task(TaskNode::new("c", "C")).unwrap();

        let waves = g.compute_waves().unwrap();
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].len(), 3);
        // All three must be in wave 0
        let in_wave0: HashSet<&str> = waves[0].iter().map(|s| s.as_str()).collect();
        assert!(in_wave0.contains("a"));
        assert!(in_wave0.contains("b"));
        assert!(in_wave0.contains("c"));
    }

    #[test]
    fn test_compute_waves_linear_chain() {
        // t1 → t2 → t3 — should produce three waves
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Step 1")).unwrap();
        g.add_task(TaskNode::new("t2", "Step 2").with_dependency("t1"))
            .unwrap();
        g.add_task(TaskNode::new("t3", "Step 3").with_dependency("t2"))
            .unwrap();

        let waves = g.compute_waves().unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec!["t1"]);
        assert_eq!(waves[1], vec!["t2"]);
        assert_eq!(waves[2], vec!["t3"]);
    }

    #[test]
    fn test_compute_waves_diamond() {
        // t1 is wave 0; t2 and t3 are wave 1 (both only depend on t1);
        // t4 is wave 2 (depends on both t2 and t3, whose max wave is 1).
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Root")).unwrap();
        g.add_task(TaskNode::new("t2", "Left").with_dependency("t1"))
            .unwrap();
        g.add_task(TaskNode::new("t3", "Right").with_dependency("t1"))
            .unwrap();
        g.add_task(
            TaskNode::new("t4", "Merge")
                .with_dependency("t2")
                .with_dependency("t3"),
        )
        .unwrap();

        let waves = g.compute_waves().unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec!["t1"]);
        // t2 and t3 in wave 1 (order may vary)
        assert_eq!(waves[1].len(), 2);
        assert!(waves[1].contains(&"t2".to_string()));
        assert!(waves[1].contains(&"t3".to_string()));
        assert_eq!(waves[2], vec!["t4"]);
    }

    #[test]
    fn test_compute_waves_empty() {
        let g = TaskGraph::new();
        let waves = g.compute_waves().unwrap();
        assert!(waves.is_empty());
    }

    // ── next_ready ──────────────────────────────────────────────────────────

    #[test]
    fn test_next_ready_initial_state() {
        // All tasks start Pending; only those with no deps should be ready
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Root")).unwrap();
        g.add_task(TaskNode::new("t2", "Branch").with_dependency("t1"))
            .unwrap();
        g.add_task(TaskNode::new("t3", "Independent")).unwrap();

        let ready: Vec<&str> = g.next_ready().iter().map(|n| n.id.as_str()).collect();
        // t1 and t3 are ready; t2 is blocked by t1
        assert!(ready.contains(&"t1"));
        assert!(ready.contains(&"t3"));
        assert!(!ready.contains(&"t2"));
    }

    #[test]
    fn test_next_ready_after_completion() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Root")).unwrap();
        g.add_task(TaskNode::new("t2", "Next").with_dependency("t1"))
            .unwrap();

        // Mark t1 completed — now t2 should become ready
        g.mark_completed("t1", dummy_result()).unwrap();
        let ready: Vec<&str> = g.next_ready().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ready, vec!["t2"]);
    }

    // ── Status transitions ──────────────────────────────────────────────────

    #[test]
    fn test_mark_running_and_completed() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Task")).unwrap();

        g.mark_running("t1").unwrap();
        assert_eq!(g.get("t1").unwrap().status, TaskStatus::Running);

        g.mark_completed("t1", dummy_result()).unwrap();
        assert_eq!(g.get("t1").unwrap().status, TaskStatus::Completed);
        assert!(g.get("t1").unwrap().result.is_some());
    }

    #[test]
    fn test_mark_failed_increments_retries() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Task")).unwrap();

        g.mark_failed("t1", "network error".to_string()).unwrap();
        match &g.get("t1").unwrap().status {
            TaskStatus::Failed { retries, .. } => assert_eq!(*retries, 0),
            _ => panic!("expected Failed"),
        }

        // Reset and fail again — retries should increment
        g.reset_for_retry("t1").unwrap();
        g.mark_failed("t1", "timeout".to_string()).unwrap();
        match &g.get("t1").unwrap().status {
            TaskStatus::Failed { retries, .. } => assert_eq!(*retries, 1),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn test_mark_cancelled() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Task")).unwrap();
        g.mark_cancelled("t1").unwrap();
        assert_eq!(g.get("t1").unwrap().status, TaskStatus::Cancelled);
    }

    #[test]
    fn test_cannot_start_from_non_pending() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "Task")).unwrap();
        g.mark_running("t1").unwrap();
        // Can't mark running again while already running
        let err = g.mark_running("t1").unwrap_err();
        assert!(err.to_string().contains("Cannot mark"));
    }

    // ── is_finished / status_counts ─────────────────────────────────────────

    #[test]
    fn test_is_finished() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "A")).unwrap();
        g.add_task(TaskNode::new("t2", "B")).unwrap();

        assert!(!g.is_finished());
        g.mark_completed("t1", dummy_result()).unwrap();
        assert!(!g.is_finished());
        g.mark_cancelled("t2").unwrap();
        assert!(g.is_finished());
    }

    #[test]
    fn test_status_counts() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("t1", "A")).unwrap();
        g.add_task(TaskNode::new("t2", "B")).unwrap();
        g.add_task(TaskNode::new("t3", "C")).unwrap();

        g.mark_completed("t1", dummy_result()).unwrap();
        g.mark_failed("t2", "oops".to_string()).unwrap();

        let (pending, running, completed, failed, cancelled) = g.status_counts();
        assert_eq!(pending, 1); // t3
        assert_eq!(running, 0);
        assert_eq!(completed, 1); // t1
        assert_eq!(failed, 1); // t2
        assert_eq!(cancelled, 0);
    }

    // ── Serialisation roundtrip ─────────────────────────────────────────────

    #[test]
    fn test_serialisation_roundtrip() {
        let mut g = TaskGraph::new();
        g.add_task(TaskNode::new("setup", "Prepare environment"))
            .unwrap();
        g.add_task(TaskNode::new("build", "Compile project").with_dependency("setup"))
            .unwrap();
        g.add_task(TaskNode::new("test", "Run test suite").with_dependency("build"))
            .unwrap();

        // Serialise to JSON
        let json = serde_json::to_string_pretty(&g).unwrap();
        assert!(json.contains("\"setup\""));
        assert!(json.contains("\"build\""));
        assert!(json.contains("\"test\""));

        // Deserialise back
        let mut g2: TaskGraph = serde_json::from_str(&json).unwrap();
        g2.rebuild_index();

        // Graph must still be valid
        assert_eq!(g2.len(), 3);
        let waves = g2.compute_waves().unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec!["setup"]);
        assert_eq!(waves[1], vec!["build"]);
        assert_eq!(waves[2], vec!["test"]);
    }
}
