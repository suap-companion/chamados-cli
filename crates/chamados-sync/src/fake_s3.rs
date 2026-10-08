//! A small in-memory S3 for tests: it keeps objects, checks the SigV4 signature of every request,
//! honors (or, on request, ignores) conditional writes, and records everything it receives.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use wiremock::{matchers::any, Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::sigv4::{authorization, encode_path, sha256_hex, Signer};

/// One request, as the server saw it.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, (Vec<u8>, String)>,
    counter: u64,
    log: Vec<Recorded>,
}

#[derive(Clone)]
pub(crate) struct FakeS3 {
    state: Arc<Mutex<State>>,
    access_key_id: String,
    secret_access_key: String,
    region: String,
    /// When false, `If-Match` / `If-None-Match` are ignored (a storage without conditional writes).
    pub honor_conditions: bool,
    /// When false, responses carry no `ETag`.
    pub send_etag: bool,
    /// Answer every request with this status instead.
    pub fail_with: Option<u16>,
}

impl FakeS3 {
    pub(crate) fn new(access_key_id: &str, secret_access_key: &str) -> Self {
        Self {
            state: Arc::default(),
            access_key_id: access_key_id.to_owned(),
            secret_access_key: secret_access_key.to_owned(),
            region: "auto".to_owned(),
            honor_conditions: true,
            send_etag: true,
            fail_with: None,
        }
    }

    pub(crate) async fn start(&self) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(self.clone())
            .mount(&server)
            .await;
        server
    }

    pub(crate) fn log(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().log.clone()
    }

    pub(crate) fn object(&self, path: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .objects
            .get(path)
            .map(|(bytes, _)| bytes.clone())
    }

    /// Whether the request carries a signature this server would accept.
    fn authentic(&self, recorded: &Recorded) -> bool {
        let Some(received) = recorded.headers.get("authorization") else {
            return false;
        };
        let signed_list = received
            .split("SignedHeaders=")
            .nth(1)
            .and_then(|rest| rest.split(',').next());
        let (Some(signed_list), Some(date)) = (signed_list, recorded.headers.get("x-amz-date"))
        else {
            return false;
        };
        let mut headers = Vec::new();
        for name in signed_list.split(';') {
            let value = recorded.headers.get(name).cloned().unwrap_or_default();
            headers.push((name.to_owned(), value));
        }
        let signer = Signer {
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
            region: &self.region,
        };
        let payload = sha256_hex(&recorded.body);
        let expected = authorization(
            &signer,
            &recorded.method,
            &encode_path(&recorded.path),
            &headers,
            &payload,
            date,
        );
        *received == expected && recorded.headers.get("x-amz-content-sha256") == Some(&payload)
    }
}

impl Respond for FakeS3 {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let headers = request
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let recorded = Recorded {
            method: request.method.to_string(),
            path: request.url.path().to_owned(),
            query: request.url.query().map(str::to_owned),
            headers,
            body: request.body.clone(),
        };
        let mut state = self.state.lock().unwrap();
        state.log.push(recorded.clone());

        if let Some(status) = self.fail_with {
            return ResponseTemplate::new(status).set_body_string("a falha de teste");
        }
        if !self.authentic(&recorded) {
            let body = format!(
                "<Error><Code>SignatureDoesNotMatch</Code><AWSAccessKeyId>{}</AWSAccessKeyId></Error>",
                self.access_key_id
            );
            return ResponseTemplate::new(403).set_body_string(body);
        }

        let key = recorded.path.clone();
        let current = state.objects.get(&key).cloned();
        match recorded.method.as_str() {
            "GET" => match current {
                Some((bytes, etag)) => {
                    let response = ResponseTemplate::new(200).set_body_bytes(bytes);
                    if self.send_etag {
                        response.insert_header("etag", etag.as_str())
                    } else {
                        response
                    }
                }
                None => ResponseTemplate::new(404)
                    .set_body_string("<Error><Code>NoSuchKey</Code></Error>"),
            },
            "PUT" => {
                let if_match = recorded.headers.get("if-match");
                let if_none_match = recorded.headers.get("if-none-match");
                let refused = (if_none_match.is_some() && current.is_some())
                    || if_match.is_some_and(|wanted| {
                        current.as_ref().is_none_or(|(_, etag)| etag != wanted)
                    });
                if self.honor_conditions && refused {
                    return ResponseTemplate::new(412);
                }
                state.counter += 1;
                let etag = format!("\"etag-{}\"", state.counter);
                state.objects.insert(key, (recorded.body, etag.clone()));
                ResponseTemplate::new(200).insert_header("etag", etag.as_str())
            }
            "DELETE" => {
                state.objects.remove(&key);
                ResponseTemplate::new(204)
            }
            _ => ResponseTemplate::new(405),
        }
    }
}
