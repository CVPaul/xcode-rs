/// Shared helpers for integration tests.
use std::path::Path;
use std::process::Output;

/// Invoke the `xcode` debug binary with the given arguments (synchronous).
///
/// NOTE: Do NOT call this from an async `#[tokio::test]` — use
/// `run_xcode_with_env` (async) instead to avoid blocking the tokio executor.
#[allow(dead_code)]
pub fn run_xcode(args: &[&str]) -> Output {
    let binary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/xcode");
    std::process::Command::new(&binary)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {:?}: {}", binary, e))
}

/// Invoke the `xcode` binary with additional environment variables.
///
/// This is async and uses `tokio::process::Command` so it does not block
/// the tokio executor when called from `#[tokio::test]`.
pub async fn run_xcode_with_env(args: &[&str], env_vars: &[(&str, &str)]) -> Output {
    let binary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/xcode");
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.args(args);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }
    cmd.output()
        .await
        .unwrap_or_else(|e| panic!("Failed to run {:?}: {}", binary, e))
}

/// Convert `Output` to a lossy UTF-8 string for assertions.
pub fn output_to_string(output: &Output) -> String {
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&output.stderr));
    s
}

/// Assert a file exists at `path` with the given `content`.
pub fn assert_file_contains(path: &Path, content: &str) {
    let actual =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("Cannot read {:?}: {}", path, e));
    assert!(
        actual.contains(content),
        "File {:?} does not contain {:?}\nActual:\n{}",
        path,
        content,
        actual
    );
}
