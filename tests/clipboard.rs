use harness_cli::clipboard::{
    AttachmentStore, ClipboardCapture, ClipboardItem, ClipboardSource, StaticClipboard,
};

#[test]
fn clipboard_capture_saves_text_as_attachment_with_prompt_fragment() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(Some(ClipboardItem::Text("hello clipboard".to_string())));
    let capture = ClipboardCapture::new(AttachmentStore::new(root.path()));

    let attachment = capture.capture(&source).unwrap().unwrap();

    assert_eq!(attachment.kind, "text");
    assert_eq!(attachment.mime_type, "text/plain; charset=utf-8");
    assert_eq!(
        std::fs::read_to_string(root.path().join(&attachment.relative_path)).unwrap(),
        "hello clipboard"
    );
    assert!(attachment.prompt_fragment.contains("hello clipboard"));
}

#[test]
fn clipboard_capture_saves_png_image_bytes_as_attachment() {
    let root = tempfile::tempdir().unwrap();
    let png = vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
    let source = StaticClipboard::new(Some(ClipboardItem::ImagePng(png.clone())));
    let capture = ClipboardCapture::new(AttachmentStore::new(root.path()));

    let attachment = capture.capture(&source).unwrap().unwrap();

    assert_eq!(attachment.kind, "image");
    assert_eq!(attachment.mime_type, "image/png");
    assert_eq!(
        std::fs::read(root.path().join(&attachment.relative_path)).unwrap(),
        png
    );
    assert!(attachment.prompt_fragment.contains("image file:"));
    assert!(attachment.relative_path.ends_with(".png"));
}

#[test]
fn clipboard_capture_returns_none_when_clipboard_is_empty() {
    let root = tempfile::tempdir().unwrap();
    let source = StaticClipboard::new(None);
    let capture = ClipboardCapture::new(AttachmentStore::new(root.path()));

    assert!(capture.capture(&source).unwrap().is_none());
}

#[test]
fn static_clipboard_implements_source_trait() {
    let source = StaticClipboard::new(Some(ClipboardItem::Text("trait".to_string())));

    assert!(matches!(
        source.read().unwrap(),
        Some(ClipboardItem::Text(_))
    ));
}
