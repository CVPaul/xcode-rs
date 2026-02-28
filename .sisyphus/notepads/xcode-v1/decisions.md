# xcode-v1 Decisions

## 2026-02-28 — Initial Setup
- Git initialized (main branch), single crate, no workspace
- worktree_path = /volume/pt-data/xqli/xcode (main directory)
- Execution order: Wave 1 (T1+T2+T3 parallel), Wave 2 (T4+T5+T6 parallel), Wave 3 (T7+T8 parallel), Wave 4 (T9, then T10), Final (F1-F4 parallel)
- T1 must complete before T2/T3 can start (they depend on Cargo.toml + module stubs)
- No worktree branching needed — single branch (main) for v1
