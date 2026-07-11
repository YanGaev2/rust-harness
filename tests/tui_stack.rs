#[test]
fn manifest_uses_ratatui_with_current_crossterm_backend() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();

    assert!(manifest.contains("ratatui"));
    assert!(manifest.contains("crossterm = \"0.29\""));
    assert!(manifest.contains("crossterm_0_29"));
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
