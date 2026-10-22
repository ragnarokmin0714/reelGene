//! Just enough OpenPGP (RFC 9580) to check one kind of signature: a detached
//! version 4 signature over a binary file, made by a version 4 RSA key. That
//! is how yt-dlp signs the checksum list of each release (`tools.rs`).
//!
//! Only the packet format is read here. The cryptography (SHA-2, RSA
//! PKCS#1 v1.5) is aws-lc-rs, already in the build for TLS: a parser that is
//! wrong refuses good signatures, while hand-written RSA that is wrong
//! accepts bad ones.

use crate::error::{Error, Result};
use aws_lc_rs::{digest, signature};
use std::borrow::Cow;

/// The primary key of an exported OpenPGP key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    n: Vec<u8>,
    e: Vec<u8>,
    fingerprint: [u8; 20],
}

impl PublicKey {
    /// An exported key, armored (`-----BEGIN PGP PUBLIC KEY BLOCK-----`) or
    /// binary. Its primary key is the one used; subkeys are not read.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let data = dearmor_if_armored(data)?;
        let body = packets(&data)?
            .into_iter()
            .find_map(|(tag, body)| (tag == TAG_PUBLIC_KEY).then_some(body))
            .ok_or_else(|| bad("no public key in it"))?;
        let mut r = Reader(body);
        match r.u8()? {
            4 => {}
            v => return Err(bad(format!("a version {v} key; only version 4 is read"))),
        }
        r.take(4)?; // creation time
        match r.u8()? {
            RSA | RSA_SIGN => {}
            a => return Err(bad(format!("not an RSA key (algorithm {a})"))),
        }
        let n = r.mpi()?.to_vec();
        let e = r.mpi()?.to_vec();
        let len = u16::try_from(body.len()).map_err(|_| bad("a key packet too long"))?;
        let mut ctx = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
        ctx.update(&[0x99]);
        ctx.update(&len.to_be_bytes());
        ctx.update(body);
        let mut fingerprint = [0; 20];
        fingerprint.copy_from_slice(ctx.finish().as_ref());
        Ok(Self { n, e, fingerprint })
    }

    /// As GnuPG prints it: `AC0C BBE6 … 3B5A 7581`, ten groups of four.
    pub fn fingerprint(&self) -> String {
        let hex = hex::encode_upper(self.fingerprint);
        let groups: Vec<&str> = (0..10).map(|i| &hex[i * 4..i * 4 + 4]).collect();
        format!("{}  {}", groups[..5].join(" "), groups[5..].join(" "))
    }

    /// Check `sig`, a detached signature (binary or armored), over `data`.
    /// Returns when it was made, in Unix seconds.
    pub fn verify(&self, data: &[u8], sig: &[u8]) -> Result<u64> {
        let sig = dearmor_if_armored(sig)?;
        let body = packets(&sig)?
            .into_iter()
            .find_map(|(tag, body)| (tag == TAG_SIGNATURE).then_some(body))
            .ok_or_else(|| bad("no signature in it"))?;
        let mut r = Reader(body);
        match r.u8()? {
            4 => {}
            v => {
                return Err(bad(format!(
                    "a version {v} signature; only version 4 is read"
                )));
            }
        }
        match r.u8()? {
            0x00 => {}
            t => {
                return Err(bad(format!(
                    "a signature of type {t:#04x}, not over a file"
                )));
            }
        }
        match r.u8()? {
            RSA | RSA_SIGN => {}
            a => return Err(bad(format!("not an RSA signature (algorithm {a})"))),
        }
        let params = match r.u8()? {
            8 => &signature::RSA_PKCS1_2048_8192_SHA256,
            9 => &signature::RSA_PKCS1_2048_8192_SHA384,
            10 => &signature::RSA_PKCS1_2048_8192_SHA512,
            h => return Err(bad(format!("hash algorithm {h} is not accepted"))),
        };
        let hashed_len = usize::from(r.u16()?);
        let hashed = r.take(hashed_len)?;
        let unhashed_len = usize::from(r.u16()?);
        let unhashed = r.take(unhashed_len)?;
        r.take(2)?; // the hash's first two bytes: a quick check only, RSA decides
        let value = r.mpi()?;

        let mut created = None;
        let mut expires = None;
        for (kind, critical, content) in subpackets(hashed)? {
            match kind {
                2 => created = Some(be_u32(content)?),
                3 => expires = Some(be_u32(content)?),
                16 => self.check_issuer_id(content)?,
                33 => {
                    // Version 4 keys: a version byte, then 20 bytes.
                    if content.len() == 21 && content[0] == 4 && content[1..] != self.fingerprint {
                        return Err(other_key());
                    }
                }
                // What is not understood may not be ignored when it is critical.
                _ if critical => {
                    return Err(bad(format!(
                        "a critical part this check does not know ({kind})"
                    )));
                }
                _ => {}
            }
        }
        // Not covered by the signature, so only a hint: the wrong key.
        for (kind, _, content) in subpackets(unhashed)? {
            if kind == 16 {
                self.check_issuer_id(content)?;
            }
        }
        let created = created.ok_or_else(|| bad("a signature without its time"))?;
        if let Some(secs) = expires.filter(|s| *s > 0) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            if now > u64::from(created) + u64::from(secs) {
                return Err(bad("an expired signature"));
            }
        }

        // What was signed: the file, the signature's hashed part, a trailer.
        let signed_part = &body[..6 + hashed_len];
        let mut message = Vec::with_capacity(data.len() + signed_part.len() + 6);
        message.extend_from_slice(data);
        message.extend_from_slice(signed_part);
        message.extend_from_slice(&[4, 0xff]);
        message.extend_from_slice(&(signed_part.len() as u32).to_be_bytes());
        // The value lost its leading zeros on the way; RSA wants them back.
        let mut value_padded = vec![0; self.n.len().saturating_sub(value.len())];
        value_padded.extend_from_slice(value);
        signature::RsaPublicKeyComponents {
            n: &self.n[..],
            e: &self.e[..],
        }
        .verify(params, &message, &value_padded)
        .map_err(|_| Error::Invalid("the signature does not match".into()))?;
        Ok(u64::from(created))
    }

    fn check_issuer_id(&self, id: &[u8]) -> Result<()> {
        if id.len() == 8 && id != &self.fingerprint[12..] {
            return Err(other_key());
        }
        Ok(())
    }
}

const TAG_SIGNATURE: u8 = 2;
const TAG_PUBLIC_KEY: u8 = 6;
const RSA: u8 = 1;
const RSA_SIGN: u8 = 3;

fn bad(why: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("OpenPGP: {why}"))
}

fn other_key() -> Error {
    Error::Invalid("the signature was made by another key".into())
}

fn be_u32(b: &[u8]) -> Result<u32> {
    b.try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| bad("a time that is not four bytes"))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(bad("cut short"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().unwrap_or_default(),
        ))
    }

    fn u32(&mut self) -> Result<u32> {
        be_u32(self.take(4)?)
    }

    /// A big number: its length in bits, then its bytes.
    fn mpi(&mut self) -> Result<&'a [u8]> {
        let bits = usize::from(self.u16()?);
        self.take(bits.div_ceil(8))
    }

    /// The length format shared by new-format packets and subpackets.
    fn length(&mut self) -> Result<usize> {
        Ok(match self.u8()? {
            o @ 0..192 => usize::from(o),
            o @ 192..224 => ((usize::from(o) - 192) << 8) + usize::from(self.u8()?) + 192,
            255 => self.u32()? as usize,
            _ => return Err(bad("partial lengths are not read")),
        })
    }
}

/// Tag and body of each packet, in order.
fn packets(data: &[u8]) -> Result<Vec<(u8, &[u8])>> {
    let mut r = Reader(data);
    let mut out = Vec::new();
    while !r.0.is_empty() {
        let head = r.u8()?;
        if head & 0x80 == 0 {
            return Err(bad("not OpenPGP data"));
        }
        let (tag, len) = if head & 0x40 != 0 {
            (head & 0x3f, r.length()?)
        } else {
            let len = match head & 3 {
                0 => usize::from(r.u8()?),
                1 => usize::from(r.u16()?),
                2 => r.u32()? as usize,
                _ => return Err(bad("a packet of unknown length")),
            };
            ((head >> 2) & 0x0f, len)
        };
        out.push((tag, r.take(len)?));
    }
    Ok(out)
}

/// Type, critical bit and content of each subpacket.
fn subpackets(area: &[u8]) -> Result<Vec<(u8, bool, &[u8])>> {
    let mut r = Reader(area);
    let mut out = Vec::new();
    while !r.0.is_empty() {
        let len = r.length()?;
        let all = r.take(len)?;
        let (&kind, content) = all.split_first().ok_or_else(|| bad("an empty subpacket"))?;
        out.push((kind & 0x7f, kind & 0x80 != 0, content));
    }
    Ok(out)
}

fn dearmor_if_armored(data: &[u8]) -> Result<Cow<'_, [u8]>> {
    match std::str::from_utf8(data) {
        Ok(text) if text.trim_start().starts_with("-----BEGIN PGP ") => {
            dearmor(text).map(Cow::Owned)
        }
        _ => Ok(Cow::Borrowed(data)),
    }
}

/// The bytes inside ASCII armor, with its checksum checked when it has one.
fn dearmor(text: &str) -> Result<Vec<u8>> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("-----BEGIN PGP "))
        .skip(1)
        // Headers ("Comment: …") end at a blank line.
        .skip_while(|l| l.contains(": "))
        .skip_while(|l| l.is_empty());
    let mut body = String::new();
    let mut checksum = None;
    loop {
        let Some(line) = lines.next() else {
            return Err(bad("armor without its END line"));
        };
        if line.starts_with("-----END PGP ") {
            break;
        }
        // Data lines may end in '=', but only the checksum starts with it.
        match line.strip_prefix('=') {
            Some(sum) => checksum = Some(sum),
            None => body.push_str(line),
        }
    }
    let bytes = base64(&body)?;
    if let Some(sum) = checksum {
        let sum = base64(sum)?;
        let want = match sum[..] {
            [a, b, c] => u32::from_be_bytes([0, a, b, c]),
            _ => return Err(bad("an armor checksum that is not three bytes")),
        };
        if crc24(&bytes) != want {
            return Err(bad("the armor's checksum does not match"));
        }
    }
    Ok(bytes)
}

fn base64(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return Err(bad("armor that is not base64")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0x00B7_04CE;
    for &b in data {
        crc ^= u32::from(b) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4CFB;
            }
        }
    }
    crc & 0x00FF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_reads_what_it_should() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8h").unwrap(), b"hello!");
        assert!(base64("aGV*bG8=").is_err());
    }

    #[test]
    fn crc24_matches_the_standard_check_value() {
        // CRC-24/OPENPGP of "123456789".
        assert_eq!(crc24(b"123456789"), 0x21CF02);
    }

    #[test]
    fn armor_checksums_are_checked() {
        let good = "-----BEGIN PGP SIGNATURE-----\nComment: x\n\naGVsbG8=\n=";
        let sum = {
            let c = crc24(b"hello").to_be_bytes();
            let v = u32::from_be_bytes([0, c[1], c[2], c[3]]);
            let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            (0..4)
                .map(|i| alpha[((v >> (18 - 6 * i)) & 63) as usize] as char)
                .collect::<String>()
        };
        let armored = format!("{good}{sum}\n-----END PGP SIGNATURE-----\n");
        assert_eq!(dearmor(&armored).unwrap(), b"hello");
        let tampered = armored.replace("aGVsbG8=", "aGVsbE8=");
        assert!(dearmor(&tampered).is_err());
        assert!(dearmor("-----BEGIN PGP SIGNATURE-----\n\naGVsbG8=\n").is_err());
    }

    #[test]
    fn lengths_in_all_three_sizes() {
        assert_eq!(Reader(&[100]).length().unwrap(), 100);
        assert_eq!(Reader(&[192, 0]).length().unwrap(), 192);
        assert_eq!(Reader(&[0xC5, 0xFB]).length().unwrap(), 1723);
        assert_eq!(Reader(&[255, 0, 1, 0, 0]).length().unwrap(), 65536);
        assert!(Reader(&[224]).length().is_err(), "partial lengths");
    }

    #[test]
    fn rubbish_is_refused_not_panicked_on() {
        for data in [
            &b""[..],
            b"\x00",
            b"\x99\x01",
            b"\xC2\xFF\x00",
            b"\x89\x02\x37",
        ] {
            assert!(PublicKey::parse(data).is_err());
        }
    }
}
