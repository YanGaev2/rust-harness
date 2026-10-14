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
    /// The match only succeeded after re-encoding line endings to the
    /// file's convention (LF-thinking model vs CRLF file, or vice versa).
    pub normalized_line_endings: bool,
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
    /// `line` is a cut of a longer line (see [`SEARCH_LINE_MAX_BYTES`]).
    pub clipped: bool,
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
    /// Workspace-relative paths the walk left out: hidden entries and
    /// entries the root `.gitignore` covers. Sorted.
    pub skipped: Vec<String>,
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
        })
    }

    /// Canonical (existing target, display path) pair every path-taking
    /// operation starts from.
    fn resolve(&self, requested_path: &str) -> Result<(PathBuf, String), ToolError> {
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path(&root, requested_path)?;
        let path = workspace_relative_display(&root, &target)?;
        Ok((target, path))
    }

    pub fn read_text_bounded(
        &self,
        requested_path: &str,
        max_bytes: usize,
    ) -> Result<ReadResult, ToolError> {
        let (target, path) = self.resolve(requested_path)?;
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
        let (target, path) = self.resolve(requested_path)?;
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
        let (target, path) = self.resolve(requested_path)?;
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
        // "." is the workspace root here, as in file.list.
        let root = self.root.canonicalize().map_err(ToolError::Io)?;
        let target = resolve_existing_workspace_path_or_root(&root, requested_path)?;
        let path = match workspace_relative_display(&root, &target)? {
            path if path.is_empty() => ".".to_string(),
            path => path,
        };
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

        let (target, path) = self.resolve(requested_path)?;
        let previous = fs::read_to_string(&target).map_err(ToolError::Io)?;
        let previous_len = previous.len();
        let limit = max_replacements.unwrap_or(usize::MAX).max(1);
        let (mut next, mut replacements) = replace_limited(&previous, old_text, new_text, limit);
        let mut normalized_line_endings = false;

        // Models think in LF while Windows files carry CRLF (and vice
        // versa): the text they ask to replace IS in the file, just with
        // different line-ending bytes. Retry with the search and the
        // replacement re-encoded to the file's own convention so the file
        // stays consistent.
        if replacements == 0 && old_text.contains('\n') {
            let to_file_endings = |text: &str| {
                let unix = text.replace("\r\n", "\n");
                if previous.contains("\r\n") {
                    unix.replace('\n', "\r\n")
                } else {
                    unix
                }
            };
            let old_normalized = to_file_endings(old_text);
            if old_normalized != old_text {
                (next, replacements) = replace_limited(
                    &previous,
                    &old_normalized,
                    &to_file_endings(new_text),
                    limit,
                );
                normalized_line_endings = replacements > 0;
            }
        }

        if replacements == 0 {
            return Err(ToolError::TextNotFound { path });
        }

        fs::write(&target, next.as_bytes()).map_err(ToolError::Io)?;
        Ok(ReplaceResult {
            path,
            replacements,
            previous_len,
            new_len: next.len(),
            normalized_line_endings,
        })
    }

    pub fn delete_path(&self, requested_path: &str) -> Result<DeleteResult, ToolError> {
        let (target, path) = self.resolve(requested_path)?;
        let was_dir = target.is_dir();

        remove_any(&target)?;

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
            remove_any(&target)?;
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
        let walk = Walk {
            root: &root,
            rules: IgnoreRules::for_target(&root, &target),
            show_hidden,
        };
        collect_files_bounded(&walk, &target, &mut entries, scan_limit, max_depth, true)?;
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

    /// Text search, with `context_lines` neighbouring
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
        let walk = Walk {
            root: &root,
            rules: IgnoreRules::for_target(&root, &target),
            show_hidden: false,
        };
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        collect_file_paths(&walk, &target, &mut files, &mut skipped, true)?;
        files.sort();
        skipped.sort();

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

            // A file can vanish between the walk and the read (a build
            // deleting its temp files): pass it over.
            let Ok(metadata) = fs::metadata(&file) else {
                continue;
            };
            if metadata.len() > max_file_bytes {
                skipped_large_files += 1;
                continue;
            }

            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            scanned_files += 1;
            let relative = workspace_relative_display(&root, &file)?;
            if !content.contains(query) {
                continue;
            }
            let lines: Vec<&str> = content.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if let Some(at) = line.find(query) {
                    let window = |range: std::ops::Range<usize>| {
                        range
                            .filter_map(|i| lines.get(i).map(|text| (i + 1, clip_line(text, 0))))
                            .collect::<Vec<_>>()
                    };
                    matches.push(FileSearchMatch {
                        path: relative.clone(),
                        line_number: index + 1,
                        line: clip_line(line, at),
                        clipped: line.len() > SEARCH_LINE_MAX_BYTES,
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
            skipped,
        })
    }
}

/// Longest line `search_text_with_context` returns. Minified bundles and
/// JSONL logs carry lines of 100+ KB; whole, 200 of them filled a prompt
/// with 1.6 MB of tool output in a real session (2026-10-09).
pub const SEARCH_LINE_MAX_BYTES: usize = 500;

/// `line` cut to about [`SEARCH_LINE_MAX_BYTES`] around byte `at`: a third
/// of the budget before it, the rest after. `…` marks each cut.
fn clip_line(line: &str, at: usize) -> String {
    if line.len() <= SEARCH_LINE_MAX_BYTES {
        return line.to_string();
    }
    let mut start = at
        .saturating_sub(SEARCH_LINE_MAX_BYTES / 3)
        .min(line.len() - SEARCH_LINE_MAX_BYTES);
    while !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start + SEARCH_LINE_MAX_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = if start > 0 { "…" } else { "" };
    let tail = if end < line.len() { "…" } else { "" };
    format!("{head}{}{tail}", &line[start..end])
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

fn remove_any(path: &Path) -> Result<(), ToolError> {
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(ToolError::Io)
    } else {
        fs::remove_file(path).map_err(ToolError::Io)
    }
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
    // "", ".", "./" and the workspace's own absolute path all normalize
    // to an empty relative path: that is the root.
    match resolve_existing_workspace_path(root, requested_path.trim()) {
        Err(ToolError::EmptyPath) => Ok(root.to_path_buf()),
        other => other,
    }
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

/// What a directory walk needs besides the directory: the root for
/// relative paths, the ignore rules, and whether dot entries count.
struct Walk<'a> {
    root: &'a Path,
    rules: IgnoreRules,
    show_hidden: bool,
}

impl Walk<'_> {
    /// Path segments of `path` below the workspace root.
    fn segments(&self, path: &Path) -> Vec<String> {
        path.strip_prefix(self.root)
            .map(|relative| {
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Dot entries (.git, .claude, …) are noise for the model unless it
    /// asks for them explicitly.
    fn is_hidden(&self, entry: &fs::DirEntry) -> bool {
        !self.show_hidden && entry.file_name().to_string_lossy().starts_with('.')
    }
}

/// The entries of `dir`. Only the walk's starting directory reports an
/// error; a subdirectory that vanished or cannot be read is passed over.
fn read_dir_entries(dir: &Path, top: bool) -> Result<Vec<fs::DirEntry>, ToolError> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(entries.flatten().collect()),
        Err(error) if top => Err(ToolError::Io(error)),
        Err(_) => Ok(Vec::new()),
    }
}

/// Bounded listing. Ignored directories are listed but never entered:
/// the model sees that `target/` exists without paying for its contents.
fn collect_files_bounded(
    walk: &Walk<'_>,
    target: &Path,
    entries: &mut Vec<FileListEntry>,
    scan_limit: usize,
    max_depth: Option<usize>,
    top: bool,
) -> Result<(), ToolError> {
    if entries.len() >= scan_limit {
        return Ok(());
    }

    if target.is_file() {
        let metadata = fs::metadata(target).map_err(ToolError::Io)?;
        entries.push(FileListEntry {
            path: workspace_relative_display(walk.root, target)?,
            is_dir: false,
            len: Some(metadata.len()),
        });
        return Ok(());
    }

    if max_depth == Some(0) {
        return Ok(());
    }

    for entry in read_dir_entries(target, top)? {
        if entries.len() >= scan_limit {
            break;
        }
        if walk.is_hidden(&entry) {
            continue;
        }
        // An entry that vanished mid-walk is simply not listed.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        entries.push(FileListEntry {
            path: workspace_relative_display(walk.root, &path)?,
            is_dir: metadata.is_dir(),
            len: metadata.is_file().then_some(metadata.len()),
        });
        if metadata.is_dir() && !walk.rules.is_ignored(&walk.segments(&path), true) {
            collect_files_bounded(
                walk,
                &path,
                entries,
                scan_limit,
                max_depth.map(|depth| depth.saturating_sub(1)),
                false,
            )?;
        }
    }

    Ok(())
}

/// Files to search, like ripgrep: hidden entries and entries the root
/// `.gitignore` covers are recorded in `skipped` instead of walked, and
/// entries that vanish or cannot be read mid-walk are passed over.
fn collect_file_paths(
    walk: &Walk<'_>,
    target: &Path,
    files: &mut Vec<PathBuf>,
    skipped: &mut Vec<String>,
    top: bool,
) -> Result<(), ToolError> {
    if target.is_file() {
        files.push(target.to_path_buf());
        return Ok(());
    }

    for entry in read_dir_entries(target, top)? {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        let segments = walk.segments(&path);
        let hidden = walk.is_hidden(&entry);
        if hidden || walk.rules.is_ignored(&segments, metadata.is_dir()) {
            // Hidden files (.gitignore, .env) are not worth naming.
            if !hidden || metadata.is_dir() {
                skipped.push(segments.join("/"));
            }
        } else if metadata.is_file() {
            files.push(path);
        } else if metadata.is_dir() {
            collect_file_paths(walk, &path, files, skipped, false)?;
        }
    }

    Ok(())
}

/// The workspace root `.gitignore`, read for the patterns that matter to a
/// walk: comments, blank lines, `/anchored` and `dir/` patterns, `*` and
/// `?` inside one path segment. Negations (`!`), `**` and nested
/// `.gitignore` files are not read — a missed rule only means a wider
/// walk.
#[derive(Debug, Default)]
struct IgnoreRules {
    rules: Vec<IgnoreRule>,
}

#[derive(Debug)]
struct IgnoreRule {
    segments: Vec<String>,
    /// Matches from the root (a leading `/` or a `/` inside), not by name
    /// at any depth.
    anchored: bool,
    /// A trailing `/`: directories only.
    dir_only: bool,
}

impl IgnoreRules {
    /// Rules for a walk that starts at `target`. A target the rules cover
    /// (or one inside such a directory) was asked for explicitly: no
    /// rules then.
    fn for_target(root: &Path, target: &Path) -> IgnoreRules {
        let rules = IgnoreRules::load(root);
        let segments: Vec<String> = target
            .strip_prefix(root)
            .map(|relative| {
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let covered = (1..=segments.len()).any(|depth| rules.is_ignored(&segments[..depth], true));
        if covered {
            IgnoreRules::default()
        } else {
            rules
        }
    }

    fn load(root: &Path) -> IgnoreRules {
        let Ok(text) = fs::read_to_string(root.join(".gitignore")) else {
            return IgnoreRules::default();
        };
        let rules = text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty()
                    || line.starts_with('#')
                    || line.starts_with('!')
                    || line.contains("**")
                {
                    return None;
                }
                let dir_only = line.ends_with('/');
                let pattern = line.trim_end_matches('/');
                let anchored = pattern.contains('/');
                let segments: Vec<String> = pattern
                    .trim_start_matches('/')
                    .split('/')
                    .map(str::to_string)
                    .collect();
                (!segments.iter().any(String::is_empty)).then_some(IgnoreRule {
                    segments,
                    anchored,
                    dir_only,
                })
            })
            .collect();
        IgnoreRules { rules }
    }

    /// Whether the entry at `segments` (its path below the root) is ignored.
    fn is_ignored(&self, segments: &[String], is_dir: bool) -> bool {
        self.rules.iter().any(|rule| {
            if rule.dir_only && !is_dir {
                return false;
            }
            if rule.anchored {
                rule.segments.len() == segments.len()
                    && rule
                        .segments
                        .iter()
                        .zip(segments)
                        .all(|(pattern, segment)| glob_match(pattern, segment))
            } else {
                segments
                    .last()
                    .is_some_and(|name| glob_match(&rule.segments[0], name))
            }
        })
    }
}

/// `*` (any run) and `?` (one char) matching within one path segment.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&ch| ch == '*')
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
