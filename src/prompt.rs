use crate::platform::ShellProfile;

pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a focused coding agent. Keep reasoning compact, verify current files before changing behavior, and prefer small reversible edits. Use native commands for the current operating system. Tool calls may contain imperfect paths or stale assumptions; infer intent conservatively, normalize paths, preserve existing data, and report exact failures. Keep system context small and cache-friendly. Never expose secrets. When a task is incomplete, state the remaining concrete gap."#;

/// The agent-loop system prompt: the stable base plus one environment line so
/// the model knows its OS, shell dialect, and working directory instead of
/// guessing (models otherwise invent Linux paths like `/mnt/<project>` on
/// Windows, or cmd idioms in PowerShell). The shell comes from the detected
/// profile — the same one the runtime actually executes with — and is
/// constant for a session, so the cache prefix computed over the system
/// prompt stays stable between requests.
pub fn agent_system_prompt(workspace: &std::path::Path, shell: Option<&ShellProfile>) -> String {
    #[cfg(windows)]
    let os = "Windows";
    #[cfg(target_os = "linux")]
    let os = "Linux";
    #[cfg(all(unix, not(target_os = "linux")))]
    let os = "Unix";

    match shell {
        Some(profile) => format!(
            "{DEFAULT_SYSTEM_PROMPT}\n\nEnvironment: {os}; shell: {dialect}. Workspace root: {} — commands already run there; `cd` does not persist between shell calls.",
            workspace.display(),
            dialect = profile.dialect_note(),
        ),
        None => format!(
            "{DEFAULT_SYSTEM_PROMPT}\n\nEnvironment: {os}; no shell is available — use the file tools. Workspace root: {}.",
            workspace.display(),
        ),
    }
}
