// src/agent/agents_md.rs
//
// Auto-loading of project-specific agent rules from AGENTS.md files.
//
// Many projects have a file that tells the AI agent how to behave in
// that specific codebase — coding conventions, forbidden patterns, preferred
// libraries, etc.  xcodeai looks for these files at session start and
// prepends their content to the system prompt so the agent is immediately
// aware of project rules.
//
// Search order (first match wins, no concatenation):
//   1. .xcodeai/AGENTS.md   — xcodeai-specific override
//   2. AGENTS.md            — convention shared with opencode / claude-code
//   3. .agents.md           — hidden variant
//   4. agents.md            — lowercase variant
//
// For Rust learners: this module shows idiomatic std::fs::read_to_string
// usage and how to build clean search-order logic without complex state.

use std::path::Path;

/// Search `project_dir` for an AGENTS.md file and return its content.
///
/// Returns `Some(content)` for the first file found in the priority order
/// listed above, or `None` if no file exists.
///
/// # Why first-match-wins?
/// Concatenating multiple files would make the system prompt unpredictable
/// in length and could smuggle conflicting instructions.  A single file is
/// intentional: the most specific file (.xcodeai/AGENTS.md) overrides
/// the generic one (AGENTS.md).
pub fn load_agents_md(project_dir: &Path) -> Option<String> {
    // Priority-ordered list of relative paths to check.
    // The order matters: more specific paths come first.
    let candidates: &[&str] = &[".xcodeai/AGENTS.md", "AGENTS.md", ".agents.md", "agents.md"];

    for relative_path in candidates {
        let full_path = project_dir.join(relative_path);
        if full_path.is_file() {
            match std::fs::read_to_string(&full_path) {
                Ok(content) if !content.trim().is_empty() => {
                    tracing::info!("Loaded AGENTS.md from: {}", full_path.display());
                    return Some(content);
                }
                Ok(_) => {
                    // File exists but is empty — skip it.
                    tracing::debug!("AGENTS.md at {} is empty, skipping", full_path.display());
                }
                Err(e) => {
                    // File exists but can't be read — warn and continue.
                    tracing::warn!("Could not read AGENTS.md at {}: {}", full_path.display(), e);
                }
            }
        }
    }

    None
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a file at `dir/relative_path` with `content`.
    fn write_file(dir: &std::path::Path, relative_path: &str, content: &str) {
        let path = dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    // ── Test: no file → None ─────────────────────────────────────────────────

    #[test]
    fn test_no_agents_md_returns_none() {
        // An empty temp directory has no AGENTS.md, so the function must
        // return None without panicking.
        let dir = tempfile::tempdir().expect("tempdir");
        let result = load_agents_md(dir.path());
        assert!(result.is_none(), "Expected None for empty directory");
    }

    // ── Test: AGENTS.md is found ─────────────────────────────────────────────

    #[test]
    fn test_agents_md_is_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "AGENTS.md", "# Rules\nAlways use snake_case.\n");

        let result = load_agents_md(dir.path());
        assert!(result.is_some(), "Expected Some(content)");
        assert!(result.unwrap().contains("snake_case"));
    }

    // ── Test: search order — .xcodeai/AGENTS.md beats AGENTS.md ─────────────

    #[test]
    fn test_xcodeai_agents_md_takes_priority() {
        // Both files exist; the .xcodeai/ variant must win.
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), ".xcodeai/AGENTS.md", "xcodeai-specific rules");
        write_file(dir.path(), "AGENTS.md", "generic rules");

        let result = load_agents_md(dir.path()).expect("Expected Some");
        assert_eq!(result.trim(), "xcodeai-specific rules");
    }

    // ── Test: AGENTS.md beats .agents.md ────────────────────────────────────

    #[test]
    fn test_agents_md_beats_dot_agents_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "AGENTS.md", "uppercase wins");
        write_file(dir.path(), ".agents.md", "hidden variant");

        let result = load_agents_md(dir.path()).expect("Expected Some");
        assert_eq!(result.trim(), "uppercase wins");
    }

    // ── Test: .agents.md is used when AGENTS.md missing ─────────────────────

    #[test]
    fn test_dot_agents_md_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), ".agents.md", "hidden rules");

        let result = load_agents_md(dir.path()).expect("Expected Some");
        assert_eq!(result.trim(), "hidden rules");
    }

    // ── Test: agents.md (lowercase) is last resort ───────────────────────────

    #[test]
    fn test_lowercase_agents_md_last_resort() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "agents.md", "lowercase rules");

        let result = load_agents_md(dir.path()).expect("Expected Some");
        assert_eq!(result.trim(), "lowercase rules");
    }

    // ── Test: empty file is skipped, falls through to next ───────────────────

    #[test]
    fn test_empty_agents_md_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        // AGENTS.md exists but is blank — should fall through to .agents.md
        write_file(dir.path(), "AGENTS.md", "   \n  \n  ");
        write_file(dir.path(), ".agents.md", "fallback content");

        let result = load_agents_md(dir.path()).expect("Expected Some from fallback");
        assert_eq!(result.trim(), "fallback content");
    }
}
