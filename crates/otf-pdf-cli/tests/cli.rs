//! End-to-end tests: run the built `otf-pdf` binary against the hand-made
//! files in `corpus/malformed`, which are committed and always present.

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
