use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Replace,
    Append,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub path: PathBuf,
    pub created: bool,
    pub previous_len: Option<usize>,
    pub required_prior_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    pub bytes_read: usize,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailResult {
    pub path: String,
    pub content: String,
    pub bytes_read: usize,
    pub total_bytes: u64,
    pub truncated_prefix: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashResult {
    pub path: String,
    pub bytes: u64,
    pub algorithm: &'static str,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatResult {
    pub path: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub len: Option<u64>,
    pub readonly: bool,
    pub modified_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceResult {
    pub path: String,
    pub replacements: usize,
    pub previous_len: usize,
    pub new_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteResult {
    pub path: String,
    pub was_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub source_path: String,
    pub target_path: String,
    pub overwritten: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListEntry {
    pub path: String,
    pub is_dir: bool,
    pub len: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListResult {
    pub entries: Vec<FileListEntry>,
    pub scanned: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearchMatch {
    pub path: String,
    pub line_number: usize,
    pub line: String,
    /// Neighbouring lines (1-based number, text) when the caller asked for
    /// grep `-C` style context; empty otherwise.
    pub before: Vec<(usize, String)>,
    pub after: Vec<(usize, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearchResult {
    pub matches: Vec<FileSearchMatch>,
    pub scanned_files: usize,
    pub skipped_large_files: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FileTool {
    root: PathBuf,
}

impl FileTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn write_text(
        &self,
        requested_path: &str,
        content: &str,
        mode: WriteMode,
    ) -> Result<WriteResult, ToolError> {
        let relative = normalize_relative_path(&self.root, requested_path)?;
        let target = self.root.join(relative);
        let root = self.root.canonicalize().map_err(ToolError::Io)?;

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(ToolError::Io)?;
            let parent = parent.canonicalize().map_err(ToolError::Io)?;
            if !parent.starts_with(&root) {
                return Err(ToolError::OutsideWorkspace {
                    path: requested_path.to_string(),
                });
            }
        }

        let previous = match fs::read(&target) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(ToolError::Io(err)),
        };

        let mut next = match (mode, previous.as_ref()) {
            (WriteMode::Append, Some(bytes)) => bytes.clone(),
            _ => Vec::new(),
        };
        next.extend_from_slice(content.as_bytes());
        fs::write(&target, next).map_err(ToolError::Io)?;

        Ok(WriteResult {
            path: target,
            created: previous.is_none(),
            previous_len: previous.as_ref().map(Vec::len),
            required_prior_read: false,
        })
    }

    pub fn read_text(&self, requested_path: &str) -> Result<String, ToolError> {
        let relative = normalize_relative_path(&self.root, requested_path)?;
        let target = self.root.join(relative);
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = target.canonicalize().map_err(ToolError::Io)?;

        if !target.starts_with(&root) {
            return Err(ToolError::OutsideWorkspace {
                path: requested_path.to_string(),
            });
        }

        fs::read_to_string(target).map_err(ToolError::Io)
    }

    pub fn read_text_bounded(
        &self,
        requested_path: &str,
        max_bytes: usize,
    ) -> Result<ReadResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let total_bytes = fs::metadata(&target).map_err(ToolError::Io)?.len();
        let max_bytes = max_bytes.max(1);
        let read_limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(4)
            .min(total_bytes);
        let mut bytes = Vec::new();
        fs::File::open(&target)
            .map_err(ToolError::Io)?
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(ToolError::Io)?;

        // PowerShell `>` redirects write UTF-16 with a BOM; without sniffing
        // the longest valid UTF-8 prefix of such bytes is EMPTY and the model
        // receives ok + "" — a silent lie (seen live in bench run5).
        let (content, bytes_read) = if bytes.starts_with(&[0xff, 0xfe]) {
            (decode_utf16(&bytes[2..], u16::from_le_bytes), bytes.len())
        } else if bytes.starts_with(&[0xfe, 0xff]) {
            (decode_utf16(&bytes[2..], u16::from_be_bytes), bytes.len())
        } else {
            let start = if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                3
            } else {
                0
            };
            let end = start + utf8_prefix_len(&bytes[start..], max_bytes.min(bytes.len() - start));
            let content = std::str::from_utf8(&bytes[start..end])
                .map_err(|_| ToolError::InvalidUtf8 { path: path.clone() })?
                .to_string();
            (content, end)
        };

        Ok(ReadResult {
            path,
            content,
            bytes_read,
            total_bytes,
            truncated: total_bytes > bytes_read as u64,
        })
    }

    pub fn tail_text(
        &self,
        requested_path: &str,
        max_bytes: usize,
        max_lines: Option<usize>,
    ) -> Result<TailResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let total_bytes = fs::metadata(&target).map_err(ToolError::Io)?.len();
        let max_bytes = max_bytes.max(1);
        let read_window = u64::try_from(max_bytes.saturating_add(4))
            .unwrap_or(u64::MAX)
            .min(total_bytes);
        let start = total_bytes.saturating_sub(read_window);
        let mut file = fs::File::open(&target).map_err(ToolError::Io)?;
        file.seek(SeekFrom::Start(start)).map_err(ToolError::Io)?;

        let mut bytes = Vec::with_capacity(read_window.min(8192) as usize);
        file.read_to_end(&mut bytes).map_err(ToolError::Io)?;
        let min_start = bytes.len().saturating_sub(max_bytes);
        let suffix_start = utf8_suffix_start(&bytes, min_start)
            .ok_or_else(|| ToolError::InvalidUtf8 { path: path.clone() })?;
        let content = std::str::from_utf8(&bytes[suffix_start..])
            .map_err(|_| ToolError::InvalidUtf8 { path: path.clone() })?;
        let content = max_lines
            .map(|lines| tail_lines(content, lines))
            .unwrap_or_else(|| content.to_string());
        let bytes_read = content.len();

        Ok(TailResult {
            path,
            content,
            bytes_read,
            total_bytes,
            truncated_prefix: total_bytes > bytes_read as u64,
        })
    }

    pub fn hash_file(&self, requested_path: &str) -> Result<HashResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let mut file = fs::File::open(&target).map_err(ToolError::Io)?;
        let mut hasher = blake3::Hasher::new();
        let bytes =
            std::io::copy(&mut file, &mut HasherWriter(&mut hasher)).map_err(ToolError::Io)?;

        Ok(HashResult {
            path,
            bytes,
            algorithm: "blake3",
            hash: hasher.finalize().to_hex().to_string(),
        })
    }

    pub fn stat_path(&self, requested_path: &str) -> Result<StatResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let metadata = fs::metadata(&target).map_err(ToolError::Io)?;
        let modified_unix_seconds = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        Ok(StatResult {
            path,
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            len: metadata.is_file().then_some(metadata.len()),
            readonly: metadata.permissions().readonly(),
            modified_unix_seconds,
        })
    }

    pub fn replace_text(
        &self,
        requested_path: &str,
        old_text: &str,
        new_text: &str,
        max_replacements: Option<usize>,
    ) -> Result<ReplaceResult, ToolError> {
        if old_text.is_empty() {
            return Err(ToolError::EmptySearch);
        }

        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let previous = fs::read_to_string(&target).map_err(ToolError::Io)?;
        let previous_len = previous.len();
        let limit = max_replacements.unwrap_or(usize::MAX).max(1);
        let (next, replacements) = replace_limited(&previous, old_text, new_text, limit);

        if replacements == 0 {
            return Err(ToolError::TextNotFound { path });
        }

        fs::write(&target, next.as_bytes()).map_err(ToolError::Io)?;
        Ok(ReplaceResult {
            path,
            replacements,
            previous_len,
            new_len: next.len(),
        })
    }

    pub fn delete_path(&self, requested_path: &str) -> Result<DeleteResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        let was_dir = target.is_dir();

        if was_dir {
            fs::remove_dir_all(&target).map_err(ToolError::Io)?;
        } else {
            fs::remove_file(&target).map_err(ToolError::Io)?;
        }

        Ok(DeleteResult { path, was_dir })
    }

    pub fn move_path(
        &self,
        source_path: &str,
        target_path: &str,
        overwrite: bool,
    ) -> Result<MoveResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let source = resolve_existing_workspace_path(&root, source_path)?;
        let source_path = workspace_relative_display(&root, &source)?;
        let target = resolve_new_workspace_path(&root, target_path)?;
        let target_exists = target.exists();

        if target_exists && !overwrite {
            return Err(ToolError::DestinationExists {
                path: target_path.to_string(),
            });
        }

        if target_exists {
            if target.is_dir() {
                fs::remove_dir_all(&target).map_err(ToolError::Io)?;
            } else {
                fs::remove_file(&target).map_err(ToolError::Io)?;
            }
        }

        fs::rename(&source, &target).map_err(ToolError::Io)?;
        let target = target.canonicalize().map_err(ToolError::Io)?;
        let target_path = workspace_relative_display(&root, &target)?;

        Ok(MoveResult {
            source_path,
            target_path,
            overwritten: target_exists,
        })
    }

    pub fn list_files(
        &self,
        requested_path: &str,
        max_results: usize,
        max_depth: Option<usize>,
        show_hidden: bool,
    ) -> Result<FileListResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path_or_root(&root, requested_path)?;
        let limit = max_results.max(1);
        let scan_limit = limit.saturating_add(1);
        let mut entries = Vec::new();
        collect_files_bounded(
            &root,
            &target,
            &mut entries,
            scan_limit,
            max_depth,
            show_hidden,
        )?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));

        let scanned = entries.len();
        let truncated = scanned > limit;
        entries.truncate(limit);

        Ok(FileListResult {
            entries,
            scanned,
            truncated,
        })
    }

    pub fn search_text(
        &self,
        requested_path: &str,
        query: &str,
        max_results: usize,
        max_file_bytes: u64,
    ) -> Result<FileSearchResult, ToolError> {
        self.search_text_with_context(requested_path, query, max_results, max_file_bytes, 0)
    }

    /// Like [`FileTool::search_text`], with `context_lines` neighbouring
    /// lines captured around each match (grep `-C` style) so callers do not
    /// have to read whole files just to see what surrounds a hit.
    pub fn search_text_with_context(
        &self,
        requested_path: &str,
        query: &str,
        max_results: usize,
        max_file_bytes: u64,
        context_lines: usize,
    ) -> Result<FileSearchResult, ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path_or_root(&root, requested_path)?;
        let mut files = Vec::new();
        collect_file_paths(&target, &mut files)?;
        files.sort();

        let limit = max_results.max(1);
        let mut matches = Vec::new();
        let mut scanned_files = 0;
        let mut skipped_large_files = 0;
        let mut truncated = false;

        for file in files {
            if matches.len() >= limit {
                truncated = true;
                break;
            }

            let metadata = fs::metadata(&file).map_err(ToolError::Io)?;
            if metadata.len() > max_file_bytes {
                skipped_large_files += 1;
                continue;
            }

            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            scanned_files += 1;
            let relative = workspace_relative_display(&root, &file)?;
            let lines: Vec<&str> = content.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if line.contains(query) {
                    let window = |range: std::ops::Range<usize>| {
                        range
                            .filter_map(|i| lines.get(i).map(|text| (i + 1, text.to_string())))
                            .collect::<Vec<_>>()
                    };
                    matches.push(FileSearchMatch {
                        path: relative.clone(),
                        line_number: index + 1,
                        line: line.to_string(),
                        before: window(index.saturating_sub(context_lines)..index),
                        after: window(index + 1..index + 1 + context_lines),
                    });
                    if matches.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
            }
        }

        Ok(FileSearchResult {
            matches,
            scanned_files,
            skipped_large_files,
            truncated,
        })
    }
}

#[derive(Debug)]
pub enum ToolError {
    OutsideWorkspace { path: String },
    DestinationExists { path: String },
    TextNotFound { path: String },
    InvalidUtf8 { path: String },
    EmptyPath,
    EmptySearch,
    Io(std::io::Error),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideWorkspace { path } => {
                write!(
                    f,
                    "path points outside the workspace: {path} (use paths relative to the workspace root)"
                )
            }
            Self::DestinationExists { path } => {
                write!(f, "destination already exists: {path}")
            }
            Self::TextNotFound { path } => {
                write!(f, "text to replace was not found in {path}")
            }
            Self::InvalidUtf8 { path } => write!(f, "file is not valid UTF-8: {path}"),
            Self::EmptyPath => write!(f, "path is empty"),
            Self::EmptySearch => write!(f, "text to replace is empty"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

struct HasherWriter<'a>(&'a mut blake3::Hasher);

impl Write for HasherWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Turn an absolute path that points inside the workspace into its
/// workspace-relative form. The system prompt tells the model the
/// workspace root, and models trained on absolute-path harnesses
/// legitimately send `<root>/file` — that path IS inside the workspace.
/// Returns `None` for relative inputs and for paths truly outside.
fn strip_workspace_root(root: &Path, input: &str) -> Option<String> {
    let target = input.replace('\\', "/");
    if !Path::new(&target).is_absolute() {
        return None;
    }
    let target = target.trim_start_matches("//?/");
    let mut bases = vec![root.to_path_buf()];
    if let Ok(canonical) = root.canonicalize() {
        bases.push(canonical);
    }
    for base in bases {
        let base = base.display().to_string().replace('\\', "/");
        let base = base.trim_start_matches("//?/").trim_end_matches('/');
        let matches = if cfg!(windows) {
            target
                .to_ascii_lowercase()
                .starts_with(&base.to_ascii_lowercase())
        } else {
            target.starts_with(base)
        };
        if !matches {
            continue;
        }
        let rest = &target[base.len()..];
        // Require a component boundary: `<root>-other/...` is outside.
        if !(rest.is_empty() || rest.starts_with('/')) {
            continue;
        }
        let rest = rest.trim_start_matches('/');
        return Some(if rest.is_empty() {
            ".".to_string()
        } else {
            rest.to_string()
        });
    }
    None
}

fn normalize_relative_path(root: &Path, input: &str) -> Result<PathBuf, ToolError> {
    let stripped;
    let input = match strip_workspace_root(root, input) {
        Some(relative) => {
            stripped = relative;
            stripped.as_str()
        }
        None => input,
    };
    let normalized = input.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(ToolError::OutsideWorkspace {
            path: input.to_string(),
        });
    }

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::OutsideWorkspace {
                    path: input.to_string(),
                });
            }
        }
    }

    if out.as_os_str().is_empty() {
        return Err(ToolError::EmptyPath);
    }

    Ok(out)
}

fn resolve_existing_workspace_path(
    root: &Path,
    requested_path: &str,
) -> Result<PathBuf, ToolError> {
    let relative = normalize_relative_path(root, requested_path)?;
    let target = root.join(relative).canonicalize().map_err(ToolError::Io)?;
    if !target.starts_with(root) {
        return Err(ToolError::OutsideWorkspace {
            path: requested_path.to_string(),
        });
    }
    Ok(target)
}

fn resolve_existing_workspace_path_or_root(
    root: &Path,
    requested_path: &str,
) -> Result<PathBuf, ToolError> {
    let trimmed = requested_path.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || strip_workspace_root(root, trimmed).as_deref() == Some(".")
    {
        return Ok(root.to_path_buf());
    }

    resolve_existing_workspace_path(root, requested_path)
}

fn resolve_new_workspace_path(root: &Path, requested_path: &str) -> Result<PathBuf, ToolError> {
    let relative = normalize_relative_path(root, requested_path)?;
    let target = root.join(relative);
    let parent = target.parent().ok_or(ToolError::EmptyPath)?;
    fs::create_dir_all(parent).map_err(ToolError::Io)?;
    let parent = parent.canonicalize().map_err(ToolError::Io)?;
    if !parent.starts_with(root) {
        return Err(ToolError::OutsideWorkspace {
            path: requested_path.to_string(),
        });
    }
    Ok(target)
}

fn collect_files_bounded(
    root: &Path,
    target: &Path,
    entries: &mut Vec<FileListEntry>,
    scan_limit: usize,
    max_depth: Option<usize>,
    show_hidden: bool,
) -> Result<(), ToolError> {
    if entries.len() >= scan_limit {
        return Ok(());
    }

    if target.is_file() {
        let metadata = fs::metadata(target).map_err(ToolError::Io)?;
        entries.push(FileListEntry {
            path: workspace_relative_display(root, target)?,
            is_dir: false,
            len: Some(metadata.len()),
        });
        return Ok(());
    }

    if max_depth == Some(0) {
        return Ok(());
    }

    for entry in fs::read_dir(target).map_err(ToolError::Io)? {
        if entries.len() >= scan_limit {
            break;
        }
        let entry = entry.map_err(ToolError::Io)?;
        let path = entry.path();
        // Dot entries (.git, .claude, …) are noise for the model unless it
        // asks for them explicitly.
        if !show_hidden && entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let metadata = entry.metadata().map_err(ToolError::Io)?;
        entries.push(FileListEntry {
            path: workspace_relative_display(root, &path)?,
            is_dir: metadata.is_dir(),
            len: metadata.is_file().then_some(metadata.len()),
        });
        if metadata.is_dir() {
            collect_files_bounded(
                root,
                &path,
                entries,
                scan_limit,
                max_depth.map(|depth| depth.saturating_sub(1)),
                show_hidden,
            )?;
        }
    }

    Ok(())
}

fn collect_file_paths(target: &Path, files: &mut Vec<PathBuf>) -> Result<(), ToolError> {
    if target.is_file() {
        files.push(target.to_path_buf());
        return Ok(());
    }

    for entry in fs::read_dir(target).map_err(ToolError::Io)? {
        let entry = entry.map_err(ToolError::Io)?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(ToolError::Io)?;
        if metadata.is_file() {
            files.push(path);
        } else if metadata.is_dir() {
            collect_file_paths(&path, files)?;
        }
    }

    Ok(())
}

fn replace_limited(
    input: &str,
    old_text: &str,
    new_text: &str,
    max_replacements: usize,
) -> (String, usize) {
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;
    let mut replacements = 0;

    while replacements < max_replacements {
        let Some(index) = remaining.find(old_text) else {
            break;
        };

        output.push_str(&remaining[..index]);
        output.push_str(new_text);
        remaining = &remaining[index + old_text.len()..];
        replacements += 1;
    }

    if replacements == 0 {
        return (input.to_string(), 0);
    }

    output.push_str(remaining);
    (output, replacements)
}

/// Decode UTF-16 payload bytes (after the BOM) with the given byte order;
/// a trailing odd byte from a bounded read is dropped, invalid pairs are
/// replaced rather than failing.
fn decode_utf16(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| to_u16([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn utf8_prefix_len(bytes: &[u8], max_len: usize) -> usize {
    let len = max_len.min(bytes.len());
    std::str::from_utf8(&bytes[..len])
        .map(|_| len)
        .unwrap_or_else(|err| err.valid_up_to())
}

fn utf8_suffix_start(bytes: &[u8], min_start: usize) -> Option<usize> {
    (min_start.min(bytes.len())..=bytes.len())
        .find(|start| std::str::from_utf8(&bytes[*start..]).is_ok())
}

fn tail_lines(content: &str, max_lines: usize) -> String {
    let max_lines = max_lines.max(1);
    let trailing_newline = content.ends_with('\n');
    let lines = content.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return content.to_string();
    }

    let mut output = lines[lines.len() - max_lines..].join("\n");
    if trailing_newline {
        output.push('\n');
    }
    output
}

fn workspace_relative_display(root: &Path, path: &Path) -> Result<String, ToolError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ToolError::OutsideWorkspace {
            path: path.display().to_string(),
        })?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}
