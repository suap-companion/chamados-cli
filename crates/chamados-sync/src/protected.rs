//! A key file protected by a passphrase.
//!
//! The passphrase goes through Argon2id (memory-hard, so guessing it is expensive) to derive a key
//! that seals the real encryption key with the same authenticated encryption as the synced document.
//! File layout, three text lines: `CSYNK1`, the salt in hexadecimal, the sealed key in hexadecimal.
//!
//! It suits interactive use on machines without a system keyring. It does not suit automation:
//! `cron` cannot type a passphrase (use the `CHAMADOS_SYNC_PASSPHRASE` variable or a plain key file).

use argon2::{Algorithm, Argon2, Params, Version};

use crate::{
    crypto::{decrypt, encrypt, from_hex_bytes, to_hex, Key, KEY_LEN},
    SyncError,
};

const HEADER: &str = "CSYNK1";
const SALT_LEN: usize = 16;
// OWASP's minimum Argon2id profile: 19 MiB of memory, 2 passes, 1 lane.
const MEMORY_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const LANES: u32 = 1;

/// The key that seals the real one, derived from the passphrase and the salt.
fn derive(passphrase: &str, salt: &[u8]) -> Key {
    let params = Params::new(MEMORY_KIB, PASSES, LANES, Some(KEY_LEN)).expect("valid parameters");
    let mut derived = [0; KEY_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut derived)
        .expect("a salt of 16 bytes and an output of 32 bytes are accepted");
    Key::from_bytes(&derived).expect("the derived key has the key length")
}

/// The text of a protected key file holding `key`.
pub fn seal(key: &Key, passphrase: &str) -> Result<String, SyncError> {
    if passphrase.is_empty() {
        return Err(SyncError::Key("the passphrase cannot be empty".to_owned()));
    }
    // A random key doubles as a source of random bytes for the salt.
    let salt = Key::generate().as_bytes()[..SALT_LEN].to_vec();
    let sealed = encrypt(&derive(passphrase, &salt), key.as_bytes());
    Ok(format!(
        "{HEADER}\n{}\n{}\n",
        to_hex(&salt),
        to_hex(&sealed)
    ))
}

/// The key inside a protected key file; fails for a wrong passphrase or a damaged file.
pub fn unseal(text: &str, passphrase: &str) -> Result<Key, SyncError> {
    let mut lines = text.lines().map(str::trim);
    let (Some(HEADER), Some(salt), Some(sealed)) = (lines.next(), lines.next(), lines.next())
    else {
        return Err(SyncError::Key(
            "not a passphrase-protected key file".to_owned(),
        ));
    };
    let salt = from_hex_bytes(salt)?;
    let sealed = from_hex_bytes(sealed)?;
    if salt.len() != SALT_LEN {
        return Err(SyncError::Key("damaged key file (salt)".to_owned()));
    }
    let bytes = decrypt(&derive(passphrase, &salt), &sealed)
        .map_err(|_| SyncError::Key("wrong passphrase or damaged key file".to_owned()))?;
    Key::from_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_key_opens_only_with_its_passphrase() {
        let key = Key::generate();
        let text = seal(&key, "frase longa e secreta").unwrap();
        assert!(text.starts_with("CSYNK1\n"));
        assert!(!text.contains(&key.to_hex()));
        assert_eq!(unseal(&text, "frase longa e secreta").unwrap(), key);
        // A new salt each time: the same key and passphrase never give the same file.
        assert_ne!(text, seal(&key, "frase longa e secreta").unwrap());

        let wrong = unseal(&text, "outra frase").unwrap_err().to_string();
        assert!(wrong.contains("wrong passphrase"), "{wrong}");
        assert!(unseal(&text, "").is_err());
        assert!(seal(&key, "")
            .unwrap_err()
            .to_string()
            .contains("cannot be empty"));
    }

    #[test]
    fn damaged_files_are_reported() {
        let key = Key::generate();
        let text = seal(&key, "frase").unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let broken = [
            String::new(),
            "CSYNK2\nab\ncd\n".to_owned(),
            format!("{}\n{}\n", lines[0], lines[1]),
            format!("{}\nzz\n{}\n", lines[0], lines[2]),
            format!("{}\n{}\nzz\n", lines[0], lines[1]),
            format!("{}\nabcd\n{}\n", lines[0], lines[2]),
        ];
        for text in broken {
            assert!(unseal(&text, "frase").is_err(), "{text:?}");
        }
        // Any altered byte of the sealed key is detected.
        let mut sealed = from_hex_bytes(lines[2]).unwrap();
        sealed[40] ^= 1;
        let altered = format!("{}\n{}\n{}\n", lines[0], lines[1], to_hex(&sealed));
        assert!(unseal(&altered, "frase").is_err());
    }
}
