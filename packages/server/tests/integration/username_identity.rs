//! A username names one account whatever its case: `Admin` cannot register
//! next to `admin`, any case signs in, and the account keeps the case it was
//! created with for display.

use crate::common::{TestApp, routes};
use serde_json::json;

#[tokio::test]
async fn a_username_differing_only_in_case_is_taken() {
    let app = TestApp::spawn().await;
    let first = app
        .post_without_token(
            routes::REGISTER,
            &json!({ "username": "CaseKeeper", "password": "password123" }),
        )
        .await;
    assert_eq!(first.status, 201, "{}", first.text);
    assert_eq!(first.body["username"], "CaseKeeper", "{}", first.text);

    let again = app
        .post_without_token(
            routes::REGISTER,
            &json!({ "username": "casekeeper", "password": "password123" }),
        )
        .await;
    assert_eq!(again.status, 409, "{}", again.text);
    assert_eq!(again.body["code"], "USERNAME_TAKEN", "{}", again.text);
}

#[tokio::test]
async fn any_case_signs_in_to_the_same_account() {
    let app = TestApp::spawn().await;
    app.create_user_with_role("MixedSignIn", "password123", "contestant")
        .await;
    let res = app
        .post_without_token(
            routes::LOGIN,
            &json!({ "username": "mixedsignin", "password": "password123" }),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    let me = app
        .get_with_token(routes::ME, res.body["token"].as_str().unwrap())
        .await;
    assert_eq!(me.body["username"], "MixedSignIn", "{}", me.text);
}

#[tokio::test]
async fn a_bulk_import_conflict_names_every_taken_username() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("bulk_case_admin", "password123", "admin")
        .await;
    app.create_user_with_role("Taken_One", "password123", "contestant")
        .await;
    app.create_user_with_role("taken_two", "password123", "contestant")
        .await;

    let res = app
        .post_with_token(
            &format!("{}/bulk", routes::USERS),
            &json!({ "users": [
                { "username": "taken_one" },
                { "username": "fresh_name" },
                { "username": "TAKEN_TWO" },
            ] }),
            &admin,
        )
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "USERNAME_TAKEN", "{}", res.text);
    assert_eq!(
        res.body["details"]["usernames"],
        json!(["taken_one", "TAKEN_TWO"]),
        "{}",
        res.text
    );
    assert_eq!(
        res.body["message"], "Usernames already taken: taken_one, TAKEN_TWO",
        "{}",
        res.text
    );
}
