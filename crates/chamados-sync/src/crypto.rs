//! Authenticated encryption of the synced document.
//!
//! Envelope: `MAGIC (5 bytes) | nonce (24 bytes) | ciphertext+tag`. The magic is bound to the
//! ciphertext as associated data, so a changed header or a changed byte fails to decrypt.

use chacha20poly1305::{
    aead::{Aead, Generate, KeyInit, Payload},
    Key as CipherKey, XChaCha20Poly1305, XNonce,
};

use crate::SyncError;

/// Length of the encryption key, in bytes.
pub const KEY_LEN: usize = 32;
const MAGIC: &[u8; 5] = b"CSYN1";
const NONCE_LEN: usize = 24;

/// The secret key that encrypts everything leaving the machine. It never leaves the user's devices.
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

impl std::fmt::Debug for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Key(..)")
    }
}

impl Key {
    /// A new random key.
    pub fn generate() -> Self {
        let key = CipherKey::generate();
        let mut bytes = [0; KEY_LEN];
        bytes.copy_from_slice(key.as_slice());
        Self(bytes)
    }

    /// The key from its raw bytes; they must be exactly [`KEY_LEN`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SyncError> {
        let bytes: [u8; KEY_LEN] = bytes.try_into().map_err(|_| {
            SyncError::Key(format!("a key has {KEY_LEN} bytes, got {}", bytes.len()))
        })?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// Lowercase hexadecimal form (64 characters), used to export and import the key.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// Parses the hexadecimal form; surrounding whitespace is ignored.
    pub fn from_hex(text: &str) -> Result<Self, SyncError> {
        let text = text.trim();
        if text.len() != KEY_LEN * 2 {
            return Err(SyncError::Key(format!(
                "expected {} hexadecimal characters",
                KEY_LEN * 2
            )));
        }
        Self::from_bytes(&from_hex_bytes(text)?)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&CipherKey::from(self.0))
    }
}

/// Lowercase hexadecimal form of `bytes`.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The bytes written by [`to_hex`] (either case); anything that is not hexadecimal is refused.
pub(crate) fn from_hex_bytes(text: &str) -> Result<Vec<u8>, SyncError> {
    let invalid = || SyncError::Key("expected hexadecimal text".to_owned());
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(invalid());
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).map_err(|_| invalid()))
        .collect()
}

/// Encrypts `plaintext` with a fresh random nonce.
pub fn encrypt(key: &Key, plaintext: &[u8]) -> Vec<u8> {
    let nonce = XNonce::generate();
    let payload = Payload {
        msg: plaintext,
        aad: MAGIC,
    };
    let ciphertext = key
        .cipher()
        .encrypt(&nonce, payload)
        .expect("encrypting an in-memory buffer cannot fail");
    let mut envelope = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(nonce.as_slice());
    envelope.extend_from_slice(&ciphertext);
    envelope
}

/// Decrypts an envelope made by [`encrypt`]; fails for a wrong key or any altered byte.
pub fn decrypt(key: &Key, envelope: &[u8]) -> Result<Vec<u8>, SyncError> {
    if envelope.len() < MAGIC.len() + NONCE_LEN || !envelope.starts_with(MAGIC) {
        return Err(SyncError::Crypto(
            "not an encrypted synchronization document".to_owned(),
        ));
    }
    let (nonce, ciphertext) = envelope[MAGIC.len()..].split_at(NONCE_LEN);
    let payload = Payload {
        msg: ciphertext,
        aad: MAGIC,
    };
    key.cipher()
        .decrypt(
            &XNonce::try_from(nonce).expect("split at the nonce length"),
            payload,
        )
        .map_err(|_| SyncError::Crypto("wrong key or corrupted data".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_uses_a_fresh_nonce_each_time() {
        let key = Key::generate();
        let first = encrypt(&key, b"titulo secreto");
        let second = encrypt(&key, b"titulo secreto");
        assert_ne!(first, second);
        assert!(!first.windows(7).any(|window| window == b"secreto"));
        assert_eq!(decrypt(&key, &first).unwrap(), b"titulo secreto");
        assert_eq!(decrypt(&key, &second).unwrap(), b"titulo secreto");
        assert_eq!(decrypt(&key, &encrypt(&key, b"")).unwrap(), b"");
    }

    #[test]
    fn detects_a_wrong_key_and_any_altered_byte() {
        let key = Key::generate();
        let envelope = encrypt(&key, b"conteudo");
        let wrong = decrypt(&Key::generate(), &envelope).unwrap_err();
        assert!(wrong.to_string().contains("wrong key or corrupted data"));
        for index in 0..envelope.len() {
            let mut altered = envelope.clone();
            altered[index] ^= 1;
            assert!(decrypt(&key, &altered).is_err(), "byte {index}");
        }
        assert!(decrypt(&key, &envelope[..10]).is_err());
        assert!(decrypt(
            &key,
            b"texto em claro qualquer, bem comprido para passar do tamanho"
        )
        .unwrap_err()
        .to_string()
        .contains("not an encrypted"));
    }

    #[test]
    fn keys_convert_to_and_from_hex_and_bytes() {
        let key = Key::generate();
        let hex = key.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Key::from_hex(&format!("  {hex}\n")).unwrap(), key);
        assert_eq!(Key::from_hex(&hex.to_uppercase()).unwrap(), key);
        assert_eq!(Key::from_bytes(key.as_bytes()).unwrap(), key);
        assert_eq!(format!("{key:?}"), "Key(..)");
        for bad in ["", "abc", &"zz".repeat(32), &"é".repeat(32)] {
            assert!(Key::from_hex(bad).is_err(), "{bad:?}");
        }
        assert!(Key::from_bytes(&[1, 2, 3]).is_err());
    }
}
