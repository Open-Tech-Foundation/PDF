//! The built-in encoding of an embedded Type 1 font program (its cleartext
//! `/Encoding`), used when the font dictionary does not name one.

/// Code → glyph name from the program's `/Encoding`, or `None` when it is
/// `StandardEncoding` or absent (the caller then uses the default).
pub fn builtin_encoding(program: &[u8]) -> Option<Vec<Option<String>>> {
    // The encoding is in the cleartext part, before `eexec`.
    let end = program
        .windows(5)
        .position(|w| w == b"eexec")
        .unwrap_or(program.len());
    let text = String::from_utf8_lossy(program.get(..end)?);
    let start = text.find("/Encoding")?;
    let rest = text.get(start + 9..)?;
    if rest.trim_start().starts_with("StandardEncoding") {
        return None;
    }
    let mut names: Vec<Option<String>> = vec![None; 256];
    let mut found = false;
    let tokens: Vec<&str> = rest.split_whitespace().take(256 * 5 + 16).collect();
    for window in tokens.windows(4) {
        if let ["dup", code, name, "put"] = window
            && let (Ok(code), Some(name)) = (code.parse::<usize>(), name.strip_prefix('/'))
            && let Some(slot) = names.get_mut(code)
        {
            *slot = Some(name.to_owned());
            found = true;
        }
        if (window.first() == Some(&"readonly") || window.first() == Some(&"def")) && found {
            break;
        }
    }
    found.then_some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_encoding() {
        let program =
            b"%!PS-AdobeFont-1.0\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n\
            dup 65 /alpha put\ndup 66 /beta put\nreadonly def\ncurrentfile eexec \x00\x01";
        let names = builtin_encoding(program).unwrap();
        assert_eq!(names[65].as_deref(), Some("alpha"));
        assert_eq!(names[66].as_deref(), Some("beta"));
        assert_eq!(names[67], None);
    }

    #[test]
    fn standard_or_missing() {
        assert!(builtin_encoding(b"/Encoding StandardEncoding def").is_none());
        assert!(builtin_encoding(b"no encoding here").is_none());
        assert!(builtin_encoding(b"/Encoding 256 array readonly def").is_none());
    }
}
