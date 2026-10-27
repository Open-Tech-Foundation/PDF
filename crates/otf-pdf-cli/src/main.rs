//! `otf-pdf`: the development and test CLI for the otf-pdf toolkit.
//!
//! Commands are added phase by phase (docs/PLAN.md §5). Arguments are parsed
//! by hand: an argument parser crate is not on the dependency allowlist.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use otf_pdf::{
    Authentication, Document, ObjId, Object, OpenOptions, Rect, Strictness, XrefEntry, to_bytes,
};

/// Every input was processed.
const EXIT_OK: u8 = 0;
/// At least one input failed; the others were still processed.
const EXIT_FAILURE: u8 = 1;
/// The command line itself was wrong.
const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
Usage: otf-pdf <command> [options] [args]

Commands:
  header <file>...            Print the PDF version and header offset of each file
  info <file>                 Print version, page count, encryption, metadata
                              and repairs made while reading
  pages <file>                List pages with their boxes and rotation
  objects <file>              List the cross-reference entries
  dump <file> <num> [gen]     Print an object in PDF syntax
  dump <file> trailer         Print the trailer dictionary
  help                        Print this help

Options:
  --password <pw>             Password for an encrypted file
  --strict                    Fail instead of repairing malformed input
  --data                      dump: write a stream's decoded data, raw
  -h, --help                  Print this help
  -V, --version               Print the version
";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    ExitCode::from(code)
}

/// Parsed options common to the commands.
#[derive(Debug, Default)]
struct Options {
    password: Vec<u8>,
    strict: bool,
    data: bool,
    positional: Vec<OsString>,
}

fn parse_options(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--password") => {
                let pw = iter.next().ok_or("--password needs a value")?;
                options.password = os_bytes(pw);
            }
            Some(s) if s.starts_with("--password=") => {
                options.password = s.as_bytes().get(11..).unwrap_or_default().to_vec();
            }
            Some("--strict") => options.strict = true,
            Some("--data") => options.data = true,
            Some(s) if s.starts_with("--") => return Err(format!("unknown option '{s}'")),
            _ => options.positional.push(arg.clone()),
        }
    }
    Ok(options)
}

fn os_bytes(s: &OsString) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

/// Runs one invocation and returns its exit code. Output is best-effort: a
/// closed stdout (e.g. piped into `head`) must not turn into a panic.
fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        let _ = write!(err, "{USAGE}");
        return EXIT_USAGE;
    };
    let command = command.to_string_lossy();
    match &*command {
        "-h" | "--help" | "help" => {
            let _ = write!(out, "{USAGE}");
            return EXIT_OK;
        }
        "-V" | "--version" => {
            let _ = writeln!(out, "otf-pdf {}", env!("CARGO_PKG_VERSION"));
            return EXIT_OK;
        }
        _ => {}
    }
    let options = match parse_options(rest) {
        Ok(o) => o,
        Err(e) => return usage_error(err, &command, &e),
    };
    let result = match &*command {
        "header" => return header(&options.positional, out, err),
        "info" => with_document(&options, err, &command, |doc| info(doc, out)),
        "pages" => with_document(&options, err, &command, |doc| pages(doc, out)),
        "objects" => with_document(&options, err, &command, |doc| objects(doc, out)),
        "dump" => {
            let target = options.positional.get(1..).unwrap_or_default().to_vec();
            if target.is_empty() || target.len() > 2 {
                return usage_error(
                    err,
                    &command,
                    "expected <file> <num> [gen] or <file> trailer",
                );
            }
            with_document(&options, err, &command, |doc| {
                dump(doc, &target, options.data, out)
            })
        }
        _ => {
            let _ = writeln!(err, "otf-pdf: unknown command '{command}'\n");
            let _ = write!(err, "{USAGE}");
            return EXIT_USAGE;
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "otf-pdf {command}: {e}");
            EXIT_FAILURE
        }
    }
}

fn usage_error(err: &mut dyn Write, command: &str, message: &str) -> u8 {
    let _ = writeln!(err, "otf-pdf {command}: {message}\n");
    let _ = write!(err, "{USAGE}");
    EXIT_USAGE
}

/// Opens the file named by the first positional argument and runs `f`.
fn with_document(
    options: &Options,
    err: &mut dyn Write,
    command: &str,
    f: impl FnOnce(&Document) -> Result<u8, String>,
) -> Result<u8, String> {
    let Some(file) = options.positional.first() else {
        return Ok(usage_error(err, command, "expected a file"));
    };
    let path = Path::new(file);
    // The CLI is the I/O boundary: it reads the file and hands the core
    // crates bytes, as es-runtime will (docs/PLAN.md §2).
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let open = OpenOptions {
        password: options.password.clone(),
        strictness: if options.strict {
            Strictness::Strict
        } else {
            Strictness::Lenient
        },
        ..OpenOptions::default()
    };
    let doc = Document::open(bytes, open).map_err(|e| format!("{}: {e}", path.display()))?;
    f(&doc)
}

fn header(files: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    if files.is_empty() {
        return usage_error(err, "header", "expected at least one file");
    }
    let mut code = EXIT_OK;
    for file in files {
        let path = Path::new(file);
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

fn info(doc: &Document, out: &mut dyn Write) -> Result<u8, String> {
    let mut lines = Vec::new();
    let version = doc
        .version()
        .map_or("unknown".to_owned(), |v| v.to_string());
    lines.push(("Version", version));
    let pages = doc.page_count().map_err(|e| e.to_string())?;
    lines.push(("Pages", pages.to_string()));
    let encrypted = match doc.file().security() {
        None => "no".to_owned(),
        Some(s) => {
            let auth = match s.authentication() {
                Authentication::User => "user password",
                Authentication::Owner => "owner password",
                Authentication::None => "not authenticated; attachments only",
            };
            format!(
                "yes (R{}, {auth}, permissions {:#010x})",
                s.revision(),
                s.permissions()
            )
        }
    };
    lines.push(("Encrypted", encrypted));
    let info = doc.info();
    for (label, value) in [
        ("Title", info.title),
        ("Author", info.author),
        ("Subject", info.subject),
        ("Keywords", info.keywords),
        ("Creator", info.creator),
        ("Producer", info.producer),
        ("CreationDate", info.creation_date),
        ("ModDate", info.mod_date),
    ] {
        if let Some(v) = value {
            lines.push((label, v));
        }
    }
    if let Some(xmp) = doc.xmp_metadata().map_err(|e| e.to_string())? {
        lines.push(("XMP", format!("{} bytes", xmp.len())));
    }
    let outlines = doc.outlines().map_err(|e| e.to_string())?;
    if !outlines.is_empty() {
        lines.push(("Outlines", format!("{} top-level items", outlines.len())));
    }
    let attachments = doc.attachments().map_err(|e| e.to_string())?;
    if !attachments.is_empty() {
        let names: Vec<_> = attachments.iter().map(|a| a.name.clone()).collect();
        lines.push(("Attachments", names.join(", ")));
    }
    let diagnostics = doc.diagnostics();
    lines.push(("Repairs", diagnostics.len().to_string()));
    for (label, value) in lines {
        let _ = writeln!(out, "{:<14}{value}", format!("{label}:"));
    }
    for d in diagnostics {
        let _ = writeln!(out, "  - {d}");
    }
    Ok(EXIT_OK)
}

fn rect(r: &Rect) -> String {
    let n = |v: f64| String::from_utf8_lossy(&to_bytes(&Object::Real(v))).into_owned();
    format!("[{} {} {} {}]", n(r.llx), n(r.lly), n(r.urx), n(r.ury))
}

fn pages(doc: &Document, out: &mut dyn Write) -> Result<u8, String> {
    let pages = doc.pages().map_err(|e| e.to_string())?;
    for (i, page) in pages.iter().enumerate() {
        let id = page.id.map_or("direct".to_owned(), |id| {
            format!("{} {} R", id.num, id.generation)
        });
        let mut line = format!("{}  {id}  MediaBox {}", i + 1, rect(&page.media_box));
        if page.crop_box != page.media_box {
            line.push_str(&format!("  CropBox {}", rect(&page.crop_box)));
        }
        if page.rotate != 0 {
            line.push_str(&format!("  Rotate {}", page.rotate));
        }
        let _ = writeln!(out, "{line}");
    }
    Ok(EXIT_OK)
}

fn objects(doc: &Document, out: &mut dyn Write) -> Result<u8, String> {
    let file = doc.file();
    for (&num, entry) in file.entries() {
        let line = match *entry {
            XrefEntry::Free => format!("{num} free"),
            XrefEntry::InUse { offset, generation } => {
                let kind = describe(doc, ObjId::new(num, generation));
                format!("{num} {generation} at {offset}  {kind}")
            }
            XrefEntry::Compressed { stream, index } => {
                let kind = describe(doc, ObjId::new(num, 0));
                format!("{num} 0 in {stream} [{index}]  {kind}")
            }
        };
        let _ = writeln!(out, "{line}");
    }
    Ok(EXIT_OK)
}

/// An object's kind and `/Type`, or why it could not be read.
fn describe(doc: &Document, id: ObjId) -> String {
    match doc.file().get(id) {
        Ok(obj) => {
            let ty = obj
                .as_dict()
                .and_then(|d| d.get_name(b"Type"))
                .map(|t| format!(" /{}", String::from_utf8_lossy(t)))
                .unwrap_or_default();
            format!("{}{ty}", obj.type_name())
        }
        Err(e) => format!("unreadable: {e}"),
    }
}

fn dump(
    doc: &Document,
    target: &[OsString],
    data: bool,
    out: &mut dyn Write,
) -> Result<u8, String> {
    let file = doc.file();
    let first = target
        .first()
        .map(|t| t.to_string_lossy().into_owned())
        .unwrap_or_default();
    let object = if first == "trailer" {
        Object::Dict(file.trailer().clone())
    } else {
        let num: u32 = first
            .parse()
            .map_err(|_| format!("'{first}' is not an object number"))?;
        let generation: u16 = match target.get(1) {
            Some(g) => g
                .to_string_lossy()
                .parse()
                .map_err(|_| "generation must be a number".to_owned())?,
            None => 0,
        };
        (*file
            .get(ObjId::new(num, generation))
            .map_err(|e| e.to_string())?)
        .clone()
    };
    if data {
        let Object::Stream(stream) = &object else {
            return Err(format!("object is a {}, not a stream", object.type_name()));
        };
        let decoded = file.decode_stream(stream).map_err(|e| e.to_string())?;
        let _ = out.write_all(&decoded.data);
        return Ok(EXIT_OK);
    }
    let mut bytes = match &object {
        // A stream's data can be binary and huge: show its dictionary and size.
        Object::Stream(s) => {
            let mut b = to_bytes(&Object::Dict(s.dict.clone()));
            b.extend_from_slice(
                format!("\nstream ({} bytes; --data to decode)", s.raw.len()).as_bytes(),
            );
            b
        }
        other => to_bytes(other),
    };
    bytes.push(b'\n');
    let _ = out.write_all(&bytes);
    Ok(EXIT_OK)
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

    #[test]
    fn option_parsing() {
        let args: Vec<OsString> = ["f.pdf", "--password", "pw", "--strict", "--data", "7"]
            .iter()
            .map(OsString::from)
            .collect();
        let o = parse_options(&args).unwrap();
        assert_eq!(o.password, b"pw");
        assert!(o.strict && o.data);
        assert_eq!(o.positional, [OsString::from("f.pdf"), OsString::from("7")]);
        let o = parse_options(&[OsString::from("--password=x y")]).unwrap();
        assert_eq!(o.password, b"x y");
        assert!(parse_options(&[OsString::from("--password")]).is_err());
        assert!(parse_options(&[OsString::from("--bogus")]).is_err());
    }

    #[test]
    fn commands_need_a_file() {
        for command in ["info", "pages", "objects"] {
            let (code, _, err) = invoke(&[command]);
            assert_eq!(code, EXIT_USAGE, "{command}");
            assert!(err.contains("expected a file"), "{command}");
        }
        assert_eq!(invoke(&["dump", "f.pdf"]).0, EXIT_USAGE);
        assert_eq!(invoke(&["info", "--bogus"]).0, EXIT_USAGE);
    }

    #[test]
    fn unreadable_file_fails() {
        let (code, _, err) = invoke(&["info", "/nonexistent/otf-pdf/x.pdf"]);
        assert_eq!(code, EXIT_FAILURE);
        assert!(err.starts_with("otf-pdf info: /nonexistent/otf-pdf/x.pdf: "));
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
