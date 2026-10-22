//! The signature check against real releases and against signatures made
//! here, good and bad.

mod support;

use reelgene_core::pgp::PublicKey;
use support::signer::{Sig, TestKey};

const YT_DLP_KEY: &[u8] = include_bytes!("../src/yt-dlp.asc");
const SUMS: &[u8] = include_bytes!("data/SHA2-256SUMS");
const SUMS_SIG: &[u8] = include_bytes!("data/SHA2-256SUMS.sig");

#[test]
fn a_real_release_checks_out_and_a_changed_one_does_not() {
    let key = PublicKey::parse(YT_DLP_KEY).unwrap();
    assert_eq!(
        key.fingerprint(),
        "AC0C BBE6 848D 6A87 3464  AF4E 57CF 6593 3B5A 7581"
    );
    let signed = key.verify(SUMS, SUMS_SIG).unwrap();
    assert_eq!(signed, 1_792_619_636, "2026-10-21, when it was released");

    let mut changed = SUMS.to_vec();
    changed[0] ^= 1;
    let e = key.verify(&changed, SUMS_SIG).unwrap_err();
    assert!(e.to_string().contains("does not match"), "{e}");

    let mut sig = SUMS_SIG.to_vec();
    let last = sig.len() - 1;
    sig[last] ^= 1;
    assert!(key.verify(SUMS, &sig).is_err());
}

#[test]
fn another_key_is_refused() {
    let theirs = PublicKey::parse(YT_DLP_KEY).unwrap();
    let ours = TestKey::new();
    let e = theirs.verify(b"data", &ours.sign(b"data")).unwrap_err();
    assert!(e.to_string().contains("another key"), "{e}");

    // Without an issuer to tell, it is the signature itself that fails.
    let anonymous = ours.sign_with(b"data", Sig::default());
    assert!(theirs.verify(b"data", &anonymous).is_err());
}

#[test]
fn signatures_made_here_check_out() {
    let k = TestKey::new();
    let key = PublicKey::parse(&k.public).unwrap();
    for hash in [8, 10] {
        let sig = k.sign_with(
            b"hello",
            Sig {
                hash,
                issuer: Some(k.fingerprint),
                ..Sig::default()
            },
        );
        assert_eq!(key.verify(b"hello", &sig).unwrap(), 1_792_000_000);
        assert!(key.verify(b"hellO", &sig).is_err());
    }
}

#[test]
fn weak_or_unknown_parts_are_refused() {
    let k = TestKey::new();
    let key = PublicKey::parse(&k.public).unwrap();
    let refused = |sig: Sig, why: &str| {
        let e = key.verify(b"x", &k.sign_with(b"x", sig)).unwrap_err();
        assert!(e.to_string().contains(why), "{e}");
    };
    refused(
        Sig {
            hash: 2, // SHA-1
            ..Sig::default()
        },
        "hash algorithm 2",
    );
    refused(
        Sig {
            kind: 1, // text, with line endings normalised
            ..Sig::default()
        },
        "type 0x01",
    );
    refused(
        Sig {
            extra: vec![(0x80 | 99, vec![1])],
            ..Sig::default()
        },
        "critical",
    );
    refused(
        Sig {
            created: 1_000_000_000,
            extra: vec![(3, 60u32.to_be_bytes().to_vec())],
            ..Sig::default()
        },
        "expired",
    );
    // The same unknown part, not critical, is fine.
    let sig = k.sign_with(
        b"x",
        Sig {
            extra: vec![(99, vec![1])],
            ..Sig::default()
        },
    );
    key.verify(b"x", &sig).unwrap();
}

#[test]
fn an_armored_signature_and_a_binary_key_both_read() {
    let k = TestKey::new();
    let sig = k.sign(b"x");
    let b64 = {
        let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in sig.chunks(3) {
            let mut b = [0u8; 3];
            b[..chunk.len()].copy_from_slice(chunk);
            let v = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            for i in 0..=chunk.len() {
                s.push(alpha[((v >> (18 - 6 * i)) & 63) as usize] as char);
            }
            s.extend(std::iter::repeat_n('=', 3 - chunk.len()));
        }
        s
    };
    let lines: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    let armored = format!(
        "-----BEGIN PGP SIGNATURE-----\n\n{}\n-----END PGP SIGNATURE-----\n",
        lines.join("\n")
    );
    let key = PublicKey::parse(&k.public).unwrap();
    key.verify(b"x", armored.as_bytes()).unwrap();
}
