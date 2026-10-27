//! Full and incremental save.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use otf_pdf_doc::{Document, Encryption, SaveMode, SaveOptions};
use otf_pdf_syntax::crypt::Authentication;
use otf_pdf_syntax::{
    Entropy, Error, Name, ObjId, Object, OpenOptions, PdfString, Permissions, Version,
};

/// Deterministic entropy for tests: a counter.
struct Counter(AtomicU8);

impl Entropy for Counter {
    fn fill(&self, buf: &mut [u8]) {
        for b in buf {
            *b = self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn entropy() -> Arc<dyn Entropy> {
    Arc::new(Counter(AtomicU8::new(0)))
}

fn corpus(dir: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus")
        .join(dir)
        .join(name)
}

fn open_with(bytes: Vec<u8>, password: &str) -> Result<Document, Error> {
    Document::open(
        bytes,
        OpenOptions {
            password: password.as_bytes().to_vec(),
            ..OpenOptions::default()
        },
    )
}

fn open_file(dir: &str, name: &str, password: &str) -> Document {
    open_with(std::fs::read(corpus(dir, name)).unwrap(), password).unwrap()
}

/// Every page's decoded content, concatenated.
fn content(doc: &Document) -> Vec<Vec<u8>> {
    doc.pages()
        .unwrap()
        .iter()
        .map(|p| {
            let contents = doc
                .store()
                .resolve(p.dict.get(b"Contents").unwrap_or(&Object::Null));
            let streams: Vec<Object> = match contents {
                Object::Array(items) => items.iter().map(|i| doc.store().resolve(i)).collect(),
                other => vec![other],
            };
            streams
                .iter()
                .filter_map(Object::as_stream)
                .flat_map(|s| doc.store().decode_stream(s).unwrap().data)
                .collect()
        })
        .collect()
}

#[test]
fn full_save_round_trips() {
    let doc = open_file("malformed", "minimal-valid.pdf", "");
    let saved = doc.save(&SaveOptions::default()).unwrap();
    assert!(saved.starts_with(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"));
    let again = open_with(saved.clone(), "").unwrap();
    assert!(again.diagnostics().is_empty(), "{:?}", again.diagnostics());
    assert_eq!(content(&again), content(&doc));
    // Object streams and an xref stream.
    assert_eq!(again.file().newest_section_is_stream().unwrap(), Some(true));
    // Deterministic.
    assert_eq!(doc.save(&SaveOptions::default()).unwrap(), saved);
}

#[test]
fn full_save_repairs_and_compacts() {
    // A file needing repair saves into one that does not.
    let doc = open_file("malformed", "wrong-stream-length.pdf", "");
    // The /Length repair happens when the stream is first read.
    let before = content(&doc);
    assert!(!doc.diagnostics().is_empty());
    let saved = doc.save(&SaveOptions::default()).unwrap();
    let again = open_with(saved, "").unwrap();
    assert_eq!(content(&again), before);
    assert!(again.diagnostics().is_empty(), "{:?}", again.diagnostics());
}

#[test]
fn unreachable_objects_are_dropped() {
    let mut doc = open_file("malformed", "minimal-valid.pdf", "");
    let orphan = doc.store_mut().add(Object::Int(42)).unwrap();
    assert_eq!(doc.store().get(orphan).unwrap().as_int(), Some(42));
    let again = open_with(doc.save(&SaveOptions::default()).unwrap(), "").unwrap();
    assert_eq!(
        again.store().object_ids().len(),
        doc.store().object_ids().len() - 1 + 2
    );
}

#[test]
fn classic_xref_and_version_options() {
    let doc = open_file("malformed", "minimal-valid.pdf", "");
    let options = SaveOptions {
        object_streams: false,
        compress: false,
        version: Version::V2_0,
        document_id: Some([7; 16]),
        ..SaveOptions::default()
    };
    let saved = doc.save(&options).unwrap();
    assert!(saved.starts_with(b"%PDF-2.0"));
    let text = String::from_utf8_lossy(&saved);
    assert!(text.contains("\nxref\n0 6\n"), "{text}");
    assert!(text.contains("(Hello) Tj"), "uncompressed content expected");
    let again = open_with(saved, "").unwrap();
    assert_eq!(
        again.file().newest_section_is_stream().unwrap(),
        Some(false)
    );
    assert_eq!(again.version(), Some(Version::V2_0));
    let id = again.store().trailer().get_array(b"ID").unwrap().to_vec();
    assert_eq!(id[0].as_string(), Some(&[7u8; 16][..]));
}

#[test]
fn encrypted_save_round_trips() {
    let doc = open_file("malformed", "minimal-valid.pdf", "");
    let options = SaveOptions {
        encryption: Some(Encryption {
            user_password: b"u".to_vec(),
            owner_password: b"o".to_vec(),
            permissions: Permissions {
                copy: false,
                ..Permissions::ALL
            },
            encrypt_metadata: true,
        }),
        entropy: Some(entropy()),
        ..SaveOptions::default()
    };
    let saved = doc.save(&options).unwrap();
    // Same entropy, same bytes.
    let options_again = SaveOptions {
        entropy: Some(entropy()),
        ..options.clone()
    };
    assert_eq!(doc.save(&options_again).unwrap(), saved);
    // The plaintext is not in the file.
    assert!(!saved.windows(5).any(|w| w == b"Hello"));

    assert_eq!(
        open_with(saved.clone(), "").err(),
        Some(Error::InvalidPassword)
    );
    for (pw, auth) in [("u", Authentication::User), ("o", Authentication::Owner)] {
        let again = open_with(saved.clone(), pw).unwrap();
        let sec = again.file().security().unwrap();
        assert_eq!(sec.authentication(), auth);
        assert_eq!(sec.revision(), 6);
        assert!(!Permissions::from_bits(sec.permissions()).copy);
        assert_eq!(content(&again), content(&doc));
        // 1.7 output declares the AES-256 extension.
        assert!(again.catalog().get_dict(b"Extensions").is_some());
    }

    let no_entropy = SaveOptions {
        entropy: None,
        ..options
    };
    assert!(matches!(
        doc.save(&no_entropy),
        Err(Error::UnsupportedEncryption(_))
    ));
}

#[test]
fn encrypted_input_saves_decrypted_or_reencrypted() {
    for (file, pw) in [
        ("r4-aes-128.pdf", "user"),
        ("r6-aes-256-object-streams.pdf", "owner"),
    ] {
        let doc = open_file("encrypted", file, pw);
        let clear = open_with(doc.save(&SaveOptions::default()).unwrap(), "").unwrap();
        assert!(!clear.is_encrypted());
        assert_eq!(clear.info().title, doc.info().title);
        assert_eq!(content(&clear), content(&doc));
    }
}

#[test]
fn incremental_save_appends() {
    let path = corpus("malformed", "minimal-valid.pdf");
    let original = std::fs::read(&path).unwrap();
    let mut doc = open_with(original.clone(), "").unwrap();
    let mut info = otf_pdf_syntax::Dict::new();
    info.insert("Title", PdfString::literal("Updated"));
    let info_id = doc.store_mut().add(Object::Dict(info)).unwrap();
    doc.store_mut()
        .set_trailer_entry("Info", Object::Ref(info_id));
    doc.store_mut().delete(ObjId::new(5, 0));

    let options = SaveOptions {
        mode: SaveMode::Incremental,
        ..SaveOptions::default()
    };
    let saved = doc.save(&options).unwrap();
    assert!(
        saved.starts_with(&original),
        "original bytes must be a prefix"
    );
    let again = open_with(saved, "").unwrap();
    assert!(again.diagnostics().is_empty(), "{:?}", again.diagnostics());
    assert_eq!(again.file().sections().len(), 2);
    assert_eq!(again.info().title.as_deref(), Some("Updated"));
    assert!(again.store().get(ObjId::new(5, 0)).is_err());
    let id = again.store().trailer().get_array(b"ID").unwrap().to_vec();
    assert_eq!(id.len(), 2);
}

#[test]
fn incremental_save_of_unchanged_document_adds_an_empty_revision() {
    let original = std::fs::read(corpus("malformed", "minimal-valid.pdf")).unwrap();
    let doc = open_with(original.clone(), "").unwrap();
    let saved = doc
        .save(&SaveOptions {
            mode: SaveMode::Incremental,
            ..SaveOptions::default()
        })
        .unwrap();
    assert!(saved.starts_with(&original));
    assert_eq!(open_with(saved, "").unwrap().page_count().unwrap(), 1);
}

#[test]
fn incremental_save_keeps_xref_stream_format_and_encryption() {
    let original = std::fs::read(corpus("encrypted", "r6-aes-256-object-streams.pdf")).unwrap();
    let mut doc = open_with(original.clone(), "user").unwrap();
    let page = doc.pages().unwrap()[0].id.unwrap();
    let mut dict = doc.pages().unwrap()[0].dict.clone();
    dict.insert("Rotate", 90i64);
    dict.insert("Note", PdfString::literal("secret note"));
    doc.store_mut().set(page, Object::Dict(dict));
    let options = SaveOptions {
        mode: SaveMode::Incremental,
        entropy: Some(entropy()),
        ..SaveOptions::default()
    };
    let saved = doc.save(&options).unwrap();
    assert!(saved.starts_with(&original));
    assert!(!saved.windows(11).any(|w| w == b"secret note"));
    let again = open_with(saved, "user").unwrap();
    assert_eq!(again.file().newest_section_is_stream().unwrap(), Some(true));
    let page = &again.pages().unwrap()[0];
    assert_eq!(page.rotate, 90);
    assert_eq!(
        page.dict.get(b"Note").unwrap().as_string(),
        Some(&b"secret note"[..])
    );

    let no_entropy = SaveOptions {
        entropy: None,
        ..options
    };
    assert!(doc.save(&no_entropy).is_err());
}

#[test]
fn incremental_save_refusals() {
    let incremental = SaveOptions {
        mode: SaveMode::Incremental,
        entropy: Some(entropy()),
        ..SaveOptions::default()
    };
    // A file whose xref needed repair has no sound chain to append to.
    for damaged in [
        "missing-xref.pdf",
        "wrong-xref-offsets.pdf",
        "startxref-past-eof.pdf",
    ] {
        let doc = open_file("malformed", damaged, "");
        assert!(doc.save(&incremental).is_err(), "{damaged}");
    }
    // Legacy encryption is never written.
    let mut rc4 = open_file("encrypted", "r4-rc4-128.pdf", "user");
    rc4.store_mut()
        .set(ObjId::new(99, 0), Object::Name(Name::from("X")));
    assert!(matches!(
        rc4.save(&incremental),
        Err(Error::UnsupportedEncryption(_))
    ));
    // Encryption cannot change incrementally.
    let doc = open_file("malformed", "minimal-valid.pdf", "");
    let with_encryption = SaveOptions {
        encryption: Some(Encryption {
            user_password: Vec::new(),
            owner_password: b"o".to_vec(),
            permissions: Permissions::ALL,
            encrypt_metadata: true,
        }),
        ..incremental
    };
    assert!(doc.save(&with_encryption).is_err());
}

#[test]
fn direct_catalog_gets_an_object_number() {
    let bytes = b"%PDF-1.4\n1 0 obj << /Kids [<< /Parent 1 0 R >>] >> endobj\n\
        trailer << /Root << /Pages 1 0 R >> >>\n%%EOF\n"
        .to_vec();
    let doc = open_with(bytes, "").unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
    let again = open_with(doc.save(&SaveOptions::default()).unwrap(), "").unwrap();
    assert!(again.store().trailer().get_ref(b"Root").is_some());
    assert_eq!(again.page_count().unwrap(), 1);
}

#[test]
fn recovered_pages_get_a_fresh_tree() {
    // The catalog's /Pages is not a page tree; the pages are found by scan.
    let bytes = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 9 >> endobj\n\
        2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 10 20] /Rotate 90 >> endobj\n\
        3 0 obj << /Type /Page /Parent 2 0 R >> endobj\n\
        4 0 obj << /Type /Page >> endobj\n"
        .to_vec();
    let doc = open_with(bytes, "").unwrap();
    assert_eq!(doc.page_count().unwrap(), 2);
    let again = open_with(doc.save(&SaveOptions::default()).unwrap(), "").unwrap();
    assert!(again.diagnostics().is_empty(), "{:?}", again.diagnostics());
    let pages = again.pages().unwrap();
    assert_eq!(pages.len(), 2);
    // Inherited attributes were written into the page.
    assert_eq!(pages[0].media_box.ury, 20.0);
    assert_eq!(pages[0].rotate, 90);
}

#[test]
fn incremental_save_keeps_header_relative_offsets() {
    // This file's xref offsets count from its header, 39 bytes in.
    let original = std::fs::read(corpus("malformed", "junk-before-header.pdf")).unwrap();
    let mut doc = open_with(original.clone(), "").unwrap();
    assert_eq!(doc.file().offset_shift().unwrap(), 39);
    let page = doc.pages().unwrap()[0].id.unwrap();
    let mut dict = doc.pages().unwrap()[0].dict.clone();
    dict.insert("Rotate", 90i64);
    doc.store_mut().set(page, Object::Dict(dict));
    let saved = doc
        .save(&SaveOptions {
            mode: SaveMode::Incremental,
            ..SaveOptions::default()
        })
        .unwrap();
    assert!(saved.starts_with(&original));
    let text = String::from_utf8_lossy(&saved);
    let stated: u64 = text[text.rfind("startxref\n").unwrap() + 10..]
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(&saved[stated as usize + 39..][..4], b"xref");
    let again = open_with(saved, "").unwrap();
    assert_eq!(again.pages().unwrap()[0].rotate, 90);
    assert_eq!(again.file().sections().len(), 2);
}
