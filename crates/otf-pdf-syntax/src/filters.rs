//! Stream filters (ISO 32000-2 §7.4).
//!
//! The general-purpose filters are decoded here. The image codecs (DCT, JPX,
//! JBIG2, CCITT) are left encoded: decoding stops at the first of them and the
//! rest of the chain is returned for the caller (the renderer, Phase 8).
//!
//! Every decoder enforces [`Limits::max_decoded_size`] and
//! [`Limits::max_compression_ratio`] while it produces output, so a
//! decompression bomb is stopped before it allocates, not after.

use flate2::{Decompress, FlushDecompress, Status};

use crate::diag::{DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::limits::{LimitKind, Limits, Strictness};
use crate::object::{Dict, Name, Object};

/// One filter in a chain, with its (already resolved) parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// The filter's name, abbreviations expanded (`Fl` → `FlateDecode`).
    pub name: Name,
    /// Its `/DecodeParms` entry, if any.
    pub params: Option<Dict>,
}

/// The result of decoding a filter chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// The decoded bytes, or the partly decoded bytes if `remaining` is not
    /// empty.
    pub data: Vec<u8>,
    /// Filters not applied because they are image codecs or unknown, in
    /// order. Empty when the data is fully decoded.
    pub remaining: Vec<Filter>,
}

/// Expands the inline-image abbreviations of filter names (§8.9.7).
pub fn canonical_filter_name(name: &[u8]) -> &[u8] {
    match name {
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        other => other,
    }
}

/// Reads the filter chain from a stream dictionary whose `/Filter` and
/// `/DecodeParms` values are already resolved to direct objects. Inline
/// image abbreviations (`/F`, `/DP`) are accepted too.
pub fn filter_chain(dict: &Dict) -> Vec<Filter> {
    let names: Vec<&[u8]> = match dict.get(b"Filter").or_else(|| dict.get(b"F")) {
        Some(Object::Name(n)) => vec![n.as_bytes()],
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
        _ => Vec::new(),
    };
    let params = dict.get(b"DecodeParms").or_else(|| dict.get(b"DP"));
    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let p = match params {
                Some(Object::Dict(d)) if i == 0 => Some(d.clone()),
                Some(Object::Array(a)) => a.get(i).and_then(Object::as_dict).cloned(),
                _ => None,
            };
            Filter {
                name: Name::from(canonical_filter_name(name)),
                params: p,
            }
        })
        .collect()
}

fn is_image_codec(name: &[u8]) -> bool {
    matches!(
        name,
        b"DCTDecode" | b"JPXDecode" | b"JBIG2Decode" | b"CCITTFaxDecode"
    )
}

/// Decodes `input` through `filters`.
pub fn decode(
    input: &[u8],
    filters: &[Filter],
    limits: &Limits,
    strictness: Strictness,
    diagnostics: Option<&Diagnostics>,
) -> Result<Decoded> {
    let mut data = input.to_vec();
    for (i, filter) in filters.iter().enumerate() {
        let name = filter.name.as_bytes();
        if is_image_codec(name) {
            return Ok(Decoded {
                data,
                remaining: filters.get(i..).unwrap_or_default().to_vec(),
            });
        }
        limits.charge(data.len() as u64)?;
        let mut out = Output::new(limits, data.len());
        let params = filter.params.as_ref();
        let result = match name {
            b"FlateDecode" => flate(&data, &mut out),
            b"LZWDecode" => {
                let early = params.and_then(|p| p.get_int(b"EarlyChange")).unwrap_or(1) != 0;
                lzw(&data, early, &mut out)
            }
            b"ASCIIHexDecode" => ascii_hex(&data, &mut out),
            b"ASCII85Decode" => ascii85(&data, &mut out),
            b"RunLengthDecode" => run_length(&data, &mut out),
            // Decryption happens before filters; the filter itself is a no-op.
            b"Crypt" => out.extend(&data),
            _ => {
                if strictness == Strictness::Strict {
                    return Err(Error::Filter("unknown filter"));
                }
                if let Some(d) = diagnostics {
                    d.push(DiagnosticKind::UnknownFilter(name.to_vec()), None, None);
                }
                return Ok(Decoded {
                    data,
                    remaining: filters.get(i..).unwrap_or_default().to_vec(),
                });
            }
        };
        match result {
            Ok(()) => {}
            // A limit is never repaired.
            Err(e @ Error::Limit(_)) => return Err(e),
            // A corrupt tail: keep what decoded, as every viewer does.
            Err(e) => {
                if strictness == Strictness::Strict || out.buf.is_empty() {
                    return Err(e);
                }
                if let Some(d) = diagnostics {
                    d.push(DiagnosticKind::TruncatedStream, None, None);
                }
            }
        }
        data = out.buf;
        if matches!(name, b"FlateDecode" | b"LZWDecode")
            && let Some(p) = params
        {
            data = predict(data, p, limits)?;
        }
    }
    Ok(Decoded {
        data,
        remaining: Vec::new(),
    })
}

/// A growing output buffer that enforces the size and ratio limits.
struct Output<'l> {
    buf: Vec<u8>,
    limit: u64,
    limits: &'l Limits,
    input_len: u64,
}

impl<'l> Output<'l> {
    fn new(limits: &'l Limits, input_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit: limits.max_decoded_size,
            limits,
            input_len: input_len as u64,
        }
    }

    fn check(&self, len: u64) -> Result<()> {
        if len > self.limit {
            return Err(Error::Limit(LimitKind::DecodedSize));
        }
        if len > self.limits.ratio_floor
            && len / self.input_len.max(1) > self.limits.max_compression_ratio
        {
            return Err(Error::Limit(LimitKind::CompressionRatio));
        }
        Ok(())
    }

    fn push(&mut self, b: u8) -> Result<()> {
        self.check(self.buf.len() as u64 + 1)?;
        self.buf.push(b);
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<()> {
        self.check((self.buf.len() + bytes.len()) as u64)?;
        self.buf.extend_from_slice(bytes);
        Ok(())
    }
}

fn flate(input: &[u8], out: &mut Output<'_>) -> Result<()> {
    match inflate(input, true, out) {
        // No zlib header: some writers emit raw deflate.
        Err(Error::Filter(_)) if out.buf.is_empty() => inflate(input, false, out),
        other => other,
    }
}

fn inflate(input: &[u8], zlib: bool, out: &mut Output<'_>) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut d = Decompress::new(zlib);
    loop {
        let consumed = usize::try_from(d.total_in()).unwrap_or(usize::MAX);
        let rest = input.get(consumed..).unwrap_or_default();
        // Room for one more chunk, but never more than the limit allows plus
        // one byte (so exceeding it is detected).
        let len = out.buf.len();
        let room = (out.limit.saturating_sub(len as u64) + 1).min(CHUNK as u64) as usize;
        // Exact, so amortized doubling cannot allocate past the limit.
        out.buf.reserve_exact(room);
        let before_out = d.total_out();
        let status = d.decompress_vec(rest, &mut out.buf, FlushDecompress::None);
        out.check(out.buf.len() as u64)?;
        match status {
            Ok(Status::StreamEnd) => return Ok(()),
            Ok(_) => {
                let consumed_now = usize::try_from(d.total_in()).unwrap_or(usize::MAX);
                // There was output room, so no progress means the input ran
                // out before the end marker: truncated, but what decoded is
                // good.
                if consumed_now == consumed && d.total_out() == before_out {
                    return Err(Error::Filter("truncated deflate data"));
                }
            }
            Err(_) => return Err(Error::Filter("corrupt deflate data")),
        }
    }
}

fn lzw(input: &[u8], early_change: bool, out: &mut Output<'_>) -> Result<()> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;
    // Each entry: (prefix code, last byte, length).
    let mut table: Vec<(u16, u8, u32)> = (0..=255u16).map(|b| (u16::MAX, b as u8, 1)).collect();
    table.push((0, 0, 0)); // CLEAR
    table.push((0, 0, 0)); // EOD
    let mut bits = 9u32;
    let mut acc = 0u32;
    let mut acc_bits = 0u32;
    let mut prev: Option<u16> = None;
    let mut scratch = Vec::new();

    let reset = |table: &mut Vec<(u16, u8, u32)>| table.truncate(258);

    for &byte in input {
        acc = (acc << 8) | u32::from(byte);
        acc_bits += 8;
        while acc_bits >= bits {
            let code = ((acc >> (acc_bits - bits)) & ((1 << bits) - 1)) as u16;
            acc_bits -= bits;
            acc &= (1 << acc_bits) - 1;
            if code == CLEAR {
                reset(&mut table);
                bits = 9;
                prev = None;
                continue;
            }
            if code == EOD {
                return Ok(());
            }
            let next = table.len() as u16;
            let first_byte;
            if usize::from(code) < table.len() {
                expand(&table, code, &mut scratch);
                first_byte = scratch.first().copied().unwrap_or(0);
                out.extend(&scratch)?;
            } else if code == next {
                // The KwKwK case: the code being defined right now.
                let Some(p) = prev else {
                    return Err(Error::Filter("invalid LZW code"));
                };
                expand(&table, p, &mut scratch);
                let first = scratch.first().copied().unwrap_or(0);
                scratch.push(first);
                first_byte = first;
                out.extend(&scratch)?;
            } else {
                return Err(Error::Filter("invalid LZW code"));
            }
            if let Some(p) = prev
                && table.len() < 4096
            {
                let len = table.get(usize::from(p)).map_or(0, |e| e.2) + 1;
                table.push((p, first_byte, len));
            }
            prev = Some(code);
            let threshold = table.len() + usize::from(early_change);
            bits = match threshold {
                0..=511 => 9,
                512..=1023 => 10,
                1024..=2047 => 11,
                _ => 12,
            };
        }
    }
    // No EOD: tolerated, as by every reader.
    Ok(())
}

/// Writes the bytes of `code` into `scratch`.
fn expand(table: &[(u16, u8, u32)], code: u16, scratch: &mut Vec<u8>) {
    scratch.clear();
    let mut c = code;
    // The chain length is bounded by the table size (4096).
    for _ in 0..4096 {
        let Some(&(prefix, byte, _)) = table.get(usize::from(c)) else {
            break;
        };
        scratch.push(byte);
        if prefix == u16::MAX {
            break;
        }
        c = prefix;
    }
    scratch.reverse();
}

fn ascii_hex(input: &[u8], out: &mut Output<'_>) -> Result<()> {
    let mut high: Option<u8> = None;
    for &b in input {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b'>' => break,
            _ if crate::lexer::is_whitespace(b) => continue,
            _ => return Err(Error::Filter("invalid ASCIIHex character")),
        };
        match high.take() {
            Some(h) => out.push(h << 4 | v)?,
            None => high = Some(v),
        }
    }
    if let Some(h) = high {
        out.push(h << 4)?;
    }
    Ok(())
}

fn ascii85(input: &[u8], out: &mut Output<'_>) -> Result<()> {
    let input = input.strip_prefix(b"<~").unwrap_or(input);
    let mut group = [0u8; 5];
    let mut n = 0usize;
    for &b in input {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend(&[0; 4])?,
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(n) {
                    *slot = b - b'!';
                }
                n += 1;
                if n == 5 {
                    out.extend(&ascii85_group(&group)?)?;
                    n = 0;
                }
            }
            _ if crate::lexer::is_whitespace(b) => {}
            _ => return Err(Error::Filter("invalid ASCII85 character")),
        }
    }
    if n == 1 {
        return Err(Error::Filter("ASCII85 final group of one character"));
    }
    if n > 1 {
        for slot in group.iter_mut().skip(n) {
            *slot = 84; // 'u' - '!'
        }
        let bytes = ascii85_group(&group)?;
        out.extend(bytes.get(..n - 1).unwrap_or_default())?;
    }
    Ok(())
}

fn ascii85_group(group: &[u8; 5]) -> Result<[u8; 4]> {
    let value = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
    let value = u32::try_from(value).map_err(|_| Error::Filter("ASCII85 group overflow"))?;
    Ok(value.to_be_bytes())
}

fn run_length(input: &[u8], out: &mut Output<'_>) -> Result<()> {
    let mut i = 0;
    while let Some(&len) = input.get(i) {
        i += 1;
        match len {
            128 => return Ok(()),
            0..=127 => {
                let n = usize::from(len) + 1;
                let Some(run) = input.get(i..i + n) else {
                    out.extend(input.get(i..).unwrap_or_default())?;
                    return Err(Error::Filter("truncated RunLength data"));
                };
                out.extend(run)?;
                i += n;
            }
            _ => {
                let Some(&b) = input.get(i) else {
                    return Err(Error::Filter("truncated RunLength data"));
                };
                i += 1;
                for _ in 0..(257 - usize::from(len)) {
                    out.push(b)?;
                }
            }
        }
    }
    Ok(())
}

/// Undoes a PNG or TIFF predictor (§7.4.4.4).
fn predict(data: Vec<u8>, params: &Dict, limits: &Limits) -> Result<Vec<u8>> {
    let predictor = params.get_int(b"Predictor").unwrap_or(1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = params.get_int(b"Colors").unwrap_or(1);
    let bpc = params.get_int(b"BitsPerComponent").unwrap_or(8);
    let columns = params.get_int(b"Columns").unwrap_or(1);
    if !(1..=32).contains(&colors)
        || !matches!(bpc, 1 | 2 | 4 | 8 | 16)
        || columns < 1
        || columns as u64 > limits.max_decoded_size
    {
        return Err(Error::Filter("invalid predictor parameters"));
    }
    let (colors, bpc, columns) = (colors as usize, bpc as usize, columns as usize);
    let bits_per_pixel = colors * bpc;
    let row_len = (bits_per_pixel * columns).div_ceil(8);
    let bpp = bits_per_pixel.div_ceil(8);
    match predictor {
        2 => Ok(tiff_predict(data, colors, bpc, row_len)),
        10..=15 => png_predict(&data, row_len, bpp),
        _ => Err(Error::Filter("unknown predictor")),
    }
}

fn png_predict(data: &[u8], row_len: usize, bpp: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_len];
    let mut row = vec![0u8; row_len];
    for chunk in data.chunks(row_len + 1) {
        let Some((&kind, encoded)) = chunk.split_first() else {
            break;
        };
        row.fill(0);
        for (i, &x) in encoded.iter().enumerate() {
            let a = if i >= bpp {
                row.get(i - bpp).copied().unwrap_or(0)
            } else {
                0
            };
            let b = prev.get(i).copied().unwrap_or(0);
            let c = if i >= bpp {
                prev.get(i - bpp).copied().unwrap_or(0)
            } else {
                0
            };
            let value = match kind {
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                // 0, and leniently any unknown filter type: None.
                _ => x,
            };
            if let Some(slot) = row.get_mut(i) {
                *slot = value;
            }
        }
        out.extend_from_slice(row.get(..encoded.len()).unwrap_or_default());
        std::mem::swap(&mut prev, &mut row);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn tiff_predict(mut data: Vec<u8>, colors: usize, bpc: usize, row_len: usize) -> Vec<u8> {
    for row in data.chunks_mut(row_len) {
        match bpc {
            8 => {
                for i in colors..row.len() {
                    let left = row.get(i - colors).copied().unwrap_or(0);
                    if let Some(x) = row.get_mut(i) {
                        *x = x.wrapping_add(left);
                    }
                }
            }
            16 => {
                let step = colors * 2;
                let mut i = step;
                while i + 1 < row.len() {
                    let left = u16::from_be_bytes([
                        row.get(i - step).copied().unwrap_or(0),
                        row.get(i - step + 1).copied().unwrap_or(0),
                    ]);
                    let cur = u16::from_be_bytes([
                        row.get(i).copied().unwrap_or(0),
                        row.get(i + 1).copied().unwrap_or(0),
                    ]);
                    let [hi, lo] = cur.wrapping_add(left).to_be_bytes();
                    if let Some(x) = row.get_mut(i) {
                        *x = hi;
                    }
                    if let Some(x) = row.get_mut(i + 1) {
                        *x = lo;
                    }
                    i += 2;
                }
            }
            _ => {
                // 1, 2 or 4 bits: work sample by sample.
                let samples = row.len() * 8 / bpc;
                let mask = (1u16 << bpc) - 1;
                let get = |row: &[u8], s: usize| -> u16 {
                    let bit = s * bpc;
                    let byte = row.get(bit / 8).copied().unwrap_or(0);
                    (u16::from(byte) >> (8 - bpc - bit % 8)) & mask
                };
                for s in colors..samples {
                    let v = (get(row, s) + get(row, s - colors)) & mask;
                    let bit = s * bpc;
                    let shift = 8 - bpc - bit % 8;
                    if let Some(byte) = row.get_mut(bit / 8) {
                        *byte = (*byte & !((mask as u8) << shift)) | ((v as u8) << shift);
                    }
                }
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn filter(name: &str) -> Filter {
        Filter {
            name: Name::from(name),
            params: None,
        }
    }

    fn run(input: &[u8], filters: &[Filter]) -> Result<Decoded> {
        decode(input, filters, &Limits::default(), Strictness::Strict, None)
    }

    fn data(input: &[u8], name: &str) -> Vec<u8> {
        run(input, &[filter(name)]).unwrap().data
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn chain_parsing() {
        let mut d = Dict::new();
        d.insert(
            "Filter",
            vec![
                Object::Name(Name::from("A85")),
                Object::Name(Name::from("Fl")),
            ],
        );
        let mut p = Dict::new();
        p.insert("Predictor", 12i64);
        d.insert("DecodeParms", vec![Object::Null, Object::Dict(p.clone())]);
        let chain = filter_chain(&d);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].name, Name::from("ASCII85Decode"));
        assert_eq!(chain[0].params, None);
        assert_eq!(chain[1].name, Name::from("FlateDecode"));
        assert_eq!(chain[1].params, Some(p));

        let mut single = Dict::new();
        single.insert("F", Name::from("AHx"));
        single.insert("DP", Dict::new());
        let chain = filter_chain(&single);
        assert_eq!(chain[0].name, Name::from("ASCIIHexDecode"));
        assert!(chain[0].params.is_some());
        assert!(filter_chain(&Dict::new()).is_empty());
    }

    #[test]
    fn flate_roundtrip() {
        let text = b"Hello, PDF world! ".repeat(100);
        assert_eq!(data(&zlib(&text), "FlateDecode"), text);
    }

    #[test]
    fn raw_deflate_without_header() {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"raw deflate").unwrap();
        assert_eq!(data(&e.finish().unwrap(), "FlateDecode"), b"raw deflate");
    }

    #[test]
    fn truncated_flate_keeps_prefix() {
        let text: Vec<u8> = (0..20000u32).map(|i| (i * 7 % 251) as u8).collect();
        let z = zlib(&text);
        let cut = &z[..z.len() / 2];
        let diags = Diagnostics::default();
        let out = decode(
            cut,
            &[filter("FlateDecode")],
            &Limits::default(),
            Strictness::Lenient,
            Some(&diags),
        )
        .unwrap();
        assert!(!out.data.is_empty());
        assert!(text.starts_with(&out.data));
        assert_eq!(diags.snapshot()[0].kind, DiagnosticKind::TruncatedStream);
        assert!(
            decode(
                cut,
                &[filter("FlateDecode")],
                &Limits::default(),
                Strictness::Strict,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn garbage_flate_is_an_error() {
        assert!(run(b"\xff\xff\xff\xff", &[filter("FlateDecode")]).is_err());
    }

    #[test]
    fn decompression_bomb_stops_at_limit() {
        let bomb = zlib(&vec![0u8; 8 << 20]);
        let limits = Limits {
            max_decoded_size: 1 << 20,
            ..Limits::default()
        };
        assert_eq!(
            decode(
                &bomb,
                &[filter("FlateDecode")],
                &limits,
                Strictness::Lenient,
                None
            ),
            Err(Error::Limit(LimitKind::DecodedSize))
        );
        let limits = Limits {
            max_compression_ratio: 100,
            ratio_floor: 1 << 16,
            ..Limits::default()
        };
        assert_eq!(
            decode(
                &bomb,
                &[filter("FlateDecode")],
                &limits,
                Strictness::Lenient,
                None
            ),
            Err(Error::Limit(LimitKind::CompressionRatio))
        );
    }

    #[test]
    fn png_predictors() {
        // Two rows of 3 bytes, one per filter type, encoded by hand.
        let raw = [1u8, 2, 3, 4, 6, 8];
        let mut p = Dict::new();
        p.insert("Predictor", 12i64);
        p.insert("Columns", 3i64);
        for (kind, encoded) in [
            (0u8, vec![0, 1, 2, 3, 0, 4, 6, 8]),
            (1, vec![1, 1, 1, 1, 1, 4, 2, 2]),
            (2, vec![2, 1, 2, 3, 2, 3, 4, 5]),
            (3, vec![3, 1, 2, 2, 3, 4, 3, 4]),
            (4, vec![4, 1, 1, 1, 4, 3, 2, 2]),
        ] {
            let f = Filter {
                name: Name::from("FlateDecode"),
                params: Some(p.clone()),
            };
            assert_eq!(run(&zlib(&encoded), &[f]).unwrap().data, raw, "type {kind}");
        }
    }

    #[test]
    fn tiff_predictor_8_bit() {
        let mut p = Dict::new();
        p.insert("Predictor", 2i64);
        p.insert("Columns", 3i64);
        p.insert("Colors", 1i64);
        let f = Filter {
            name: Name::from("FlateDecode"),
            params: Some(p),
        };
        assert_eq!(
            run(&zlib(&[1, 1, 1, 5, 1, 1]), &[f]).unwrap().data,
            [1, 2, 3, 5, 6, 7]
        );
    }

    #[test]
    fn tiff_predictor_16_and_4_bit() {
        assert_eq!(tiff_predict(vec![0, 1, 0, 2], 1, 16, 4), vec![0, 1, 0, 3]);
        // Four 4-bit samples 1,1,1,1 → 1,2,3,4.
        assert_eq!(tiff_predict(vec![0x11, 0x11], 1, 4, 2), vec![0x12, 0x34]);
    }

    #[test]
    fn bad_predictor_params() {
        let mut p = Dict::new();
        p.insert("Predictor", 12i64);
        p.insert("BitsPerComponent", 3i64);
        let f = Filter {
            name: Name::from("FlateDecode"),
            params: Some(p),
        };
        assert!(run(&zlib(b"x"), &[f]).is_err());
    }

    #[test]
    fn lzw() {
        // The example from ISO 32000-2 §7.4.4.2.
        let encoded = [0x80, 0x0b, 0x60, 0x50, 0x22, 0x0c, 0x0c, 0x85, 0x01];
        assert_eq!(data(&encoded, "LZWDecode"), b"-----A---B");
    }

    #[test]
    fn lzw_invalid_code() {
        // 9-bit code 300 with an empty table.
        assert!(run(&[0x96, 0x00], &[filter("LZWDecode")]).is_err());
    }

    #[test]
    fn ascii_hex() {
        assert_eq!(data(b"48 65 6C6c6f>", "ASCIIHexDecode"), b"Hello");
        assert_eq!(data(b"901FA>", "ASCIIHexDecode"), b"\x90\x1f\xa0");
        assert_eq!(data(b"41", "ASCIIHexDecode"), b"A");
        assert!(run(b"4G>", &[filter("ASCIIHexDecode")]).is_err());
    }

    #[test]
    fn ascii85() {
        assert_eq!(
            data(b"87cURD]i,\"Ebo80~>", "ASCII85Decode"),
            b"Hello World!"
        );
        assert_eq!(
            data(b"<~87cURD]i,\"Ebo80~>", "ASCII85Decode"),
            b"Hello World!"
        );
        assert_eq!(data(b"z~>", "ASCII85Decode"), [0, 0, 0, 0]);
        assert_eq!(
            data(b"87cUR D]i,\n\"Ebo8~>", "ASCII85Decode"),
            b"Hello World"
        );
        assert!(run(b"s8W-\"~>", &[filter("ASCII85Decode")]).is_err());
        assert!(run(b"87cURD]i,\"Ebo80a~>", &[filter("ASCII85Decode")]).is_err());
        assert!(run(b"\x01", &[filter("ASCII85Decode")]).is_err());
    }

    #[test]
    fn run_length() {
        assert_eq!(
            data(&[2, b'a', b'b', b'c', 254, b'x', 128, 9], "RunLengthDecode"),
            b"abcxxx"
        );
        assert!(run(&[5, b'a'], &[filter("RunLengthDecode")]).is_err());
        assert!(run(&[200], &[filter("RunLengthDecode")]).is_err());
    }

    #[test]
    fn image_codecs_are_left_encoded() {
        let out = run(b"48>", &[filter("ASCIIHexDecode"), filter("DCTDecode")]).unwrap();
        assert_eq!(out.data, b"H");
        assert_eq!(out.remaining, vec![filter("DCTDecode")]);
    }

    #[test]
    fn unknown_filter() {
        let diags = Diagnostics::default();
        let out = decode(
            b"abc",
            &[filter("Foo")],
            &Limits::default(),
            Strictness::Lenient,
            Some(&diags),
        )
        .unwrap();
        assert_eq!(out.data, b"abc");
        assert_eq!(out.remaining.len(), 1);
        assert_eq!(
            diags.snapshot()[0].kind,
            DiagnosticKind::UnknownFilter(b"Foo".to_vec())
        );
        assert!(
            decode(
                b"abc",
                &[filter("Foo")],
                &Limits::default(),
                Strictness::Strict,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn crypt_filter_passthrough() {
        assert_eq!(data(b"abc", "Crypt"), b"abc");
    }
}
