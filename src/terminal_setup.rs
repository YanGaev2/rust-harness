//! `harness terminal-setup`: a Windows Terminal profile tuned for the
//! harness, installed as a JSON fragment extension.
//!
//! An application cannot change the terminal's font, but Windows Terminal
//! lets it ship a profile. The fragment adds a "Harness" profile
//! (PowerShell inside) with real bold, a roomier line height and padding,
//! and a dark/light color scheme pair that matches the chat palette. The
//! user's own `settings.json` is never touched: deleting the fragment file
//! (`terminal-setup --remove`) undoes everything.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Folder name under `…\Windows Terminal\Fragments\`; unique per app.
const APP_NAME: &str = "harness";
const FILE_NAME: &str = "harness.json";
pub const PROFILE_NAME: &str = "Harness";

/// The per-user fragment folder Windows Terminal reads:
/// `%LOCALAPPDATA%\Microsoft\Windows Terminal\Fragments\harness`.
pub fn fragment_dir(local_app_data: &Path) -> PathBuf {
    local_app_data
        .join("Microsoft")
        .join("Windows Terminal")
        .join("Fragments")
        .join(APP_NAME)
}

/// The fragment: one profile plus the two color schemes it switches
/// between with the Windows Terminal theme.
///
/// - `intenseTextStyle: bold` — SGR 1 renders as a heavier weight; the
///   default (`bright`) only brightens the color, so headings and tool
///   names lose their weight.
/// - `cellHeight: 1.2` — line height (Windows Terminal 1.18+).
/// - Cascadia Mono ships with Windows Terminal, so the face always exists.
pub fn fragment_json() -> Value {
    json!({
        "profiles": [{
            "name": PROFILE_NAME,
            "commandline": "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoLogo",
            "font": { "face": "Cascadia Mono", "size": 12, "cellHeight": "1.2" },
            "intenseTextStyle": "bold",
            "padding": "14, 10",
            "colorScheme": { "dark": "Harness Dark", "light": "Harness Light" }
        }],
        "schemes": [
            {
                "name": "Harness Dark",
                "background": "#121317",
                "foreground": "#D4D6DC",
                "cursorColor": "#8ABEB7",
                "selectionBackground": "#2F3A44",
                "black": "#121317",
                "red": "#CC6666",
                "green": "#B5BD68",
                "yellow": "#F0C674",
                "blue": "#81A2BE",
                "purple": "#B294BB",
                "cyan": "#8ABEB7",
                "white": "#C5C8C6",
                "brightBlack": "#5C6370",
                "brightRed": "#E26A75",
                "brightGreen": "#B8DB87",
                "brightYellow": "#F5A742",
                "brightBlue": "#A5B4FC",
                "brightPurple": "#C9A7EB",
                "brightCyan": "#A3D9D2",
                "brightWhite": "#EAEAEA"
            },
            {
                "name": "Harness Light",
                "background": "#FAFAF8",
                "foreground": "#2B2D31",
                "cursorColor": "#2F7D78",
                "selectionBackground": "#D7E3F0",
                "black": "#2B2D31",
                "red": "#AA4444",
                "green": "#588458",
                "yellow": "#9A6B10",
                "blue": "#3B6EA8",
                "purple": "#7A4F9A",
                "cyan": "#2F7D78",
                "white": "#C8C8C8",
                "brightBlack": "#6C6C6C",
                "brightRed": "#C0392B",
                "brightGreen": "#2E7D32",
                "brightYellow": "#B0700C",
                "brightBlue": "#4F46E5",
                "brightPurple": "#8E44AD",
                "brightCyan": "#1F8A84",
                "brightWhite": "#FFFFFF"
            }
        ]
    })
}

/// Write (or overwrite) the fragment; returns the file path. UTF-8
/// without BOM, as Windows Terminal requires.
pub fn install(local_app_data: &Path) -> io::Result<PathBuf> {
    let dir = fragment_dir(local_app_data);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(FILE_NAME);
    let text = serde_json::to_string_pretty(&fragment_json()).map_err(io::Error::other)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Delete the fragment file and its folder (only if nothing else is in
/// it). `Ok(false)` when no fragment was installed.
pub fn remove(local_app_data: &Path) -> io::Result<bool> {
    let dir = fragment_dir(local_app_data);
    match std::fs::remove_file(dir.join(FILE_NAME)) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    }
    // A non-empty folder is someone else's business; leave it.
    let _ = std::fs::remove_dir(&dir);
    Ok(true)
}
