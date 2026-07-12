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
        // PowerShell 5.1 encodes its piped streams with the OEM code page
        // (CP866 on Russian Windows) — the model would receive mojibake
        // error text and retry blindly. Switching the console to UTF-8
        // first also covers nested processes the command spawns.
        let command_buf;
        let command = if self.profile.program().contains("powershell") {
            command_buf = format!(
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
                 $OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            );
            &command_buf
        } else {
            command
        };
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
        content: decode_console_bytes(&captured),
        truncated,
    })
}

/// Decode captured pipe bytes: UTF-8 first, then (Windows) the console's
/// OEM code page — PowerShell parse errors are emitted before our UTF-8
/// prologue can run, so they arrive OEM-encoded.
pub fn decode_console_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            #[cfg(windows)]
            if let Some(text) = oem::decode(bytes) {
                return text;
            }
            String::from_utf8_lossy(bytes).to_string()
        }
    }
}

#[cfg(windows)]
mod oem {
    unsafe extern "system" {
        fn GetOEMCP() -> u32;
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            bytes: *const u8,
            byte_len: i32,
            out: *mut u16,
            out_len: i32,
        ) -> i32;
    }

    /// Decode with the active OEM code page via kernel32; `None` on any
    /// conversion failure so the caller can fall back to lossy UTF-8.
    pub fn decode(bytes: &[u8]) -> Option<String> {
        if bytes.is_empty() || bytes.len() > i32::MAX as usize {
            return None;
        }
        let code_page = unsafe { GetOEMCP() };
        let len = bytes.len() as i32;
        let needed = unsafe {
            MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0)
        };
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0_u16; needed as usize];
        let written = unsafe {
            MultiByteToWideChar(code_page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed)
        };
        if written != needed {
            return None;
        }
        Some(String::from_utf16_lossy(&wide))
    }
}
