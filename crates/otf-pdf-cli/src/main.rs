//! `otf-pdf`: the development and test CLI for the otf-pdf toolkit.
//!
//! Commands are added phase by phase (docs/PLAN.md §5); Phase 1 brings `dump`,
//! `info`, `pages` and `objects`. Arguments are parsed by hand: an argument
//! parser crate is not on the dependency allowlist.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// Every input was processed.
const EXIT_OK: u8 = 0;
/// At least one input failed; the others were still processed.
const EXIT_FAILURE: u8 = 1;
/// The command line itself was wrong.
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
Usage: otf-pdf <command> [args]

Commands:
  header <file>...   Print the PDF version and header offset of each file
  help               Print this help

Options:
  -h, --help         Print this help
  -V, --version      Print the version
";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    ExitCode::from(code)
}

/// Runs one invocation and returns its exit code. Output is best-effort: a
/// closed stdout (e.g. piped into `head`) must not turn into a panic.
fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        let _ = write!(err, "{USAGE}");
        return EXIT_USAGE;
    };
    match command.to_str() {
        Some("-h" | "--help" | "help") => {
            let _ = write!(out, "{USAGE}");
            EXIT_OK
        }
        Some("-V" | "--version") => {
            let _ = writeln!(out, "otf-pdf {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        Some("header") => header(rest, out, err),
        _ => {
            let _ = writeln!(
                err,
                "otf-pdf: unknown command '{}'\n",
                command.to_string_lossy()
            );
            let _ = write!(err, "{USAGE}");
            EXIT_USAGE
        }
    }
}

fn header(files: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    if files.is_empty() {
        let _ = writeln!(err, "otf-pdf header: expected at least one file\n");
        let _ = write!(err, "{USAGE}");
        return EXIT_USAGE;
    }
    let mut code = EXIT_OK;
    for file in files {
        let path = Path::new(file);
        // The CLI is the I/O boundary: it reads the file and hands the core
        // crates bytes, as es-runtime will (docs/PLAN.md §2).
        let result = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| otf_pdf::find_header(&bytes).map_err(|e| e.to_string()));
        match result {
            Ok(h) => {
                let mut line = format!("{}: PDF {}", path.display(), h.version);
                if !h.version.is_known() {
                    line.push_str(" (unknown version)");
                }
                if h.has_leading_junk() {
                    line.push_str(&format!(" at byte {}", h.offset));
                }
                let _ = writeln!(out, "{line}");
            }
            Err(e) => {
                let _ = writeln!(err, "{}: {e}", path.display());
                code = EXIT_FAILURE;
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(args: &[&str]) -> (u8, String, String) {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        let (code, out, err) = invoke(&[]);
        assert_eq!(code, EXIT_USAGE);
        assert!(out.is_empty());
        assert!(err.starts_with("Usage:"));
    }

    #[test]
    fn help_goes_to_stdout() {
        for flag in ["-h", "--help", "help"] {
            let (code, out, err) = invoke(&[flag]);
            assert_eq!(code, EXIT_OK);
            assert_eq!(out, USAGE);
            assert!(err.is_empty());
        }
    }

    #[test]
    fn version_flag() {
        let (code, out, _) = invoke(&["--version"]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(out, format!("otf-pdf {}\n", env!("CARGO_PKG_VERSION")));
        assert_eq!(invoke(&["-V"]).1, out);
    }

    #[test]
    fn unknown_command() {
        let (code, _, err) = invoke(&["frobnicate"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.starts_with("otf-pdf: unknown command 'frobnicate'"));
    }

    #[test]
    fn header_without_files() {
        let (code, _, err) = invoke(&["header"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("expected at least one file"));
    }

    #[test]
    fn header_of_missing_file() {
        let (code, out, err) = invoke(&["header", "/nonexistent/otf-pdf/x.pdf"]);
        assert_eq!(code, EXIT_FAILURE);
        assert!(out.is_empty());
        assert!(err.starts_with("/nonexistent/otf-pdf/x.pdf: "));
    }

    /// A writer that always fails, standing in for a closed pipe.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn closed_output_does_not_panic() {
        let args = [OsString::from("--help")];
        assert_eq!(run(&args, &mut Closed, &mut Closed), EXIT_OK);
    }
}
