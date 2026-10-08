//! S3-compatible object storage (Cloudflare R2, AWS S3, MinIO, Backblaze B2...), over HTTPS.
//!
//! Requests are signed with SigV4 and use path-style addressing (`<endpoint>/<bucket>/<key>`). Only
//! plain `GET`, `PUT` and `DELETE` of one object are used: no ACLs, no presigned URLs, no listing, so
//! nothing here can make an object public or hand it to somebody else.

use std::time::SystemTime;

use reqwest::{Method, StatusCode, Url};
use suap_core::SyncSettings;

use crate::{
    backend::{version_of, Condition, Object, PutOutcome, SyncBackend},
    credentials::S3Credentials,
    sigv4::{amz_date, authorization, encode_path, sha256_hex, Signer},
    SyncError,
};

/// Region assumed when none is configured (what Cloudflare R2 expects).
pub const DEFAULT_REGION: &str = "auto";
const ERROR_SNIPPET_CHARS: usize = 200;

/// Where the objects live, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Settings {
    pub endpoint: Url,
    pub region: String,
    pub bucket: String,
    /// Key prefix, without leading or trailing slashes (may be empty).
    pub prefix: String,
    /// Whether the storage honors `If-Match` / `If-None-Match` on writes.
    pub conditional_writes: bool,
}

fn is_loopback(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// The `Host` header value for `url`: the host, plus the port when it is not the scheme's default.
fn authority(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

fn valid_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 255
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl S3Settings {
    /// Reads and validates the `[sync]` settings of an `s3` backend.
    ///
    /// The endpoint must be HTTPS (plain HTTP is only accepted for a loopback address, to test against
    /// a local MinIO) and cannot carry credentials, a query or a fragment.
    pub fn from_settings(settings: &SyncSettings) -> Result<Self, SyncError> {
        let missing = |what: &str| SyncError::Backend(format!("the s3 backend needs {what}"));
        let endpoint = settings
            .endpoint
            .as_deref()
            .ok_or_else(|| missing("an endpoint"))?;
        let endpoint = Url::parse(endpoint)
            .map_err(|error| SyncError::Backend(format!("invalid endpoint: {error}")))?;
        let bucket = settings.bucket.clone().ok_or_else(|| missing("a bucket"))?;

        let secure =
            endpoint.scheme() == "https" || (endpoint.scheme() == "http" && is_loopback(&endpoint));
        if !secure || endpoint.host_str().is_none() {
            return Err(SyncError::Backend(
                "the endpoint must use https (http is only accepted for localhost)".to_owned(),
            ));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(SyncError::Backend(
                "the endpoint must not contain credentials, a query or a fragment".to_owned(),
            ));
        }
        if !valid_label(&bucket) {
            return Err(SyncError::Backend(format!(
                "invalid bucket name {bucket:?}"
            )));
        }
        let region = settings
            .region
            .clone()
            .unwrap_or_else(|| DEFAULT_REGION.to_owned());
        if !valid_label(&region) {
            return Err(SyncError::Backend(format!("invalid region {region:?}")));
        }
        let prefix = settings
            .prefix
            .as_deref()
            .unwrap_or_default()
            .trim_matches('/')
            .to_owned();
        if prefix
            .split('/')
            .any(|part| part == "." || part == ".." || part.contains(['\\', '?', '#']))
        {
            return Err(SyncError::Backend(format!("invalid prefix {prefix:?}")));
        }
        Ok(Self {
            endpoint,
            region,
            bucket,
            prefix,
            conditional_writes: settings.conditional_writes.unwrap_or(true),
        })
    }
}

/// An S3-compatible bucket used as the synchronization storage.
pub struct S3Backend {
    client: reqwest::Client,
    settings: S3Settings,
    credentials: S3Credentials,
    now: fn() -> SystemTime,
}

impl S3Backend {
    pub fn new(settings: S3Settings, credentials: S3Credentials) -> Self {
        Self::with_clock(settings, credentials, SystemTime::now)
    }

    /// Like [`S3Backend::new`], with the clock used for the signature date injected (for tests).
    pub fn with_clock(
        settings: S3Settings,
        credentials: S3Credentials,
        now: fn() -> SystemTime,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            settings,
            credentials,
            now,
        }
    }

    /// The key of object `name` inside the bucket (prefix included).
    fn key(&self, name: &str) -> Result<String, SyncError> {
        if name.is_empty() || name.contains(['/', '\\', '?', '#']) || name.starts_with('.') {
            return Err(SyncError::Backend(format!("invalid object name {name:?}")));
        }
        Ok(if self.settings.prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.settings.prefix)
        })
    }

    /// Sends one signed request; `extra` are additional signed headers (lowercase names).
    async fn send(
        &self,
        method: Method,
        name: &str,
        body: &[u8],
        extra: &[(&str, String)],
    ) -> Result<reqwest::Response, SyncError> {
        let key = self.key(name)?;
        let path = format!("{}/{}", self.settings.bucket, key);
        let mut url = self.settings.endpoint.clone();
        let base = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base}/{path}"));

        let authority = authority(&url);
        let date = amz_date((self.now)());
        let payload_hash = sha256_hex(body);
        let mut headers = vec![
            ("host".to_owned(), authority),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), date.clone()),
        ];
        headers.extend(
            extra
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone())),
        );
        headers.sort();

        let signer = Signer {
            access_key_id: &self.credentials.access_key_id,
            secret_access_key: &self.credentials.secret_access_key,
            region: &self.settings.region,
        };
        let signed = authorization(
            &signer,
            method.as_str(),
            &encode_path(url.path()),
            &headers,
            &payload_hash,
            &date,
        );

        let mut request = self
            .client
            .request(method, url)
            .header("authorization", signed);
        for (name, value) in headers.iter().filter(|(name, _)| name != "host") {
            request = request.header(name.as_str(), value.as_str());
        }
        Ok(request.body(body.to_vec()).send().await?)
    }

    /// Turns an unexpected response into an error that never repeats a credential.
    async fn failure(&self, what: &str, response: reqwest::Response) -> SyncError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let snippet: String = self
            .credentials
            .redact(&body)
            .chars()
            .filter(|c| !c.is_control())
            .take(ERROR_SNIPPET_CHARS)
            .collect();
        let hint = match status {
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                " (check the credentials and the token's permissions on the bucket)"
            }
            _ => "",
        };
        SyncError::Backend(format!(
            "{what} failed with status {status}{hint}: {snippet}"
        ))
    }

    /// Finds out whether the storage really honors conditional writes: two `If-None-Match: *` writes
    /// of the same new object must succeed once and then be refused. Leaves nothing behind.
    pub async fn probe_conditional_writes(&self) -> Result<bool, SyncError> {
        let nanos = (self.now)()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = format!("chamados-sync-probe-{nanos}");
        let first = self.put(&name, b"probe", Condition::Absent).await?;
        let second = self.put(&name, b"probe", Condition::Absent).await;
        let cleanup = self.delete(&name).await;
        let second = second?;
        cleanup?;
        Ok(first == PutOutcome::Stored && second == PutOutcome::PreconditionFailed)
    }

    /// Removes object `name`; a missing object is not an error.
    pub async fn delete(&self, name: &str) -> Result<(), SyncError> {
        let response = self.send(Method::DELETE, name, b"", &[]).await?;
        match response.status() {
            status if status.is_success() || status == StatusCode::NOT_FOUND => Ok(()),
            _ => Err(self.failure("delete", response).await),
        }
    }
}

impl SyncBackend for S3Backend {
    async fn get(&self, name: &str) -> Result<Option<Object>, SyncError> {
        let response = self.send(Method::GET, name, b"", &[]).await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => {
                let etag = response
                    .headers()
                    .get("etag")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let bytes = response.bytes().await?.to_vec();
                let version = etag.unwrap_or_else(|| version_of(&bytes));
                Ok(Some(Object { bytes, version }))
            }
            _ => Err(self.failure("read", response).await),
        }
    }

    async fn put(
        &self,
        name: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<PutOutcome, SyncError> {
        let extra: Vec<(&str, String)> = match condition {
            Condition::Always => Vec::new(),
            Condition::Absent => vec![("if-none-match", "*".to_owned())],
            Condition::Version(version) => vec![("if-match", version)],
        };
        let response = self.send(Method::PUT, name, bytes, &extra).await?;
        match response.status() {
            // 409: another conditional write to the same object is in flight; retry like a lost race.
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
                Ok(PutOutcome::PreconditionFailed)
            }
            status if status.is_success() => Ok(PutOutcome::Stored),
            _ => Err(self.failure("write", response).await),
        }
    }

    fn supports_conditional_writes(&self) -> bool {
        self.settings.conditional_writes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_s3::FakeS3;
    use std::time::Duration;
    use wiremock::MockServer;

    const ID: &str = "AKIATESTID123";
    const SECRET: &str = "segredo/de-teste+valor";

    fn base(endpoint: &str, bucket: &str) -> SyncSettings {
        SyncSettings {
            backend: Some("s3".to_owned()),
            endpoint: Some(endpoint.to_owned()),
            bucket: Some(bucket.to_owned()),
            ..SyncSettings::default()
        }
    }

    fn rejects(settings: &SyncSettings, expected: &str) {
        let error = S3Settings::from_settings(settings).unwrap_err().to_string();
        assert!(error.contains(expected), "{expected}: {error}");
    }

    #[test]
    fn validates_the_settings() {
        let mut settings = base("https://conta.r2.cloudflarestorage.com/", "meu-bucket");
        settings.prefix = Some("/chamados/sync/".to_owned());
        let ok = S3Settings::from_settings(&settings).unwrap();
        assert_eq!(
            (
                ok.region.as_str(),
                ok.prefix.as_str(),
                ok.conditional_writes
            ),
            ("auto", "chamados/sync", true)
        );
        settings.region = Some("us-east-1".to_owned());
        settings.conditional_writes = Some(false);
        let custom = S3Settings::from_settings(&settings).unwrap();
        assert_eq!(
            (custom.region.as_str(), custom.conditional_writes),
            ("us-east-1", false)
        );
        assert!(S3Settings::from_settings(&base("http://localhost:9000", "b")).is_ok());
        assert!(S3Settings::from_settings(&base("http://127.0.0.1:9000", "b")).is_ok());

        rejects(&SyncSettings::default(), "needs an endpoint");
        rejects(
            &SyncSettings {
                endpoint: Some("https://x.example".to_owned()),
                ..SyncSettings::default()
            },
            "needs a bucket",
        );
        rejects(&base("não é url", "b"), "invalid endpoint");
        rejects(&base("http://nuvem.example", "b"), "must use https");
        rejects(&base("ftp://nuvem.example", "b"), "must use https");
        rejects(
            &base("https://usuario:senha@nuvem.example", "b"),
            "must not contain credentials",
        );
        rejects(
            &base("https://nuvem.example/?x=1", "b"),
            "must not contain credentials",
        );
        rejects(
            &base("https://nuvem.example/#frag", "b"),
            "must not contain credentials",
        );
        rejects(
            &base("https://nuvem.example", "com espaço"),
            "invalid bucket",
        );
        rejects(&base("https://nuvem.example", ""), "invalid bucket");
        let mut bad_region = base("https://nuvem.example", "b");
        bad_region.region = Some("a/b".to_owned());
        rejects(&bad_region, "invalid region");
        for prefix in ["a/../b", "./x", "a\\b", "a?b", "a#b"] {
            let mut bad = base("https://nuvem.example", "b");
            bad.prefix = Some(prefix.to_owned());
            rejects(&bad, "invalid prefix");
        }
    }

    #[test]
    fn host_header_includes_only_non_default_ports() {
        assert_eq!(
            authority(&Url::parse("https://conta.r2.cloudflarestorage.com/x").unwrap()),
            "conta.r2.cloudflarestorage.com"
        );
        assert_eq!(
            authority(&Url::parse("http://127.0.0.1:9000/x").unwrap()),
            "127.0.0.1:9000"
        );
    }

    fn fixed_clock() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_268_800)
    }

    fn backend(server: &MockServer, prefix: &str, secret: &str) -> S3Backend {
        let mut settings = base(&server.uri(), "cofre");
        settings.prefix = Some(prefix.to_owned());
        let credentials = S3Credentials {
            access_key_id: ID.to_owned(),
            secret_access_key: secret.to_owned(),
        };
        S3Backend::with_clock(
            S3Settings::from_settings(&settings).unwrap(),
            credentials,
            fixed_clock,
        )
    }

    #[tokio::test]
    async fn stores_reads_and_replaces_objects_with_conditional_writes() {
        let fake = FakeS3::new(ID, SECRET);
        let server = fake.start().await;
        let backend = backend(&server, "pre/fixo", SECRET);
        assert!(backend.supports_conditional_writes());
        assert_eq!(backend.get("doc.bin").await.unwrap(), None);

        assert_eq!(
            backend
                .put("doc.bin", b"um", Condition::Absent)
                .await
                .unwrap(),
            PutOutcome::Stored
        );
        assert_eq!(fake.object("/cofre/pre/fixo/doc.bin").unwrap(), b"um");
        let first = backend.get("doc.bin").await.unwrap().unwrap();
        assert_eq!(
            (first.bytes.as_slice(), first.version.as_str()),
            (&b"um"[..], "\"etag-1\"")
        );

        let again = backend
            .put("doc.bin", b"x", Condition::Absent)
            .await
            .unwrap();
        assert_eq!(again, PutOutcome::PreconditionFailed);
        let stale = backend
            .put("doc.bin", b"x", Condition::Version("\"antiga\"".to_owned()))
            .await
            .unwrap();
        assert_eq!(stale, PutOutcome::PreconditionFailed);
        let fresh = backend
            .put("doc.bin", b"dois", Condition::Version(first.version))
            .await
            .unwrap();
        assert_eq!(fresh, PutOutcome::Stored);
        let missing = backend
            .put("novo.bin", b"x", Condition::Version("\"v\"".to_owned()))
            .await
            .unwrap();
        assert_eq!(missing, PutOutcome::PreconditionFailed);
        assert_eq!(
            backend
                .put("doc.bin", b"tres", Condition::Always)
                .await
                .unwrap(),
            PutOutcome::Stored
        );
        assert_eq!(
            backend.get("doc.bin").await.unwrap().unwrap().bytes,
            b"tres"
        );

        backend.delete("doc.bin").await.unwrap();
        assert_eq!(fake.object("/cofre/pre/fixo/doc.bin"), None);
        backend.delete("doc.bin").await.unwrap();

        // Every request was signed for the fixed date and used no ACL, query or presigned URL.
        let log = fake.log();
        assert!(!log.is_empty());
        for request in &log {
            let authorization = &request.headers["authorization"];
            assert!(authorization.starts_with(&format!(
                "AWS4-HMAC-SHA256 Credential={ID}/20260101/auto/s3/aws4_request, "
            )));
            assert_eq!(request.headers["x-amz-date"], "20260101T120000Z");
            assert!(request.query.is_none() && !request.headers.contains_key("x-amz-acl"));
            assert!(!authorization.contains(SECRET));
        }
    }

    #[tokio::test]
    async fn works_without_a_prefix_and_rejects_unsafe_names() {
        let fake = FakeS3::new(ID, SECRET);
        let server = fake.start().await;
        let backend = backend(&server, "", SECRET);
        backend
            .put("doc.bin", b"1", Condition::Always)
            .await
            .unwrap();
        assert_eq!(fake.object("/cofre/doc.bin").unwrap(), b"1");
        for name in ["", "a/b", ".oculto", "a?b", "a#b", "a\\b"] {
            assert!(backend.get(name).await.is_err(), "{name:?}");
        }
    }

    #[tokio::test]
    async fn probes_whether_conditional_writes_are_honored_and_cleans_up() {
        let mut fake = FakeS3::new(ID, SECRET);
        let server = fake.start().await;
        assert!(backend(&server, "p", SECRET)
            .probe_conditional_writes()
            .await
            .unwrap());
        let log = fake.log();
        assert_eq!(
            log.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(),
            ["PUT", "PUT", "DELETE"]
        );

        fake.honor_conditions = false;
        let sloppy = fake.start().await;
        assert!(!backend(&sloppy, "p", SECRET)
            .probe_conditional_writes()
            .await
            .unwrap());

        fake.fail_with = Some(500);
        let broken = fake.start().await;
        assert!(backend(&broken, "p", SECRET)
            .probe_conditional_writes()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn falls_back_to_a_content_version_when_the_server_sends_no_etag() {
        let mut fake = FakeS3::new(ID, SECRET);
        fake.send_etag = false;
        let server = fake.start().await;
        let backend = backend(&server, "p", SECRET);
        backend
            .put("doc.bin", b"conteudo", Condition::Always)
            .await
            .unwrap();
        let object = backend.get("doc.bin").await.unwrap().unwrap();
        assert_eq!(object.version, version_of(b"conteudo"));
    }

    #[tokio::test]
    async fn explains_failures_without_leaking_credentials() {
        let fake = FakeS3::new(ID, SECRET);
        let server = fake.start().await;
        // A wrong secret: the fake answers 403 and echoes the access key id in its body.
        let wrong = backend(&server, "p", "outro-segredo");
        for error in [
            wrong.get("doc.bin").await.unwrap_err(),
            wrong
                .put("doc.bin", b"x", Condition::Always)
                .await
                .unwrap_err(),
            wrong.delete("doc.bin").await.unwrap_err(),
        ] {
            let text = error.to_string();
            assert!(
                text.contains("status 403") && text.contains("check the credentials"),
                "{text}"
            );
            assert!(
                !text.contains(ID) && !text.contains("outro-segredo") && !text.contains(SECRET),
                "{text}"
            );
        }

        let mut failing = FakeS3::new(ID, SECRET);
        failing.fail_with = Some(500);
        let broken = failing.start().await;
        let text = backend(&broken, "p", SECRET)
            .get("doc.bin")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            text.contains("read failed with status 500") && text.ends_with("a falha de teste"),
            "{text}"
        );
        assert!(backend(&broken, "p", SECRET)
            .put("doc.bin", b"x", Condition::Always)
            .await
            .is_err());
        assert!(backend(&broken, "p", SECRET)
            .delete("doc.bin")
            .await
            .is_err());

        let mut racing = FakeS3::new(ID, SECRET);
        racing.fail_with = Some(409);
        let conflict = racing.start().await;
        let outcome = backend(&conflict, "p", SECRET)
            .put("doc.bin", b"x", Condition::Absent)
            .await
            .unwrap();
        assert_eq!(outcome, PutOutcome::PreconditionFailed);
    }

    #[tokio::test]
    async fn the_fake_server_refuses_unsigned_requests_and_unknown_methods() {
        let fake = FakeS3::new(ID, SECRET);
        let server = fake.start().await;
        let url = format!("{}/cofre/doc.bin", server.uri());
        let client = reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 403);
        let garbled = client
            .get(&url)
            .header("authorization", "isto nao e uma assinatura")
            .send()
            .await
            .unwrap();
        assert_eq!(garbled.status(), 403);
        // A signed request with a method S3 objects do not take here.
        let backend = backend(&server, "", SECRET);
        let response = backend
            .send(Method::POST, "doc.bin", b"", &[])
            .await
            .unwrap();
        assert_eq!(response.status(), 405);
    }

    #[tokio::test]
    async fn reports_an_unreachable_server_as_an_http_error() {
        // Nothing listens on port 1 of the loopback address.
        let mut settings = base("http://127.0.0.1:1", "cofre");
        settings.prefix = Some("p".to_owned());
        let credentials = S3Credentials {
            access_key_id: ID.to_owned(),
            secret_access_key: SECRET.to_owned(),
        };
        let unreachable =
            S3Backend::new(S3Settings::from_settings(&settings).unwrap(), credentials);
        let error = unreachable.get("doc.bin").await.unwrap_err();
        assert!(matches!(error, SyncError::Http(_)), "{error}");
    }
}
