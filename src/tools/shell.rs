use std::error::Error;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::platform::ShellProfile;

pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ShellTool {
    cwd: PathBuf,
    timeout: Duration,
    profile: ShellProfile,
    max_output_bytes: usize,
}

impl ShellTool {
    pub fn native(cwd: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            cwd: cwd.into(),
            timeout,
            profile: ShellProfile::native(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_output_limit(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }

    pub fn run(&self, command: &str) -> Result<ShellOutput, ShellError> {
        let mut child = Command::new(self.profile.program())
            .args(self.profile.args())
            .arg(command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ShellError::Io)?;

        let stdout_reader = spawn_pipe_reader(child.stdout.take(), self.max_output_bytes);
        let stderr_reader = spawn_pipe_reader(child.stderr.take(), self.max_output_bytes);

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(ShellError::Io)? {
                break status;
            }

            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let stdout = join_pipe_reader(stdout_reader)?;
                let stderr = join_pipe_reader(stderr_reader)?;
                return Err(ShellError::TimedOut {
                    timeout: self.timeout,
                    output: ShellOutput {
                        program: self.profile.program().to_string(),
                        exit_code: None,
                        stdout: stdout.content,
                        stderr: stderr.content,
                        stdout_truncated: stdout.truncated,
                        stderr_truncated: stderr.truncated,
                        max_output_bytes: self.max_output_bytes,
                    },
                });
            }

            thread::sleep(Duration::from_millis(5));
        };

        let stdout = join_pipe_reader(stdout_reader)?;
        let stderr = join_pipe_reader(stderr_reader)?;

        Ok(ShellOutput {
            program: self.profile.program().to_string(),
            exit_code: status.code(),
            stdout: stdout.content,
            stderr: stderr.content,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            max_output_bytes: self.max_output_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellOutput {
    pub program: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub max_output_bytes: usize,
}

#[derive(Debug)]
pub enum ShellError {
    TimedOut {
        timeout: Duration,
        output: ShellOutput,
    },
    Io(std::io::Error),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut { timeout, output } => write!(
                f,
                "shell command timed out after {timeout:?}; captured stdout={} bytes stderr={} bytes",
                output.stdout.len(),
                output.stderr.len()
            ),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ShellError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::TimedOut { .. } => None,
        }
    }
}

#[derive(Debug)]
struct BoundedPipeOutput {
    content: String,
    truncated: bool,
}

fn spawn_pipe_reader<R>(
    pipe: Option<R>,
    max_bytes: usize,
) -> JoinHandle<Result<BoundedPipeOutput, std::io::Error>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || read_pipe_bounded(pipe, max_bytes))
}

fn join_pipe_reader(
    reader: JoinHandle<Result<BoundedPipeOutput, std::io::Error>>,
) -> Result<BoundedPipeOutput, ShellError> {
    reader
        .join()
        .map_err(|_| ShellError::Io(std::io::Error::other("shell output reader panicked")))?
        .map_err(ShellError::Io)
}

fn read_pipe_bounded(
    pipe: Option<impl Read>,
    max_bytes: usize,
) -> Result<BoundedPipeOutput, std::io::Error> {
    let Some(mut pipe) = pipe else {
        return Ok(BoundedPipeOutput {
            content: String::new(),
            truncated: false,
        });
    };

    let max_bytes = max_bytes.max(1);
    let mut captured = Vec::with_capacity(max_bytes.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        let remaining = max_bytes.saturating_sub(captured.len());
        if remaining > 0 {
            let kept = read.min(remaining);
            captured.extend_from_slice(&buffer[..kept]);
            if kept < read {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    Ok(BoundedPipeOutput {
        content: String::from_utf8_lossy(&captured).to_string(),
        truncated,
    })
}
