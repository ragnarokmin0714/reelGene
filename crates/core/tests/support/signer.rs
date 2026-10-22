//! An OpenPGP signer for tests: a fresh RSA key, exported and used the way
//! GnuPG would, so `pgp.rs` can be tried on signatures it has never seen and
//! the yt-dlp checks on releases that never existed.

use aws_lc_rs::digest;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{self, KeyPair as _};

pub struct TestKey {
    pair: KeyPair,
    /// The exported public key, binary.
    pub public: Vec<u8>,
    pub fingerprint: [u8; 20],
}

/// How to sign; `Sig::default()` is what GnuPG does today.
#[derive(Clone)]
pub struct Sig {
    /// 8 SHA-256, 10 SHA-512 (others are refused by the checker).
    pub hash: u8,
    pub kind: u8,
    pub created: u32,
    /// The issuer's fingerprint in the hashed part; `None` leaves it out.
    pub issuer: Option<[u8; 20]>,
    /// More hashed subpackets: type byte (with the critical bit) and content.
    pub extra: Vec<(u8, Vec<u8>)>,
}

impl Default for Sig {
    fn default() -> Self {
        Self {
            hash: 10,
            kind: 0,
            created: 1_792_000_000,
            issuer: None,
            extra: Vec::new(),
        }
    }
}

fn mpi(v: &[u8]) -> Vec<u8> {
    let start = v.iter().position(|b| *b != 0).unwrap_or(v.len() - 1);
    let v = &v[start..];
    let bits = (v.len() * 8 - v[0].leading_zeros() as usize) as u16;
    let mut out = bits.to_be_bytes().to_vec();
    out.extend_from_slice(v);
    out
}

/// An old-format packet with a two-byte length, as GnuPG writes them.
fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | (tag << 2) | 1];
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

impl TestKey {
    pub fn new() -> Self {
        let pair = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let pk = PublicKeyComponents::<Vec<u8>>::from(pair.public_key());
        let mut body = vec![4, 0x65, 0, 0, 0, 1];
        body.extend(mpi(&pk.n));
        body.extend(mpi(&pk.e));
        let public = packet(6, &body);
        let mut fingerprint = [0; 20];
        // The fingerprint hashes exactly what the packet is: 0x99, length, body.
        fingerprint
            .copy_from_slice(digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &public).as_ref());
        Self {
            pair,
            public,
            fingerprint,
        }
    }

    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.sign_with(
            data,
            Sig {
                issuer: Some(self.fingerprint),
                ..Sig::default()
            },
        )
    }

    pub fn sign_with(&self, data: &[u8], sig: Sig) -> Vec<u8> {
        let mut hashed = vec![5, 2];
        hashed.extend(sig.created.to_be_bytes());
        if let Some(fpr) = sig.issuer {
            hashed.extend([22, 33, 4]);
            hashed.extend(fpr);
        }
        for (kind, content) in &sig.extra {
            hashed.push(content.len() as u8 + 1);
            hashed.push(*kind);
            hashed.extend(content);
        }
        let mut head = vec![4, sig.kind, 1, sig.hash];
        head.extend((hashed.len() as u16).to_be_bytes());
        head.extend(&hashed);
        let mut message = data.to_vec();
        message.extend(&head);
        message.extend([4, 0xff]);
        message.extend((head.len() as u32).to_be_bytes());
        let (encoding, algorithm) = match sig.hash {
            8 => (&signature::RSA_PKCS1_SHA256, &digest::SHA256),
            _ => (&signature::RSA_PKCS1_SHA512, &digest::SHA512),
        };
        let mut value = vec![0; self.pair.public_modulus_len()];
        self.pair
            .sign(encoding, &SystemRandom::new(), &message, &mut value)
            .unwrap();
        let mut body = head;
        body.extend([0, 0]); // no unhashed subpackets
        body.extend(&digest::digest(algorithm, &message).as_ref()[..2]);
        body.extend(mpi(&value));
        packet(2, &body)
    }
}
