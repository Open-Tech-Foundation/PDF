//! Full sanitizing through the facade: hidden layers and invisible text cut
//! from page content, forms and appearances, with visible text left in
//! place, checked on the saved bytes.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use otf_pdf::{Document, OpenOptions, SanitizeOptions, SaveOptions, TextExtractor};

fn build(objects: &[(u32, String)]) -> Vec<u8> {
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

fn stream(dict: &str, content: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    )
}

/// One page with a visible layer (10) and a hidden one (11), hidden
/// sections in the page, a form and an appearance, a hidden image, a form
/// drawn only inside a hidden section, a hidden link and a hidden widget,
/// and an OCR-style invisible line.
fn layered() -> Vec<u8> {
    let fonts = "/Font << /F1 5 0 R >>";
    build(&[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [21 0 R] >> \
             /OCProperties << /OCGs [10 0 R 11 0 R] /D << /OFF [11 0 R] >> >> >>"
                .into(),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
                 /Resources << {fonts} /Properties << /Vis 10 0 R /Hid 11 0 R >> \
                 /XObject << /Img 6 0 R /Fm 7 0 R /Deep 8 0 R >> >> \
                 /Annots [20 0 R 21 0 R 22 0 R] >>"
            ),
        ),
        (
            4,
            stream(
                "",
                "BT /F1 12 Tf 10 250 Td /OC /Vis BDC (Shown) Tj EMC \
                 /OC /Hid BDC (Secret) Tj EMC (After) Tj ET \
                 /OC /Hid BDC /Deep Do EMC /Img Do /Fm Do \
                 BT /F1 12 Tf 10 100 Td 3 Tr (OCR words) Tj 0 Tr (Visible) Tj ET",
            ),
        ),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ),
        (
            6,
            stream(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
                 /ColorSpace /DeviceGray /OC 11 0 R /Marker (img-marker)",
                "Z",
            ),
        ),
        (
            7,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 300 300] \
                     /Resources << {fonts} /Properties << /Hid 11 0 R >> >>"
                ),
                "BT /F1 12 Tf 10 200 Td /OC /Hid BDC (FormSecret) Tj EMC (FormShown) Tj ET",
            ),
        ),
        (
            8,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 300 300] \
                     /Resources << {fonts} >> /Marker (form-marker)"
                ),
                "BT /F1 12 Tf 10 20 Td (DeepSecret) Tj ET",
            ),
        ),
        (10, "<< /Type /OCG /Name (Shown layer) >>".into()),
        (11, "<< /Type /OCG /Name (Hidden layer name) >>".into()),
        (
            20,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /OC 11 0 R /NM (link-secret) >>".into(),
        ),
        (
            21,
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (field) /Rect [0 0 1 1] /OC 11 0 R >>"
                .into(),
        ),
        (
            22,
            "<< /Type /Annot /Subtype /Link /Rect [0 150 300 170] /AP << /N 23 0 R >> >>".into(),
        ),
        (
            23,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 300 20] \
                     /Resources << {fonts} /Properties << /Hid 11 0 R >> >>"
                ),
                "/OC /Hid BDC BT /F1 12 Tf 2 5 Td (ApSecret) Tj ET EMC",
            ),
        ),
    ])
}

fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes, OpenOptions::default()).unwrap()
}

/// Where the first character of `word` starts on page 1.
fn position(doc: &Document, word: &str) -> (f64, f64) {
    let page = TextExtractor::new(doc).page(0).unwrap();
    for line in &page.lines {
        if let Some(at) = line.text.find(word) {
            let index = line.text[..at].chars().count();
            return line.chars[index].origin;
        }
    }
    panic!("{word:?} not on the page: {:?}", page.text());
}

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
}

#[test]
fn privacy_cuts_hidden_layers_and_keeps_ocr_text() {
    let original = open(layered());
    let after = position(&original, "After");
    let visible = position(&original, "Visible");

    let mut doc = open(layered());
    let report = otf_pdf::sanitize(&mut doc, &SanitizeOptions::PRIVACY).unwrap();
    // Sections: page text, page Do, form, appearance; the hidden image's
    // Do; the hidden link; and the layer list.
    assert_eq!(report.hidden_layers, 7, "{report:?}");
    assert_eq!(report.invisible_text, 0);

    let saved = doc.save(&SaveOptions::default()).unwrap();
    let bytes = String::from_utf8_lossy(&saved).into_owned();
    let clean = open(saved);
    // Content streams are compressed in the output, so check the text.
    let text = TextExtractor::new(&clean).text().unwrap();
    for gone in ["Secret", "FormSecret", "DeepSecret", "ApSecret"] {
        assert!(!text.contains(gone), "{gone} in {text:?}");
    }
    for kept in ["Shown", "After", "FormShown", "OCR words", "Visible"] {
        assert!(text.contains(kept), "{kept} missing from {text:?}");
    }
    // Objects only hidden content used are gone from the file.
    for gone in [
        "img-marker",
        "form-marker",
        "Hidden layer name",
        "link-secret",
        "OCProperties",
    ] {
        assert!(!bytes.contains(gone), "{gone} still in the file");
    }
    // Visible text after cut text has not moved.
    assert!(close(position(&clean, "After"), after));
    assert!(close(position(&clean, "Visible"), visible));
    // The form widget stays, in the page and in the form.
    let annots = clean.pages().unwrap()[0]
        .dict
        .get_array(b"Annots")
        .unwrap()
        .to_vec();
    assert_eq!(annots.len(), 2);
    let fields = clean
        .catalog()
        .get_dict(b"AcroForm")
        .unwrap()
        .get_array(b"Fields")
        .unwrap()
        .to_vec();
    assert_eq!(fields.len(), 1);
}

#[test]
fn all_also_removes_invisible_text() {
    let original = open(layered());
    let visible = position(&original, "Visible");
    let mut doc = open(layered());
    let report = otf_pdf::sanitize(&mut doc, &SanitizeOptions::ALL).unwrap();
    assert_eq!(report.invisible_text, 1, "{report:?}");
    let clean = open(doc.save(&SaveOptions::default()).unwrap());
    let text = TextExtractor::new(&clean).text().unwrap();
    assert!(!text.contains("OCR words"), "{text:?}");
    assert!(close(position(&clean, "Visible"), visible));
}

#[test]
fn invisible_text_alone_leaves_layers() {
    let mut doc = open(layered());
    let report = otf_pdf::sanitize(
        &mut doc,
        &SanitizeOptions {
            invisible_text: true,
            ..SanitizeOptions::NONE
        },
    )
    .unwrap();
    assert_eq!(report.invisible_text, 1);
    assert_eq!(report.hidden_layers, 0);
    assert!(doc.catalog().get(b"OCProperties").is_some());
}

#[test]
fn documents_without_layers_are_unchanged() {
    let bytes = build(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R \
             /Resources << /Properties << /X 9 0 R >> >> >>"
                .into(),
        ),
        // An /OC section with no /OCProperties means nothing: keep it.
        (4, stream("", "/OC /X BDC 0 0 1 1 re f EMC")),
        (9, "<< /Type /OCG >>".into()),
    ]);
    let mut doc = open(bytes);
    let report = otf_pdf::sanitize(&mut doc, &SanitizeOptions::PRIVACY).unwrap();
    assert!(report.is_empty(), "{report:?}");
    assert!(!doc.store().is_modified());
}

#[test]
fn document_sanitize_refuses_what_it_cannot_do() {
    let mut doc = open(layered());
    assert!(doc.sanitize(&SanitizeOptions::PRIVACY).is_err());
    assert!(
        doc.sanitize(&SanitizeOptions::PRIVACY.without_content())
            .is_ok()
    );
}

#[path = "../../otf-pdf-syntax/tests/support/mod.rs"]
mod support;

/// Every external file that opens: sanitizing with every category, a full
/// save and a reopen all work, and the result still extracts on every page
/// the original did. Failures are baselined.
#[test]
fn external_corpus_sanitizes() {
    let passwords = support::passwords();
    support::against_baseline("sanitize", |key, bytes| {
        let options = || OpenOptions {
            password: passwords
                .get(key)
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..OpenOptions::default()
        };
        let Ok(mut doc) = Document::open(bytes.to_vec(), options()) else {
            return true;
        };
        let Ok(count) = doc.page_count() else {
            return true;
        };
        let readable: Vec<bool> = {
            let ex = TextExtractor::new(&doc);
            (0..count.min(20)).map(|i| ex.page(i).is_ok()).collect()
        };
        if otf_pdf::sanitize(&mut doc, &SanitizeOptions::ALL).is_err() {
            return false;
        }
        let Ok(saved) = doc.save(&SaveOptions::default()) else {
            return false;
        };
        let Ok(clean) = Document::open(saved, OpenOptions::default()) else {
            return false;
        };
        let ex = TextExtractor::new(&clean);
        clean.page_count().is_ok_and(|n| n == count)
            && readable
                .iter()
                .enumerate()
                .all(|(i, ok)| !ok || ex.page(i).is_ok())
    });
}
