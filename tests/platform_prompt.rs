use harness_cli::platform::ShellProfile;
use harness_cli::prompt::{DEFAULT_SYSTEM_PROMPT, agent_system_prompt};

#[test]
fn default_system_prompt_stays_under_one_thousand_words() {
    let words = DEFAULT_SYSTEM_PROMPT.split_whitespace().count();

    assert!(words <= 1000, "prompt has {words} words");
}

#[test]
fn agent_system_prompt_describes_the_environment() {
    let prompt = agent_system_prompt(std::path::Path::new("F:/rust-harness"));

    // The base prompt is kept verbatim, followed by the environment line.
    assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
    assert!(prompt.contains("F:/rust-harness"));
    // The model may cd wherever it likes; what it must know is that each shell
    // call starts fresh in the workspace root.
    assert!(prompt.contains("cd` does not persist"));

    #[cfg(windows)]
    {
        assert!(prompt.contains("Windows"));
        assert!(prompt.contains("PowerShell"));
        assert!(prompt.contains("&&"), "warn about the missing operator");
    }
    #[cfg(target_os = "linux")]
    assert!(prompt.contains("bash"));

    let words = prompt.split_whitespace().count();
    assert!(words <= 1000, "prompt has {words} words");
}

#[test]
fn agent_system_prompt_is_stable_for_a_session() {
    // The prompt feeds the provider cache prefix; two builds for the same
    // workspace must be byte-identical.
    let first = agent_system_prompt(std::path::Path::new("F:/rust-harness"));
    let second = agent_system_prompt(std::path::Path::new("F:/rust-harness"));
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
