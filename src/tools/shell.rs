use std::error::Error;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::platform::{ShellKind, ShellProfile};

pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Runs after the model's PowerShell command and sets the process exit
/// code the way a POSIX shell would: the exit code of the last native
/// program, else success unless a real error happened. `powershell
/// -Command` itself exits 1 whenever `$?` is false, and Windows
/// PowerShell 5.1 sets `$?` to false for every stderr line of a native
/// program merged by `2>&1` (a `NativeCommandError` record): `cargo build
/// 2>&1 | Select-Object -Last 5` reported failure although cargo
/// succeeded, because cargo prints its progress to stderr. It also turned
/// every native exit code into 1. Starts on its own line, so a trailing
/// comment in the command cannot swallow it. The code goes through
/// `$host.SetShouldExit`, not `exit`: PowerShell holds implicit table
/// output (custom objects, e.g. from a calculated property) for ~300 ms to
/// size the columns, and an `exit` before that flush dropped the table.
const POWERSHELL_EXIT_EPILOGUE: &str = "$__harness_ok=$?\n\
if ($global:LASTEXITCODE) { $__harness_code = $global:LASTEXITCODE }\n\
elseif ($__harness_ok -or -not ($Error | Where-Object { \"$($_.FullyQualifiedErrorId)\" -notlike 'NativeCommandError*' })) { $__harness_code = 0 }\n\
else { $__harness_code = 1 }\n\
$host.SetShouldExit($__harness_code)";

#[derive(Debug, Clone)]
pub struct ShellTool {
    cwd: PathBuf,
    timeout: Duration,
    profile: ShellProfile,
    max_output_bytes: usize,
}

impl ShellTool {
    pub fn native(cwd: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self::with_profile(cwd, timeout, ShellProfile::native())
    }

    pub fn with_profile(cwd: impl Into<PathBuf>, timeout: Duration, profile: ShellProfile) -> Self {
        Self {
            cwd: cwd.into(),
            timeout,
            profile,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_output_limit(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes.max(1);
        self
    }

    pub fn run(&self, command: &str) -> Result<ShellOutput, ShellError> {
        self.run_with_cancel(command, None)
    }

    /// `run` with cooperative cancellation: when `cancel` flips to true
    /// (Esc/Ctrl+C in the UI), the child is killed and the captured output
    /// comes back inside [`ShellError::Cancelled`]. Only the direct shell
    /// process is killed — like the timeout path, this is not a tree kill,
    /// so a build the command started keeps running detached.
    pub fn run_with_cancel(
        &self,
        command: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<ShellOutput, ShellError> {
        // Windows shells encode their piped streams with the OEM code page
        // (CP866 on Russian Windows) — the model would receive mojibake
        // error text and retry blindly. Switching the console to UTF-8
        // first also covers nested processes the command spawns.
        let command_buf;
        let command = match self.profile.kind() {
            ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
                command_buf = format!(
                    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
                     $OutputEncoding=[System.Text.Encoding]::UTF8;$global:LASTEXITCODE=0\n\
                     {command}\n{POWERSHELL_EXIT_EPILOGUE}"
                );
                &command_buf
            }
            ShellKind::Cmd => {
                command_buf = format!("chcp 65001>nul & {command}");
                &command_buf
            }
            ShellKind::Bash | ShellKind::PosixSh => command,
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

            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ShellError::Cancelled {
                    output: self.drain_output(None, stdout_reader, stderr_reader)?,
                });
            }

            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ShellError::TimedOut {
                    timeout: self.timeout,
                    output: self.drain_output(None, stdout_reader, stderr_reader)?,
                });
            }

            thread::sleep(Duration::from_millis(5));
        };

        self.drain_output(status.code(), stdout_reader, stderr_reader)
    }

    /// Fold both pipe captures into the final output shape. Called once the
    /// direct child is gone (exited, or killed by timeout/cancel).
    fn drain_output(
        &self,
        exit_code: Option<i32>,
        stdout: Arc<Mutex<PipeState>>,
        stderr: Arc<Mutex<PipeState>>,
    ) -> Result<ShellOutput, ShellError> {
        let stdout = collect_pipe_capture(stdout)?;
        let stderr = collect_pipe_capture(stderr)?;
        Ok(ShellOutput {
            program: self.profile.program().to_string(),
            exit_code,
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
    /// The user interrupted the command (Esc/Ctrl+C); partial output kept.
    Cancelled {
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
            Self::Cancelled { output } => write!(
                f,
                "shell command cancelled by user; captured stdout={} bytes stderr={} bytes",
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
            Self::TimedOut { .. } | Self::Cancelled { .. } => None,
        }
    }
}

#[derive(Debug)]
struct BoundedPipeOutput {
    content: String,
    truncated: bool,
}

/// How long to keep waiting for pipe EOF once the direct child is gone.
/// Background processes the command left behind (servers started via
/// `Start-Process`, `cmd &`) inherit the pipe's write end and can hold it
/// open indefinitely — observed live as a 21-minute freeze after powershell
/// itself had exited. Past the grace the captured bytes are returned as-is
/// and the reader thread is abandoned; it exits on its own when the orphan
/// finally closes the pipe.
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
struct PipeState {
    bytes: Vec<u8>,
    truncated: bool,
    done: bool,
    error: Option<std::io::Error>,
}

fn spawn_pipe_reader<R>(pipe: Option<R>, max_bytes: usize) -> Arc<Mutex<PipeState>>
where
    R: Read + Send + 'static,
{
    let state = Arc::new(Mutex::new(PipeState::default()));
    let shared = state.clone();
    thread::spawn(move || read_pipe_into_state(pipe, max_bytes, &shared));
    state
}

/// A poisoned pipe state still holds valid captured bytes — degrade to using
/// them rather than panicking inside a tool worker (the scheduler treats a
/// dead worker as a bug in the harness itself).
fn lock_state(state: &Mutex<PipeState>) -> MutexGuard<'_, PipeState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wait for the reader to see EOF, but only up to [`PIPE_DRAIN_GRACE`]; a
/// pipe kept open by an orphaned grandchild must not stall the tool result.
fn collect_pipe_capture(state: Arc<Mutex<PipeState>>) -> Result<BoundedPipeOutput, ShellError> {
    let deadline = Instant::now() + PIPE_DRAIN_GRACE;
    while !lock_state(&state).done && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }

    let mut state = lock_state(&state);
    if let Some(error) = state.error.take() {
        return Err(ShellError::Io(error));
    }
    Ok(BoundedPipeOutput {
        content: decode_console_bytes(&state.bytes),
        truncated: state.truncated,
    })
}

fn read_pipe_into_state(pipe: Option<impl Read>, max_bytes: usize, state: &Mutex<PipeState>) {
    let mark_done = |error: Option<std::io::Error>| {
        let mut state = lock_state(state);
        state.error = error;
        state.done = true;
    };

    let Some(mut pipe) = pipe else {
        mark_done(None);
        return;
    };

    let max_bytes = max_bytes.max(1);
    let mut buffer = [0_u8; 8192];

    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                mark_done(Some(error));
                return;
            }
        };

        let mut state = lock_state(state);
        let remaining = max_bytes.saturating_sub(state.bytes.len());
        if remaining > 0 {
            let kept = read.min(remaining);
            state.bytes.extend_from_slice(&buffer[..kept]);
            if kept < read {
                state.truncated = true;
            }
        } else {
            state.truncated = true;
        }
    }

    mark_done(None);
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
