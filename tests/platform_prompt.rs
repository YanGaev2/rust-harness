use harness_cli::platform::{OsFamily, ShellKind, ShellProfile};
use harness_cli::prompt::{DEFAULT_SYSTEM_PROMPT, agent_system_prompt};

/// A directory holding fake shell executables for `detect_in`.
fn shell_dir(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        std::fs::write(dir.path().join(name), "stub").unwrap();
    }
    dir
}

fn no_probe(_: &std::path::Path) -> Option<u32> {
    panic!("powershell version probe must not run in this scenario");
}

#[test]
fn detect_prefers_pwsh7_on_windows_without_probing_version() {
    let dir = shell_dir(&["pwsh.exe", "powershell.exe", "cmd.exe"]);

    let profile =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &no_probe)
            .expect("pwsh must be detected");

    assert_eq!(profile.kind(), ShellKind::Pwsh);
    assert!(profile.dialect_note().contains("PowerShell 7"));
    assert!(profile.program().ends_with("pwsh.exe"));
}

#[test]
fn detect_uses_windows_powershell_when_probe_reports_modern_version() {
    let dir = shell_dir(&["powershell.exe", "cmd.exe"]);

    let profile =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &|_path| {
            Some(5)
        })
        .expect("powershell must be detected");

    assert_eq!(profile.kind(), ShellKind::WindowsPowerShell);
    assert!(profile.dialect_note().contains("5.1"));
    assert!(
        profile.dialect_note().contains("&&"),
        "note must warn about the missing operator"
    );
}

#[test]
fn detect_falls_back_to_cmd_when_powershell_is_legacy_or_broken() {
    let dir = shell_dir(&["powershell.exe", "cmd.exe"]);

    // Windows 7 era PowerShell 2.0: prefer cmd.exe, which the model knows well.
    let legacy =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &|_path| {
            Some(2)
        })
        .expect("cmd must be detected");
    assert_eq!(legacy.kind(), ShellKind::Cmd);
    assert!(legacy.dialect_note().contains("cmd.exe"));

    // A powershell.exe that cannot even report a version is unusable.
    let broken =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &|_path| {
            None
        })
        .expect("cmd must be detected");
    assert_eq!(broken.kind(), ShellKind::Cmd);
}

#[test]
fn detect_prefers_bash_and_falls_back_to_posix_sh_on_unix() {
    let both = shell_dir(&["bash", "sh"]);
    let profile = ShellProfile::detect_in(OsFamily::Unix, &[both.path().to_path_buf()], &no_probe)
        .expect("bash must be detected");
    assert_eq!(profile.kind(), ShellKind::Bash);

    // Alpine/busybox containers ship sh but not bash.
    let sh_only = shell_dir(&["sh"]);
    let profile =
        ShellProfile::detect_in(OsFamily::Unix, &[sh_only.path().to_path_buf()], &no_probe)
            .expect("sh must be detected");
    assert_eq!(profile.kind(), ShellKind::PosixSh);
    assert!(profile.dialect_note().contains("POSIX sh"));
}

#[test]
fn detect_reports_no_shell_in_a_bare_environment() {
    let empty = shell_dir(&[]);

    let windows =
        ShellProfile::detect_in(OsFamily::Windows, &[empty.path().to_path_buf()], &|_| None);
    let unix = ShellProfile::detect_in(OsFamily::Unix, &[empty.path().to_path_buf()], &no_probe);

    assert!(windows.is_none());
    assert!(unix.is_none());
}

#[test]
fn detected_profile_matches_this_machine() {
    // On any machine that runs this suite a real shell exists, and repeated
    // calls must return the identical cached profile (cache prefix stability).
    let first = ShellProfile::detected().expect("test hosts always have a shell");
    let second = ShellProfile::detected().unwrap();
    assert_eq!(first, second);

    #[cfg(windows)]
    assert!(matches!(
        first.kind(),
        ShellKind::Pwsh | ShellKind::WindowsPowerShell | ShellKind::Cmd
    ));
    #[cfg(unix)]
    assert!(matches!(first.kind(), ShellKind::Bash | ShellKind::PosixSh));
}

#[test]
fn default_system_prompt_stays_under_one_thousand_words() {
    let words = DEFAULT_SYSTEM_PROMPT.split_whitespace().count();

    assert!(words <= 1000, "prompt has {words} words");
}

#[test]
fn agent_system_prompt_describes_the_detected_shell() {
    let dir = shell_dir(&["powershell.exe", "cmd.exe"]);
    let profile =
        ShellProfile::detect_in(OsFamily::Windows, &[dir.path().to_path_buf()], &|_path| {
            Some(5)
        })
        .unwrap();

    let prompt = agent_system_prompt(std::path::Path::new("F:/rust-harness"), Some(&profile));

    // The base prompt is kept verbatim, followed by the environment line.
    assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
    assert!(prompt.contains("F:/rust-harness"));
    // The model may cd wherever it likes; what it must know is that each shell
    // call starts fresh in the workspace root.
    assert!(prompt.contains("cd` does not persist"));
    // The dialect line comes from the profile, not from compile-time guesses.
    assert!(prompt.contains("PowerShell 5.1"));
    assert!(prompt.contains("&&"), "warn about the missing operator");

    let words = prompt.split_whitespace().count();
    assert!(words <= 1000, "prompt has {words} words");
}

#[test]
fn agent_system_prompt_explains_a_missing_shell() {
    // Distroless containers have no shell at all; the model must know to lean
    // on the file tools instead of retrying shell calls.
    let prompt = agent_system_prompt(std::path::Path::new("/workspace"), None);

    assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
    assert!(prompt.contains("no shell is available"));
    assert!(prompt.contains("/workspace"));
}

#[test]
fn agent_system_prompt_describes_harness_config_and_attachments() {
    // The model runs inside the harness CLI and gets asked about it; without
    // these lines it guesses config paths instead of knowing them.
    let profile = ShellProfile::native();
    let prompt = agent_system_prompt(std::path::Path::new("F:/rust-harness"), Some(&profile));

    assert!(prompt.contains(".harness/providers.json"));
    assert!(prompt.contains("harness provider"));
    assert!(prompt.contains(".harness/attachments"));
    assert!(prompt.contains("attachment.read"));

    // Also present when no shell is available — the harness facts do not
    // depend on the shell profile.
    let no_shell = agent_system_prompt(std::path::Path::new("/workspace"), None);
    assert!(no_shell.contains(".harness/providers.json"));
}

#[test]
fn agent_system_prompt_is_stable_for_a_session() {
    // The prompt feeds the provider cache prefix; two builds for the same
    // workspace must be byte-identical.
    let profile = ShellProfile::native();
    let first = agent_system_prompt(std::path::Path::new("F:/rust-harness"), Some(&profile));
    let second = agent_system_prompt(std::path::Path::new("F:/rust-harness"), Some(&profile));
    assert_eq!(first, second);
}

#[test]
fn native_shell_profile_matches_current_os() {
    let profile = ShellProfile::native();

    #[cfg(windows)]
    {
        assert_eq!(profile.program(), "powershell.exe");
        assert!(profile.args().contains(&"-NoProfile".to_string()));
    }

    #[cfg(target_os = "linux")]
    {
        assert_eq!(profile.program(), "bash");
        assert!(profile.args().contains(&"-lc".to_string()));
    }
}
