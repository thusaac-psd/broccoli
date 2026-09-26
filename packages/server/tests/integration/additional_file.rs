use crate::common::{TestApp, routes};

/// `problem:edit` is the sole gate on every `handlers/additional_file.rs`
/// endpoint (these are judge-private files, never contestant-facing) - this
/// pins that gate (previously untested) for the guard test's
/// `visibility-bypass-audited` comment above that file's entity import.
mod problem_edit_permission {
    use super::*;

    #[tokio::test]
    async fn contestant_cannot_upload_additional_file() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_af_upload", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("contestant_af_upload", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected problem").await;

        let res = app
            .upload_additional_file(
                problem_id,
                "extra.txt",
                b"data".to_vec(),
                "cpp",
                &contestant,
            )
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_list_additional_files() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_af_list", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("contestant_af_list", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected problem").await;

        let res = app
            .get_with_token(&routes::additional_files(problem_id), &contestant)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_download_additional_file() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_af_dl", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("contestant_af_dl", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected problem").await;

        let upload_res = app
            .upload_additional_file(problem_id, "extra.txt", b"data".to_vec(), "cpp", &admin)
            .await;
        assert_eq!(
            upload_res.status, 201,
            "setup upload failed: {}",
            upload_res.text
        );
        let ref_id = upload_res.body["id"].as_str().unwrap();

        let res = app
            .get_with_token(&routes::additional_file(problem_id, ref_id), &contestant)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn contestant_cannot_delete_additional_file() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_af_del", "pass1234", "admin")
            .await;
        let contestant = app
            .create_user_with_role("contestant_af_del", "pass1234", "contestant")
            .await;
        let problem_id = app.create_problem(&admin, "Protected problem").await;

        let upload_res = app
            .upload_additional_file(problem_id, "extra.txt", b"data".to_vec(), "cpp", &admin)
            .await;
        assert_eq!(
            upload_res.status, 201,
            "setup upload failed: {}",
            upload_res.text
        );
        let ref_id = upload_res.body["id"].as_str().unwrap();

        let res = app
            .delete_with_token(&routes::additional_file(problem_id, ref_id), &contestant)
            .await;
        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn admin_can_upload_list_download_and_delete_additional_file() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_af_full", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Owned problem").await;

        let upload_res = app
            .upload_additional_file(problem_id, "extra.txt", b"data".to_vec(), "cpp", &admin)
            .await;
        assert_eq!(upload_res.status, 201, "upload failed: {}", upload_res.text);
        let ref_id = upload_res.body["id"].as_str().unwrap().to_string();

        let list_res = app
            .get_with_token(&routes::additional_files(problem_id), &admin)
            .await;
        assert_eq!(list_res.status, 200);
        assert_eq!(list_res.body["total"], 1);

        let download_res = app
            .get_with_token(&routes::additional_file(problem_id, &ref_id), &admin)
            .await;
        assert_eq!(download_res.status, 200);

        let delete_res = app
            .delete_with_token(&routes::additional_file(problem_id, &ref_id), &admin)
            .await;
        assert_eq!(delete_res.status, 204);
    }
}
