//! Document-level reading: page tree, metadata, outlines, attachments.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use otf_pdf_doc::{Document, Rect};
use otf_pdf_syntax::{DiagnosticKind, OpenOptions, Strictness};

/// Builds a classic-xref PDF from `(number, body)` pairs; object 1 is the
/// catalog.
fn build(objects: &[(u32, &[u8])], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (num, body) in objects {
        offsets.push((*num, out.len()));
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
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
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes, OpenOptions::default()).unwrap()
}

#[test]
fn nested_page_tree_with_inheritance() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 /MediaBox [0 0 595 842] \
                  /Resources << /Font << /F1 9 0 R >> >> /Rotate 90 >>",
            ),
            (
                3,
                b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 /Rotate -90 >>",
            ),
            (4, b"<< /Type /Page /Parent 3 0 R >>"),
            (
                5,
                b"<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 200] /CropBox [10 10 500 500] >>",
            ),
            (6, b"<< /Type /Page /Parent 2 0 R /Rotate 45 >>"),
            (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        "",
    ));
    let pages = doc.pages().unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(doc.page_count().unwrap(), 3);
    let ids: Vec<_> = pages.iter().map(|p| p.id.unwrap().num).collect();
    assert_eq!(ids, [4, 5, 6]);

    let a4 = Rect {
        llx: 0.0,
        lly: 0.0,
        urx: 595.0,
        ury: 842.0,
    };
    assert_eq!(pages[0].media_box, a4);
    assert_eq!(pages[0].crop_box, a4);
    assert_eq!(pages[0].rotate, 270);
    assert!(
        pages[0]
            .resources
            .as_ref()
            .unwrap()
            .get_dict(b"Font")
            .is_some()
    );

    // The crop box is clipped to the page's own media box.
    assert_eq!(pages[1].media_box.urx, 100.0);
    assert_eq!(
        pages[1].crop_box,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 100.0,
            ury: 200.0
        }
    );
    // An invalid rotation is ignored; inheritance is not.
    assert_eq!(pages[2].rotate, 0);
    assert!(doc.diagnostics().is_empty(), "{:?}", doc.diagnostics());
}

#[test]
fn missing_media_box_defaults_to_letter() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
        ],
        "",
    ));
    assert_eq!(doc.pages().unwrap()[0].media_box, Rect::LETTER);
}

#[test]
fn cyclic_page_tree_is_cut() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R 2 0 R 4 0 R] /Count 2 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
        ],
        "",
    ));
    assert_eq!(doc.page_count().unwrap(), 2);
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::BadPageTree)
    );
}

#[test]
fn lone_kid_reference_is_one_kid() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids 3 0 R /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
        ],
        "",
    ));
    assert_eq!(doc.page_count().unwrap(), 1);
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::BadPageTree)
    );
}

#[test]
fn broken_tree_falls_back_to_scanning_for_pages() {
    let bytes = build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids 7 /Count 2 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
            (4, b"<< /Type /Page /Parent 2 0 R >>"),
        ],
        "",
    );
    let doc = open(bytes.clone());
    assert_eq!(doc.page_count().unwrap(), 2);
    assert_eq!(doc.pages().unwrap()[0].media_box.urx, 10.0);
    assert!(
        doc.diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::PagesRecovered)
    );

    let strict = Document::open(
        bytes,
        OpenOptions {
            strictness: Strictness::Strict,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    assert!(strict.pages().is_err());
}

#[test]
fn version_from_catalog() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /Version /2.0 >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ],
        "",
    ));
    assert_eq!(doc.version().unwrap().to_string(), "2.0");
    let older = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /Version /1.4 >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ],
        "",
    ));
    assert_eq!(older.version().unwrap().to_string(), "1.7");
}

#[test]
fn info_and_xmp() {
    let xmp = b"<x:xmpmeta><dc:title>XMP title</dc:title></x:xmpmeta>";
    let meta = [
        format!(
            "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n",
            xmp.len()
        )
        .as_bytes(),
        xmp,
        b"\nendstream",
    ]
    .concat();
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 4 0 R >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
            (
                3,
                b"<< /Title <FEFF0054006900740072006500200394> /Author (Ann \\(A\\)) \
                  /CreationDate (D:20260101120000Z) /Producer 5 0 R /Trapped /False >>",
            ),
            (4, &meta),
            (5, b"(otf-pdf)"),
        ],
        "/Info 3 0 R ",
    ));
    let info = doc.info();
    assert_eq!(info.title.as_deref(), Some("Titre Δ"));
    assert_eq!(info.author.as_deref(), Some("Ann (A)"));
    assert_eq!(info.producer.as_deref(), Some("otf-pdf"));
    assert_eq!(info.creation_date.as_deref(), Some("D:20260101120000Z"));
    assert_eq!(info.trapped.as_deref(), Some("False"));
    assert_eq!(info.subject, None);
    assert_eq!(doc.xmp_metadata().unwrap().unwrap(), xmp);
}

#[test]
fn no_info_no_xmp_no_outlines_no_attachments() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ],
        "",
    ));
    assert_eq!(doc.info(), otf_pdf_doc::Info::default());
    assert_eq!(doc.xmp_metadata().unwrap(), None);
    assert!(doc.outlines().unwrap().is_empty());
    assert!(doc.attachments().unwrap().is_empty());
    assert!(!doc.is_encrypted());
}

#[test]
fn outline_tree() {
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R >>"),
            (10, b"<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 3 >>"),
            (11, b"<< /Title (Chapter 1) /Parent 10 0 R /Next 12 0 R /First 13 0 R /Last 13 0 R /Count 1 /Dest [3 0 R /Fit] >>"),
            (12, b"<< /Title <FEFF0032> /Parent 10 0 R /Prev 11 0 R /Next 11 0 R /A << /S /URI /URI (https://example.org) >> >>"),
            (13, b"<< /Title (Section 1.1) /Parent 11 0 R >>"),
        ],
        "",
    ));
    let outlines = doc.outlines().unwrap();
    // Item 12's /Next loops back to 11; the walk stops there.
    assert_eq!(outlines.len(), 2);
    assert_eq!(outlines[0].title, "Chapter 1");
    assert!(outlines[0].open);
    assert!(outlines[0].dest.as_ref().unwrap().as_array().is_some());
    assert_eq!(outlines[0].children.len(), 1);
    assert_eq!(outlines[0].children[0].title, "Section 1.1");
    assert_eq!(outlines[1].title, "2");
    assert_eq!(
        outlines[1].action.as_ref().unwrap().get_name(b"S"),
        Some(&b"URI"[..])
    );
}

#[test]
fn attachments_from_name_tree() {
    let data = b"hello attachment";
    let ef = [
        format!(
            "<< /Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size {} >> /Length {} >>\nstream\n",
            data.len(),
            data.len()
        )
        .as_bytes(),
        data,
        b"\nendstream",
    ]
    .concat();
    let doc = open(build(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 5 0 R >> >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
            (5, b"<< /Kids [6 0 R 7 0 R] >>"),
            (6, b"<< /Limits [(a) (a)] /Names [(a.txt) 8 0 R] >>"),
            (7, b"<< /Names [(b.bin) << /Type /Filespec /F (b.bin) >>] >>"),
            (8, b"<< /Type /Filespec /F (a.txt) /UF <FEFF0061002E007400780074> /Desc (An A) /EF << /F 9 0 R >> >>"),
            (9, &ef),
        ],
        "",
    ));
    let atts = doc.attachments().unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0].name, "a.txt");
    assert_eq!(atts[0].filename.as_deref(), Some("a.txt"));
    assert_eq!(atts[0].description.as_deref(), Some("An A"));
    assert_eq!(atts[0].mime_type.as_deref(), Some("text/plain"));
    assert_eq!(atts[0].size, Some(data.len() as u64));
    assert_eq!(doc.attachment_data(&atts[0]).unwrap(), data);
    // No embedded stream: listed, but without data.
    assert_eq!(atts[1].name, "b.bin");
    assert_eq!(atts[1].stream, None);
    assert!(doc.attachment_data(&atts[1]).is_err());
}

#[test]
fn encrypted_corpus_pages_and_info() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/encrypted");
    let bytes = std::fs::read(dir.join("r6-aes-256-object-streams.pdf")).unwrap();
    let doc = Document::open(
        bytes,
        OpenOptions {
            password: b"user".to_vec(),
            ..OpenOptions::default()
        },
    )
    .unwrap();
    assert!(doc.is_encrypted());
    assert_eq!(doc.page_count().unwrap(), 1);
    assert_eq!(
        doc.info().title.as_deref(),
        Some("Title r6-aes-256-object-streams")
    );
    let xmp = doc.xmp_metadata().unwrap().unwrap();
    assert!(String::from_utf8_lossy(&xmp).contains("Title r6-aes-256-object-streams"));
}
