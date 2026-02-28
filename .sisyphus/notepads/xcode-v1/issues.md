# xcode-v1 Issues & Gotchas

## Known Issues
(none yet — starting fresh)

## Rust Learner Gotchas to Watch For
- async-trait required for traits with async methods
- Box<dyn Tool> requires Send + Sync bounds
- rusqlite Connection is not Send — must use single thread or Arc<Mutex<Connection>>
- reqwest-eventsource: use EventSource::new() + stream() to get SSE events
- tokio::process::Command for async subprocess execution
- tempfile::TempDir for test isolation (auto-deleted on drop)
