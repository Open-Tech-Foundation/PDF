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
    let report = doc
        .sanitize(&SanitizeOptions::ALL.without_content())
        .unwrap();
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
    assert!(
        doc.sanitize(&SanitizeOptions::ALL.without_content())
            .unwrap()
            .is_empty()
    );
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

/// Comments, hidden annotations and the hidden-data catalog entries the
/// profiles differ on, plus a form widget that every profile keeps.
fn private() -> Vec<u8> {
    build(&[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 20 0 R /Dests << /d [3 0 R /Fit] >> \
             /Threads [21 0 R] /PageLabels << /Nums [0 << /S /D >>] >> \
             /SpiderInfo << /V 1.0 >> /StructTreeRoot 23 0 R /MarkInfo << /Marked true >> \
             /Names << /Dests << /Names [] >> /IDS << /Names [] >> /URLS << /Names [] >> >> \
             /AcroForm << /Fields [14 0 R] >> >>",
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /Thumb 24 0 R /B [22 0 R] /ID (wc-set) \
             /StructParents 0 /LastModified (D:20240101) \
             /Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R] >>",
        ),
        (9, "<< /Title (T) >>"),
        (
            10,
            "<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents (secret remark) /Popup 11 0 R >>",
        ),
        (
            11,
            "<< /Type /Annot /Subtype /Popup /Rect [0 0 1 1] /Parent 10 0 R >>",
        ),
        (
            12,
            "<< /Type /Annot /Subtype /Highlight /Rect [0 0 1 1] /QuadPoints [0 0 1 0 0 1 1 1] >>",
        ),
        (
            13,
            "<< /Type /Annot /Subtype /Square /Rect [0 0 1 1] /F 2 /Contents (hidden) >>",
        ),
        (
            14,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [0 0 1 1] /F 2 >>",
        ),
        (
            15,
            "<< /Type /Annot /Subtype /PrinterMark /Rect [0 0 1 1] /F 32 /StructParent 1 >>",
        ),
        (16, "<< /Type /Annot /Subtype /Redact /Rect [0 0 1 1] >>"),
        (20, "<< /Type /Outlines /Count 0 >>"),
        (21, "<< /Type /Thread /F 22 0 R >>"),
        (22, "<< /Type /Bead /T 21 0 R /P 3 0 R /R [0 0 1 1] >>"),
        (23, "<< /Type /StructTreeRoot /K [] >>"),
        (
            24,
            "<< /Width 1 /Height 1 /Length 1 >>\nstream\nX\nendstream",
        ),
    ])
}

fn subtypes(doc: &Document) -> Vec<String> {
    doc.pages().unwrap()[0]
        .dict
        .get_array(b"Annots")
        .unwrap_or_default()
        .iter()
        .map(|a| {
            let d = doc.store().resolve_dict(a).unwrap();
            String::from_utf8_lossy(d.get_name(b"Subtype").unwrap()).into_owned()
        })
        .collect()
}

fn sanitized(options: SanitizeOptions) -> (otf_pdf_doc::SanitizeReport, Document, String) {
    let mut doc = open(private());
    let report = doc.sanitize(&options).unwrap();
    let saved = doc.save(&SaveOptions::default()).unwrap();
    let text = String::from_utf8_lossy(&saved).into_owned();
    (report, open(saved), text)
}

#[test]
fn default_keeps_comments_and_hidden_data() {
    let (report, clean, _) = sanitized(SanitizeOptions::default());
    assert!(report.is_empty(), "{report:?}");
    assert_eq!(subtypes(&clean).len(), 7);
    assert!(clean.catalog().get(b"Outlines").is_some());
}

#[test]
fn basic_removes_comments_and_hidden_annotations_but_keeps_widgets() {
    let (report, clean, text) = sanitized(SanitizeOptions::BASIC);
    // Text, Popup, Highlight; and the hidden Square is markup too.
    assert_eq!(report.comments, 4, "{report:?}");
    // The PrinterMark (NoView); the hidden widget is kept.
    assert_eq!(report.hidden_annotations, 1, "{report:?}");
    assert_eq!(subtypes(&clean), ["Widget", "Redact"]);
    assert!(!text.contains("secret remark"));
    // Page /LastModified and the Info dictionary are metadata.
    let page = &clean.pages().unwrap()[0];
    assert!(page.dict.get(b"LastModified").is_none());
    assert_eq!(clean.info().title, None);
    // Hidden data that BASIC leaves alone.
    assert!(page.dict.get(b"Thumb").is_some());
    assert!(page.dict.get(b"ID").is_some());
    assert!(clean.catalog().get(b"Outlines").is_some());
    assert!(clean.catalog().get(b"StructTreeRoot").is_some());
    assert_eq!(
        report.thumbnails + report.web_capture + report.navigation + report.structure,
        0
    );
}

#[test]
fn privacy_also_removes_thumbnails_and_web_capture() {
    let (report, clean, _) = sanitized(SanitizeOptions::PRIVACY.without_content());
    assert_eq!(report.thumbnails, 1);
    // /SpiderInfo, /IDS, /URLS and the page's content-set /ID.
    assert_eq!(report.web_capture, 4, "{report:?}");
    let catalog = clean.catalog();
    let page = &clean.pages().unwrap()[0];
    assert!(page.dict.get(b"Thumb").is_none() && page.dict.get(b"ID").is_none());
    assert!(catalog.get(b"SpiderInfo").is_none());
    let names = catalog.get_dict(b"Names").unwrap();
    assert!(names.get(b"IDS").is_none() && names.get(b"URLS").is_none());
    // Navigation and tagging stay.
    assert!(names.get(b"Dests").is_some());
    assert!(catalog.get(b"Outlines").is_some() && catalog.get(b"MarkInfo").is_some());
    assert_eq!(subtypes(&clean), ["Widget", "Redact"]);
}

#[test]
fn strict_removes_navigation_and_structure_too() {
    let (report, clean, _) = sanitized(SanitizeOptions::STRICT.without_content());
    // /Outlines, /Dests, /Threads, /PageLabels, the /Dests tree, the bead.
    assert_eq!(report.navigation, 6, "{report:?}");
    // /StructTreeRoot, /MarkInfo, the page's /StructParents and the
    // PrinterMark's /StructParent (every object is cleaned, removed
    // annotations included).
    assert_eq!(report.structure, 4, "{report:?}");
    let catalog = clean.catalog();
    for key in [
        &b"Outlines"[..],
        b"Dests",
        b"Threads",
        b"PageLabels",
        b"StructTreeRoot",
        b"MarkInfo",
    ] {
        assert!(
            catalog.get(key).is_none(),
            "{}",
            String::from_utf8_lossy(key)
        );
    }
    assert!(catalog.get_dict(b"Names").unwrap().get(b"Dests").is_none());
    let page = &clean.pages().unwrap()[0];
    assert!(page.dict.get(b"B").is_none() && page.dict.get(b"StructParents").is_none());
    // The form survives every profile.
    assert_eq!(subtypes(&clean), ["Widget", "Redact"]);
    let fields = catalog
        .get_dict(b"AcroForm")
        .unwrap()
        .get_array(b"Fields")
        .unwrap();
    assert_eq!(fields.len(), 1);
}

#[test]
fn single_categories_can_be_switched_on_alone() {
    let mut doc = open(private());
    let report = doc
        .sanitize(&SanitizeOptions {
            thumbnails: true,
            ..SanitizeOptions::NONE
        })
        .unwrap();
    assert_eq!(
        report,
        otf_pdf_doc::SanitizeReport {
            thumbnails: 1,
            ..Default::default()
        }
    );
    // A profile with one category turned back off.
    let mut doc = open(private());
    let report = doc
        .sanitize(&SanitizeOptions {
            comments: false,
            ..SanitizeOptions::BASIC
        })
        .unwrap();
    assert_eq!(report.comments, 0);
    // The hidden Square is still removed, as a hidden annotation.
    assert_eq!(report.hidden_annotations, 2, "{report:?}");
}
