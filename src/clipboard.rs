use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardItem {
    Text(String),
    ImagePng(Vec<u8>),
}

pub trait ClipboardSource {
    fn read(&self) -> Result<Option<ClipboardItem>, ClipboardError>;
}

#[derive(Debug, Clone)]
pub struct StaticClipboard {
    item: Option<ClipboardItem>,
}

impl StaticClipboard {
    pub fn new(item: Option<ClipboardItem>) -> Self {
        Self { item }
    }
}

impl ClipboardSource for StaticClipboard {
    fn read(&self) -> Result<Option<ClipboardItem>, ClipboardError> {
        Ok(self.item.clone())
    }
}

#[derive(Debug, Clone)]
pub struct SystemClipboard;

impl ClipboardSource for SystemClipboard {
    fn read(&self) -> Result<Option<ClipboardItem>, ClipboardError> {
        if let Some(image) = read_system_image_png()? {
            return Ok(Some(ClipboardItem::ImagePng(image)));
        }

        if let Some(text) = read_system_text()? {
            return Ok(Some(ClipboardItem::Text(text)));
        }

        Ok(None)
    }
}

#[derive(Debug, Clone)]
pub struct ClipboardCapture {
    store: AttachmentStore,
}

impl ClipboardCapture {
    pub fn new(store: AttachmentStore) -> Self {
        Self { store }
    }

    pub fn capture(
        &self,
        source: &impl ClipboardSource,
    ) -> Result<Option<ClipboardAttachment>, ClipboardError> {
        let Some(item) = source.read()? else {
            return Ok(None);
        };
        Ok(Some(self.store.save(item)?))
    }
}

#[derive(Debug, Clone)]
pub struct AttachmentStore {
    workspace: PathBuf,
}

impl AttachmentStore {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    pub fn save(&self, item: ClipboardItem) -> Result<ClipboardAttachment, ClipboardError> {
        let (kind, mime_type, extension, bytes, prompt_fragment) = match item {
            ClipboardItem::Text(text) => {
                let bytes = text.into_bytes();
                let preview = String::from_utf8_lossy(&bytes).to_string();
                (
                    "text",
                    "text/plain; charset=utf-8",
                    "txt",
                    bytes,
                    format!("clipboard text:\n{preview}"),
                )
            }
            ClipboardItem::ImagePng(bytes) => ("image", "image/png", "png", bytes, String::new()),
        };

        let digest = blake3::hash(&bytes).to_hex().to_string();
        let relative_path = PathBuf::from(".harness").join("attachments").join(format!(
            "paste-{}.{}",
            &digest[..16],
            extension
        ));
        let absolute_path = self.workspace.join(&relative_path);
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(ClipboardError::Io)?;
        }
        fs::write(&absolute_path, &bytes).map_err(ClipboardError::Io)?;

        let prompt_fragment = if kind == "image" {
            format!("image file: {}", absolute_path.display())
        } else {
            prompt_fragment
        };

        Ok(ClipboardAttachment {
            kind: kind.to_string(),
            mime_type: mime_type.to_string(),
            relative_path: path_to_slash_string(&relative_path),
            absolute_path: absolute_path.display().to_string(),
            bytes: bytes.len(),
            prompt_fragment,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClipboardAttachment {
    pub kind: String,
    pub mime_type: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub bytes: usize,
    pub prompt_fragment: String,
}

#[derive(Debug)]
pub enum ClipboardError {
    Io(std::io::Error),
    CommandFailed { program: String, stderr: String },
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::CommandFailed { program, stderr } => {
                write!(f, "{program} failed while reading clipboard: {stderr}")
            }
        }
    }
}

impl Error for ClipboardError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::CommandFailed { .. } => None,
        }
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(windows)]
fn read_system_image_png() -> Result<Option<Vec<u8>>, ClipboardError> {
    let path = std::env::temp_dir().join(format!(
        "harness-clipboard-{}-image.png",
        std::process::id()
    ));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         Add-Type -AssemblyName System.Drawing; \
         $img=[System.Windows.Forms.Clipboard]::GetImage(); \
         if ($null -eq $img) {{ exit 3 }}; \
         $img.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
        path.display().to_string().replace('\'', "''")
    );
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .output()
        .map_err(ClipboardError::Io)?;

    if output.status.code() == Some(3) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(ClipboardError::CommandFailed {
            program: "powershell.exe".to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let bytes = fs::read(&path).map_err(ClipboardError::Io)?;
    let _ = fs::remove_file(path);
    Ok(Some(bytes))
}

#[cfg(windows)]
fn read_system_text() -> Result<Option<String>, ClipboardError> {
    let script = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; \
                  $text=Get-Clipboard -Raw -Format Text; \
                  if ($null -eq $text) { exit 3 }; \
                  [Console]::Out.Write($text)";
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .output()
        .map_err(ClipboardError::Io)?;

    if output.status.code() == Some(3) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(ClipboardError::CommandFailed {
            program: "powershell.exe".to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(all(unix, not(windows)))]
fn read_system_image_png() -> Result<Option<Vec<u8>>, ClipboardError> {
    for command in [
        ("wl-paste", vec!["--type", "image/png"]),
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ] {
        if let Some(bytes) = run_optional_clipboard_command(command.0, &command.1)?
            && !bytes.is_empty()
        {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[cfg(all(unix, not(windows)))]
fn read_system_text() -> Result<Option<String>, ClipboardError> {
    for command in [
        ("wl-paste", vec!["--no-newline"]),
        ("xclip", vec!["-selection", "clipboard", "-o"]),
    ] {
        if let Some(bytes) = run_optional_clipboard_command(command.0, &command.1)? {
            let text = String::from_utf8_lossy(&bytes).to_string();
            if !text.is_empty() {
                return Ok(Some(text));
            }
        }
    }
    Ok(None)
}

#[cfg(all(unix, not(windows)))]
fn run_optional_clipboard_command(
    program: &str,
    args: &[&str],
) -> Result<Option<Vec<u8>>, ClipboardError> {
    let output = match Command::new(program).args(args).output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ClipboardError::Io(err)),
    };
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}
