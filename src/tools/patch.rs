use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};

pub struct PatchTool;

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff (patch) to a file. More efficient than file_write when making targeted changes to existing files. The patch should be in standard unified diff format."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to patch (relative to working_dir or absolute)"
                },
                "diff": {
                    "type": "string",
                    "description": "The unified diff to apply. Should be in standard unified diff format with @@ hunk headers."
                }
            },
            "required": ["path", "diff"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match args["path"].as_str() {
            Some(p) => p.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'path' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };
        let diff_str = match args["diff"].as_str() {
            Some(d) => d.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'diff' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };
        let path = resolve_path(&path_str, &ctx.working_dir);
        let patch_result = apply_patch(&path, &diff_str);
        match patch_result {
            Ok((hunks, added, removed)) => Ok(ToolResult {
                output: format!("Successfully applied patch to {} ({} hunks applied, +{}/-{})", path_str, hunks, added, removed),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                output: format!("Error applying patch: {}", e),
                is_error: true,
            }),
        }
    }
}

fn resolve_path(path: &str, working_dir: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        working_dir.join(p)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct PatchHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<PatchLine>,
}

#[derive(Debug)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn apply_patch(path: &Path, diff: &str) -> std::result::Result<(usize, usize, usize), String> {
    let mut hunks = Vec::new();
    let mut lines = diff.lines().peekable();
    let mut creating_file = false;
    while let Some(line) = lines.peek() {
        if line.starts_with("--- ") {
            // source_file = Some(line.trim_start_matches("--- ").trim());
            lines.next();
        } else if line.starts_with("+++ ") {
            let f = line.trim_start_matches("+++ ").trim();
            // target_file = Some(f);
            if f == "/dev/null" {
                creating_file = true;
            }
            lines.next();
        } else if line.starts_with("@@ ") {
            match parse_hunk(&mut lines) {
                Ok(hunk) => hunks.push(hunk),
                Err(e) => return Err(format!("Malformed diff hunk: {}", e)),
            }
        } else {
            lines.next();
        }
    }
    // Read file or create new
    let original_content = if creating_file || !path.exists() {
        String::new()
    } else {
        match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Err("File not found and not creating from /dev/null".to_string()),
        }
    };
    let has_trailing_newline = original_content.ends_with('\n') || original_content.is_empty();
    let mut file_lines: Vec<String> = original_content.lines().map(|l| l.to_string()).collect();
    let mut added = 0;
    let mut removed = 0;
    for hunk in &hunks {
        let idx = if hunk.old_count == 0 { hunk.old_start.min(file_lines.len()) } else { hunk.old_start.saturating_sub(1) };
        let mut out = Vec::new();
        let mut file_idx = idx;
        let mut context_mismatch = None;
        for pl in &hunk.lines {
            match pl {
                PatchLine::Context(expected) => {
                    if file_idx >= file_lines.len() || file_lines[file_idx] != *expected {
                        // Fuzzy: allow offset drift of up to 2 lines
                        let mut found = false;
                        for offset in 1..=2 {
                            if file_idx + offset < file_lines.len() && file_lines[file_idx + offset] == *expected {
                                file_idx += offset;
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            context_mismatch = Some((file_idx + 1, expected.clone(), file_lines.get(file_idx).cloned()));
                            break;
                        }
                    }
                    out.push(file_lines[file_idx].clone());
                    file_idx += 1;
                }
                PatchLine::Remove(expected) => {
                    if file_idx >= file_lines.len() || file_lines[file_idx] != *expected {
                        context_mismatch = Some((file_idx + 1, expected.clone(), file_lines.get(file_idx).cloned()));
                        break;
                    }
                    removed += 1;
                    file_idx += 1;
                }
                PatchLine::Add(added_line) => {
                    out.push(added_line.clone());
                    added += 1;
                }
            }
        }
        if let Some((line_num, expected, actual)) = context_mismatch {
            return Err(format!("Context mismatch at line {}: expected '{}', got '{:?}'", line_num, expected, actual));
        }
        // Replace lines in file_lines
        let replace_start = idx;
        let replace_end = file_idx;
        let mut new_file_lines = Vec::new();
        new_file_lines.extend_from_slice(&file_lines[..replace_start]);
        new_file_lines.extend_from_slice(&out);
        new_file_lines.extend_from_slice(&file_lines[replace_end..]);
        file_lines = new_file_lines;
    }
    // Write back
    let mut new_content = file_lines.join("\n");
    if has_trailing_newline && !new_content.is_empty() {
        new_content.push('\n');
    }
    if let Err(e) = fs::write(path, new_content) {
        return Err(format!("Failed to write file: {}", e));
    }
    Ok((hunks.len(), added, removed))
}

fn parse_hunk<'a, I>(lines: &mut std::iter::Peekable<I>) -> std::result::Result<PatchHunk, String>
where
    I: Iterator<Item = &'a str>,
{
    let header = lines.next().ok_or("Missing hunk header")?;
    let mut parts = header.split_whitespace();
    let _at = parts.next();
    let old = parts.next().ok_or("Missing old range")?;
    let new = parts.next().ok_or("Missing new range")?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    // Skip trailing @@
    for p in parts {
        if p == "@@" { break; }
    }
    let mut hunk_lines = Vec::new();
    while let Some(&line) = lines.peek() {
        if line.starts_with("@@ ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            break;
        }
        if let Some(s) = line.strip_prefix(' ') {
            hunk_lines.push(PatchLine::Context(s.to_string()));
        } else if let Some(s) = line.strip_prefix('-') {
            hunk_lines.push(PatchLine::Remove(s.to_string()));
        } else if let Some(s) = line.strip_prefix('+') {
            hunk_lines.push(PatchLine::Add(s.to_string()));
        } else {
            return Err(format!("Malformed hunk line: {}", line));
        }
        lines.next();
    }
    Ok(PatchHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: hunk_lines,
    })
}

fn parse_range(s: &str) -> std::result::Result<(usize, usize), String> {
    let s = s.trim_start_matches(['-', '+']);
    let mut parts = s.split(',');
    let start = parts.next().ok_or("Missing start")?.parse::<usize>().map_err(|_| "Invalid start")?;
    let count = parts.next().unwrap_or("1").parse::<usize>().map_err(|_| "Invalid count")?;
    Ok((start, count))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }
    fn read_file(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn test_patch_simple_change() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("foo.txt");
        write_file(&file, "a\nb\nc\n");
        let diff = "--- a/foo.txt\n+++ b/foo.txt\n@@ -2,1 +2,1 @@\n-b\n+d\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_ok());
        assert_eq!(read_file(&file), "a\nd\nc\n");
    }
    #[test]
    fn test_patch_multiple_hunks() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("bar.txt");
        write_file(&file, "x\ny\nz\n");
        let diff = "--- a/bar.txt\n+++ b/bar.txt\n@@ -1,1 +1,1 @@\n-x\n+a\n@@ -3,1 +3,1 @@\n-z\n+b\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_ok());
        assert_eq!(read_file(&file), "a\ny\nb\n");
    }
    #[test]
    fn test_patch_add_lines() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("add.txt");
        write_file(&file, "1\n2\n");
        let diff = "--- a/add.txt\n+++ b/add.txt\n@@ -2,0 +3,2 @@\n+3\n+4\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_ok());
        assert_eq!(read_file(&file), "1\n2\n3\n4\n");
    }
    #[test]
    fn test_patch_remove_lines() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("rem.txt");
        write_file(&file, "a\nb\nc\n");
        let diff = "--- a/rem.txt\n+++ b/rem.txt\n@@ -2,1 +2,0 @@\n-b\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_ok());
        assert_eq!(read_file(&file), "a\nc\n");
    }
    #[test]
    fn test_patch_context_mismatch() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("ctx.txt");
        write_file(&file, "foo\nbar\nbaz\n");
        let diff = "--- a/ctx.txt\n+++ b/ctx.txt\n@@ -2,1 +2,1 @@\n-xxx\n+yyy\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Context mismatch"));
    }
    #[test]
    fn test_patch_missing_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("missing.txt");
        let diff = "--- a/missing.txt\n+++ b/missing.txt\n@@ -1,0 +1,1 @@\n+hello\n";
        let res = apply_patch(&file, diff);
        assert!(res.is_ok());
        assert_eq!(read_file(&file), "hello\n");
    }
    #[test]
    fn test_patch_missing_params() {
        let ctx = ToolContext {
            working_dir: PathBuf::from("."),
            sandbox_enabled: false,
            io: Arc::new(crate::io::NullIO),
            compact_mode: false,
            lsp_client: Arc::new(tokio::sync::Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(crate::llm::NullLlmProvider),
            tools: Arc::new(crate::tools::ToolRegistry::new()),
        };
        let tool = PatchTool;
        let args = serde_json::json!({"diff": "--- a/foo\n+++ b/foo\n@@ -1,0 +1,1 @@\n+bar\n"});
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(tool.execute(args, &ctx)).unwrap();
        assert!(res.is_error);
        let args2 = serde_json::json!({"path": "foo"});
        let res2 = rt.block_on(tool.execute(args2, &ctx)).unwrap();
        assert!(res2.is_error);
    }
}
