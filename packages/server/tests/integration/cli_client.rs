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

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_reads_the_languages_and_extensions_the_server_registered() {
    let app = TestApp::spawn().await;
    let token = app
        .create_authenticated_user("lang_reader", "pass1234")
        .await;
    let client = Client::new(broccoli_cli_core::session::Session::explicit(
        server(&app.url("")),
        token,
    ));
    let languages = blocking(move || client.languages()).await.unwrap();
    let cpp = languages
        .iter()
        .find(|l| l.id == "cpp")
        .expect("the test server registers cpp");
    assert!(!cpp.extensions.is_empty(), "{cpp:?}");
}

/// The server pages contests 20 to a page, newest first; the CLI reads every
/// page, so a contest older than the newest page is still listed and found by
/// name.
#[tokio::test(flavor = "multi_thread")]
async fn contests_past_the_first_page_are_listed_and_found_by_name() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("organiser", "password123", "admin")
        .await;
    let oldest = app
        .create_contest(&token, "Oldest Round", true, false)
        .await;
    for i in 1..=104 {
        app.create_contest(&token, &format!("Round {i}"), true, false)
            .await;
    }

    let client = Client::new(broccoli_cli_core::session::Session::explicit(
        server(&app.url("")),
        token,
    ));
    let (listed, found, exact) = blocking(move || {
        (
            client.contests(None).unwrap(),
            broccoli_cli_core::resolve::contest_id(&client, "oldest").unwrap(),
            broccoli_cli_core::resolve::contest_id(&client, "Round 1").unwrap(),
        )
    })
    .await;
    assert_eq!(listed.len(), 105);
    assert_eq!(listed.last().map(|c| c.id), Some(oldest));
    assert_eq!(found.to_string(), oldest.to_string());
    assert_eq!(exact.to_string(), (oldest + 1).to_string());
}

/// Text that is a contest's id names that contest, whatever its title, and
/// so does its whole title; a question that merely mentions either does not.
#[tokio::test(flavor = "multi_thread")]
async fn a_contest_is_named_by_its_id_whatever_its_title() {
    let app = TestApp::spawn().await;
    let token = app
        .create_user_with_role("organiser", "password123", "admin")
        .await;
    let id = app
        .create_contest(&token, "Spring Round", true, false)
        .await;

    let client = Client::new(broccoli_cli_core::session::Session::explicit(
        server(&app.url("")),
        token,
    ));
    let named = blocking(move || {
        [
            id.to_string(),
            "spring round".to_string(),
            (id + 1000).to_string(),
            "Is Spring Round rated?".to_string(),
        ]
        .map(|q| {
            broccoli_cli_core::resolve::contest_named(&client, &q)
                .unwrap()
                .map(|c| c.id.to_string())
        })
    })
    .await;
    let id = Some(id.to_string());
    assert_eq!(named, [id.clone(), id, None, None]);
}
