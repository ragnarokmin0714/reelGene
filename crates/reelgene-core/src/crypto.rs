//! HLS segment decryption. Only AES-128 (CBC/Pkcs7), which is what
//! `#EXT-X-KEY:METHOD=AES-128` means in practice. SAMPLE-AES and DRM are refused
//! earlier, while the playlist is parsed (see `hls::check_key`). Ported from
//! reelhold unchanged in behaviour.

use crate::error::{Error, Result};
use aes::Aes128;
use cbc::cipher::{
    BlockModeDecrypt, KeyIvInit,
    block_padding::{NoPadding, Pkcs7},
};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// Decrypt one segment in place; `buf` is truncated to the unpadded plaintext.
///
/// The last segment of a stream is the only one that legitimately carries
/// padding, but every block is unpadded the same way because encoders are
/// inconsistent about it.
pub fn decrypt_aes128(key: &[u8; 16], iv: &[u8; 16], buf: &mut Vec<u8>) -> Result<()> {
    if !buf.len().is_multiple_of(16) {
        return Err(Error::Decrypt(format!(
            "encrypted segment is {} bytes, not a multiple of 16",
            buf.len()
        )));
    }
    if buf.is_empty() {
        return Ok(());
    }
    let n = match Aes128CbcDec::new(key.into(), iv.into()).decrypt_padded::<Pkcs7>(buf) {
        Ok(pt) => pt.len(),
        Err(_) => {
            // Some packagers emit segments whose final block is not padded at all.
            // A raw block decrypt keeps those playable instead of failing the episode.
            Aes128CbcDec::new(key.into(), iv.into())
                .decrypt_padded::<NoPadding>(buf)
                .map_err(|_| Error::Decrypt("AES-128 decrypt failed (wrong key?)".into()))?
                .len()
        }
    };
    buf.truncate(n);
    Ok(())
}

/// The IV an unlabelled `#EXT-X-KEY` implies: the segment's media sequence
/// number as a 128-bit big-endian integer (RFC 8216 §5.2).
pub fn iv_from_sequence(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};

    type Enc = cbc::Encryptor<aes::Aes128>;

    fn encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; plain.len() + 16];
        let n = Enc::new(key.into(), iv.into())
            .encrypt_padded_b2b::<Pkcs7>(plain, &mut buf)
            .unwrap()
            .len();
        buf.truncate(n);
        buf
    }

    #[test]
    fn aes128_roundtrips_a_segment() {
        let (key, iv) = ([7u8; 16], [3u8; 16]);
        let plain: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let mut ct = encrypt(&key, &iv, &plain);
        decrypt_aes128(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plain);
    }

    #[test]
    fn exact_block_multiple_still_carries_a_full_pad_block() {
        let (key, iv) = ([1u8; 16], [2u8; 16]);
        let plain = vec![0xABu8; 64];
        let mut ct = encrypt(&key, &iv, &plain);
        assert_eq!(ct.len(), 80, "Pkcs7 always appends at least one block");
        decrypt_aes128(&key, &iv, &mut ct).unwrap();
        assert_eq!(ct, plain);
    }

    #[test]
    fn unpadded_ciphertext_falls_back_instead_of_failing() {
        let (key, iv) = ([9u8; 16], [0u8; 16]);
        let mut ct = encrypt(&key, &iv, &[0x5Au8; 48]);
        ct.truncate(48); // drop the pad block, leaving a valid block multiple
        assert!(decrypt_aes128(&key, &iv, &mut ct).is_ok());
        assert_eq!(ct.len(), 48);
    }

    #[test]
    fn non_block_sized_input_is_rejected() {
        let mut buf = vec![0u8; 17];
        assert!(decrypt_aes128(&[0; 16], &[0; 16], &mut buf).is_err());
    }

    #[test]
    fn implicit_iv_is_the_sequence_number_big_endian() {
        assert_eq!(iv_from_sequence(0), [0u8; 16]);
        let iv = iv_from_sequence(1);
        assert_eq!(iv[15], 1, "low byte last");
        assert_eq!(iv[..15], [0u8; 15]);
        assert_eq!(
            &iv_from_sequence(0x0102_0304_0506_0708)[8..],
            &[1, 2, 3, 4, 5, 6, 7, 8]
        );
    }
}
