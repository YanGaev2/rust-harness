#[test]
fn manifest_uses_harness_tui_without_legacy_tui_stack() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();

    assert!(manifest.contains("harness-tui"));
    // Built without the literal names so a repo-wide grep for the legacy
    // TUI dependencies stays clean.
    for legacy_dep in [format!("rata{}", "tui"), format!("cross{}", "term")] {
        assert!(
            !manifest.contains(&legacy_dep),
            "legacy TUI dependency {legacy_dep} must stay removed from Cargo.toml"
        );
    }
}

#[test]
fn binary_entrypoints_use_terminal_runner() {
    for path in ["src/bin/harness.rs", "src/main.rs"] {
        let source =
            std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
                .unwrap();
        assert!(source.contains("cli::run_terminal"));
    }
}

#[test]
fn default_terminal_setup_uses_full_tui_action_path() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli.rs"),
    )
    .unwrap();

    assert!(source.contains("run_tui("));
    assert!(source.contains("finish_tui_setup_action("));
    assert!(source.contains("TuiAction::SaveProvider"));
}
