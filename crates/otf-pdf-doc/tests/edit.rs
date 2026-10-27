//! Editing: pages, merge/extract, outlines, destinations, metadata,
//! attachments, each checked through a save and reopen.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use otf_pdf_doc::{AttachmentOptions, Document, Info, OutlineItem, Rect, SaveMode, SaveOptions};
use otf_pdf_syntax::{Dict, ObjId, Object, OpenOptions};

/// A classic-xref PDF from `(number, body)` pairs; object 1 is the catalog.
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
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Three pages (10, 11, 12) under an intermediate node that supplies the
/// media box and rotation; content `(Page N)`; one shared font; outlines to
/// pages 11 (explicit), 12 (named, via /Dests) and 11 (GoTo, via the name
/// tree); named destinations for both.
fn three_pages() -> Vec<u8> {
    let content = |n: u32| format!("<< /Length 14 >>\nstream\nBT (Page {n}) ET\nendstream");
    build(&[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 30 0 R /Dests << /twelve [12 0 R /Fit] >> \
             /Names << /Dests << /Names [(eleven) [11 0 R /Fit]] >> >> >>",
        ),
        (2, "<< /Type /Pages /Kids [3 0 R 12 0 R] /Count 3 >>"),
        (
            3,
            "<< /Type /Pages /Parent 2 0 R /Kids [10 0 R 11 0 R] /Count 2 /MediaBox [0 0 300 400] /Rotate 90 >>",
        ),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (
            10,
            "<< /Type /Page /Parent 3 0 R /Contents 20 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        ),
        (
            11,
            "<< /Type /Page /Parent 3 0 R /Contents 21 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        ),
        (
            12,
            "<< /Type /Page /Parent 2 0 R /Contents 22 0 R /MediaBox [0 0 612 792] >>",
        ),
        (20, &content(10)),
        (21, &content(11)),
        (22, &content(12)),
        (
            30,
            "<< /Type /Outlines /First 31 0 R /Last 34 0 R /Count 4 >>",
        ),
        (
            31,
            "<< /Title (To 11) /Parent 30 0 R /Next 32 0 R /Dest [11 0 R /Fit] /First 35 0 R /Last 35 0 R /Count 1 /C [1 0 0] >>",
        ),
        (
            32,
            "<< /Title (To 12) /Parent 30 0 R /Prev 31 0 R /Next 33 0 R /Dest /twelve >>",
        ),
        (
            33,
            "<< /Title (Go 11) /Parent 30 0 R /Prev 32 0 R /Next 34 0 R /A << /S /GoTo /D (eleven) >> >>",
        ),
        (
            34,
            "<< /Title (To 10) /Parent 30 0 R /Prev 33 0 R /Dest [10 0 R /Fit] >>",
        ),
        (
            35,
            "<< /Title (Child of 11) /Parent 31 0 R /Dest [10 0 R /XYZ 0 0 0] >>",
        ),
    ])
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes, OpenOptions::default()).unwrap()
}

fn reopen(doc: &Document) -> Document {
    open(doc.save(&SaveOptions::default()).unwrap())
}

fn texts(doc: &Document) -> Vec<String> {
    doc.pages()
        .unwrap()
        .iter()
        .map(|p| {
            let Object::Stream(s) = doc.store().resolve(p.dict.get(b"Contents").unwrap()) else {
                panic!("no content");
            };
            String::from_utf8(doc.store().decode_stream(&s).unwrap().data).unwrap()
        })
        .collect()
}

fn titles(items: &[OutlineItem]) -> Vec<String> {
    items.iter().map(|i| i.title.clone()).collect()
}

#[test]
fn reorder_keeps_inherited_attributes() {
    let mut doc = open(three_pages());
    doc.reorder_pages(&[2, 0, 1]).unwrap();
    for d in [&doc, &reopen(&doc)] {
        assert_eq!(
            texts(d),
            ["BT (Page 12) ET", "BT (Page 10) ET", "BT (Page 11) ET"]
        );
        let pages = d.pages().unwrap();
        assert_eq!(pages[1].media_box.urx, 300.0);
        assert_eq!(pages[1].rotate, 90);
        assert_eq!(pages[0].media_box.urx, 612.0);
        assert_eq!(pages[0].rotate, 0);
    }
    assert!(doc.reorder_pages(&[0, 1]).is_err());
    assert!(doc.reorder_pages(&[0, 0, 1]).is_err());
}

#[test]
fn delete_cleans_up_outlines_and_named_destinations() {
    let mut doc = open(three_pages());
    doc.delete_pages(&[1]).unwrap();
    for d in [&doc, &reopen(&doc)] {
        assert_eq!(texts(d), ["BT (Page 10) ET", "BT (Page 12) ET"]);
        let outlines = d.outlines().unwrap();
        // "To 11" and "Go 11" are gone; the child of "To 11" is promoted.
        assert_eq!(titles(&outlines), ["Child of 11", "To 12", "To 10"]);
        let names: Vec<_> = d
            .named_destinations()
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(names, [b"twelve".to_vec()]);
    }
    assert!(doc.delete_pages(&[9]).is_err());
}

#[test]
fn outline_extras_survive_a_rewrite() {
    let mut doc = open(three_pages());
    doc.delete_pages(&[2]).unwrap();
    let outlines = reopen(&doc).outlines().unwrap();
    assert_eq!(titles(&outlines), ["To 11", "Go 11", "To 10"]);
    assert_eq!(outlines[0].extra.get_array(b"C").map(<[_]>::len), Some(3));
    assert_eq!(outlines[0].children.len(), 1);
}

#[test]
fn duplicate_selection_makes_distinct_pages() {
    let mut doc = open(three_pages());
    doc.select_pages(&[0, 0]).unwrap();
    let again = reopen(&doc);
    let pages = again.pages().unwrap();
    assert_eq!(pages.len(), 2);
    assert_ne!(pages[0].id, pages[1].id);
    assert_eq!(texts(&again), ["BT (Page 10) ET", "BT (Page 10) ET"]);
}

#[test]
fn rotate_and_crop() {
    let mut doc = open(three_pages());
    doc.rotate_page(0, 90).unwrap();
    doc.rotate_page(0, 90).unwrap();
    doc.rotate_page(2, -90).unwrap();
    assert!(doc.rotate_page(0, 45).is_err());
    assert!(doc.rotate_page(7, 90).is_err());
    let crop = Rect {
        llx: 10.0,
        lly: 10.0,
        urx: 100.0,
        ury: 100.0,
    };
    doc.set_crop_box(1, crop).unwrap();
    let again = reopen(&doc);
    let pages = again.pages().unwrap();
    assert_eq!(pages[0].rotate, 270);
    assert_eq!(pages[2].rotate, 270);
    assert_eq!(pages[1].crop_box, crop);
}

#[test]
fn incremental_edit_appends_only_changes() {
    let original = three_pages();
    let mut doc = open(original.clone());
    doc.rotate_page(2, 180).unwrap();
    let saved = doc
        .save(&SaveOptions {
            mode: SaveMode::Incremental,
            ..SaveOptions::default()
        })
        .unwrap();
    assert!(saved.starts_with(&original));
    let again = open(saved);
    assert_eq!(again.pages().unwrap()[2].rotate, 180);
    assert_eq!(again.file().sections().len(), 2);
}

#[test]
fn merge_copies_pages_and_shares_objects() {
    let mut a = open(three_pages());
    let b = open(three_pages());
    let before = a.store().object_ids().len();
    a.append_pages_from(&b, Some(&[1, 0])).unwrap();
    let merged = reopen(&a);
    assert_eq!(
        texts(&merged),
        [
            "BT (Page 10) ET",
            "BT (Page 11) ET",
            "BT (Page 12) ET",
            "BT (Page 11) ET",
            "BT (Page 10) ET"
        ]
    );
    // Copied pages keep inherited attributes.
    assert_eq!(merged.pages().unwrap()[3].rotate, 90);
    // Both imported pages use font 5 of `b`: it is copied once (two pages,
    // two contents, one font, plus the rebuilt tree).
    let added = a.store().object_ids().len() - before;
    assert_eq!(added, 2 + 2 + 1);
    // Merging a document into itself works too.
    let copy = open(three_pages());
    a.append_pages_from(&copy, None).unwrap();
    assert_eq!(reopen(&a).page_count().unwrap(), 8);
}

#[test]
fn extract_into_a_new_document() {
    let doc = open(three_pages());
    let part = doc.extract_pages(&[2]).unwrap();
    let again = reopen(&part);
    assert_eq!(texts(&again), ["BT (Page 12) ET"]);
    assert!(Document::new().unwrap().extract_pages(&[0]).is_err());
}

#[test]
fn info_and_xmp_stay_in_sync() {
    let mut doc = open(three_pages());
    let info = Info {
        title: Some("Résumé Δ".into()),
        author: Some("Ann".into()),
        mod_date: Some("D:20261027120000Z".into()),
        trapped: Some("False".into()),
        ..Info::default()
    };
    doc.set_info(&info).unwrap();
    for d in [&doc, &reopen(&doc)] {
        assert_eq!(d.info(), info);
        let xmp = String::from_utf8(d.xmp_metadata().unwrap().unwrap()).unwrap();
        assert!(xmp.contains(">Résumé Δ<"), "{xmp}");
        assert!(xmp.contains("<xmp:ModifyDate>2026-10-27T12:00:00Z</xmp:ModifyDate>"));
    }
    // Clearing a field removes it from both.
    doc.set_info(&Info {
        author: None,
        ..info
    })
    .unwrap();
    let again = reopen(&doc);
    assert_eq!(again.info().author, None);
    let xmp = String::from_utf8(again.xmp_metadata().unwrap().unwrap()).unwrap();
    assert!(!xmp.contains("dc:creator"));
}

#[test]
fn outlines_are_written_with_counts() {
    let mut doc = open(three_pages());
    let page = |n: u32| {
        Some(Object::Array(vec![
            Object::Ref(ObjId::new(n, 0)),
            Object::Name("Fit".into()),
        ]))
    };
    let leaf = |t: &str, n: u32| OutlineItem {
        id: None,
        title: t.into(),
        dest: page(n),
        action: None,
        open: false,
        children: vec![],
        extra: Dict::new(),
    };
    let tree = vec![
        OutlineItem {
            open: true,
            children: vec![leaf("a1", 10), leaf("a2", 11)],
            ..leaf("a", 10)
        },
        OutlineItem {
            open: false,
            children: vec![leaf("b1", 12)],
            ..leaf("b", 12)
        },
    ];
    doc.set_outlines(&tree).unwrap();
    let again = reopen(&doc);
    let read = again.outlines().unwrap();
    assert_eq!(titles(&read), ["a", "b"]);
    assert_eq!(titles(&read[0].children), ["a1", "a2"]);
    assert!(read[0].open && !read[1].open);
    let root = again
        .store()
        .resolve_dict(again.catalog().get(b"Outlines").unwrap())
        .unwrap();
    // a, a1, a2, b visible.
    assert_eq!(root.get_int(b"Count"), Some(4));
    let b = again.store().get(read[1].id.unwrap()).unwrap();
    assert_eq!(b.as_dict().unwrap().get_int(b"Count"), Some(-1));

    doc.set_outlines(&[]).unwrap();
    assert!(reopen(&doc).outlines().unwrap().is_empty());
}

#[test]
fn attachments_add_replace_remove() {
    let mut doc = open(three_pages());
    let options = AttachmentOptions {
        mime_type: Some("text/plain".into()),
        description: Some("Notes".into()),
        ..AttachmentOptions::default()
    };
    doc.add_attachment("b.txt", b"bee", &options).unwrap();
    doc.add_attachment("ä.txt", &vec![b'x'; 1000], &AttachmentOptions::default())
        .unwrap();
    doc.add_attachment("b.txt", b"bee 2", &options).unwrap();
    let again = reopen(&doc);
    let atts = again.attachments().unwrap();
    let names: Vec<_> = atts.iter().map(|a| a.name.clone()).collect();
    assert_eq!(names, ["b.txt", "ä.txt"]);
    assert_eq!(atts[0].mime_type.as_deref(), Some("text/plain"));
    assert_eq!(atts[0].description.as_deref(), Some("Notes"));
    assert_eq!(again.attachment_data(&atts[0]).unwrap(), b"bee 2");
    assert_eq!(again.attachment_data(&atts[1]).unwrap(), vec![b'x'; 1000]);
    assert_eq!(atts[1].filename.as_deref(), Some("ä.txt"));

    let mut again = again;
    assert!(again.remove_attachment("b.txt").unwrap());
    assert!(!again.remove_attachment("missing").unwrap());
    assert!(again.remove_attachment("ä.txt").unwrap());
    let empty = reopen(&again);
    assert!(empty.attachments().unwrap().is_empty());
    // The other named trees survive.
    assert_eq!(empty.named_destinations().unwrap().len(), 2);
}

#[test]
fn new_document_builds_up() {
    let mut doc = Document::new().unwrap();
    doc.append_pages_from(&open(three_pages()), Some(&[0]))
        .unwrap();
    doc.set_info(&Info {
        title: Some("Built".into()),
        ..Info::default()
    })
    .unwrap();
    let again = reopen(&doc);
    assert_eq!(again.page_count().unwrap(), 1);
    assert_eq!(again.info().title.as_deref(), Some("Built"));
}

#[test]
fn full_save_writes_identical_streams_once() {
    let mut doc = open(three_pages());
    let copy = open(three_pages());
    // Two separate merges copy the same content streams twice.
    doc.append_pages_from(&copy, Some(&[0])).unwrap();
    doc.append_pages_from(&copy, Some(&[0])).unwrap();
    let again = reopen(&doc);
    assert_eq!(again.page_count().unwrap(), 5);
    let contents: std::collections::HashSet<_> = again
        .pages()
        .unwrap()
        .iter()
        .map(|p| p.dict.get_ref(b"Contents").unwrap())
        .collect();
    // Pages 10, 11, 12 plus one shared copy of page 10's content.
    assert_eq!(contents.len(), 3);
}
