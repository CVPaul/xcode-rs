pub mod executor;
/// Orchestrator module: task graph data structures and parallel execution engine.
///
/// # Overview
///
/// The orchestrator enables "fully autonomous delegation" (完全安全托管) — the user
/// can describe a complex, multi-step project and the orchestrator will:
///
/// 1. Decompose it into a **task graph** (DAG of `TaskNode`s with dependency edges)
/// 2. Identify which tasks can run **in parallel** (topological waves)
/// 3. **Execute** each task by spawning a `CoderAgent` for it (Task 26 — executor)
/// 4. **Retry** failed tasks up to a configurable limit
///
/// This module (Task 22) provides only the data structures and graph algorithms.
/// The actual execution engine lives in `graph_executor.rs` (Task 26).
///
/// # Example
///
/// ```ignore
/// use xcodeai::orchestrator::graph::{TaskGraph, TaskNode, TaskStatus};
///
/// let mut graph = TaskGraph::new();
/// graph.add_task(TaskNode::new("t1", "Write tests"))?;
/// graph.add_task(TaskNode::new("t2", "Run CI").with_dependency("t1"))?;
///
/// let waves = graph.compute_waves()?;
/// // waves[0] == ["t1"]   ← runs first
/// // waves[1] == ["t2"]   ← runs after t1 completes
/// ```
pub mod graph;
