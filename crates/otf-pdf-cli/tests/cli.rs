//! End-to-end tests: run the built `otf-pdf` binary against the hand-made
//! files in `corpus/malformed`, which are committed and always present.

// Helpers here are test code too; clippy only exempts `#[test]` bodies.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_otf-pdf"))
}

fn malformed(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/malformed")
        .join(name)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_flag() {
    let output = bin().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        format!("otf-pdf {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_arguments_exits_2() {
    let output = bin().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).starts_with("Usage:"));
}

#[test]
fn header_of_a_minimal_file() {
    let file = malformed("minimal-valid.pdf");
    let output = bin().arg("header").arg(&file).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("{}: PDF 1.7\n", file.display()));
}

#[test]
fn header_reports_leading_junk() {
    let file = malformed("junk-before-header.pdf");
    let output = bin().arg("header").arg(&file).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("{}: PDF 1.4 at byte 39\n", file.display())
    );
}

#[test]
fn header_reports_unknown_version() {
    let file = malformed("unknown-version.pdf");
    let output = bin().arg("header").arg(&file).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("{}: PDF 1.9 (unknown version)\n", file.display())
    );
}

#[test]
fn one_bad_file_fails_but_the_rest_are_reported() {
    let good = malformed("minimal-valid.pdf");
    let bad = malformed("not-a-pdf.pdf");
    let empty = malformed("empty.pdf");
    let output = bin()
        .arg("header")
        .args([&good, &bad, &empty])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), format!("{}: PDF 1.7\n", good.display()));
    let err = stderr(&output);
    assert!(err.contains(&format!(
        "{}: no %PDF- header in the first 1024 bytes",
        bad.display()
    )));
    assert!(err.contains(&format!(
        "{}: no %PDF- header in the first 1024 bytes",
        empty.display()
    )));
}

#[test]
fn header_with_malformed_version() {
    let file = malformed("malformed-version.pdf");
    let output = bin().arg("header").arg(&file).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "{}: malformed version after %PDF- at byte 0\n",
            file.display()
        )
    );
}

fn encrypted(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/encrypted")
        .join(name)
}

#[test]
fn info_of_a_minimal_file() {
    let output = bin()
        .arg("info")
        .arg(malformed("minimal-valid.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Version:      1.7\n"), "{out}");
    assert!(out.contains("Pages:        1\n"), "{out}");
    assert!(out.contains("Encrypted:    no\n"), "{out}");
    assert!(out.contains("Repairs:      0\n"), "{out}");
}

#[test]
fn info_reports_repairs() {
    let output = bin()
        .arg("info")
        .arg(malformed("missing-xref.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("Pages:        1\n"), "{out}");
    assert!(out.contains("  - xref rebuilt by scanning\n"), "{out}");
}

#[test]
fn strict_mode_refuses_repairs() {
    let output = bin()
        .args(["info", "--strict"])
        .arg(malformed("missing-xref.pdf"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("otf-pdf info: "));
}

#[test]
fn info_of_an_encrypted_file() {
    let file = encrypted("r6-aes-256.pdf");
    let output = bin().arg("info").arg(&file).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("incorrect password"));

    let output = bin()
        .args(["info", "--password", "owner"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("Encrypted:    yes (R6, owner password"),
        "{out}"
    );
    assert!(out.contains("Title:        Title r6-aes-256\n"), "{out}");
    assert!(out.contains("XMP:"), "{out}");
}

#[test]
fn pages_listing() {
    let output = bin()
        .arg("pages")
        .arg(malformed("minimal-valid.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "1  3 0 R  MediaBox [0 0 612 792]\n");
}

#[test]
fn objects_listing() {
    let output = bin()
        .arg("objects")
        .arg(malformed("minimal-valid.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "0 free");
    assert!(
        lines[1].starts_with("1 0 at ") && lines[1].ends_with("dictionary /Catalog"),
        "{out}"
    );
    assert!(lines[4].ends_with("stream"), "{out}");
    assert_eq!(lines.len(), 6);
}

#[test]
fn dump_objects_trailer_and_data() {
    let file = malformed("minimal-valid.pdf");
    let output = bin().arg("dump").arg(&file).arg("2").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "<</Count 1 /Kids [3 0 R] /Type /Pages >>\n"
    );

    let output = bin()
        .arg("dump")
        .arg(&file)
        .arg("trailer")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "<</Root 1 0 R /Size 6 >>\n");

    let output = bin().arg("dump").arg(&file).arg("4").output().unwrap();
    assert!(stdout(&output).ends_with("stream (36 bytes; --data to decode)\n"));

    let output = bin()
        .args(["dump", "--data"])
        .arg(&file)
        .arg("4")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "BT /F1 24 Tf 72 720 Td (Hello) Tj ET");

    let output = bin()
        .args(["dump", "--data"])
        .arg(&file)
        .arg("2")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("not a stream"));

    let output = bin().arg("dump").arg(&file).arg("99").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("object 99 0 not found"));

    let output = bin().arg("dump").arg(&file).arg("x").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn dump_decrypts_streams() {
    // Object 6 is page 1's content stream in the qpdf-written file.
    let output = bin()
        .args(["dump", "--data", "--password", "user"])
        .arg(encrypted("r4-aes-128.pdf"))
        .arg("6")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "BT /F1 24 Tf 72 720 Td (Secret r4-aes-128) Tj ET"
    );
}

fn tmp(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn page_count(path: &Path, password: &str) -> String {
    let output = bin()
        .args(["info", "--password", password])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    stdout(&output)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:").map(|v| v.trim().to_owned()))
        .unwrap()
}

#[test]
fn save_repairs_a_damaged_file() {
    let out = tmp("repaired.pdf");
    let output = bin()
        .arg("save")
        .arg(malformed("missing-xref.pdf"))
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let info = bin().arg("info").arg(&out).output().unwrap();
    assert!(
        stdout(&info).contains("Repairs:      0\n"),
        "{}",
        stdout(&info)
    );
}

#[test]
fn save_encrypts_and_incremental_appends() {
    let encrypted = tmp("encrypted.pdf");
    let output = bin()
        .args(["save", "--user-password", "u", "--owner-password", "o"])
        .arg(malformed("minimal-valid.pdf"))
        .arg(&encrypted)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(page_count(&encrypted, "u"), "1");
    let info = bin().arg("info").arg(&encrypted).output().unwrap();
    assert!(stderr(&info).contains("incorrect password"));

    let appended = tmp("appended.pdf");
    let output = bin()
        .args(["rotate", "--incremental", "--password", "u"])
        .arg(&encrypted)
        .arg(&appended)
        .args(["1", "90"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let before = std::fs::read(&encrypted).unwrap();
    let after = std::fs::read(&appended).unwrap();
    assert!(after.starts_with(&before));
    let pages = bin()
        .args(["pages", "--password", "u"])
        .arg(&appended)
        .output()
        .unwrap();
    assert!(stdout(&pages).contains("Rotate 90"), "{}", stdout(&pages));
}

#[test]
fn merge_extract_delete() {
    let merged = tmp("merged.pdf");
    let output = bin()
        .arg("merge")
        .arg(&merged)
        .arg(malformed("minimal-valid.pdf"))
        .arg(malformed("junk-before-header.pdf"))
        .arg(malformed("minimal-valid.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(page_count(&merged, ""), "3");

    let extracted = tmp("extracted.pdf");
    let output = bin()
        .arg("extract")
        .arg(&merged)
        .arg(&extracted)
        .arg("2-")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(page_count(&extracted, ""), "2");

    let deleted = tmp("deleted.pdf");
    let output = bin()
        .args(["delete", "--pdf-version", "2.0", "--classic-xref"])
        .arg(&merged)
        .arg(&deleted)
        .arg("1,3")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(page_count(&deleted, ""), "1");
    assert!(std::fs::read(&deleted).unwrap().starts_with(b"%PDF-2.0"));

    let output = bin()
        .arg("delete")
        .arg(&merged)
        .arg(&deleted)
        .arg("9")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("out of range"));
    let output = bin().arg("merge").arg(&merged).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let output = bin().arg("save").arg(&merged).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
