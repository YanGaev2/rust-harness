use std::time::Duration;

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
