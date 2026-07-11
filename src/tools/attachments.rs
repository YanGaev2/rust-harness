use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentReadResult {
    pub path: String,
    pub kind: String,
    pub mime_type: String,
    pub bytes: u64,
    pub content: String,
    pub bytes_read: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct AttachmentTool {
    workspace: PathBuf,
}

impl AttachmentTool {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    pub fn read(
        &self,
        reference: &str,
        max_bytes: usize,
    ) -> Result<AttachmentReadResult, AttachmentError> {
        let reference = clean_attachment_reference(reference)?;
        let workspace = self.workspace.canonicalize().map_err(AttachmentError::Io)?;
        let target = if reference.is_absolute() {
            reference
        } else {
            workspace.join(reference)
        };
        let target = target.canonicalize().map_err(AttachmentError::Io)?;
        let allowed_roots = allowed_attachment_roots(&workspace);
        if !allowed_roots.iter().any(|root| target.starts_with(root)) {
            return Err(AttachmentError::OutsideAllowedRoots {
                path: target.display().to_string(),
            });
        }

        let bytes = fs::metadata(&target).map_err(AttachmentError::Io)?.len();
        let mut prefix = Vec::new();
        let max_bytes = max_bytes.max(1);
        let read_limit = bytes.min(
            u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(16),
        );
        fs::File::open(&target)
            .map_err(AttachmentError::Io)?
            .take(read_limit)
            .read_to_end(&mut prefix)
            .map_err(AttachmentError::Io)?;

        let (kind, mime_type) = classify_attachment(&target, &prefix);
        let (content, bytes_read, truncated) = if kind == "text" {
            let end = utf8_prefix_len(&prefix, max_bytes.min(prefix.len()));
            let content = std::str::from_utf8(&prefix[..end])
                .map_err(|_| AttachmentError::InvalidUtf8 {
                    path: target.display().to_string(),
                })?
                .to_string();
            let bytes_read = content.len();
            (content, bytes_read, bytes > bytes_read as u64)
        } else {
            (String::new(), 0, false)
        };

        Ok(AttachmentReadResult {
            path: display_attachment_path(&workspace, &target),
            kind,
            mime_type,
            bytes,
            content,
            bytes_read,
            truncated,
        })
    }
}

#[derive(Debug)]
pub enum AttachmentError {
    EmptyPath,
    OutsideAllowedRoots { path: String },
    InvalidUtf8 { path: String },
    Io(std::io::Error),
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "attachment path is empty"),
            Self::OutsideAllowedRoots { path } => {
                write!(
                    f,
                    "attachment path is outside allowed attachment roots: {path}"
                )
            }
            Self::InvalidUtf8 { path } => write!(f, "attachment is not valid UTF-8: {path}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for AttachmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

fn clean_attachment_reference(input: &str) -> Result<PathBuf, AttachmentError> {
    let trimmed = input.trim().trim_matches('"').trim_matches('\'').trim();
    let without_label = trimmed
        .strip_prefix("image file:")
        .or_else(|| trimmed.strip_prefix("Image file:"))
        .or_else(|| trimmed.strip_prefix("file:"))
        .unwrap_or(trimmed)
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    if without_label.is_empty() {
        return Err(AttachmentError::EmptyPath);
    }

    Ok(PathBuf::from(without_label))
}

fn allowed_attachment_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let workspace_attachments = workspace.join(".harness").join("attachments");
    if let Ok(path) = workspace_attachments.canonicalize() {
        roots.push(path);
    }

    if let Some(home) = home_dir() {
        let codex_attachments = home.join(".codex").join("attachments");
        if let Ok(path) = codex_attachments.canonicalize() {
            roots.push(path);
        }
    }

    roots
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn classify_attachment(path: &Path, prefix: &[u8]) -> (String, String) {
    if prefix.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        return ("image".to_string(), "image/png".to_string());
    }

    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => ("image".to_string(), "image/png".to_string()),
        Some("txt" | "md" | "json" | "toml" | "yaml" | "yml" | "rs") => {
            ("text".to_string(), "text/plain; charset=utf-8".to_string())
        }
        _ if std::str::from_utf8(prefix).is_ok() => {
            ("text".to_string(), "text/plain; charset=utf-8".to_string())
        }
        _ => ("binary".to_string(), "application/octet-stream".to_string()),
    }
}

fn utf8_prefix_len(bytes: &[u8], max_len: usize) -> usize {
    let len = max_len.min(bytes.len());
    std::str::from_utf8(&bytes[..len])
        .map(|_| len)
        .unwrap_or_else(|err| err.valid_up_to())
}

fn display_attachment_path(workspace: &Path, target: &Path) -> String {
    target
        .strip_prefix(workspace)
        .unwrap_or(target)
        .to_string_lossy()
        .replace('\\', "/")
}
