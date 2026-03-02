use anyhow::Result;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::time::Duration;

/// Result from executing a command in a sandbox or directly.
#[derive(Debug)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

/// No-sandbox executor: runs commands directly via sh -c in the working directory.
pub struct NoSandbox {
    pub working_dir: PathBuf,
}

impl NoSandbox {
    pub fn new(working_dir: PathBuf) -> Self {
        NoSandbox { working_dir }
    }

    pub async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult> {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Take stdout/stderr handles BEFORE consuming child ownership
        let mut stdout_handle = child.stdout.take().expect("stdout piped");
        let mut stderr_handle = child.stderr.take().expect("stderr piped");

        let timeout_dur = Duration::from_secs(timeout_secs);

        // Read stdout/stderr concurrently while waiting for process exit
        let read_stdout = async {
            let mut buf = Vec::new();
            let _ = stdout_handle.read_to_end(&mut buf).await;
            buf
        };
        let read_stderr = async {
            let mut buf = Vec::new();
            let _ = stderr_handle.read_to_end(&mut buf).await;
            buf
        };

        // Wait for the process with a timeout
        match tokio::time::timeout(timeout_dur, async {
            let (stdout_bytes, stderr_bytes) = tokio::join!(read_stdout, read_stderr);
            let status = child.wait().await?;
            Ok::<_, anyhow::Error>((stdout_bytes, stderr_bytes, status))
        })
        .await
        {
            Ok(Ok((stdout_bytes, stderr_bytes, status))) => {
                let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
                let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                let exit_code = status.code().unwrap_or(-1);
                Ok(ExecResult {
                    stdout,
                    stderr,
                    exit_code,
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(e),
            Err(_elapsed) => {
                // Timeout: kill the child process (best-effort)
                let _ = child.kill().await;
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    timed_out: true,
                })
            }
        }
    }
}

/// sbox-backed sandbox session.
#[allow(dead_code)] // Future sandbox integration — not yet wired up
pub struct SboxSession {
    pub session_name: String,
    pub project_dir: PathBuf,
    pub sbox_path: String,
    pub is_initialized: bool,
}

#[allow(dead_code)] // Future sandbox integration — not yet wired up
impl SboxSession {
    pub fn new(session_name: String, project_dir: PathBuf, sbox_path: String) -> Self {
        SboxSession {
            session_name,
            project_dir,
            sbox_path,
            is_initialized: false,
        }
    }

    /// Create the sbox session and mount the project directory.
    pub fn init(&mut self) -> Result<()> {
        // sbox create <session_name>
        let status = std::process::Command::new(&self.sbox_path)
            .args(["create", &self.session_name])
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run sbox create: {}", e))?;

        if !status.success() {
            return Err(anyhow::anyhow!(
                "sbox create failed with exit code: {:?}",
                status.code()
            ));
        }

        // sbox mount <session_name> <project_dir> /project
        let project_str = self.project_dir.to_string_lossy().to_string();
        let status = std::process::Command::new(&self.sbox_path)
            .args(["mount", &self.session_name, &project_str, "/project"])
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run sbox mount: {}", e))?;

        if !status.success() {
            return Err(anyhow::anyhow!(
                "sbox mount failed with exit code: {:?}",
                status.code()
            ));
        }

        self.is_initialized = true;
        Ok(())
    }

    /// Execute a command inside the sbox session.
    pub async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult> {
        let mut child = tokio::process::Command::new(&self.sbox_path)
            .args(["exec", &self.session_name, "--", "sh", "-c", command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Take handles before any ownership move
        let mut stdout_handle = child.stdout.take().expect("stdout piped");
        let mut stderr_handle = child.stderr.take().expect("stderr piped");

        let timeout_dur = Duration::from_secs(timeout_secs);

        let read_stdout = async {
            let mut buf = Vec::new();
            let _ = stdout_handle.read_to_end(&mut buf).await;
            buf
        };
        let read_stderr = async {
            let mut buf = Vec::new();
            let _ = stderr_handle.read_to_end(&mut buf).await;
            buf
        };

        match tokio::time::timeout(timeout_dur, async {
            let (stdout_bytes, stderr_bytes) = tokio::join!(read_stdout, read_stderr);
            let status = child.wait().await?;
            Ok::<_, anyhow::Error>((stdout_bytes, stderr_bytes, status))
        })
        .await
        {
            Ok(Ok((stdout_bytes, stderr_bytes, status))) => {
                let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
                let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                let exit_code = status.code().unwrap_or(-1);
                Ok(ExecResult {
                    stdout,
                    stderr,
                    exit_code,
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(e),
            Err(_elapsed) => {
                let _ = child.kill().await;
                Ok(ExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    timed_out: true,
                })
            }
        }
    }

    /// Destroy the sbox session (best-effort cleanup).
    pub fn destroy(&mut self) -> Result<()> {
        let status = std::process::Command::new(&self.sbox_path)
            .args(["destroy", &self.session_name])
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                tracing::warn!(
                    "sbox destroy returned non-zero exit code {:?} for session {}",
                    s.code(),
                    self.session_name
                );
            }
            Err(e) => {
                tracing::warn!(
                    "sbox destroy failed for session {}: {}",
                    self.session_name,
                    e
                );
            }
        }
        self.is_initialized = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_nosandbox_exec_simple() {
        let sandbox = NoSandbox::new(std::path::PathBuf::from("/tmp"));
        let result = sandbox.exec("echo hello", 10).await.unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_nosandbox_exec_exit_code() {
        let sandbox = NoSandbox::new(std::path::PathBuf::from("/tmp"));
        let result = sandbox.exec("exit 42", 10).await.unwrap();
        assert_eq!(result.exit_code, 42);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_nosandbox_timeout() {
        let sandbox = NoSandbox::new(std::path::PathBuf::from("/tmp"));
        let result = sandbox.exec("sleep 10", 1).await.unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }
}
