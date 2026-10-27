//! Known-plaintext decryption (docs/PLAN.md Phase 1 acceptance): the files
//! in `corpus/encrypted` were encrypted by qpdf, and `expected.tsv` holds
//! their passwords, page text and title.
// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use otf_pdf_syntax::crypt::Authentication;
use otf_pdf_syntax::{Error, File, Object, OpenOptions};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/encrypted")
}

struct Case {
    file: String,
    user: String,
    owner: String,
    text: String,
    title: String,
}

fn cases() -> Vec<Case> {
    let tsv = std::fs::read_to_string(dir().join("expected.tsv")).unwrap();
    tsv.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Case {
                file: f[0].into(),
                user: f[1].into(),
                owner: f[2].into(),
                text: f[3].into(),
                title: f[4].into(),
            }
        })
        .collect()
}

fn open(case: &Case, password: &str) -> Result<File, Error> {
    let bytes = std::fs::read(dir().join(&case.file)).unwrap();
    File::open(
        Arc::new(bytes),
        OpenOptions {
            password: password.as_bytes().to_vec(),
            ..OpenOptions::default()
        },
    )
}

/// The decoded content stream of page 1 and the Info title.
fn page_text_and_title(f: &File) -> (Vec<u8>, Vec<u8>) {
    let root = f.resolve_dict(f.trailer().get(b"Root").unwrap()).unwrap();
    let pages = f.resolve_dict(root.get(b"Pages").unwrap()).unwrap();
    let page = f
        .resolve_dict(&pages.get_array(b"Kids").unwrap()[0])
        .unwrap();
    let Object::Stream(contents) = f.resolve(page.get(b"Contents").unwrap()) else {
        panic!("contents is not a stream");
    };
    let decoded = f.decode_stream(&contents).unwrap();
    let info = f.resolve_dict(f.trailer().get(b"Info").unwrap()).unwrap();
    let title = f.resolve(info.get(b"Title").unwrap());
    (decoded.data, title.as_string().unwrap().to_vec())
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn every_variant_decrypts_with_user_and_owner_passwords() {
    let cases = cases();
    assert_eq!(cases.len(), 11);
    for case in &cases {
        for (password, auth) in [
            (&case.user, Authentication::User),
            (&case.owner, Authentication::Owner),
        ] {
            let f = open(case, password).unwrap_or_else(|e| panic!("{}: {e}", case.file));
            assert_eq!(
                f.security().unwrap().authentication(),
                auth,
                "{}",
                case.file
            );
            let (content, title) = page_text_and_title(&f);
            assert!(
                contains(&content, &format!("({})", case.text)),
                "{}: {}",
                case.file,
                String::from_utf8_lossy(&content)
            );
            assert_eq!(title, case.title.as_bytes(), "{}", case.file);
        }
    }
}

#[test]
fn wrong_password_is_rejected() {
    for case in cases().iter().filter(|c| !c.user.is_empty()) {
        assert_eq!(
            open(case, "wrong").err(),
            Some(Error::InvalidPassword),
            "{}",
            case.file
        );
        assert_eq!(
            open(case, "").err(),
            Some(Error::InvalidPassword),
            "{}",
            case.file
        );
    }
}

#[test]
fn empty_user_password_opens_without_one() {
    let empty: Vec<_> = cases().into_iter().filter(|c| c.user.is_empty()).collect();
    assert_eq!(empty.len(), 2);
    for case in &empty {
        let f = open(case, "").unwrap();
        assert_eq!(f.security().unwrap().authentication(), Authentication::User);
    }
}

#[test]
fn clear_metadata_is_not_decrypted() {
    let case = cases()
        .into_iter()
        .find(|c| c.file == "r4-aes-128-clear-metadata.pdf")
        .unwrap();
    let f = open(&case, &case.user).unwrap();
    assert!(!f.security().unwrap().encrypt_metadata());
    let root = f.resolve_dict(f.trailer().get(b"Root").unwrap()).unwrap();
    let Object::Stream(meta) = f.resolve(root.get(b"Metadata").unwrap()) else {
        panic!("no metadata stream");
    };
    let xmp = f.decode_stream(&meta).unwrap().data;
    assert!(
        contains(&xmp, &case.title),
        "{}",
        String::from_utf8_lossy(&xmp)
    );
}
