use crate::common::{TestApp, routes};

mod attachment_upload {
    use super::*;

    #[tokio::test]
    async fn admin_can_upload_attachment_to_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem with attachment").await;

        let res = app
            .upload_attachment(problem_id, "figure.png", b"PNG_DATA".to_vec(), None, &token)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["filename"].as_str().unwrap(), "figure.png");
        assert_eq!(res.body["path"].as_str().unwrap(), "figure.png");
        assert!(res.body["id"].as_str().is_some());
        assert!(res.body["content_hash"].as_str().is_some());
        assert_eq!(res.body["size"].as_i64().unwrap(), 8);
    }

    #[tokio::test]
    async fn upload_with_explicit_path() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin2", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem upload path").await;

        let res = app
            .upload_attachment(
                problem_id,
                "figure.png",
                b"DATA".to_vec(),
                Some("images/figure1.png"),
                &token,
            )
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["path"].as_str().unwrap(), "images/figure1.png");
        assert_eq!(res.body["filename"].as_str().unwrap(), "figure.png");
    }

    #[tokio::test]
    async fn upload_auto_detects_mime_type() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin3", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem MIME").await;

        let res = app
            .upload_attachment(problem_id, "photo.jpg", b"JPEG".to_vec(), None, &token)
            .await;

        assert_eq!(res.status, 201);
        assert_eq!(res.body["content_type"].as_str().unwrap(), "image/jpeg");
    }

    #[tokio::test]
    async fn upload_deduplicates_identical_content() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin4", "pass1234", "admin")
            .await;
        let p1 = app.create_problem(&token, "Problem A").await;
        let p2 = app.create_problem(&token, "Problem B").await;

        let data = b"shared content".to_vec();
        let res1 = app
            .upload_attachment(p1, "file.txt", data.clone(), None, &token)
            .await;
        let res2 = app
            .upload_attachment(p2, "file.txt", data, None, &token)
            .await;

        assert_eq!(res1.status, 201);
        assert_eq!(res2.status, 201);
        assert_eq!(
            res1.body["content_hash"].as_str().unwrap(),
            res2.body["content_hash"].as_str().unwrap()
        );
        assert_ne!(
            res1.body["id"].as_str().unwrap(),
            res2.body["id"].as_str().unwrap()
        );
    }

    #[tokio::test]
    async fn upload_to_same_path_replaces_content() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin5", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem upsert").await;

        let res1 = app
            .upload_attachment(
                problem_id,
                "readme.md",
                b"v1".to_vec(),
                Some("docs/readme.md"),
                &token,
            )
            .await;
        assert_eq!(res1.status, 201);

        let res2 = app
            .upload_attachment(
                problem_id,
                "readme.md",
                b"v2".to_vec(),
                Some("docs/readme.md"),
                &token,
            )
            .await;
        assert_eq!(res2.status, 201);

        assert_ne!(
            res1.body["content_hash"].as_str().unwrap(),
            res2.body["content_hash"].as_str().unwrap()
        );

        let list = app
            .get_with_token(&routes::attachments(problem_id), &token)
            .await;
        assert_eq!(list.body["total"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn upload_validates_path() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin6", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem path validation").await;

        let res = app
            .upload_attachment(
                problem_id,
                "file.txt",
                b"data".to_vec(),
                Some("../etc/passwd"),
                &token,
            )
            .await;
        assert_eq!(res.status, 400);

        let res = app
            .upload_attachment(
                problem_id,
                "file.txt",
                b"data".to_vec(),
                Some(".hidden/file.txt"),
                &token,
            )
            .await;
        assert_eq!(res.status, 400);
    }

    #[tokio::test]
    async fn upload_to_nonexistent_problem_returns_404() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin7", "pass1234", "admin")
            .await;

        let res = app
            .upload_attachment(99999, "file.txt", b"data".to_vec(), None, &token)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn unauthorized_user_cannot_upload() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin8", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("user8", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected problem").await;

        let res = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &contestant)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn upload_rejects_file_without_filename() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin8b", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "No filename").await;

        let part = reqwest::multipart::Part::bytes(b"data".to_vec())
            .mime_str("application/octet-stream")
            .unwrap();
        let form = reqwest::multipart::Form::new().part("file", part);
        let res = app
            .client
            .post(format!(
                "http://{}{}",
                app.addr,
                routes::attachments(problem_id)
            ))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 400);
    }

    #[tokio::test]
    async fn upload_rejects_control_characters_in_filename() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin8c", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "CRLF filename").await;

        let res = app
            .upload_attachment(
                problem_id,
                "file\r\nname.txt",
                b"data".to_vec(),
                None,
                &token,
            )
            .await;
        assert_eq!(res.status, 400);
    }

    #[tokio::test]
    async fn upload_without_file_field_returns_400() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin9", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Missing file").await;

        let form = reqwest::multipart::Form::new().text("path", "images/fig.png");
        let res = app
            .client
            .post(format!(
                "http://{}{}",
                app.addr,
                routes::attachments(problem_id)
            ))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 400);
    }
}

mod attachment_list {
    use super::*;

    #[tokio::test]
    async fn list_returns_all_attachments_for_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin10", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Problem with attachments").await;

        app.upload_attachment(problem_id, "a.txt", b"aaa".to_vec(), None, &token)
            .await;
        app.upload_attachment(problem_id, "b.txt", b"bbb".to_vec(), None, &token)
            .await;

        let res = app
            .get_with_token(&routes::attachments(problem_id), &token)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["total"].as_u64().unwrap(), 2);
        assert_eq!(res.body["attachments"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_returns_empty_for_new_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin11", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Empty problem").await;

        let res = app
            .get_with_token(&routes::attachments(problem_id), &token)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["total"].as_u64().unwrap(), 0);
    }

    #[tokio::test]
    async fn list_does_not_return_other_problems_attachments() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin12", "pass1234", "admin")
            .await;
        let p1 = app.create_problem(&token, "Problem 1").await;
        let p2 = app.create_problem(&token, "Problem 2").await;

        app.upload_attachment(p1, "p1.txt", b"data1".to_vec(), None, &token)
            .await;
        app.upload_attachment(p2, "p2.txt", b"data2".to_vec(), None, &token)
            .await;

        let res = app.get_with_token(&routes::attachments(p1), &token).await;
        assert_eq!(res.body["total"].as_u64().unwrap(), 1);
        assert_eq!(
            res.body["attachments"][0]["filename"].as_str().unwrap(),
            "p1.txt"
        );
    }

    /// Pin: `total` is derived from the POST-kernel-filter `attachments`
    /// array (`attachments.len()`, see `handlers/attachment.rs`), never from
    /// a raw pre-filter `COUNT(*)` over `problem_attachment`. This is the
    /// correct sibling of the bug fixed in `list_contest_submissions` (see
    /// `submission.rs`'s `contest_submission_visibility` module) - a denied
    /// row must never be reflected in an aggregate the viewer cannot see.
    ///
    /// Caveat this test cannot get around: `Resource::Attachment`'s host
    /// decision is, by construction, the exact same
    /// `decide_standalone_problem_access` call keyed on `problem_id` that
    /// gates the list at all (see `visibility::host_rules`), and no plugin
    /// in this tree currently denies an individual attachment once its
    /// parent problem is reachable. So today there is no black-box way to
    /// force a per-row Deny distinct from the top-level 404 gate, and a
    /// naive pre-filter `COUNT(*)` would coincidentally still pass this
    /// exact scenario. This test's value is holding the NON-admin,
    /// full-kernel code path (a hidden problem reachable only via contest
    /// membership rules, not the `is_public` shortcut) to the invariant, so
    /// it fails immediately the day either a host rule or a plugin CAN
    /// deny one attachment but not another for the same problem and the
    /// count computation was reverted to pre-filter in the meantime.
    #[tokio::test]
    async fn contestant_total_matches_post_filter_attachments_for_hidden_contest_problem() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_att_pin", "pass1234", "admin")
            .await;
        let problem_id = app
            .create_hidden_problem(&admin_token, "Hidden Contest Problem")
            .await;
        let contest_id = app
            .create_contest(&admin_token, "Public Contest For Attachments", true, true)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin_token)
            .await;

        app.upload_attachment(problem_id, "a.txt", b"aaa".to_vec(), None, &admin_token)
            .await;
        app.upload_attachment(problem_id, "b.txt", b"bbb".to_vec(), None, &admin_token)
            .await;

        // Authenticated, non-admin, never registered for the contest - only
        // reachable via the public-contest rule, not the `is_public`
        // problem shortcut nor any permission bypass.
        let contestant_token = app
            .create_authenticated_user("contestant_att_pin", "pass1234")
            .await;

        let res = app
            .get_with_token(&routes::attachments(problem_id), &contestant_token)
            .await;

        assert_eq!(res.status, 200);
        let attachments = res.body["attachments"]
            .as_array()
            .expect("attachments should be array");
        assert_eq!(attachments.len(), 2);
        assert_eq!(
            res.body["total"].as_u64().unwrap(),
            attachments.len() as u64,
            "total must equal the post-filter attachments length, not a pre-filter count"
        );
    }
}

mod attachment_download {
    use super::*;

    #[tokio::test]
    async fn download_returns_correct_content() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin13", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Download test").await;

        let data = b"hello world content".to_vec();
        let upload = app
            .upload_attachment(problem_id, "test.txt", data.clone(), None, &token)
            .await;
        assert_eq!(upload.status, 201);
        let ref_id = upload.body["id"].as_str().unwrap();

        let res = app
            .download_raw(&routes::attachment(problem_id, ref_id), &token)
            .await;
        assert_eq!(res.status().as_u16(), 200);

        let bytes = res.bytes().await.unwrap();
        assert_eq!(bytes.as_ref(), b"hello world content");
    }

    #[tokio::test]
    async fn download_sets_correct_headers() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin14", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Headers test").await;

        let upload = app
            .upload_attachment(problem_id, "image.png", b"PNG".to_vec(), None, &token)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();
        let hash = upload.body["content_hash"].as_str().unwrap();

        let res = app
            .download_raw(&routes::attachment(problem_id, ref_id), &token)
            .await;
        assert_eq!(res.status().as_u16(), 200);

        let headers = res.headers();
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "image/png"
        );
        assert_eq!(
            headers.get("etag").unwrap().to_str().unwrap(),
            format!("\"{}\"", hash)
        );
        assert!(headers.get("cache-control").is_some());
        assert_eq!(
            headers.get("content-length").unwrap().to_str().unwrap(),
            "3"
        );
        let cd = headers
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cd.contains("filename=\"image.png\""), "ASCII filename");
        assert!(
            cd.contains("filename*=UTF-8''image.png"),
            "RFC 5987 filename"
        );
    }

    #[tokio::test]
    async fn download_returns_304_with_matching_etag() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin15", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "ETag test").await;

        let upload = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &token)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();
        let hash = upload.body["content_hash"].as_str().unwrap();

        let res = app
            .client
            .get(format!(
                "http://{}{}",
                app.addr,
                routes::attachment(problem_id, ref_id)
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("If-None-Match", format!("\"{}\"", hash))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 304);
    }

    #[tokio::test]
    async fn download_returns_404_for_nonexistent_ref() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin16", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Missing ref").await;

        let res = app
            .get_with_token(
                &routes::attachment(problem_id, "01936f0e-1234-7abc-8000-000000000001"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn download_returns_404_for_wrong_problem() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin17", "pass1234", "admin")
            .await;
        let p1 = app.create_problem(&token, "Problem A").await;
        let p2 = app.create_problem(&token, "Problem B").await;

        let upload = app
            .upload_attachment(p1, "file.txt", b"data".to_vec(), None, &token)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();

        let res = app
            .get_with_token(&routes::attachment(p2, ref_id), &token)
            .await;
        assert_eq!(res.status, 404);
    }
}

mod attachment_delete {
    use super::*;

    #[tokio::test]
    async fn admin_can_delete_attachment() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin18", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Delete test").await;

        let upload = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &token)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();

        let res = app
            .delete_with_token(&routes::attachment(problem_id, ref_id), &token)
            .await;
        assert_eq!(res.status, 204);

        let list = app
            .get_with_token(&routes::attachments(problem_id), &token)
            .await;
        assert_eq!(list.body["total"].as_u64().unwrap(), 0);
    }

    #[tokio::test]
    async fn delete_preserves_blob_for_other_refs() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin22", "pass1234", "admin")
            .await;
        let p1 = app.create_problem(&token, "Blob share A").await;
        let p2 = app.create_problem(&token, "Blob share B").await;

        let content = b"shared-content-blob".to_vec();

        let upload1 = app
            .upload_attachment(p1, "shared.bin", content.clone(), None, &token)
            .await;
        let upload2 = app
            .upload_attachment(p2, "shared.bin", content.clone(), None, &token)
            .await;

        assert_eq!(
            upload1.body["content_hash"].as_str().unwrap(),
            upload2.body["content_hash"].as_str().unwrap()
        );

        let ref_id_1 = upload1.body["id"].as_str().unwrap();
        let res = app
            .delete_with_token(&routes::attachment(p1, ref_id_1), &token)
            .await;
        assert_eq!(res.status, 204);

        let ref_id_2 = upload2.body["id"].as_str().unwrap();
        let res = app
            .download_raw(&routes::attachment(p2, ref_id_2), &token)
            .await;
        assert_eq!(res.status().as_u16(), 200);
        let body = res.bytes().await.unwrap();
        assert_eq!(body.as_ref(), b"shared-content-blob");
    }

    #[tokio::test]
    async fn delete_returns_404_for_nonexistent_ref() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin19", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Delete 404").await;

        let res = app
            .delete_with_token(
                &routes::attachment(problem_id, "01936f0e-1234-7abc-8000-000000000001"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn unauthorized_user_cannot_delete() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin20", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("user20", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected delete").await;

        let upload = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &admin)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();

        let res = app
            .delete_with_token(&routes::attachment(problem_id, ref_id), &contestant)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn deleting_problem_hides_attachments() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_role("admin21", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&token, "Cascade test").await;

        app.upload_attachment(problem_id, "a.txt", b"aaa".to_vec(), None, &token)
            .await;
        app.upload_attachment(problem_id, "b.txt", b"bbb".to_vec(), None, &token)
            .await;

        let res = app
            .delete_with_token(&routes::problem(problem_id), &token)
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(&routes::attachments(problem_id), &token)
            .await;
        assert_eq!(res.status, 404);
    }
}

async fn setup_contest_with_attachment(
    app: &TestApp,
    admin_name: &str,
    is_public: bool,
) -> (i32, i32, String, String) {
    let token = app
        .create_user_with_role(admin_name, "pass1234", "admin")
        .await;
    let problem_id = app.create_hidden_problem(&token, "Contest problem").await;
    let contest_id = app
        .create_contest(&token, "Test contest", is_public, true)
        .await;
    app.add_problem_to_contest(contest_id, problem_id, &token)
        .await;

    let upload = app
        .upload_attachment(
            problem_id,
            "figure.png",
            b"PNG_BYTES".to_vec(),
            None,
            &token,
        )
        .await;
    assert_eq!(upload.status, 201);
    let ref_id = upload.body["id"].as_str().unwrap().to_string();

    (contest_id, problem_id, ref_id, token)
}

async fn enroll_contestant(
    app: &TestApp,
    username: &str,
    contest_id: i32,
    admin_token: &str,
) -> String {
    let contestant = app
        .create_user_with_role(username, "pass1234", "contestant")
        .await;
    let me = app.get_with_token("/api/v1/auth/me", &contestant).await;
    let user_id = me.body["id"].as_i64().unwrap() as i32;
    app.post_with_token(
        &routes::contest_participants(contest_id),
        &serde_json::json!({ "user_id": user_id }),
        admin_token,
    )
    .await;
    contestant
}

mod contest_based_access {
    use super::*;

    #[tokio::test]
    async fn participant_can_download_via_problem_endpoint() {
        let app = TestApp::spawn().await;
        let (_contest_id, problem_id, ref_id, admin_token) =
            setup_contest_with_attachment(&app, "cadm1", false).await;
        let contestant = enroll_contestant(&app, "cuser1", _contest_id, &admin_token).await;

        let res = app
            .download_raw(&routes::attachment(problem_id, &ref_id), &contestant)
            .await;
        assert_eq!(res.status().as_u16(), 200);
        let body = res.bytes().await.unwrap();
        assert_eq!(body.as_ref(), b"PNG_BYTES");
    }

    #[tokio::test]
    async fn public_contest_grants_access_to_any_user() {
        let app = TestApp::spawn().await;
        let (_contest_id, problem_id, ref_id, _admin_token) =
            setup_contest_with_attachment(&app, "cadm2", true).await;

        let outsider = app
            .create_user_with_role("cuser2", "pass1234", "contestant")
            .await;

        let res = app
            .download_raw(&routes::attachment(problem_id, &ref_id), &outsider)
            .await;
        assert_eq!(res.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn non_participant_gets_404_for_private_contest_download() {
        let app = TestApp::spawn().await;
        let (_contest_id, problem_id, ref_id, _admin_token) =
            setup_contest_with_attachment(&app, "cadm3", false).await;

        let outsider = app
            .create_user_with_role("cuser3", "pass1234", "contestant")
            .await;

        let res = app
            .get_with_token(&routes::attachment(problem_id, &ref_id), &outsider)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn problem_not_in_any_contest_returns_404_for_contestant() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("cadm4", "pass1234", "admin")
            .await;
        let problem_id = app
            .create_hidden_problem(&admin, "Standalone problem")
            .await;

        let upload = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &admin)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();

        let contestant = app
            .create_user_with_role("cuser4", "pass1234", "contestant")
            .await;

        let res = app
            .get_with_token(&routes::attachment(problem_id, ref_id), &contestant)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn admin_can_always_download_regardless_of_contest() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("cadm5", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Admin-only problem").await;

        let upload = app
            .upload_attachment(problem_id, "file.txt", b"data".to_vec(), None, &admin)
            .await;
        let ref_id = upload.body["id"].as_str().unwrap();

        let res = app
            .download_raw(&routes::attachment(problem_id, ref_id), &admin)
            .await;
        assert_eq!(res.status().as_u16(), 200);
    }

    #[tokio::test]
    async fn etag_304_works_for_contest_participant() {
        let app = TestApp::spawn().await;
        let (contest_id, problem_id, ref_id, admin_token) =
            setup_contest_with_attachment(&app, "cadm6", false).await;
        let contestant = enroll_contestant(&app, "cuser6", contest_id, &admin_token).await;

        let res = app
            .download_raw(&routes::attachment(problem_id, &ref_id), &contestant)
            .await;
        let etag = res
            .headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let res = app
            .client
            .get(format!(
                "http://{}{}",
                app.addr,
                routes::attachment(problem_id, &ref_id)
            ))
            .header("Authorization", format!("Bearer {contestant}"))
            .header("If-None-Match", &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 304);
    }

    #[tokio::test]
    async fn participant_can_list_attachments_via_problem_endpoint() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("cladm1", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Listed problem").await;
        let contest_id = app
            .create_contest(&admin, "List contest", false, true)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin)
            .await;

        app.upload_attachment(problem_id, "a.png", b"aaa".to_vec(), None, &admin)
            .await;
        app.upload_attachment(problem_id, "b.png", b"bbb".to_vec(), None, &admin)
            .await;

        let contestant = enroll_contestant(&app, "cluser1", contest_id, &admin).await;

        let res = app
            .get_with_token(&routes::attachments(problem_id), &contestant)
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["total"].as_u64().unwrap(), 2);
    }

    #[tokio::test]
    async fn non_participant_gets_404_for_list() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("cladm2", "pass1234", "admin")
            .await;
        let problem_id = app.create_hidden_problem(&admin, "Private problem").await;
        let contest_id = app
            .create_contest(&admin, "Private contest", false, true)
            .await;
        app.add_problem_to_contest(contest_id, problem_id, &admin)
            .await;

        let outsider = app
            .create_user_with_role("cluser2", "pass1234", "contestant")
            .await;

        let res = app
            .get_with_token(&routes::attachments(problem_id), &outsider)
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn contestant_cannot_upload_even_with_contest_access() {
        let app = TestApp::spawn().await;
        let (contest_id, problem_id, _ref_id, admin_token) =
            setup_contest_with_attachment(&app, "cadm7", false).await;
        let contestant = enroll_contestant(&app, "cuser7", contest_id, &admin_token).await;

        let res = app
            .upload_attachment(problem_id, "evil.txt", b"data".to_vec(), None, &contestant)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn contestant_cannot_delete_even_with_contest_access() {
        let app = TestApp::spawn().await;
        let (contest_id, problem_id, ref_id, admin_token) =
            setup_contest_with_attachment(&app, "cadm8", false).await;
        let contestant = enroll_contestant(&app, "cuser8", contest_id, &admin_token).await;

        let res = app
            .delete_with_token(&routes::attachment(problem_id, &ref_id), &contestant)
            .await;
        assert_eq!(res.status, 403);
    }
}
