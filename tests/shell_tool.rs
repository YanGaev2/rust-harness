use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use harness_cli::tools::shell::{ShellError, ShellTool};

#[test]
fn shell_tool_runs_command_with_native_profile() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(2));
    let command = native_echo_command();

    let output = tool.run(command).unwrap();

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout.trim(), "harness-shell");

    #[cfg(windows)]
    assert_eq!(output.program, "powershell.exe");

    #[cfg(target_os = "linux")]
    assert_eq!(output.program, "bash");
}

#[cfg(windows)]
#[test]
fn powershell_error_stream_reaches_the_model_as_valid_utf8() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(10));

    // PowerShell 5.1 encodes its piped error stream with the OEM code
    // page by default (CP866 on Russian Windows), which our UTF-8 pipe
    // reader turns into mojibake — the model then retries blindly
    // because the error text is unreadable (seen live in bench traces).
    let output = tool.run("Write-Error \"кириллица-в-ошибке\"").unwrap();

    assert!(
        !output.stderr.contains('\u{FFFD}'),
        "stderr is mojibake: {}",
        output.stderr
    );
    assert!(
        output.stderr.contains("кириллица-в-ошибке"),
        "stderr lost the message: {}",
        output.stderr
    );
}

#[cfg(windows)]
#[test]
fn nested_powershell_error_stream_is_valid_utf8_too() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(20));

    // The bench traces showed models spawning a second `powershell
    // -Command` inside ours; the inner process picks its encoding from
    // the console code page, which our UTF-8 prologue must have set.
    let output = tool
        .run("powershell -NoProfile -Command \"Write-Error 'кириллица-вложенно'\"")
        .unwrap();

    assert!(
        !output.stderr.contains('\u{FFFD}'),
        "nested stderr is mojibake: {}",
        output.stderr
    );
    assert!(
        output.stderr.contains("кириллица-вложенно"),
        "nested stderr lost the message: {}",
        output.stderr
    );
}

#[cfg(windows)]
#[test]
fn oem_bytes_decode_without_replacement_characters() {
    use harness_cli::tools::shell::decode_console_bytes;

    // "кириллица" in CP866 — what PowerShell parse errors look like when
    // they are emitted before any prologue can switch the encoding. The
    // exact decoded text depends on the machine's OEM code page; the
    // guarantee is a clean single-byte decode instead of U+FFFD noise.
    let cp866 = [0xAAu8, 0xA8, 0xE0, 0xA8, 0xAB, 0xAB, 0xA8, 0xE6, 0xA0];
    let decoded = decode_console_bytes(&cp866);
    assert!(!decoded.contains('\u{FFFD}'), "mojibake: {decoded}");
    assert!(!decoded.is_empty());
}

#[test]
fn shell_tool_times_out_instead_of_hanging() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_millis(50));
    let command = native_sleep_command();

    let err = tool.run(command).unwrap_err();

    assert!(err.to_string().contains("timed out"));
}

#[test]
fn shell_tool_timeout_returns_bounded_partial_output() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_millis(750)).with_output_limit(64);
    let command = native_output_then_sleep_command();

    let err = tool.run(command).unwrap_err();

    match err {
        ShellError::TimedOut { output, .. } => {
            assert_eq!(output.exit_code, None);
            assert_eq!(output.stdout, "before-timeout");
            assert_eq!(output.stderr, "");
            assert!(!output.stdout_truncated);
            assert!(!output.stderr_truncated);
            assert_eq!(output.max_output_bytes, 64);
        }
        other => panic!("expected timeout error, got {other}"),
    }
}

#[test]
fn shell_tool_truncates_large_stdout_without_timing_out() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(2)).with_output_limit(4096);
    let command = native_large_stdout_command(70_000);

    let output = tool.run(&command).unwrap();

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout.len(), 4096);
    assert!(output.stdout.chars().all(|ch| ch == 'x'));
    assert!(output.stdout_truncated);
    assert_eq!(output.stderr, "");
    assert!(!output.stderr_truncated);
    assert_eq!(output.max_output_bytes, 4096);
}

#[test]
fn shell_tool_truncates_large_stderr_without_timing_out() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(2)).with_output_limit(2048);
    let command = native_large_stderr_command(70_000);

    let output = tool.run(&command).unwrap();

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stderr.len(), 2048);
    assert!(output.stderr.chars().all(|ch| ch == 'e'));
    assert!(output.stderr_truncated);
    assert_eq!(output.stdout, "");
    assert!(!output.stdout_truncated);
    assert_eq!(output.max_output_bytes, 2048);
}

#[test]
fn shell_tool_returns_after_child_exits_even_if_background_process_holds_pipe() {
    let root = tempfile::tempdir().unwrap();
    let tool = ShellTool::native(root.path(), Duration::from_secs(10));
    // The spawned background process inherits the shell's stdout/stderr pipe
    // handles (observed live: `Start-Process node server.js` with redirects
    // kept the agent frozen for 21+ minutes after powershell itself exited).
    // The tool must return once its direct child is gone, not wait for EOF.
    let command = native_spawn_detached_command();

    let started = Instant::now();
    let output = tool.run(command).unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "run() blocked for {elapsed:?} on a pipe held by the background child"
    );
    assert_eq!(output.exit_code, Some(0));
    assert!(
        output.stdout.contains("spawned"),
        "stdout lost pre-exit output: {:?}",
        output.stdout
    );
}

#[test]
fn shell_tool_cancel_flag_kills_running_command() {
    let root = tempfile::tempdir().unwrap();
    // Timeout far above the cancel point: the early return must come from
    // the flag, not from the timeout machinery.
    let tool = ShellTool::native(root.path(), Duration::from_secs(30));
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::SeqCst);
        })
    };

    let started = Instant::now();
    let err = tool
        .run_with_cancel(native_long_sleep_command(), Some(&cancel))
        .unwrap_err();
    let elapsed = started.elapsed();
    setter.join().unwrap();

    assert!(
        elapsed < Duration::from_secs(5),
        "cancel did not interrupt the command: took {elapsed:?}"
    );
    match err {
        ShellError::Cancelled { output } => assert_eq!(output.exit_code, None),
        other => panic!("expected cancelled error, got {other}"),
    }
}

#[cfg(windows)]
fn native_long_sleep_command() -> &'static str {
    "Start-Sleep -Seconds 30"
}

#[cfg(target_os = "linux")]
fn native_long_sleep_command() -> &'static str {
    "sleep 30"
}

#[cfg(windows)]
fn native_spawn_detached_command() -> &'static str {
    // Redirects force UseShellExecute=false → CreateProcess inherits ALL
    // inheritable handles, including our capture pipes — the live repro.
    "Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 8' \
     -RedirectStandardOutput out.log -RedirectStandardError err.log -WindowStyle Hidden; \
     Write-Output spawned"
}

#[cfg(target_os = "linux")]
fn native_spawn_detached_command() -> &'static str {
    "sleep 8 & printf spawned"
}

#[cfg(windows)]
fn native_echo_command() -> &'static str {
    "Write-Output harness-shell"
}

#[cfg(target_os = "linux")]
fn native_echo_command() -> &'static str {
    "printf harness-shell"
}

#[cfg(windows)]
fn native_sleep_command() -> &'static str {
    "Start-Sleep -Milliseconds 500"
}

#[cfg(target_os = "linux")]
fn native_sleep_command() -> &'static str {
    "sleep 1"
}

#[cfg(windows)]
fn native_output_then_sleep_command() -> &'static str {
    "[Console]::Out.Write('before-timeout'); Start-Sleep -Milliseconds 2000"
}

#[cfg(target_os = "linux")]
fn native_output_then_sleep_command() -> &'static str {
    "printf before-timeout; sleep 2"
}

#[cfg(windows)]
fn native_large_stdout_command(bytes: usize) -> String {
    format!("[Console]::Out.Write(('x' * {bytes}))")
}

#[cfg(target_os = "linux")]
fn native_large_stdout_command(bytes: usize) -> String {
    format!("printf '%*s' {bytes} '' | tr ' ' x")
}

#[cfg(windows)]
fn native_large_stderr_command(bytes: usize) -> String {
    format!("[Console]::Error.Write(('e' * {bytes}))")
}

#[cfg(target_os = "linux")]
fn native_large_stderr_command(bytes: usize) -> String {
    format!("printf '%*s' {bytes} '' | tr ' ' e >&2")
}
