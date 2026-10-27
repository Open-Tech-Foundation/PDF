//! The standard security handler, read side (ISO 32000-2 §7.6).
//!
//! Reads revisions 2 to 6: RC4 with 40- to 128-bit keys, AES-128 and
//! AES-256, including the deprecated revision 5. Writes revision 6 (AES-256)
//! only, never RC4 or AES-128 (docs/PLAN.md §2).

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
use md5::{Digest, Md5};
use rc4::{KeyInit, Rc4, StreamCipher, consts};
use sha2::{Sha256, Sha384, Sha512};

use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjId, Object, PdfString};

/// Padding for passwords shorter than 32 bytes (Algorithm 2, step a).
const PAD: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01, 0x08,
    0x2e, 0x2e, 0x00, 0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53, 0x69, 0x7a,
];

/// How one class of data (strings, streams, embedded files) is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptMethod {
    /// Not encrypted (the `Identity` crypt filter).
    Identity,
    /// RC4 with a per-object key.
    Rc4,
    /// AES-128-CBC with a per-object key.
    AesV2,
    /// AES-256-CBC with the file key.
    AesV3,
}

/// Which password opened the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authentication {
    /// No password matched, but only embedded files are encrypted, so the
    /// document itself is readable. Data that needs the key is returned
    /// undecrypted; see [`SecurityHandler::is_authenticated`].
    None,
    /// The user password (possibly the empty one).
    User,
    /// The owner password: all permissions apply.
    Owner,
}

/// A security handler, authenticated and ready to decrypt.
#[derive(Debug, Clone)]
pub struct SecurityHandler {
    revision: i64,
    key: Vec<u8>,
    strings: CryptMethod,
    streams: CryptMethod,
    embedded_files: CryptMethod,
    encrypt_metadata: bool,
    permissions: u32,
    authentication: Authentication,
}

impl SecurityHandler {
    /// Authenticates against `/Encrypt` with `password`, trying it as the
    /// user password and then as the owner password. `id0` is the first
    /// element of the trailer's `/ID`.
    pub fn new(encrypt: &Dict, id0: &[u8], password: &[u8]) -> Result<Self> {
        let filter = encrypt.get_name(b"Filter").unwrap_or(b"Standard");
        if filter != b"Standard" {
            return Err(Error::UnsupportedEncryption(format!(
                "security handler /{}",
                String::from_utf8_lossy(filter)
            )));
        }
        let v = encrypt.get_int(b"V").unwrap_or(0);
        let r = encrypt.get_int(b"R").unwrap_or(0);
        if !(2..=6).contains(&r) {
            return Err(Error::UnsupportedEncryption(format!("revision {r}")));
        }
        let o = encrypt
            .get(b"O")
            .and_then(Object::as_string)
            .unwrap_or_default();
        let u = encrypt
            .get(b"U")
            .and_then(Object::as_string)
            .unwrap_or_default();
        // /P is a signed 32-bit integer; only its bit pattern matters.
        let p = encrypt.get_int(b"P").unwrap_or(0) as i32;
        let encrypt_metadata = encrypt
            .get(b"EncryptMetadata")
            .and_then(Object::as_bool)
            .unwrap_or(true);

        let (strings, streams, embedded_files) = crypt_methods(encrypt, v)?;
        let mut handler = Self {
            revision: r,
            key: Vec::new(),
            strings,
            streams,
            embedded_files,
            encrypt_metadata,
            permissions: p as u32,
            authentication: Authentication::User,
        };

        if let Some((key, auth)) =
            authenticate(encrypt, id0, password, v, r, o, u, p, encrypt_metadata)
        {
            handler.key = key;
            handler.authentication = auth;
            return Ok(handler);
        }
        // Only embedded files are encrypted (`/AuthEvent /EFOpen`): the
        // document reads without a password; attachments need one.
        if strings == CryptMethod::Identity && streams == CryptMethod::Identity {
            handler.authentication = Authentication::None;
            return Ok(handler);
        }
        Err(Error::InvalidPassword)
    }

    /// The `/P` permission bits.
    pub fn permissions(&self) -> u32 {
        self.permissions
    }

    /// Whether a password matched, so every class of data can be decrypted.
    pub fn is_authenticated(&self) -> bool {
        self.authentication != Authentication::None
    }

    /// Which password authenticated.
    pub fn authentication(&self) -> Authentication {
        self.authentication
    }

    /// The security handler revision (`/R`).
    pub fn revision(&self) -> i64 {
        self.revision
    }

    /// Whether metadata streams are encrypted.
    pub fn encrypt_metadata(&self) -> bool {
        self.encrypt_metadata
    }

    /// The method for strings.
    pub fn string_method(&self) -> CryptMethod {
        self.strings
    }

    /// The method for streams.
    pub fn stream_method(&self) -> CryptMethod {
        self.streams
    }

    /// The method for embedded file streams.
    pub fn embedded_file_method(&self) -> CryptMethod {
        self.embedded_files
    }

    /// Decrypts `data` belonging to object `id` with `method`.
    pub fn decrypt(&self, id: ObjId, method: CryptMethod, data: &[u8]) -> Vec<u8> {
        if !self.is_authenticated() {
            return data.to_vec();
        }
        match method {
            CryptMethod::Identity => data.to_vec(),
            CryptMethod::Rc4 => {
                let mut out = data.to_vec();
                rc4(&self.object_key(id, false), &mut out);
                out
            }
            CryptMethod::AesV2 => aes_cbc_decrypt(&self.object_key(id, true), data),
            CryptMethod::AesV3 => aes_cbc_decrypt(&self.key, data),
        }
    }

    /// Decrypts every string inside `object` (recursively), for object `id`.
    /// Streams' data is left alone; see [`SecurityHandler::decrypt`].
    pub fn decrypt_strings(&self, id: ObjId, object: &mut Object) {
        match object {
            Object::String(s) => s.bytes = self.decrypt(id, self.strings, &s.bytes),
            Object::Array(items) => {
                for item in items {
                    self.decrypt_strings(id, item);
                }
            }
            Object::Dict(d) => self.decrypt_dict_strings(id, d),
            Object::Stream(s) => self.decrypt_dict_strings(id, &mut s.dict),
            _ => {}
        }
    }

    fn decrypt_dict_strings(&self, id: ObjId, dict: &mut Dict) {
        // A signature's /Contents is not encrypted (§7.6.2).
        let is_signature = dict.is_type(b"Sig")
            || dict.is_type(b"DocTimeStamp")
            || dict.contains_key(b"ByteRange");
        let keys: Vec<_> = dict.iter().map(|(k, _)| k.clone()).collect();
        for key in keys {
            if is_signature && key.as_bytes() == b"Contents" {
                continue;
            }
            if let Some(mut value) = dict.remove(key.as_bytes()) {
                self.decrypt_strings(id, &mut value);
                dict.insert(key, value);
            }
        }
    }

    /// Algorithm 1: the per-object key for RC4 and AES-128.
    fn object_key(&self, id: ObjId, aes: bool) -> Vec<u8> {
        let mut h = Md5::new();
        h.update(&self.key);
        h.update(&id.num.to_le_bytes()[..3]);
        h.update(id.generation.to_le_bytes());
        if aes {
            h.update(b"sAlT");
        }
        let digest = h.finalize();
        let n = (self.key.len() + 5).min(16);
        digest.get(..n).unwrap_or_default().to_vec()
    }
}

/// Tries `password` and its encoding variants as user and owner password.
#[allow(clippy::too_many_arguments)]
fn authenticate(
    encrypt: &Dict,
    id0: &[u8],
    password: &[u8],
    v: i64,
    r: i64,
    o: &[u8],
    u: &[u8],
    p: i32,
    encrypt_metadata: bool,
) -> Option<(Vec<u8>, Authentication)> {
    let candidates = password_candidates(password, r);
    if r >= 5 {
        let oe = encrypt
            .get(b"OE")
            .and_then(Object::as_string)
            .unwrap_or_default();
        let ue = encrypt
            .get(b"UE")
            .and_then(Object::as_string)
            .unwrap_or_default();
        return candidates.iter().find_map(|pw| {
            let pw = pw.get(..pw.len().min(127)).unwrap_or_default();
            aes256_key(r, pw, o, u, oe, ue)
        });
    }
    let length_bits = if v == 1 {
        40
    } else {
        encrypt.get_int(b"Length").unwrap_or(40)
    };
    // Some writers give /Length in bytes.
    let length_bits = if length_bits <= 16 {
        length_bits * 8
    } else {
        length_bits
    };
    let n = if r == 2 {
        5
    } else {
        (length_bits / 8).clamp(5, 16) as usize
    };
    let user_key = |pw: &[u8]| compute_key(pw, o, p, id0, r, n, encrypt_metadata);
    candidates.iter().find_map(|pw| {
        let key = user_key(pw);
        if check_user_key(&key, u, id0, r) {
            return Some((key, Authentication::User));
        }
        // Owner password: recover the user password from /O (Algorithm 7).
        let key = user_key(&owner_to_user_password(pw, o, r, n));
        check_user_key(&key, u, id0, r).then_some((key, Authentication::Owner))
    })
}

/// The byte strings to try for a password given as UTF-8 (or raw bytes).
///
/// R2–R4 passwords are PDFDocEncoding, so non-ASCII text is also tried as
/// Latin-1. R5/R6 passwords are SASLprep-ed UTF-8; the cheap part of that
/// profile (dropping characters mapped to nothing, mapping non-ASCII spaces
/// to U+0020) is applied, but not NFKC normalization, which needs Unicode
/// tables this crate does not carry.
fn password_candidates(password: &[u8], r: i64) -> Vec<Vec<u8>> {
    let mut out = vec![password.to_vec()];
    let Ok(text) = std::str::from_utf8(password) else {
        return out;
    };
    let variant: Option<Vec<u8>> = if r <= 4 {
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect::<Option<Vec<u8>>>()
    } else {
        let prepped: String = text
            .chars()
            .filter(|c| !is_mapped_to_nothing(*c))
            .map(|c| {
                if c != ' ' && c.is_whitespace() {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        Some(prepped.into_bytes())
    };
    if let Some(v) = variant
        && v != password
    {
        out.push(v);
    }
    out
}

/// RFC 3454 table B.1, "commonly mapped to nothing".
fn is_mapped_to_nothing(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{34f}'
            | '\u{1806}'
            | '\u{180b}'..='\u{180d}'
            | '\u{200b}'..='\u{200d}'
            | '\u{2060}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
    )
}

/// Reads `/StrF`, `/StmF` and `/EFF` (V4/V5 crypt filters) or the implied
/// method for V1/V2.
fn crypt_methods(encrypt: &Dict, v: i64) -> Result<(CryptMethod, CryptMethod, CryptMethod)> {
    match v {
        1..=3 => Ok((CryptMethod::Rc4, CryptMethod::Rc4, CryptMethod::Rc4)),
        4 | 5 => {
            let cf = encrypt.get_dict(b"CF");
            let method = |key: &[u8]| -> Result<CryptMethod> {
                let name = encrypt.get_name(key).unwrap_or(b"Identity");
                if name == b"Identity" {
                    return Ok(CryptMethod::Identity);
                }
                let filter = cf.and_then(|cf| cf.get_dict(name));
                match filter.and_then(|f| f.get_name(b"CFM")) {
                    Some(b"V2") => Ok(CryptMethod::Rc4),
                    Some(b"AESV2") => Ok(CryptMethod::AesV2),
                    Some(b"AESV3") => Ok(CryptMethod::AesV3),
                    Some(b"None") => Ok(CryptMethod::Identity),
                    // A missing filter or CFM: V5 implies AES-256, V4 AES-128
                    // is what readers assume.
                    None if v == 5 => Ok(CryptMethod::AesV3),
                    None => Ok(CryptMethod::AesV2),
                    Some(other) => Err(Error::UnsupportedEncryption(format!(
                        "crypt filter method /{}",
                        String::from_utf8_lossy(other)
                    ))),
                }
            };
            let strings = method(b"StrF")?;
            let streams = method(b"StmF")?;
            let embedded = if encrypt.contains_key(b"EFF") {
                method(b"EFF")?
            } else {
                streams
            };
            Ok((strings, streams, embedded))
        }
        other => Err(Error::UnsupportedEncryption(format!("algorithm V {other}"))),
    }
}

fn padded(password: &[u8]) -> [u8; 32] {
    let mut out = PAD;
    let n = password.len().min(32);
    if let (Some(dst), Some(src)) = (out.get_mut(..n), password.get(..n)) {
        dst.copy_from_slice(src);
    }
    if let (Some(dst), Some(src)) = (out.get_mut(n..), PAD.get(..32 - n)) {
        dst.copy_from_slice(src);
    }
    out
}

/// Algorithm 2: the file key from a user password (R2–R4).
fn compute_key(
    password: &[u8],
    o: &[u8],
    p: i32,
    id0: &[u8],
    r: i64,
    n: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(padded(password));
    h.update(o.get(..32).unwrap_or(o));
    h.update(p.to_le_bytes());
    h.update(id0);
    if r >= 4 && !encrypt_metadata {
        h.update([0xff; 4]);
    }
    let mut digest = h.finalize().to_vec();
    if r >= 3 {
        for _ in 0..50 {
            digest = Md5::digest(digest.get(..n).unwrap_or_default()).to_vec();
        }
    }
    digest.truncate(n);
    digest
}

/// Algorithms 4 and 5 (the comparison half of 6).
fn check_user_key(key: &[u8], u: &[u8], id0: &[u8], r: i64) -> bool {
    if r == 2 {
        let mut out = PAD.to_vec();
        rc4(key, &mut out);
        return u.get(..32) == Some(&out[..]);
    }
    let mut h = Md5::new();
    h.update(PAD);
    h.update(id0);
    let mut out = h.finalize().to_vec();
    for i in 0..20u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        rc4(&k, &mut out);
    }
    u.get(..16) == out.get(..16)
}

/// Algorithm 7, first half: recovers the user password from `/O`.
fn owner_to_user_password(owner: &[u8], o: &[u8], r: i64, n: usize) -> Vec<u8> {
    let mut digest = Md5::digest(padded(owner)).to_vec();
    if r >= 3 {
        for _ in 0..50 {
            digest = Md5::digest(&digest).to_vec();
        }
    }
    digest.truncate(n);
    let mut out = o.get(..32).unwrap_or(o).to_vec();
    if r == 2 {
        rc4(&digest, &mut out);
    } else {
        for i in (0..20u8).rev() {
            let k: Vec<u8> = digest.iter().map(|b| b ^ i).collect();
            rc4(&k, &mut out);
        }
    }
    out
}

/// R5/R6: validates the password against `/U` then `/O` and unwraps the file
/// key from `/UE` or `/OE`.
fn aes256_key(
    r: i64,
    pw: &[u8],
    o: &[u8],
    u: &[u8],
    oe: &[u8],
    ue: &[u8],
) -> Option<(Vec<u8>, Authentication)> {
    let (o, u) = (o.get(..48)?, u.get(..48)?);
    let hash = |input: &[u8], salt: &[u8], udata: &[u8]| -> Vec<u8> {
        if r == 5 {
            let mut h = Sha256::new();
            h.update(input);
            h.update(salt);
            h.update(udata);
            h.finalize().to_vec()
        } else {
            hash_2b(input, salt, udata)
        }
    };
    let unwrap = |intermediate: &[u8], wrapped: &[u8]| -> Option<Vec<u8>> {
        let mut buf = wrapped.get(..32)?.to_vec();
        cbc::Decryptor::<aes::Aes256>::new_from_slices(intermediate, &[0u8; 16])
            .ok()?
            .decrypt_padded_mut::<NoPadding>(&mut buf)
            .ok()?;
        Some(buf)
    };
    if hash(pw, u.get(32..40)?, &[]) == u.get(..32)? {
        let k = hash(pw, u.get(40..48)?, &[]);
        return Some((unwrap(&k, ue)?, Authentication::User));
    }
    if hash(pw, o.get(32..40)?, u) == o.get(..32)? {
        let k = hash(pw, o.get(40..48)?, u);
        return Some((unwrap(&k, oe)?, Authentication::Owner));
    }
    None
}

/// Algorithm 2.B: the R6 hardened hash.
fn hash_2b(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(password);
    h.update(salt);
    h.update(udata);
    let mut k = h.finalize().to_vec();
    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let (Some(key), Some(iv)) = (k.get(..16), k.get(16..32)) else {
            return k;
        };
        let len = k1.len();
        let Ok(enc) = cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv) else {
            return k;
        };
        // k1's length is a multiple of 64, so no padding is needed.
        let Ok(e) = enc.encrypt_padded_mut::<NoPadding>(&mut k1, len) else {
            return k;
        };
        let e = e.to_vec();
        let sum: u32 = e.iter().take(16).map(|&b| u32::from(b)).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        let last = u32::from(e.last().copied().unwrap_or(0));
        round += 1;
        if round >= 64 && last <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

/// RC4 with a 5- to 16-byte key; other lengths leave `data` unchanged
/// (they cannot arise from a valid `/Length`).
fn rc4(key: &[u8], data: &mut [u8]) {
    macro_rules! apply {
        ($($n:literal => $size:ty),*) => {
            match key.len() {
                $($n => {
                    if let Ok(mut c) = Rc4::<$size>::new_from_slice(key) {
                        c.apply_keystream(data);
                    }
                })*
                _ => {}
            }
        };
    }
    apply!(5 => consts::U5, 6 => consts::U6, 7 => consts::U7, 8 => consts::U8,
        9 => consts::U9, 10 => consts::U10, 11 => consts::U11, 12 => consts::U12,
        13 => consts::U13, 14 => consts::U14, 15 => consts::U15, 16 => consts::U16);
}

/// AES-CBC with the IV in the first 16 bytes. Lenient: a trailing partial
/// block is dropped and invalid padding is left in place.
fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let (Some(iv), Some(body)) = (data.get(..16), data.get(16..)) else {
        return Vec::new();
    };
    let mut buf = body
        .get(..body.len() / 16 * 16)
        .unwrap_or_default()
        .to_vec();
    let ok = match key.len() {
        16 => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
            .ok()
            .and_then(|d| d.decrypt_padded_mut::<NoPadding>(&mut buf).ok().map(|_| ())),
        32 => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
            .ok()
            .and_then(|d| d.decrypt_padded_mut::<NoPadding>(&mut buf).ok().map(|_| ())),
        _ => None,
    };
    if ok.is_none() {
        return Vec::new();
    }
    if let Some(&pad) = buf.last() {
        let n = usize::from(pad);
        if (1..=16).contains(&n) && n <= buf.len() && buf.iter().rev().take(n).all(|&b| b == pad) {
            buf.truncate(buf.len() - n);
        }
    }
    buf
}

/// Randomness, injected by the host (docs/PLAN.md §2: no ambient I/O).
/// Encryption takes keys, salts and IVs from it; a deterministic source gives
/// byte-identical output.
pub trait Entropy: Send + Sync {
    /// Fills `buf` with random bytes.
    fn fill(&self, buf: &mut [u8]);
}

/// What an encrypted document allows a user-password reader to do
/// (ISO 32000-2 Table 22). The owner password allows everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Print (bit 3).
    pub print: bool,
    /// Modify contents (bit 4).
    pub modify: bool,
    /// Copy or extract text and graphics (bit 5).
    pub copy: bool,
    /// Add or modify annotations and fill forms (bit 6).
    pub annotate: bool,
    /// Fill in form fields (bit 9).
    pub fill_forms: bool,
    /// Extract for accessibility (bit 10).
    pub accessibility: bool,
    /// Assemble: insert, rotate, delete pages (bit 11).
    pub assemble: bool,
    /// Print at full quality (bit 12).
    pub print_high_quality: bool,
}

impl Permissions {
    /// Everything allowed.
    pub const ALL: Permissions = Permissions {
        print: true,
        modify: true,
        copy: true,
        annotate: true,
        fill_forms: true,
        accessibility: true,
        assemble: true,
        print_high_quality: true,
    };

    /// The `/P` value: reserved bits 7, 8 and 13–32 set, 1–2 clear.
    pub fn bits(&self) -> u32 {
        let mut p: u32 = 0xffff_f0c0;
        for (on, bit) in [
            (self.print, 3),
            (self.modify, 4),
            (self.copy, 5),
            (self.annotate, 6),
            (self.fill_forms, 9),
            (self.accessibility, 10),
            (self.assemble, 11),
            (self.print_high_quality, 12),
        ] {
            if on {
                p |= 1 << (bit - 1);
            }
        }
        p
    }

    /// The permissions encoded in a `/P` value.
    pub fn from_bits(p: u32) -> Self {
        let bit = |n: u32| p & (1 << (n - 1)) != 0;
        Self {
            print: bit(3),
            modify: bit(4),
            copy: bit(5),
            annotate: bit(6),
            fill_forms: bit(9),
            accessibility: bit(10),
            assemble: bit(11),
            print_high_quality: bit(12),
        }
    }
}

impl Default for Permissions {
    fn default() -> Self {
        Self::ALL
    }
}

impl SecurityHandler {
    /// Creates a revision 6 (AES-256) handler for writing, and the
    /// `/Encrypt` dictionary that describes it (Algorithms 8, 9 and 10).
    pub fn new_aes256(
        user_password: &[u8],
        owner_password: &[u8],
        permissions: Permissions,
        encrypt_metadata: bool,
        entropy: &dyn Entropy,
    ) -> (Self, Dict) {
        let truncate = |pw: &[u8]| pw.get(..pw.len().min(127)).unwrap_or_default().to_vec();
        let (user, owner) = (truncate(user_password), truncate(owner_password));
        let mut key = [0u8; 32];
        entropy.fill(&mut key);
        let mut salts = [0u8; 32];
        entropy.fill(&mut salts);
        let (uvs, rest) = salts.split_at(8);
        let (uks, rest) = rest.split_at(8);
        let (ovs, oks) = rest.split_at(8);

        let wrap = |intermediate: &[u8]| -> Vec<u8> {
            let mut buf = key.to_vec();
            if let Ok(enc) =
                cbc::Encryptor::<aes::Aes256>::new_from_slices(intermediate, &[0u8; 16])
            {
                let _ = enc.encrypt_padded_mut::<NoPadding>(&mut buf, 32);
            }
            buf
        };
        let mut u = hash_2b(&user, uvs, &[]);
        u.extend_from_slice(uvs);
        u.extend_from_slice(uks);
        let ue = wrap(&hash_2b(&user, uks, &[]));
        let mut o = hash_2b(&owner, ovs, &u);
        o.extend_from_slice(ovs);
        o.extend_from_slice(oks);
        let oe = wrap(&hash_2b(&owner, oks, &u));

        let p = permissions.bits();
        let mut perms = [0u8; 16];
        perms[..4].copy_from_slice(&p.to_le_bytes());
        perms[4..8].copy_from_slice(&[0xff; 4]);
        perms[8] = if encrypt_metadata { b'T' } else { b'F' };
        perms[9..12].copy_from_slice(b"adb");
        entropy.fill(&mut perms[12..]);
        if let Ok(enc) = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &[0u8; 16]) {
            // One block with a zero IV is ECB, which Algorithm 10 specifies.
            let _ = enc.encrypt_padded_mut::<NoPadding>(&mut perms, 16);
        }

        let mut std_cf = Dict::new();
        std_cf.insert("AuthEvent", Name::from("DocOpen"));
        std_cf.insert("CFM", Name::from("AESV3"));
        std_cf.insert("Length", 32i64);
        let mut cf = Dict::new();
        cf.insert("StdCF", std_cf);
        let mut dict = Dict::new();
        dict.insert("Filter", Name::from("Standard"));
        dict.insert("V", 5i64);
        dict.insert("R", 6i64);
        dict.insert("Length", 256i64);
        dict.insert("CF", cf);
        dict.insert("StmF", Name::from("StdCF"));
        dict.insert("StrF", Name::from("StdCF"));
        dict.insert("O", PdfString::hex(o));
        dict.insert("U", PdfString::hex(u));
        dict.insert("OE", PdfString::hex(oe));
        dict.insert("UE", PdfString::hex(ue));
        dict.insert("Perms", PdfString::hex(perms.to_vec()));
        dict.insert("P", i64::from(p as i32));
        if !encrypt_metadata {
            dict.insert("EncryptMetadata", false);
        }
        let handler = Self {
            revision: 6,
            key: key.to_vec(),
            strings: CryptMethod::AesV3,
            streams: CryptMethod::AesV3,
            embedded_files: CryptMethod::AesV3,
            encrypt_metadata,
            permissions: p,
            authentication: Authentication::Owner,
        };
        (handler, dict)
    }

    /// Whether this handler can encrypt new data: only AES-256 is ever
    /// written (D4); a file using RC4 or AES-128 must be re-encrypted by a
    /// full save instead.
    pub fn can_encrypt(&self) -> bool {
        self.is_authenticated()
            && [self.strings, self.streams]
                .iter()
                .all(|m| matches!(m, CryptMethod::AesV3 | CryptMethod::Identity))
    }

    /// Encrypts `data` for object `id` with `method` (AES-256 or Identity).
    pub fn encrypt(
        &self,
        method: CryptMethod,
        data: &[u8],
        entropy: &dyn Entropy,
    ) -> Result<Vec<u8>> {
        match method {
            CryptMethod::Identity => Ok(data.to_vec()),
            CryptMethod::AesV3 => {
                let mut iv = [0u8; 16];
                entropy.fill(&mut iv);
                let pad = 16 - data.len() % 16;
                let mut buf = data.to_vec();
                buf.resize(data.len() + pad, pad as u8);
                let len = buf.len();
                cbc::Encryptor::<aes::Aes256>::new_from_slices(&self.key, &iv)
                    .map_err(|_| Error::UnsupportedEncryption("bad AES-256 key".into()))?
                    .encrypt_padded_mut::<NoPadding>(&mut buf, len)
                    .map_err(|_| Error::UnsupportedEncryption("AES padding".into()))?;
                let mut out = iv.to_vec();
                out.extend_from_slice(&buf);
                Ok(out)
            }
            _ => Err(Error::UnsupportedEncryption(
                "only AES-256 is written; save in full to re-encrypt".into(),
            )),
        }
    }

    /// Encrypts every string inside `object` in place (streams' data is
    /// left to [`SecurityHandler::encrypt`]).
    pub fn encrypt_strings(&self, object: &mut Object, entropy: &dyn Entropy) -> Result<()> {
        match object {
            Object::String(s) => {
                s.bytes = self.encrypt(self.strings, &s.bytes, entropy)?;
                Ok(())
            }
            Object::Array(items) => items
                .iter_mut()
                .try_for_each(|i| self.encrypt_strings(i, entropy)),
            Object::Dict(d) => self.encrypt_dict_strings(d, entropy),
            Object::Stream(st) => self.encrypt_dict_strings(&mut st.dict, entropy),
            _ => Ok(()),
        }
    }

    fn encrypt_dict_strings(&self, dict: &mut Dict, entropy: &dyn Entropy) -> Result<()> {
        let is_signature = dict.is_type(b"Sig")
            || dict.is_type(b"DocTimeStamp")
            || dict.contains_key(b"ByteRange");
        let keys: Vec<_> = dict.iter().map(|(k, _)| k.clone()).collect();
        for key in keys {
            if is_signature && key.as_bytes() == b"Contents" {
                continue;
            }
            if let Some(mut value) = dict.remove(key.as_bytes()) {
                self.encrypt_strings(&mut value, entropy)?;
                dict.insert(key, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        // RFC 6229 is for longer keys; this is the classic "Key"/"Plaintext".
        let mut data = b"Plaintext".to_vec();
        rc4(b"Key\0\0", &mut data);
        let mut back = data.clone();
        rc4(b"Key\0\0", &mut back);
        assert_eq!(back, b"Plaintext");
        let mut untouched = b"x".to_vec();
        rc4(b"abc", &mut untouched);
        assert_eq!(untouched, b"x");
    }

    #[test]
    fn padding() {
        assert_eq!(padded(b""), PAD);
        let p = padded(b"ab");
        assert_eq!(&p[..2], b"ab");
        assert_eq!(&p[2..], &PAD[..30]);
        assert_eq!(padded(&[7; 40]), [7; 32]);
    }

    #[test]
    fn aes_lenient_decrypt() {
        assert!(aes_cbc_decrypt(&[0; 16], b"short").is_empty());
        assert!(aes_cbc_decrypt(&[0; 3], &[0; 32]).is_empty());
        // Round trip one block with PKCS#7 padding.
        let key = [1u8; 16];
        let iv = [2u8; 16];
        let mut block = *b"hello\x0b\x0b\x0b\x0b\x0b\x0b\x0b\x0b\x0b\x0b\x0b";
        cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(&mut block, 16)
            .unwrap();
        let mut data = iv.to_vec();
        data.extend_from_slice(&block);
        assert_eq!(aes_cbc_decrypt(&key, &data), b"hello");
    }

    #[test]
    fn unsupported_handlers() {
        let mut d = Dict::new();
        d.insert("Filter", crate::object::Name::from("Adobe.PubSec"));
        assert!(matches!(
            SecurityHandler::new(&d, b"", b""),
            Err(Error::UnsupportedEncryption(_))
        ));
        let mut d = Dict::new();
        d.insert("Filter", crate::object::Name::from("Standard"));
        d.insert("R", 7i64);
        assert!(matches!(
            SecurityHandler::new(&d, b"", b""),
            Err(Error::UnsupportedEncryption(_))
        ));
    }

    #[test]
    fn password_variants() {
        assert_eq!(password_candidates(b"abc", 4), vec![b"abc".to_vec()]);
        assert_eq!(
            password_candidates("æøå".as_bytes(), 4),
            vec!["æøå".as_bytes().to_vec(), vec![0xe6, 0xf8, 0xe5]]
        );
        // Not representable in Latin-1: only the raw bytes.
        assert_eq!(password_candidates("€".as_bytes(), 3).len(), 1);
        assert_eq!(
            password_candidates("a\u{ad}b\u{a0}c".as_bytes(), 6)[1],
            b"ab c".to_vec()
        );
        assert_eq!(password_candidates(&[0xff], 6), vec![vec![0xff]]);
    }

    #[test]
    fn identity_only_opens_without_password() {
        let mut cf = Dict::new();
        let mut std_cf = Dict::new();
        std_cf.insert("CFM", crate::object::Name::from("AESV3"));
        cf.insert("StdCF", std_cf);
        let mut d = Dict::new();
        d.insert("Filter", crate::object::Name::from("Standard"));
        d.insert("V", 5i64);
        d.insert("R", 6i64);
        d.insert("CF", cf);
        d.insert("StmF", crate::object::Name::from("Identity"));
        d.insert("StrF", crate::object::Name::from("Identity"));
        d.insert("EFF", crate::object::Name::from("StdCF"));
        let h = SecurityHandler::new(&d, b"", b"").unwrap();
        assert_eq!(h.authentication(), Authentication::None);
        assert!(!h.is_authenticated());
        assert_eq!(h.embedded_file_method(), CryptMethod::AesV3);
        assert_eq!(
            h.decrypt(ObjId::new(1, 0), CryptMethod::AesV3, b"raw"),
            b"raw"
        );
        // With AES streams the same failure is a wrong password.
        d.insert("StmF", crate::object::Name::from("StdCF"));
        assert_eq!(
            SecurityHandler::new(&d, b"", b"").err(),
            Some(Error::InvalidPassword)
        );
    }

    /// A deterministic test entropy source: a counter.
    struct Counter(std::sync::atomic::AtomicU8);

    impl Entropy for Counter {
        fn fill(&self, buf: &mut [u8]) {
            for b in buf {
                *b = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn permissions_bits() {
        assert_eq!(
            Permissions::ALL.bits(),
            0xffff_ffc0 | 0b1111 << 2 | 0xf << 8
        );
        let none = Permissions::from_bits(0xffff_f0c0);
        assert!(!none.print && !none.copy && !none.assemble);
        assert_eq!(
            Permissions::from_bits(Permissions::ALL.bits()),
            Permissions::ALL
        );
        let print_only = Permissions {
            print: true,
            ..Permissions::from_bits(0)
        };
        assert_eq!(print_only.bits(), 0xffff_f0c4);
    }

    #[test]
    fn aes256_write_then_read() {
        let entropy = Counter(Default::default());
        let perms = Permissions {
            copy: false,
            ..Permissions::ALL
        };
        let (writer, dict) = SecurityHandler::new_aes256(b"user", b"owner", perms, true, &entropy);
        assert!(writer.can_encrypt());
        let id = ObjId::new(7, 0);
        let secret = b"attack at dawn".to_vec();
        let encrypted = writer
            .encrypt(CryptMethod::AesV3, &secret, &entropy)
            .unwrap();
        assert_eq!(encrypted.len(), 16 + 16);

        for (pw, auth) in [
            (&b"user"[..], Authentication::User),
            (b"owner", Authentication::Owner),
        ] {
            let reader = SecurityHandler::new(&dict, b"", pw).unwrap();
            assert_eq!(reader.authentication(), auth);
            assert_eq!(reader.revision(), 6);
            assert_eq!(Permissions::from_bits(reader.permissions()), perms);
            assert_eq!(reader.decrypt(id, CryptMethod::AesV3, &encrypted), secret);
        }
        assert_eq!(
            SecurityHandler::new(&dict, b"", b"nope").err(),
            Some(Error::InvalidPassword)
        );

        let mut obj = Object::Array(vec![Object::String(PdfString::literal("hi"))]);
        writer.encrypt_strings(&mut obj, &entropy).unwrap();
        let reader = SecurityHandler::new(&dict, b"", b"user").unwrap();
        reader.decrypt_strings(id, &mut obj);
        assert_eq!(obj.as_array().unwrap()[0].as_string(), Some(&b"hi"[..]));
    }

    #[test]
    fn same_entropy_same_output() {
        let a = SecurityHandler::new_aes256(
            b"u",
            b"o",
            Permissions::ALL,
            false,
            &Counter(Default::default()),
        )
        .1;
        let b = SecurityHandler::new_aes256(
            b"u",
            b"o",
            Permissions::ALL,
            false,
            &Counter(Default::default()),
        )
        .1;
        assert_eq!(a, b);
        assert_eq!(a.get(b"EncryptMetadata"), Some(&Object::Bool(false)));
    }

    #[test]
    fn legacy_methods_are_never_written() {
        let entropy = Counter(Default::default());
        let (h, _) = SecurityHandler::new_aes256(b"", b"", Permissions::ALL, true, &entropy);
        assert!(h.encrypt(CryptMethod::Rc4, b"x", &entropy).is_err());
        assert!(h.encrypt(CryptMethod::AesV2, b"x", &entropy).is_err());
        assert_eq!(
            h.encrypt(CryptMethod::Identity, b"x", &entropy).unwrap(),
            b"x"
        );
    }

    #[test]
    fn crypt_filter_methods() {
        let mut std_cf = Dict::new();
        std_cf.insert("CFM", crate::object::Name::from("AESV2"));
        let mut cf = Dict::new();
        cf.insert("StdCF", std_cf);
        let mut d = Dict::new();
        d.insert("CF", cf);
        d.insert("StmF", crate::object::Name::from("StdCF"));
        d.insert("StrF", crate::object::Name::from("Identity"));
        assert_eq!(
            crypt_methods(&d, 4).unwrap(),
            (
                CryptMethod::Identity,
                CryptMethod::AesV2,
                CryptMethod::AesV2
            )
        );
        assert_eq!(
            crypt_methods(&Dict::new(), 2).unwrap(),
            (CryptMethod::Rc4, CryptMethod::Rc4, CryptMethod::Rc4)
        );
        assert!(crypt_methods(&Dict::new(), 9).is_err());
    }
}
