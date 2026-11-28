//! Attachment preparation through the production adapters with synthetic
//! fixtures: pinned snapshot reads, the PDF sandbox, image validation and
//! metadata removal, and sandboxed recognition of a validated JPEG.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use agent_core::{
    agent::{AgentErrorCode, AgentResult},
    attachment::AttachmentKind,
    knowledge::SourceReference,
};
use agent_tools::{
    attachment::SelectedAttachment,
    ocr::{Tesseract, character_error_rate},
};

fn ok() -> AgentResult<()> {
    Ok(())
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn real_jpeg_and_png_validate_and_lose_their_metadata() {
    let original = std::fs::read(fixture("ocr-latin-eng-14.jpg")).unwrap();
    for secret in [
        &b"SYNTHETIC-PRIVATE-DESCRIPTION"[..],
        b"synthetic private comment",
    ] {
        assert!(original.windows(secret.len()).any(|w| w == secret));
    }
    let snapshot = SelectedAttachment::pin(&fixture("ocr-latin-eng-14.jpg"))
        .unwrap()
        .read(&ok)
        .unwrap();
    assert_eq!(snapshot.kind(), AttachmentKind::Jpeg);
    let image = snapshot.validate_image(&ok).unwrap();
    assert_eq!((image.width, image.height), (277, 72));
    for secret in [
        &b"SYNTHETIC-PRIVATE-DESCRIPTION"[..],
        b"synthetic private comment",
        b"Exif",
    ] {
        assert!(!image.bytes.windows(secret.len()).any(|w| w == secret));
    }
    let png = SelectedAttachment::pin(&fixture("ocr-latin-eng-14.png"))
        .unwrap()
        .read(&ok)
        .unwrap()
        .validate_image(&ok)
        .unwrap();
    assert_eq!(png.kind, AttachmentKind::Png);
}

#[test]
#[ignore = "uses the system Poppler and Tesseract (English) with Bubblewrap isolation"]
fn pdfs_extract_with_page_references_and_stripped_jpegs_are_recognized() {
    let pdf = SelectedAttachment::pin(&fixture("conversation.pdf"))
        .unwrap()
        .read(&ok)
        .unwrap()
        .extract_document(&ok)
        .unwrap();
    assert!(
        pdf.chunks
            .iter()
            .any(|chunk| chunk.text.contains("Juniper Orchard"))
    );
    assert!(matches!(
        pdf.chunks[0].source,
        SourceReference::Page { page: 1 }
    ));
    assert!(!pdf.pdf_coverage.unwrap().is_partial());
    // A scan is reported as unread rather than pretended.
    let scanned = SelectedAttachment::pin(&fixture("image-only.pdf"))
        .unwrap()
        .read(&ok)
        .unwrap()
        .extract_document(&ok)
        .unwrap_err();
    assert_eq!(scanned.code, AgentErrorCode::PdfNoText);
    assert!(scanned.message.contains("scan"));
    let encrypted = SelectedAttachment::pin(&fixture("protected.pdf"))
        .unwrap()
        .read(&ok)
        .unwrap()
        .extract_document(&ok)
        .unwrap_err();
    assert_eq!(encrypted.code, AgentErrorCode::PdfEncrypted);

    let image = SelectedAttachment::pin(&fixture("ocr-latin-eng-14.jpg"))
        .unwrap()
        .read(&ok)
        .unwrap()
        .validate_image(&ok)
        .unwrap();
    let tesseract = Tesseract::discover().expect("Tesseract with English data");
    let output = tesseract
        .recognize_image(&image.bytes, &["eng".to_owned()], &|| Ok(()))
        .unwrap();
    let cer = character_error_rate(
        "Settings Privacy Notifications Help\nSave changes before closing?",
        &output.text,
    );
    assert!(cer <= 0.05, "CER {cer}: {}", output.text);
}
