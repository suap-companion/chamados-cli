//! Access credentials of the S3-compatible storage.
//!
//! They are secrets: never written to `config.toml`, never synchronized, never printed. They come from
//! environment variables (for automation) or from the system keyring.

use crate::{keys::keyring_entry_named, SyncError};

/// Environment variable with the access key id.
pub const ACCESS_KEY_ENV: &str = "CHAMADOS_S3_ACCESS_KEY_ID";
/// Environment variable with the secret access key.
pub const SECRET_KEY_ENV: &str = "CHAMADOS_S3_SECRET_ACCESS_KEY";
const FALLBACK_ACCESS_KEY_ENV: &str = "AWS_ACCESS_KEY_ID";
const FALLBACK_SECRET_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
const KEYRING_USER: &str = "s3-credentials";

/// An access key id and its secret.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("S3Credentials(..)")
    }
}

impl S3Credentials {
    /// Parses two lines of text: the access key id, then the secret access key.
    pub fn parse(text: &str) -> Result<Self, SyncError> {
        let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
        match (lines.next(), lines.next(), lines.next()) {
            (Some(id), Some(secret), None) => Ok(Self {
                access_key_id: id.to_owned(),
                secret_access_key: secret.to_owned(),
            }),
            _ => Err(SyncError::Key(
                "expected two lines: the access key id, then the secret access key".to_owned(),
            )),
        }
    }

    /// From the environment: `CHAMADOS_S3_*`, or else the standard `AWS_*` variables.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let pick = |own: &str, standard: &str| {
            env(own).or_else(|| env(standard)).filter(|v| !v.is_empty())
        };
        Some(Self {
            access_key_id: pick(ACCESS_KEY_ENV, FALLBACK_ACCESS_KEY_ENV)?,
            secret_access_key: pick(SECRET_KEY_ENV, FALLBACK_SECRET_KEY_ENV)?,
        })
    }

    /// The credentials from the environment or, failing that, from the keyring.
    pub fn load() -> Result<Option<Self>, SyncError> {
        Self::load_with_env(&|name| std::env::var(name).ok())
    }

    /// Like [`S3Credentials::load`], with the environment lookup injected (for tests).
    pub fn load_with_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, SyncError> {
        if let Some(found) = Self::from_env(env) {
            return Ok(Some(found));
        }
        match keyring_entry_named(KEYRING_USER)?.get_secret() {
            Err(keyring_core::Error::NoEntry) => Ok(None),
            found => Ok(Some(Self::parse(&String::from_utf8_lossy(&found?))?)),
        }
    }

    /// Saves the credentials in the keyring.
    pub fn store(&self) -> Result<(), SyncError> {
        let text = format!("{}\n{}", self.access_key_id, self.secret_access_key);
        Ok(keyring_entry_named(KEYRING_USER)?.set_secret(text.as_bytes())?)
    }

    /// A description that is safe to print: the length of each value and whether it looks malformed
    /// (anything but visible ASCII, or quotes), never the values themselves.
    pub fn describe(&self) -> String {
        let suspicious = |value: &str| {
            !value.chars().all(|c| c.is_ascii_graphic()) || value.contains(['"', '\''])
        };
        let mut text = format!(
            "Access Key ID: {} caracteres; Secret: {} caracteres",
            self.access_key_id.chars().count(),
            self.secret_access_key.chars().count()
        );
        if suspicious(&self.access_key_id) || suspicious(&self.secret_access_key) {
            text.push_str("; atenção: há espaços, aspas ou caracteres não ASCII nos valores");
        }
        text
    }

    /// `text` with both values hidden, so error messages can echo what a server said safely.
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.secret_access_key, "***")
            .replace(&self.access_key_id, "***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> S3Credentials {
        S3Credentials {
            access_key_id: "AKIA123".to_owned(),
            secret_access_key: "s3cr3t/valor".to_owned(),
        }
    }

    #[test]
    fn parses_two_lines_and_rejects_anything_else() {
        assert_eq!(
            S3Credentials::parse("AKIA123\ns3cr3t/valor\n").unwrap(),
            credentials()
        );
        assert_eq!(
            S3Credentials::parse("\n  AKIA123  \r\n\n s3cr3t/valor \n").unwrap(),
            credentials()
        );
        for bad in ["", "so-uma-linha", "a\nb\nc"] {
            assert!(S3Credentials::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn describes_the_credentials_without_revealing_them() {
        let text = credentials().describe();
        assert_eq!(text, "Access Key ID: 7 caracteres; Secret: 12 caracteres");
        assert!(!text.contains("AKIA") && !text.contains("s3cr3t"));
        for (id, secret) in [("AKIA 1", "ok"), ("ok", "\"quoted\""), ("ok", "açúcar")] {
            let odd = S3Credentials {
                access_key_id: id.to_owned(),
                secret_access_key: secret.to_owned(),
            };
            assert!(odd.describe().contains("atenção"), "{id} {secret}");
        }
    }

    #[test]
    fn reads_the_environment_preferring_the_chamados_names() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let own = env(&[
            (ACCESS_KEY_ENV, "own-id"),
            (SECRET_KEY_ENV, "own-secret"),
            ("AWS_ACCESS_KEY_ID", "aws-id"),
        ]);
        let found = S3Credentials::from_env(&own).unwrap();
        assert_eq!(
            (
                found.access_key_id.as_str(),
                found.secret_access_key.as_str()
            ),
            ("own-id", "own-secret")
        );
        let aws = env(&[
            ("AWS_ACCESS_KEY_ID", "aws-id"),
            ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
        ]);
        assert_eq!(
            S3Credentials::from_env(&aws).unwrap().access_key_id,
            "aws-id"
        );
        assert!(S3Credentials::from_env(&env(&[(ACCESS_KEY_ENV, "so-o-id")])).is_none());
        assert!(
            S3Credentials::from_env(&env(&[(ACCESS_KEY_ENV, ""), (SECRET_KEY_ENV, "x")])).is_none()
        );
        let loaded = S3Credentials::load_with_env(&aws).unwrap();
        assert_eq!(loaded.unwrap().secret_access_key, "aws-secret");
    }

    /// One test on purpose: it changes the process-wide default keyring store.
    #[test]
    fn falls_back_to_the_keyring_and_never_prints_the_secret() {
        let _guard = crate::keys::KEYRING_TEST_LOCK.lock().unwrap();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let nothing = |_: &str| None;
        assert_eq!(S3Credentials::load_with_env(&nothing).unwrap(), None);
        credentials().store().unwrap();
        assert_eq!(
            S3Credentials::load_with_env(&nothing).unwrap(),
            Some(credentials())
        );
        let loaded = S3Credentials::load().unwrap();
        assert!(loaded.is_some());

        assert_eq!(format!("{:?}", credentials()), "S3Credentials(..)");
        let echoed = credentials().redact("erro: chave AKIA123 com segredo s3cr3t/valor recusada");
        assert_eq!(echoed, "erro: chave *** com segredo *** recusada");
    }
}
