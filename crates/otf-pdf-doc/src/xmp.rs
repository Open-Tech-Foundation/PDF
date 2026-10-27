//! The XMP packet written to keep XMP in step with the Info dictionary
//! (DECISIONS.md D23).
//!
//! The packet is regenerated rather than edited: this crate has no XML
//! parser. The PDF/A and PDF/UA identification of a previous packet is
//! carried over, since dropping it would silently break conformance claims.

use crate::document::Info;

/// An XMP packet holding the Info fields, plus the PDF/A and PDF/UA
/// identification found in `previous`.
pub fn packet(info: &Info, previous: Option<&[u8]>) -> String {
    let previous = previous.map(String::from_utf8_lossy).unwrap_or_default();
    let mut props = String::new();
    let mut add = |line: String| {
        props.push_str("   ");
        props.push_str(&line);
        props.push('\n');
    };
    add("<dc:format>application/pdf</dc:format>".into());
    if let Some(t) = &info.title {
        add(format!(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            escape(t)
        ));
    }
    if let Some(a) = &info.author {
        add(format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            escape(a)
        ));
    }
    if let Some(s) = &info.subject {
        add(format!(
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>",
            escape(s)
        ));
    }
    if let Some(k) = &info.keywords {
        add(format!("<pdf:Keywords>{}</pdf:Keywords>", escape(k)));
    }
    if let Some(p) = &info.producer {
        add(format!("<pdf:Producer>{}</pdf:Producer>", escape(p)));
    }
    if let Some(c) = &info.creator {
        add(format!("<xmp:CreatorTool>{}</xmp:CreatorTool>", escape(c)));
    }
    if let Some(d) = info.creation_date.as_deref().and_then(iso_date) {
        add(format!("<xmp:CreateDate>{d}</xmp:CreateDate>"));
    }
    if let Some(d) = info.mod_date.as_deref().and_then(iso_date) {
        add(format!("<xmp:ModifyDate>{d}</xmp:ModifyDate>"));
    }
    let mut namespaces = String::new();
    for (prefix, uri, keys) in [
        (
            "pdfaid",
            "http://www.aiim.org/pdfa/ns/id/",
            &["part", "conformance", "amd", "rev"][..],
        ),
        (
            "pdfuaid",
            "http://www.aiim.org/pdfua/ns/id/",
            &["part", "rev"][..],
        ),
    ] {
        let mut found = false;
        for key in keys {
            if let Some(v) = property(&previous, prefix, key) {
                add(format!("<{prefix}:{key}>{}</{prefix}:{key}>", escape(&v)));
                found = true;
            }
        }
        if found {
            namespaces.push_str(&format!("\n    xmlns:{prefix}=\"{uri}\""));
        }
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n\
    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n\
    xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"\n\
    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"{namespaces}>\n\
{props}  </rdf:Description>\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>"
    )
}

/// A simple property's value, written as an element or an attribute.
fn property(xml: &str, prefix: &str, key: &str) -> Option<String> {
    let name = format!("{prefix}:{key}");
    if let Some(start) = xml.find(&format!("<{name}>")) {
        let rest = &xml[start + name.len() + 2..];
        let end = rest.find('<')?;
        return Some(unescape(rest[..end].trim()));
    }
    let start = xml.find(&format!("{name}="))? + name.len() + 1;
    let rest = &xml[start..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let end = rest[1..].find(quote)?;
    Some(unescape(&rest[1..=end]))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A PDF date (`D:YYYYMMDDHHmmSSOHH'mm'`) as an XMP (ISO 8601) date, with
/// as much precision as it has.
pub fn iso_date(pdf: &str) -> Option<String> {
    let s = pdf.strip_prefix("D:").unwrap_or(pdf);
    let digits = |from: usize, len: usize| -> Option<&str> {
        let part = s.get(from..from + len)?;
        part.bytes().all(|b| b.is_ascii_digit()).then_some(part)
    };
    let year = digits(0, 4)?;
    let mut out = year.to_owned();
    let Some(month) = digits(4, 2) else {
        return Some(out);
    };
    out.push_str(&format!("-{month}"));
    let Some(day) = digits(6, 2) else {
        return Some(out);
    };
    out.push_str(&format!("-{day}"));
    let Some(hour) = digits(8, 2) else {
        return Some(out);
    };
    let minute = digits(10, 2).unwrap_or("00");
    out.push_str(&format!("T{hour}:{minute}"));
    if let Some(second) = digits(12, 2) {
        out.push_str(&format!(":{second}"));
    }
    match s.get(14..15) {
        Some("Z") => out.push('Z'),
        Some(sign @ ("+" | "-")) => {
            let oh = digits(15, 2).unwrap_or("00");
            let om = digits(18, 2).unwrap_or("00");
            out.push_str(&format!("{sign}{oh}:{om}"));
        }
        _ => {}
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(iso_date("D:2026"), Some("2026".into()));
        assert_eq!(iso_date("D:20261027"), Some("2026-10-27".into()));
        assert_eq!(iso_date("D:202610271530"), Some("2026-10-27T15:30".into()));
        assert_eq!(
            iso_date("D:20261027153005Z"),
            Some("2026-10-27T15:30:05Z".into())
        );
        assert_eq!(
            iso_date("D:20261027153005+05'30'"),
            Some("2026-10-27T15:30:05+05:30".into())
        );
        assert_eq!(
            iso_date("D:20261027153005-08'00"),
            Some("2026-10-27T15:30:05-08:00".into())
        );
        assert_eq!(iso_date("garbage"), None);
    }

    #[test]
    fn packet_holds_info_and_keeps_pdfa_identification() {
        let info = Info {
            title: Some("A & <B>".into()),
            author: Some("Ann".into()),
            mod_date: Some("D:20260101".into()),
            ..Info::default()
        };
        let previous = br#"<rdf:Description pdfaid:part="2" pdfaid:conformance='B'><pdfuaid:part>1</pdfuaid:part></rdf:Description>"#;
        let xmp = packet(&info, Some(previous));
        assert!(xmp.contains("<rdf:li xml:lang=\"x-default\">A &amp; &lt;B&gt;</rdf:li>"));
        assert!(xmp.contains("<dc:creator><rdf:Seq><rdf:li>Ann</rdf:li>"));
        assert!(xmp.contains("<xmp:ModifyDate>2026-01-01</xmp:ModifyDate>"));
        assert!(xmp.contains("<pdfaid:part>2</pdfaid:part>"));
        assert!(xmp.contains("<pdfaid:conformance>B</pdfaid:conformance>"));
        assert!(xmp.contains("<pdfuaid:part>1</pdfuaid:part>"));
        assert!(xmp.contains("xmlns:pdfaid="));
        assert!(!xmp.contains("Producer"));
        let plain = packet(&Info::default(), None);
        assert!(!plain.contains("pdfaid"));
        assert!(plain.starts_with("<?xpacket begin="));
    }
}
