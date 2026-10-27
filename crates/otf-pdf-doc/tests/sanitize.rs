//! Sanitizing removes active and hidden content and keeps the rest.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use otf_pdf_doc::{Document, SanitizeOptions, SaveOptions};
use otf_pdf_syntax::{Object, OpenOptions};

fn build(objects: &[(u32, &str)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (num, body) in objects {
        offsets.push((*num, out.len()));
        out.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.iter().find(|(m, _)| *m == n) {
            Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R /Info 9 0 R >>\nstartxref\n{xref}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// One of everything risky, plus things that must survive.
fn risky() -> Vec<u8> {
    build(&[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert(1)) >> \
             /AA << /WC 4 0 R >> /Names 5 0 R /AcroForm << /XFA 6 0 R /Fields [] >> \
             /Metadata 8 0 R >>",
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /Annots [10 0 R 11 0 R 12 0 R 13 0 R] \
             /AA << /O << /S /Launch /F (calc.exe) >> >> /PieceInfo << >> >>",
        ),
        (4, "<< /S /JavaScript /JS (x) >>"),
        (
            5,
            "<< /JavaScript << /Names [(s) 4 0 R] >> /EmbeddedFiles << /Names [] >> /Dests << /Names [] >> >>",
        ),
        (6, "<< /Length 0 >>\nstream\n\nendstream"),
        (
            8,
            "<< /Type /Metadata /Subtype /XML /Length 0 >>\nstream\n\nendstream",
        ),
        (9, "<< /Title (T) >>"),
        (
            10,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI (https://x) >> >>",
        ),
        (
            11,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /GoTo /D [3 0 R /Fit] /Next << /S /SubmitForm >> >> >>",
        ),
        (
            12,
            "<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 1 1] >>",
        ),
        (13, "<< /Type /Annot /Subtype /RichMedia /Rect [0 0 1 1] >>"),
    ])
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes, OpenOptions::default()).unwrap()
}

#[test]
fn default_removes_active_content_and_keeps_links_and_metadata() {
    let mut doc = open(risky());
    let report = doc.sanitize(&SanitizeOptions::default()).unwrap();
    assert_eq!(report.javascript, 3, "{report:?}");
    assert_eq!(report.external_actions, 2, "{report:?}");
    assert_eq!(report.links, 0);
    assert_eq!(report.attachments, 2, "{report:?}");
    assert_eq!(report.xfa, 1);
    assert_eq!(report.multimedia, 1);
    assert_eq!(report.metadata, 0);

    let saved = doc.save(&SaveOptions::default()).unwrap();
    let text = String::from_utf8_lossy(&saved).into_owned();
    let clean = open(saved);
    let catalog = clean.catalog();
    assert!(catalog.get(b"OpenAction").is_none());
    assert!(catalog.get(b"AA").is_none());
    assert!(catalog.get_dict(b"AcroForm").unwrap().get(b"XFA").is_none());
    assert!(catalog.get(b"Metadata").is_some());
    let names = clean
        .store()
        .resolve_dict(catalog.get(b"Names").unwrap())
        .unwrap();
    assert!(names.get(b"JavaScript").is_none() && names.get(b"EmbeddedFiles").is_none());
    assert!(names.get(b"Dests").is_some());
    let page = &clean.pages().unwrap()[0];
    assert!(page.dict.get(b"AA").is_none());
    // The two links stay; the one that chained to SubmitForm loses its action.
    let annots: Vec<_> = page
        .dict
        .get_array(b"Annots")
        .unwrap()
        .iter()
        .map(|a| clean.store().resolve_dict(a).unwrap())
        .collect();
    assert_eq!(annots.len(), 2);
    assert_eq!(
        annots[0].get_dict(b"A").unwrap().get_name(b"S"),
        Some(&b"URI"[..])
    );
    assert!(annots[1].get(b"A").is_none());
    assert_eq!(clean.info().title.as_deref(), Some("T"));
    // The scripts are gone from the bytes, not just unreferenced.
    assert!(!text.contains("app.alert") && !text.contains("calc.exe"));
}

#[test]
fn all_removes_links_and_metadata_too() {
    let mut doc = open(risky());
    let report = doc.sanitize(&SanitizeOptions::ALL).unwrap();
    assert_eq!(report.links, 1);
    assert_eq!(report.metadata, 3, "{report:?}");
    let clean = open(doc.save(&SaveOptions::default()).unwrap());
    assert!(clean.catalog().get(b"Metadata").is_none());
    assert_eq!(clean.info().title, None);
    let annots = clean.pages().unwrap()[0]
        .dict
        .get_array(b"Annots")
        .unwrap()
        .to_vec();
    assert!(
        annots
            .iter()
            .all(|a| clean.store().resolve_dict(a).unwrap().get(b"A").is_none())
    );
    assert!(clean.store().trailer().get(b"Info").is_none());
}

#[test]
fn clean_documents_report_nothing() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/malformed/minimal-valid.pdf");
    let mut doc = open(std::fs::read(path).unwrap());
    assert!(doc.sanitize(&SanitizeOptions::ALL).unwrap().is_empty());
    assert!(!doc.store().is_modified());
    let _ = Object::Null;
}

#[test]
fn indirect_trigger_dictionaries_are_filtered() {
    let bytes = build(&[
        (1, "<< /Type /Catalog /Pages 2 0 R /AA 4 0 R >>"),
        (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
        (
            4,
            "<< /WC << /S /JavaScript /JS (x) >> /DS << /S /GoTo /D [] >> >>",
        ),
        (9, "<< >>"),
    ]);
    let mut doc = open(bytes);
    assert_eq!(
        doc.sanitize(&SanitizeOptions::default())
            .unwrap()
            .javascript,
        1
    );
    let aa = doc.catalog().get_dict(b"AA").cloned().unwrap();
    assert!(aa.get(b"WC").is_none() && aa.get(b"DS").is_some());
}
