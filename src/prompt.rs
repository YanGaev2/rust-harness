pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a focused coding agent. Keep reasoning compact, verify current files before changing behavior, and prefer small reversible edits. Use native commands for the current operating system. Tool calls may contain imperfect paths or stale assumptions; infer intent conservatively, normalize paths, preserve existing data, and report exact failures. Keep system context small and cache-friendly. Never expose secrets. When a task is incomplete, state the remaining concrete gap."#;

/// The agent-loop system prompt: the stable base plus one environment line so
/// the model knows its OS, shell dialect, and working directory instead of
/// guessing (models otherwise invent Linux paths like `/mnt/<project>` on
/// Windows). The line is constant for a session, so the cache prefix computed
/// over the system prompt stays stable between requests.
pub fn agent_system_prompt(workspace: &std::path::Path) -> String {
    #[cfg(windows)]
    let (os, shell) = (
        "Windows",
        "Windows PowerShell 5.1 ('&&'/'||' are not supported; use ';' or `if ($?) { ... }`)",
    );
    #[cfg(target_os = "linux")]
    let (os, shell) = ("Linux", "bash -lc");
    #[cfg(all(unix, not(target_os = "linux")))]
    let (os, shell) = ("Unix", "sh -lc");

    format!(
        "{DEFAULT_SYSTEM_PROMPT}\n\nEnvironment: {os}; shell: {shell}. Workspace root: {} — commands already run there; `cd` does not persist between shell calls.",
        workspace.display()
    )
}
