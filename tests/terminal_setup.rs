use harness_cli::terminal_setup::{PROFILE_NAME, fragment_dir, fragment_json, install, remove};

const SCHEME_COLORS: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "purple",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightPurple",
    "brightCyan",
    "brightWhite",
];

fn is_hex_color(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
    })
}

#[test]
fn fragment_profile_sets_font_bold_and_scheme_pair() {
    let fragment = fragment_json();
    let profile = &fragment["profiles"][0];
    assert_eq!(profile["name"], PROFILE_NAME);
    assert_eq!(profile["font"]["face"], "Cascadia Mono");
    // Windows Terminal wants cell sizes as strings.
    assert_eq!(profile["font"]["cellHeight"], "1.2");
    assert_eq!(profile["intenseTextStyle"], "bold");
    assert!(
        profile["commandline"]
            .as_str()
            .unwrap()
            .contains("powershell.exe")
    );
    let names: Vec<&str> = fragment["schemes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|scheme| scheme["name"].as_str().unwrap())
        .collect();
    for variant in ["dark", "light"] {
        let scheme = profile["colorScheme"][variant].as_str().unwrap();
        assert!(
            names.contains(&scheme),
            "{variant} scheme {scheme} is defined"
        );
    }
}

#[test]
fn every_fragment_scheme_defines_the_full_color_table() {
    // Windows Terminal rejects fragment schemes missing any of the 16 colors.
    for scheme in fragment_json()["schemes"].as_array().unwrap() {
        for color in SCHEME_COLORS {
            assert!(is_hex_color(&scheme[color]), "{} {color}", scheme["name"]);
        }
        assert!(is_hex_color(&scheme["background"]));
        assert!(is_hex_color(&scheme["foreground"]));
    }
}

#[test]
fn install_writes_bomless_json_and_remove_undoes_it() {
    let base = tempfile::tempdir().unwrap();
    let path = install(base.path()).unwrap();
    assert!(path.starts_with(fragment_dir(base.path())));
    assert!(path.ends_with("Microsoft/Windows Terminal/Fragments/harness/harness.json"));
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "no UTF-8 BOM");
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed, fragment_json());

    // Installing again overwrites in place.
    assert_eq!(install(base.path()).unwrap(), path);

    assert!(remove(base.path()).unwrap());
    assert!(!path.exists());
    assert!(
        !fragment_dir(base.path()).exists(),
        "empty folder removed too"
    );
    assert!(
        !remove(base.path()).unwrap(),
        "second remove reports nothing to do"
    );
}
