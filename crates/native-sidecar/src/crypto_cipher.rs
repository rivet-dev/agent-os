//! Pure-Rust (RustCrypto) AES cipher primitives backing guest `node:crypto`
//! `createCipheriv`/`createDecipheriv` and SubtleCrypto AES, replacing the prior
//! OpenSSL `Crypter`. Supports `aes-{128,192,256}-{cbc,ctr,gcm}` with streaming
//! `update`/`final`, PKCS#7 auto-padding (CBC), and AEAD aad/auth-tag handling
//! (GCM). Behaviour matches Node so the concatenation of every `update()` output
//! plus `final()` equals the reference ciphertext/plaintext byte-for-byte.
//!
//! This module is intentionally backend-agnostic (no OpenSSL, no host coupling)
//! so the same implementation can serve the native and wasm sidecars.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::{Aes128, Aes192, Aes256};
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::AesGcm;

const AES_BLOCK_LEN: usize = 16;

/// Error type for cipher operations. Mapped by the caller to a `SidecarError`.
#[derive(Debug)]
pub(crate) struct CipherError(pub(crate) String);

impl CipherError {
    fn new(message: impl Into<String>) -> Self {
        CipherError(message.into())
    }
}

type Result<T> = std::result::Result<T, CipherError>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AesMode {
    Cbc,
    Ctr,
    Gcm,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AesBits {
    A128,
    A192,
    A256,
}

impl AesBits {
    fn key_len(self) -> usize {
        match self {
            AesBits::A128 => 16,
            AesBits::A192 => 24,
            AesBits::A256 => 32,
        }
    }
}

/// Parse a Node cipher name (`aes-256-cbc`) into (bits, mode).
fn parse_algorithm(name: &str) -> Result<(AesBits, AesMode)> {
    let lower = name.to_ascii_lowercase();
    let (bits, mode) = match lower.as_str() {
        "aes-128-cbc" => (AesBits::A128, AesMode::Cbc),
        "aes-192-cbc" => (AesBits::A192, AesMode::Cbc),
        "aes-256-cbc" => (AesBits::A256, AesMode::Cbc),
        "aes-128-ctr" => (AesBits::A128, AesMode::Ctr),
        "aes-192-ctr" => (AesBits::A192, AesMode::Ctr),
        "aes-256-ctr" => (AesBits::A256, AesMode::Ctr),
        "aes-128-gcm" => (AesBits::A128, AesMode::Gcm),
        "aes-192-gcm" => (AesBits::A192, AesMode::Gcm),
        "aes-256-gcm" => (AesBits::A256, AesMode::Gcm),
        other => {
            return Err(CipherError::new(format!(
                "unsupported crypto cipher algorithm {other}"
            )))
        }
    };
    Ok((bits, mode))
}

/// Returns true when the algorithm is an AEAD (GCM) cipher.
pub(crate) fn is_aead(algorithm: &str) -> bool {
    algorithm.to_ascii_lowercase().ends_with("-gcm")
}

/// Default AEAD authentication tag length in bytes.
pub(crate) fn default_aead_tag_len() -> usize {
    AES_BLOCK_LEN
}

enum AesBlockCipher {
    A128(Aes128),
    A192(Aes192),
    A256(Aes256),
}

impl AesBlockCipher {
    fn new(bits: AesBits, key: &[u8]) -> Result<Self> {
        if key.len() != bits.key_len() {
            return Err(CipherError::new(format!(
                "Invalid key length: expected {} bytes, got {}",
                bits.key_len(),
                key.len()
            )));
        }
        Ok(match bits {
            AesBits::A128 => AesBlockCipher::A128(Aes128::new(GenericArray::from_slice(key))),
            AesBits::A192 => AesBlockCipher::A192(Aes192::new(GenericArray::from_slice(key))),
            AesBits::A256 => AesBlockCipher::A256(Aes256::new(GenericArray::from_slice(key))),
        })
    }

    fn encrypt_block(&self, block: &mut [u8; AES_BLOCK_LEN]) {
        let ga = GenericArray::from_mut_slice(block);
        match self {
            AesBlockCipher::A128(cipher) => cipher.encrypt_block(ga),
            AesBlockCipher::A192(cipher) => cipher.encrypt_block(ga),
            AesBlockCipher::A256(cipher) => cipher.encrypt_block(ga),
        }
    }

    fn decrypt_block(&self, block: &mut [u8; AES_BLOCK_LEN]) {
        let ga = GenericArray::from_mut_slice(block);
        match self {
            AesBlockCipher::A128(cipher) => cipher.decrypt_block(ga),
            AesBlockCipher::A192(cipher) => cipher.decrypt_block(ga),
            AesBlockCipher::A256(cipher) => cipher.decrypt_block(ga),
        }
    }
}

enum AesCtrStream {
    A128(ctr::Ctr128BE<Aes128>),
    A192(ctr::Ctr128BE<Aes192>),
    A256(ctr::Ctr128BE<Aes256>),
}

impl AesCtrStream {
    fn new(bits: AesBits, key: &[u8], iv: &[u8]) -> Result<Self> {
        if key.len() != bits.key_len() {
            return Err(CipherError::new(format!(
                "Invalid key length: expected {} bytes, got {}",
                bits.key_len(),
                key.len()
            )));
        }
        if iv.len() != AES_BLOCK_LEN {
            return Err(CipherError::new(format!(
                "Invalid IV length: expected {AES_BLOCK_LEN} bytes, got {}",
                iv.len()
            )));
        }
        let invalid = |_| CipherError::new("Invalid key/IV length for AES-CTR");
        Ok(match bits {
            AesBits::A128 => AesCtrStream::A128(
                ctr::Ctr128BE::<Aes128>::new_from_slices(key, iv).map_err(invalid)?,
            ),
            AesBits::A192 => AesCtrStream::A192(
                ctr::Ctr128BE::<Aes192>::new_from_slices(key, iv).map_err(invalid)?,
            ),
            AesBits::A256 => AesCtrStream::A256(
                ctr::Ctr128BE::<Aes256>::new_from_slices(key, iv).map_err(invalid)?,
            ),
        })
    }

    fn apply(&mut self, buf: &mut [u8]) {
        match self {
            AesCtrStream::A128(stream) => stream.apply_keystream(buf),
            AesCtrStream::A192(stream) => stream.apply_keystream(buf),
            AesCtrStream::A256(stream) => stream.apply_keystream(buf),
        }
    }
}

struct CbcState {
    cipher: AesBlockCipher,
    /// Previous ciphertext block used for chaining (initialised to the IV).
    chain: [u8; AES_BLOCK_LEN],
    /// Pending input bytes that have not yet formed a processable block.
    buffer: Vec<u8>,
    decrypt: bool,
    pad: bool,
}

enum CipherKind {
    Cbc(CbcState),
    Ctr(AesCtrStream),
    Gcm(GcmState),
}

struct GcmState {
    bits: AesBits,
    key: Vec<u8>,
    iv: Vec<u8>,
    aad: Vec<u8>,
    buffer: Vec<u8>,
    decrypt: bool,
    /// Caller-supplied authentication tag (decrypt only).
    auth_tag: Option<Vec<u8>>,
    tag_len: usize,
}

/// Streaming AES cipher session mirroring Node's `Cipheriv`/`Decipheriv`.
pub(crate) struct StreamCipherSession {
    kind: CipherKind,
}

impl StreamCipherSession {
    /// Construct a cipher session. `pad` controls PKCS#7 auto-padding for CBC
    /// (Node's `setAutoPadding`). `aad`/`auth_tag` apply to AEAD (GCM) modes.
    #[allow(clippy::too_many_arguments)] // mirrors Node's createCipheriv surface
    pub(crate) fn new(
        algorithm: &str,
        key: &[u8],
        iv: Option<&[u8]>,
        decrypt: bool,
        pad: bool,
        aad: Option<&[u8]>,
        auth_tag: Option<&[u8]>,
        tag_len: usize,
    ) -> Result<Self> {
        let (bits, mode) = parse_algorithm(algorithm)?;
        let kind = match mode {
            AesMode::Cbc => {
                let iv = iv.ok_or_else(|| CipherError::new("CBC cipher requires an IV"))?;
                if iv.len() != AES_BLOCK_LEN {
                    return Err(CipherError::new(format!(
                        "Invalid IV length: expected {AES_BLOCK_LEN} bytes, got {}",
                        iv.len()
                    )));
                }
                let mut chain = [0_u8; AES_BLOCK_LEN];
                chain.copy_from_slice(iv);
                CipherKind::Cbc(CbcState {
                    cipher: AesBlockCipher::new(bits, key)?,
                    chain,
                    buffer: Vec::new(),
                    decrypt,
                    pad,
                })
            }
            AesMode::Ctr => {
                let iv = iv.ok_or_else(|| CipherError::new("CTR cipher requires an IV"))?;
                CipherKind::Ctr(AesCtrStream::new(bits, key, iv)?)
            }
            AesMode::Gcm => {
                let iv = iv.ok_or_else(|| CipherError::new("GCM cipher requires an IV"))?;
                if key.len() != bits.key_len() {
                    return Err(CipherError::new(format!(
                        "Invalid key length: expected {} bytes, got {}",
                        bits.key_len(),
                        key.len()
                    )));
                }
                CipherKind::Gcm(GcmState {
                    bits,
                    key: key.to_vec(),
                    iv: iv.to_vec(),
                    aad: aad.map(<[u8]>::to_vec).unwrap_or_default(),
                    buffer: Vec::new(),
                    decrypt,
                    auth_tag: auth_tag.map(<[u8]>::to_vec),
                    tag_len: if tag_len == 0 {
                        default_aead_tag_len()
                    } else {
                        tag_len
                    },
                })
            }
        };
        Ok(StreamCipherSession { kind })
    }

    /// Process input, returning whatever output is ready (Node `update`).
    pub(crate) fn update(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        match &mut self.kind {
            CipherKind::Cbc(state) => Ok(cbc_update(state, data)),
            CipherKind::Ctr(stream) => {
                let mut output = data.to_vec();
                stream.apply(&mut output);
                Ok(output)
            }
            CipherKind::Gcm(state) => {
                state.buffer.extend_from_slice(data);
                Ok(Vec::new())
            }
        }
    }

    /// Flush the cipher (Node `final`). For AEAD encryption, the authentication
    /// tag is returned alongside the final output bytes.
    pub(crate) fn finalize(self) -> Result<CipherFinal> {
        match self.kind {
            CipherKind::Cbc(state) => Ok(CipherFinal {
                data: cbc_finalize(state)?,
                auth_tag: None,
            }),
            CipherKind::Ctr(_) => Ok(CipherFinal {
                data: Vec::new(),
                auth_tag: None,
            }),
            CipherKind::Gcm(state) => gcm_finalize(state),
        }
    }
}

/// Output of `finalize`, optionally carrying an AEAD auth tag.
pub(crate) struct CipherFinal {
    pub(crate) data: Vec<u8>,
    pub(crate) auth_tag: Option<Vec<u8>>,
}

fn cbc_update(state: &mut CbcState, data: &[u8]) -> Vec<u8> {
    state.buffer.extend_from_slice(data);
    let mut output = Vec::new();
    // For decryption with padding we must retain the final block so `finalize`
    // can strip PKCS#7; otherwise process every complete block now.
    let reserve = usize::from(state.decrypt && state.pad);
    while state.buffer.len() > reserve * AES_BLOCK_LEN
        && state.buffer.len() >= AES_BLOCK_LEN
        && state.buffer.len() - AES_BLOCK_LEN >= reserve * AES_BLOCK_LEN
    {
        let mut block = [0_u8; AES_BLOCK_LEN];
        block.copy_from_slice(&state.buffer[..AES_BLOCK_LEN]);
        let processed = cbc_process_block(state, block);
        output.extend_from_slice(&processed);
        state.buffer.drain(..AES_BLOCK_LEN);
    }
    output
}

fn cbc_process_block(state: &mut CbcState, mut block: [u8; AES_BLOCK_LEN]) -> [u8; AES_BLOCK_LEN] {
    if state.decrypt {
        let ciphertext = block;
        state.cipher.decrypt_block(&mut block);
        for (byte, chain) in block.iter_mut().zip(state.chain.iter()) {
            *byte ^= *chain;
        }
        state.chain = ciphertext;
        block
    } else {
        for (byte, chain) in block.iter_mut().zip(state.chain.iter()) {
            *byte ^= *chain;
        }
        state.cipher.encrypt_block(&mut block);
        state.chain = block;
        block
    }
}

fn cbc_finalize(mut state: CbcState) -> Result<Vec<u8>> {
    if state.decrypt {
        if state.pad {
            if state.buffer.len() != AES_BLOCK_LEN {
                return Err(CipherError::new(
                    "wrong final block length (bad decrypt input)",
                ));
            }
            let mut block = [0_u8; AES_BLOCK_LEN];
            block.copy_from_slice(&state.buffer);
            let plaintext = cbc_process_block(&mut state, block);
            strip_pkcs7(&plaintext)
        } else {
            if !state.buffer.is_empty() {
                return Err(CipherError::new(
                    "wrong final block length (bad decrypt input)",
                ));
            }
            Ok(Vec::new())
        }
    } else if state.pad {
        // PKCS#7: always emit one padded block, even when the remainder is empty.
        let remainder = state.buffer.len();
        let pad = AES_BLOCK_LEN - remainder;
        let mut block = [0_u8; AES_BLOCK_LEN];
        block[..remainder].copy_from_slice(&state.buffer);
        for byte in block.iter_mut().skip(remainder) {
            *byte = pad as u8;
        }
        Ok(cbc_process_block(&mut state, block).to_vec())
    } else {
        if !state.buffer.is_empty() {
            return Err(CipherError::new(
                "data not a multiple of block length (no padding)",
            ));
        }
        Ok(Vec::new())
    }
}

fn strip_pkcs7(block: &[u8]) -> Result<Vec<u8>> {
    let pad = *block
        .last()
        .ok_or_else(|| CipherError::new("empty block during unpad"))? as usize;
    if pad == 0 || pad > AES_BLOCK_LEN || pad > block.len() {
        return Err(CipherError::new("bad decrypt (invalid PKCS#7 padding)"));
    }
    if block[block.len() - pad..]
        .iter()
        .any(|&byte| byte as usize != pad)
    {
        return Err(CipherError::new("bad decrypt (invalid PKCS#7 padding)"));
    }
    Ok(block[..block.len() - pad].to_vec())
}

fn gcm_cipher(bits: AesBits, key: &[u8]) -> Result<GcmAead> {
    let invalid = |_| CipherError::new("Invalid key length for AES-GCM");
    Ok(match bits {
        AesBits::A128 => GcmAead::A128(Box::new(
            AesGcm::<Aes128, U12>::new_from_slice(key).map_err(invalid)?,
        )),
        AesBits::A192 => GcmAead::A192(Box::new(
            AesGcm::<Aes192, U12>::new_from_slice(key).map_err(invalid)?,
        )),
        AesBits::A256 => GcmAead::A256(Box::new(
            AesGcm::<Aes256, U12>::new_from_slice(key).map_err(invalid)?,
        )),
    })
}

enum GcmAead {
    A128(Box<AesGcm<Aes128, U12>>),
    A192(Box<AesGcm<Aes192, U12>>),
    A256(Box<AesGcm<Aes256, U12>>),
}

impl GcmAead {
    fn encrypt_detached(&self, nonce: &[u8], aad: &[u8], buffer: &mut [u8]) -> Result<Vec<u8>> {
        let nonce = GenericArray::from_slice(nonce);
        let tag = match self {
            GcmAead::A128(c) => c.encrypt_in_place_detached(nonce, aad, buffer),
            GcmAead::A192(c) => c.encrypt_in_place_detached(nonce, aad, buffer),
            GcmAead::A256(c) => c.encrypt_in_place_detached(nonce, aad, buffer),
        }
        .map_err(|_| CipherError::new("AEAD encryption failed"))?;
        Ok(tag.to_vec())
    }

    fn decrypt_detached(
        &self,
        nonce: &[u8],
        aad: &[u8],
        buffer: &mut [u8],
        tag: &[u8],
    ) -> Result<()> {
        let nonce = GenericArray::from_slice(nonce);
        let tag = GenericArray::from_slice(tag);
        match self {
            GcmAead::A128(c) => c.decrypt_in_place_detached(nonce, aad, buffer, tag),
            GcmAead::A192(c) => c.decrypt_in_place_detached(nonce, aad, buffer, tag),
            GcmAead::A256(c) => c.decrypt_in_place_detached(nonce, aad, buffer, tag),
        }
        .map_err(|_| CipherError::new("Unsupported state or unable to authenticate data"))
    }
}

fn gcm_finalize(state: GcmState) -> Result<CipherFinal> {
    if state.iv.len() != 12 {
        return Err(CipherError::new(format!(
            "GCM requires a 12-byte IV, got {}",
            state.iv.len()
        )));
    }
    let cipher = gcm_cipher(state.bits, &state.key)?;
    let mut buffer = state.buffer.clone();
    if state.decrypt {
        let tag = state
            .auth_tag
            .as_ref()
            .ok_or_else(|| CipherError::new("missing AEAD auth tag for decryption"))?;
        if tag.len() != default_aead_tag_len() {
            return Err(CipherError::new(format!(
                "GCM auth tag must be {} bytes, got {}",
                default_aead_tag_len(),
                tag.len()
            )));
        }
        cipher.decrypt_detached(&state.iv, &state.aad, &mut buffer, tag)?;
        Ok(CipherFinal {
            data: buffer,
            auth_tag: None,
        })
    } else {
        let mut tag = cipher.encrypt_detached(&state.iv, &state.aad, &mut buffer)?;
        tag.truncate(state.tag_len);
        Ok(CipherFinal {
            data: buffer,
            auth_tag: Some(tag),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt_all(
        algorithm: &str,
        key: &[u8],
        iv: &[u8],
        plaintext: &[u8],
    ) -> (Vec<u8>, Option<Vec<u8>>) {
        let mut session =
            StreamCipherSession::new(algorithm, key, Some(iv), false, true, None, None, 16)
                .unwrap();
        let mut out = session.update(plaintext).unwrap();
        let fin = session.finalize().unwrap();
        out.extend_from_slice(&fin.data);
        (out, fin.auth_tag)
    }

    fn decrypt_all(
        algorithm: &str,
        key: &[u8],
        iv: &[u8],
        ciphertext: &[u8],
        tag: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut session =
            StreamCipherSession::new(algorithm, key, Some(iv), true, true, None, tag, 16).unwrap();
        let mut out = session.update(ciphertext).unwrap();
        let fin = session.finalize().unwrap();
        out.extend_from_slice(&fin.data);
        out
    }

    #[test]
    fn aes_256_cbc_roundtrip() {
        let key = [7_u8; 32];
        let iv = [9_u8; 16];
        let plaintext = b"agentos-crypto-surface";
        let (ciphertext, _) = encrypt_all("aes-256-cbc", &key, &iv, plaintext);
        let recovered = decrypt_all("aes-256-cbc", &key, &iv, &ciphertext, None);
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn aes_256_cbc_matches_known_vector() {
        // NIST SP 800-38A F.2.5 CBC-AES256.Encrypt, first block.
        let key = [
            0x60, 0x3d, 0xeb, 0x10, 0x15, 0xca, 0x71, 0xbe, 0x2b, 0x73, 0xae, 0xf0, 0x85, 0x7d,
            0x77, 0x81, 0x1f, 0x35, 0x2c, 0x07, 0x3b, 0x61, 0x08, 0xd7, 0x2d, 0x98, 0x10, 0xa3,
            0x09, 0x14, 0xdf, 0xf4,
        ];
        let iv = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plaintext = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        // Disable padding so the single block maps 1:1 to the known ciphertext.
        let mut session =
            StreamCipherSession::new("aes-256-cbc", &key, Some(&iv), false, false, None, None, 16)
                .unwrap();
        let mut out = session.update(&plaintext).unwrap();
        out.extend_from_slice(&session.finalize().unwrap().data);
        assert_eq!(
            out,
            vec![
                0xf5, 0x8c, 0x4c, 0x04, 0xd6, 0xe5, 0xf1, 0xba, 0x77, 0x9e, 0xab, 0xfb, 0x5f, 0x7b,
                0xfb, 0xd6
            ]
        );
    }

    #[test]
    fn aes_128_ctr_roundtrip_chunked() {
        let key = [1_u8; 16];
        let iv = [2_u8; 16];
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        // Chunked update must equal a single update for stream ciphers.
        let mut session =
            StreamCipherSession::new("aes-128-ctr", &key, Some(&iv), false, true, None, None, 16)
                .unwrap();
        let mut ciphertext = session.update(&plaintext[..10]).unwrap();
        ciphertext.extend_from_slice(&session.update(&plaintext[10..]).unwrap());
        ciphertext.extend_from_slice(&session.finalize().unwrap().data);
        let recovered = decrypt_all("aes-128-ctr", &key, &iv, &ciphertext, None);
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn aes_256_gcm_roundtrip_with_tag() {
        let key = [3_u8; 32];
        let iv = [4_u8; 12];
        let plaintext = b"authenticated payload";
        let (ciphertext, tag) = encrypt_all("aes-256-gcm", &key, &iv, plaintext);
        let tag = tag.expect("gcm tag");
        assert_eq!(tag.len(), 16);
        let recovered = decrypt_all("aes-256-gcm", &key, &iv, &ciphertext, Some(&tag));
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn aes_256_gcm_rejects_tampered_tag() {
        let key = [3_u8; 32];
        let iv = [4_u8; 12];
        let (ciphertext, tag) = encrypt_all("aes-256-gcm", &key, &iv, b"payload");
        let mut tag = tag.unwrap();
        tag[0] ^= 0xff;
        let mut session = StreamCipherSession::new(
            "aes-256-gcm",
            &key,
            Some(&iv),
            true,
            true,
            None,
            Some(&tag),
            16,
        )
        .unwrap();
        session.update(&ciphertext).unwrap();
        assert!(session.finalize().is_err());
    }

    #[test]
    fn cbc_chunked_update_matches_single() {
        let key = [5_u8; 16];
        let iv = [6_u8; 16];
        let plaintext = b"0123456789abcdefghABCDEFGH"; // 26 bytes, spans blocks
        let (single, _) = encrypt_all("aes-128-cbc", &key, &iv, plaintext);

        let mut session =
            StreamCipherSession::new("aes-128-cbc", &key, Some(&iv), false, true, None, None, 16)
                .unwrap();
        let mut chunked = session.update(&plaintext[..5]).unwrap();
        chunked.extend_from_slice(&session.update(&plaintext[5..20]).unwrap());
        chunked.extend_from_slice(&session.update(&plaintext[20..]).unwrap());
        chunked.extend_from_slice(&session.finalize().unwrap().data);
        assert_eq!(chunked, single);
    }
}
