use std::fs;

use suap_core::{AppPaths, SuapClient, SuapConfig, SuapError};
use tempfile::tempdir;
use url::Url;
use wiremock::{matchers::{body_string_contains, header_regex, method, path}, Mock, MockServer, ResponseTemplate};

fn config(server: &MockServer) -> SuapConfig {
    SuapConfig {
        base_url: Url::parse(&format!("{}/", server.uri())).unwrap(),
        username: Some("kelson".to_owned()),
    }
}

fn paths() -> (tempfile::TempDir, AppPaths) {
    let directory = tempdir().unwrap();
    let paths = AppPaths::from_dirs(
        directory.path().join("config"),
        directory.path().join("data"),
    );
    (directory, paths)
}

#[tokio::test]
async fn login_sends_csrf_credentials_and_persists_session_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/accounts/login/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "csrftoken=test-csrf; Path=/")
                .set_body_string(r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="test-csrf"></form>"#),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/accounts/login/"))
        .and(body_string_contains("username=kelson"))
        .and(body_string_contains("password=secret"))
        .and(body_string_contains("csrfmiddlewaretoken=test-csrf"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/")
                .insert_header("set-cookie", "sessionid=authenticated; Path=/; Max-Age=3600"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/"))
        .and(header_regex("cookie", "sessionid=authenticated"))
        .respond_with(ResponseTemplate::new(200).set_body_string("dashboard"))
        .mount(&server)
        .await;

    let (_directory, paths) = paths();
    let client = SuapClient::open(&paths, &config(&server)).unwrap();
    client.login("kelson", "secret").await.unwrap();

    assert!(paths.session_file().exists());
    let content = fs::read_to_string(paths.session_file()).unwrap();
    assert!(content.contains("sessionid"));
}

#[tokio::test]
async fn login_rejects_return_to_login_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/accounts/login/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="test-csrf"></form>"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/accounts/login/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("invalid credentials"))
        .mount(&server)
        .await;

    let (_directory, paths) = paths();
    let client = SuapClient::open(&paths, &config(&server)).unwrap();
    let result = client.login("kelson", "wrong").await;
    assert!(matches!(result, Err(SuapError::AuthenticationFailed)));
    assert!(!paths.session_file().exists());
}

#[tokio::test]
async fn login_reports_missing_csrf_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/accounts/login/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<form></form>"))
        .mount(&server)
        .await;

    let (_directory, paths) = paths();
    let client = SuapClient::open(&paths, &config(&server)).unwrap();
    let result = client.login("kelson", "secret").await;
    assert!(matches!(result, Err(SuapError::Parse(_))));
}

#[tokio::test]
async fn authenticated_check_accepts_protected_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("dashboard"))
        .mount(&server)
        .await;

    let (_directory, paths) = paths();
    let client = SuapClient::open(&paths, &config(&server)).unwrap();
    assert!(client.is_authenticated().await.unwrap());
}

#[tokio::test]
async fn authenticated_check_rejects_login_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        )
        .mount(&server)
        .await;

    let (_directory, paths) = paths();
    let client = SuapClient::open(&paths, &config(&server)).unwrap();
    assert!(!client.is_authenticated().await.unwrap());
}
