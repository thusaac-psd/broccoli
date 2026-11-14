//! The contestant and developer CLIs' API client against a real server: how
//! it addresses the server and what its errors say.

use broccoli_cli_core::client::{Client, ClientError, NetworkCause};
use broccoli_cli_core::server::ServerUrl;

use crate::common::TestApp;

fn server(address: &str) -> ServerUrl {
    address.parse().unwrap()
}

/// Run a blocking CLI client call off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_works_however_the_server_address_is_spelled() {
    let app = TestApp::spawn().await;
    app.create_user_with_role("bob", "correct-horse", "contestant")
        .await;

    for spelling in [app.url(""), app.url("/"), app.url("//").to_uppercase()] {
        let client = Client::anonymous(&server(&spelling));
        let login = blocking(move || client.login("bob", "correct-horse")).await;
        assert!(login.is_ok(), "{spelling}: {:?}", login.err());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_reports_the_credentials() {
    let app = TestApp::spawn().await;
    app.create_user_with_role("bob", "correct-horse", "contestant")
        .await;

    let client = Client::anonymous(&server(&app.url("")));
    let err = blocking(move || client.login("bob", "wrong"))
        .await
        .unwrap_err();
    match err {
        ClientError::Api(api) => assert_eq!(api.code, "INVALID_CREDENTIALS"),
        other => panic!("expected the server's refusal, got {other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_that_is_not_the_api_says_so() {
    let app = TestApp::spawn().await;

    let client = Client::anonymous(&server(&app.url("/not-broccoli")));
    let err = blocking(move || client.login("bob", "pw"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::NotBroccoli { .. }),
        "expected NotBroccoli, got {err:?}"
    );
    assert!(
        err.to_string()
            .contains("does not look like a Broccoli server")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_port_is_reported_as_refused() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let client = Client::anonymous(&server(&address));
    let err = blocking(move || client.login("bob", "pw"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Network {
                cause: NetworkCause::Refused,
                ..
            }
        ),
        "expected a refused connection, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_explicit_token_is_the_servers_answer_not_an_expired_session() {
    let app = TestApp::spawn().await;
    let client = Client::new(broccoli_cli_core::session::Session::explicit(
        server(&app.url("")),
        "not-a-token".into(),
    ));
    let err = blocking(move || client.me()).await.unwrap_err();
    match err {
        ClientError::Api(api) => assert!(api.rejects_token(), "{api:?}"),
        other => panic!("expected the token refusal, got {other}"),
    }
}
