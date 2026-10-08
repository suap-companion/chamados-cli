//! Encrypted synchronization of the local `chamados` data (ticket titles and profile settings).
//!
//! The pieces, from the bottom up:
//!
//! - [`document`]: the syncable document of each profile and its entry-by-entry merge;
//! - [`crypto`]: authenticated encryption, so a backend only ever stores ciphertext;
//! - [`keys`]: where the encryption key lives (system keyring, a `0600` file or an environment variable);
//! - [`backend`]: the transport interface and a directory backend;
//! - [`engine`]: one synchronization round (download, merge, upload, apply) and its single-instance lock.
//!
//! Sessions, passwords, cloud credentials and the key itself are never part of the document.

use thiserror::Error;

pub mod backend;
pub mod credentials;
pub mod crypto;
pub mod document;
pub mod engine;
#[cfg(test)]
mod fake_s3;
pub mod keys;
pub mod s3;
pub mod setup;
pub mod sigv4;

pub use backend::{Condition, DirectoryBackend, Object, PutOutcome, SyncBackend};
pub use credentials::S3Credentials;
pub use crypto::Key;
pub use document::{
    apply_merged, collect_local, merge, ApplyReport, ProfileDoc, SettingsEntry, SyncDocument,
};
pub use engine::{sync_once, SyncLock, SyncOptions, SyncReport, OBJECT_NAME};
pub use keys::{load_key, store_key, KeySource, KEY_ENV};
pub use s3::{S3Backend, S3Settings};
pub use setup::{
    backend_from, key_source_from, validate_settings, AnyBackend, DIRECTORY_BACKEND, S3_BACKEND,
};

/// Everything that can go wrong while synchronizing.
#[derive(Debug, Error)]
pub enum SyncError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid synced document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("encryption key: {0}")]
    Key(String),
    #[error("{0}")]
    Crypto(String),
    #[error("keyring: {0}")]
    Keyring(#[from] keyring_core::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("backend: {0}")]
    Backend(String),
    #[error("could not agree with the remote copy after several attempts")]
    Conflict,
    #[error(transparent)]
    Config(#[from] suap_core::SuapError),
    #[error(transparent)]
    Titles(#[from] chamados_core::TicketError),
}
