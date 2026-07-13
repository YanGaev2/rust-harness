use harness_cli::tools::files::{FileTool, WriteMode};

#[test]
fn write_text_does_not_require_a_prior_read_and_records_previous_content() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());

    let created = tool
        .write_text("src\\nested/notes.txt", "hello", WriteMode::Replace)
        .unwrap();
    assert!(created.created);
    assert!(!created.required_prior_read);
    assert_eq!(created.previous_len, None);

    let updated = tool
        .write_text("src/nested/notes.txt", "hello v2", WriteMode::Replace)
        .unwrap();
    assert!(!updated.created);
    assert!(!updated.required_prior_read);
    assert_eq!(updated.previous_len, Some(5));

    let written =
        std::fs::read_to_string(root.path().join("src").join("nested").join("notes.txt")).unwrap();
    assert_eq!(written, "hello v2");
}

#[test]
fn write_text_rejects_paths_outside_workspace() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());

    let err = tool
        .write_text("../escape.txt", "bad", WriteMode::Replace)
        .unwrap_err();

    assert!(err.to_string().contains("outside the workspace"));
}

#[test]
fn read_text_bounded_decodes_utf16_files_from_powershell_redirects() {
    // PowerShell 5.1 `>` writes UTF-16 LE with a BOM. Bench run5: reading
    // such a file returned ok with EMPTY content (the longest valid UTF-8
    // prefix of "\xff\xfe..." is zero bytes) — the silent-lie failure mode.
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());

    let mut le = vec![0xff, 0xfe];
    le.extend("9926\r\n".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(root.path().join("result.txt"), &le).unwrap();
    let result = tool.read_text_bounded("result.txt", 4096).unwrap();
    assert!(result.content.contains("9926"), "{:?}", result.content);

    let mut be = vec![0xfe, 0xff];
    be.extend("beacon".encode_utf16().flat_map(u16::to_be_bytes));
    std::fs::write(root.path().join("be.txt"), &be).unwrap();
    let result = tool.read_text_bounded("be.txt", 4096).unwrap();
    assert!(result.content.contains("beacon"), "{:?}", result.content);

    // A UTF-8 BOM is stripped rather than leaked into the content.
    std::fs::write(root.path().join("sig.txt"), b"\xef\xbb\xbfplain").unwrap();
    let result = tool.read_text_bounded("sig.txt", 4096).unwrap();
    assert_eq!(result.content, "plain");
}

#[test]
fn read_text_bounded_truncates_large_file_and_reports_metadata() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src").join("large.txt"),
        "0123456789abcdef",
    )
    .unwrap();

    let result = tool.read_text_bounded("src/large.txt", 6).unwrap();

    assert_eq!(result.path, "src/large.txt");
    assert_eq!(result.content, "012345");
    assert_eq!(result.bytes_read, 6);
    assert_eq!(result.total_bytes, 16);
    assert!(result.truncated);
}

#[test]
fn tail_text_reads_only_bounded_suffix_and_can_keep_last_lines() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("logs")).unwrap();
    let content = "ignore\nline1\nline2\nline3\nline4\n";
    std::fs::write(root.path().join("logs").join("app.log"), content).unwrap();

    let result = tool.tail_text("logs/app.log", 1024, Some(2)).unwrap();

    assert_eq!(result.path, "logs/app.log");
    assert_eq!(result.content, "line3\nline4\n");
    assert_eq!(result.bytes_read, "line3\nline4\n".len());
    assert_eq!(result.total_bytes, content.len() as u64);
    assert!(result.truncated_prefix);
}

#[test]
fn hash_file_streams_bytes_and_reports_blake3_digest() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"hash me").unwrap();

    let result = tool.hash_file("artifacts/data.bin").unwrap();

    assert_eq!(result.path, "artifacts/data.bin");
    assert_eq!(result.bytes, 7);
    assert_eq!(result.algorithm, "blake3");
    assert_eq!(result.hash, blake3::hash(b"hash me").to_hex().to_string());
}

#[test]
fn stat_path_reports_file_and_directory_metadata_without_reading_content() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("artifacts")).unwrap();
    std::fs::write(root.path().join("artifacts").join("data.bin"), b"metadata").unwrap();

    let file = tool.stat_path("artifacts/data.bin").unwrap();
    assert_eq!(file.path, "artifacts/data.bin");
    assert!(file.is_file);
    assert!(!file.is_dir);
    assert_eq!(file.len, Some(8));
    assert!(file.modified_unix_seconds.is_some());

    let dir = tool.stat_path("artifacts").unwrap();
    assert_eq!(dir.path, "artifacts");
    assert!(!dir.is_file);
    assert!(dir.is_dir);
    assert_eq!(dir.len, None);
}

#[test]
fn replace_text_updates_first_literal_match_and_reports_lengths() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("notes.txt"), "alpha beta beta").unwrap();

    let result = tool
        .replace_text("src/notes.txt", "beta", "done", Some(1))
        .unwrap();

    assert_eq!(result.path, "src/notes.txt");
    assert_eq!(result.replacements, 1);
    assert_eq!(result.previous_len, 15);
    assert_eq!(result.new_len, 15);
    assert_eq!(
        std::fs::read_to_string(root.path().join("src").join("notes.txt")).unwrap(),
        "alpha done beta"
    );
}

#[test]
fn replace_text_returns_error_without_writing_when_match_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("notes.txt"), "alpha beta").unwrap();

    let err = tool
        .replace_text("src/notes.txt", "gamma", "done", Some(1))
        .unwrap_err();

    assert!(err.to_string().contains("text to replace was not found"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("src").join("notes.txt")).unwrap(),
        "alpha beta"
    );
}

#[test]
fn list_files_returns_sorted_bounded_workspace_relative_entries() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src").join("nested")).unwrap();
    std::fs::write(root.path().join("src").join("b.txt"), "bravo").unwrap();
    std::fs::write(root.path().join("src").join("a.txt"), "alpha").unwrap();
    std::fs::write(
        root.path().join("src").join("nested").join("c.txt"),
        "charlie",
    )
    .unwrap();

    let result = tool.list_files("src", 2, None, false).unwrap();

    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].path, "src/a.txt");
    assert_eq!(result.entries[1].path, "src/b.txt");
    assert!(result.truncated);
    assert_eq!(result.scanned, 3);
}

#[test]
fn list_files_stops_scanning_after_limit_plus_one() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("many")).unwrap();
    for index in 0..40 {
        std::fs::write(
            root.path()
                .join("many")
                .join(format!("file-{index:02}.txt")),
            "entry",
        )
        .unwrap();
    }

    let result = tool.list_files("many", 5, None, false).unwrap();

    assert_eq!(result.entries.len(), 5);
    assert_eq!(result.scanned, 6);
    assert!(result.truncated);
    assert!(
        result
            .entries
            .iter()
            .all(|entry| entry.path.starts_with("many/"))
    );
}

#[test]
fn search_text_finds_matches_with_line_numbers_and_limits_large_files() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("a.txt"), "alpha\nneedle one\n").unwrap();
    std::fs::write(
        root.path().join("src").join("b.txt"),
        "needle two\nneedle three\n",
    )
    .unwrap();

    let result = tool.search_text("src", "needle", 2, 1024).unwrap();

    assert_eq!(result.matches.len(), 2);
    assert_eq!(result.matches[0].path, "src/a.txt");
    assert_eq!(result.matches[0].line_number, 2);
    assert_eq!(result.matches[0].line, "needle one");
    assert_eq!(result.matches[1].path, "src/b.txt");
    assert_eq!(result.matches[1].line_number, 1);
    assert!(result.truncated);
    assert_eq!(result.scanned_files, 2);
}

#[test]
fn delete_path_removes_workspace_file_and_reports_kind() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("notes")).unwrap();
    std::fs::write(root.path().join("notes").join("old.txt"), "remove me").unwrap();

    let result = tool.delete_path("notes/old.txt").unwrap();

    assert_eq!(result.path, "notes/old.txt");
    assert!(!result.was_dir);
    assert!(!root.path().join("notes").join("old.txt").exists());
}

#[test]
fn move_path_renames_workspace_file_without_overwriting_by_default() {
    let root = tempfile::tempdir().unwrap();
    let tool = FileTool::new(root.path());
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src").join("draft.txt"), "draft").unwrap();

    let result = tool
        .move_path("src/draft.txt", "notes/final.txt", false)
        .unwrap();

    assert_eq!(result.source_path, "src/draft.txt");
    assert_eq!(result.target_path, "notes/final.txt");
    assert!(!result.overwritten);
    assert!(!root.path().join("src").join("draft.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("notes").join("final.txt")).unwrap(),
        "draft"
    );

    std::fs::write(root.path().join("src").join("draft.txt"), "new").unwrap();
    let err = tool
        .move_path("src/draft.txt", "notes/final.txt", false)
        .unwrap_err();
    assert!(err.to_string().contains("destination already exists"));
}
