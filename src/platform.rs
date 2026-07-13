use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Which family of shell candidates to search. Kept explicit (instead of
/// `#[cfg]` inside the detector) so every branch is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Windows,
    Unix,
}

impl OsFamily {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(not(windows))]
        {
            Self::Unix
        }
    }
}

/// The interpreter dialect the model must be told about. Shell probe
/// 2026-07-13: the model writes both PowerShell and cmd near-perfectly when
/// it knows which one it is writing, and mixes them up when it has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// PowerShell 7+ (`pwsh`): `&&`/`||` work.
    Pwsh,
    /// Windows PowerShell 5.1 and older: `&&`/`||` are parse errors.
    WindowsPowerShell,
    /// cmd.exe batch — the fallback when PowerShell is legacy (2.0) or broken.
    Cmd,
    Bash,
    /// busybox/dash `sh` — docker images without bash.
    PosixSh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    program: String,
    args: Vec<String>,
    kind: ShellKind,
}

const POWERSHELL_ARGS: &[&str] = &[
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

impl ShellProfile {
    /// The compile-time default: assumed native shell without probing the
    /// machine. Used by tests and as the base for `ToolRuntime::new`.
    pub fn native() -> Self {
        #[cfg(windows)]
        {
            Self::with_kind("powershell.exe", ShellKind::WindowsPowerShell)
        }

        #[cfg(target_os = "linux")]
        {
            Self::with_kind("bash", ShellKind::Bash)
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            Self::with_kind("sh", ShellKind::PosixSh)
        }
    }

    fn with_kind(program: impl Into<String>, kind: ShellKind) -> Self {
        let args = match kind {
            ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
                POWERSHELL_ARGS.iter().map(|s| s.to_string()).collect()
            }
            ShellKind::Cmd => vec!["/d".to_string(), "/c".to_string()],
            ShellKind::Bash | ShellKind::PosixSh => vec!["-lc".to_string()],
        };
        Self {
            program: program.into(),
            args,
            kind,
        }
    }

    /// The shell that actually exists on this machine, probed once per
    /// process and cached (the dialect line feeds the provider cache prefix,
    /// so it must not change within a session). `None` means no shell at all
    /// (distroless containers) — the shell tool should not be advertised.
    pub fn detected() -> Option<&'static ShellProfile> {
        static DETECTED: OnceLock<Option<ShellProfile>> = OnceLock::new();
        DETECTED
            .get_or_init(|| {
                ShellProfile::detect_in(
                    OsFamily::current(),
                    &env_path_dirs(),
                    &probe_powershell_major,
                )
            })
            .as_ref()
    }

    /// Deterministic detector core. `powershell_major` reports the major
    /// version of a `powershell.exe` found on disk (spawning it is the only
    /// way to learn whether it is 5.1 or a Windows 7 era 2.0), injected so
    /// tests can cover every branch without spawning anything.
    pub fn detect_in(
        family: OsFamily,
        path_dirs: &[PathBuf],
        powershell_major: &dyn Fn(&Path) -> Option<u32>,
    ) -> Option<Self> {
        match family {
            OsFamily::Windows => {
                if let Some(pwsh) = find_program(path_dirs, "pwsh.exe") {
                    return Some(Self::with_kind(pwsh, ShellKind::Pwsh));
                }
                let powershell = find_program(path_dirs, "powershell.exe");
                let cmd = find_program(path_dirs, "cmd.exe");
                if let Some(ref path) = powershell {
                    // 3.0 is the oldest dialect close enough to 5.1; below
                    // that (or unprobeable) the model's cmd knowledge beats
                    // a PowerShell it would constantly misjudge.
                    match powershell_major(Path::new(path)) {
                        Some(major) if major >= 3 => {
                            return Some(Self::with_kind(
                                path.clone(),
                                ShellKind::WindowsPowerShell,
                            ));
                        }
                        _ => {}
                    }
                }
                if let Some(cmd) = cmd {
                    return Some(Self::with_kind(cmd, ShellKind::Cmd));
                }
                None
            }
            OsFamily::Unix => {
                if let Some(bash) = find_program(path_dirs, "bash") {
                    return Some(Self::with_kind(bash, ShellKind::Bash));
                }
                find_program(path_dirs, "sh").map(|sh| Self::with_kind(sh, ShellKind::PosixSh))
            }
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn kind(&self) -> ShellKind {
        self.kind
    }

    /// One line of truth about the interpreter, phrased for the model. Used
    /// verbatim in both the system prompt and the shell tool description so
    /// the model never has to guess the dialect.
    pub fn dialect_note(&self) -> &'static str {
        match self.kind {
            ShellKind::Pwsh => "PowerShell 7 (pwsh); '&&' and '||' are supported",
            ShellKind::WindowsPowerShell => {
                "Windows PowerShell 5.1 ('&&'/'||' are not supported; use ';' or `if ($?) { ... }`)"
            }
            ShellKind::Cmd => "Windows cmd.exe (batch syntax; PowerShell cmdlets are unavailable)",
            ShellKind::Bash => "bash",
            ShellKind::PosixSh => "POSIX sh (bash is not available; avoid bash-only syntax)",
        }
    }
}

fn find_program(path_dirs: &[PathBuf], name: &str) -> Option<String> {
    path_dirs
        .iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(|path| path.display().to_string())
}

fn env_path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// Ask a concrete powershell.exe for its major version. One spawn per
/// process (guarded by the `detected` cache); `None` when the binary cannot
/// even answer, which the detector treats as "unusable".
fn probe_powershell_major(path: &Path) -> Option<u32> {
    let output = std::process::Command::new(path)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.Major",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}
