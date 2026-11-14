use axum::http::{HeaderValue, header};
use tower_http::set_header::SetResponseHeaderLayer;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::extractors::body_limit::WithBodyLimit;
use crate::handlers;
use crate::state::AppState;

pub fn routes(settings: super::RouteSettings) -> OpenApiRouter<AppState> {
    let submission_max_size = settings.submission_max_size;

    OpenApiRouter::new()
        .routes(routes!(handlers::meta::get_version))
        .routes(routes!(handlers::health::get_health))
        .nest("/auth", auth_routes())
        .nest("/users", user_routes())
        .nest("/roles", role_routes())
        .nest("/admin", admin_routes())
        .nest("/plugins", plugin_routes())
        .nest("/p", proxy_routes())
        .nest("/i18n", i18n_routes())
        .nest("/config/upload", config_upload_routes())
        .nest("/problems", problem_routes(submission_max_size))
        .nest("/contests", contest_routes(submission_max_size))
        .nest("/submissions", submission_routes())
        .nest("/code-runs", code_run_routes())
        .nest("/dlq", dlq_routes())
        .nest("/telemetry", telemetry_routes())
}

fn telemetry_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::telemetry::report_error))
        .routes(routes!(handlers::telemetry::report_vitals))
        .body_limit(handlers::telemetry::telemetry_body_limit())
}

fn auth_routes() -> OpenApiRouter<AppState> {
    // Every auth response is session material (tokens, refresh cookies, the
    // caller's identity): mark the whole group `no-store` so neither browsers,
    // intermediaries nor the idempotency cache ever persist it.
    OpenApiRouter::new()
        .routes(routes!(handlers::auth::register))
        .routes(routes!(handlers::auth::get_registration_status))
        .routes(routes!(handlers::auth::login))
        .routes(routes!(handlers::auth::request_device_code))
        .routes(routes!(handlers::auth::refresh))
        .routes(routes!(handlers::auth::logout))
        .routes(routes!(handlers::auth::me))
        .routes(routes!(handlers::auth::authorize_device))
        .routes(routes!(handlers::auth::poll_device_token))
        .routes(routes!(handlers::auth::issue_cli_token))
        .routes(routes!(handlers::auth::cli_refresh))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

fn user_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::user::list_users))
        .routes(routes!(handlers::user::create_user))
        .routes(routes!(handlers::user::bulk_create_users))
        .routes(routes!(handlers::user::get_user))
        .routes(routes!(handlers::user::delete_user))
        .routes(routes!(handlers::user::update_user))
        .routes(routes!(handlers::user::assign_role))
        .routes(routes!(handlers::user::revoke_role))
}

fn role_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::role::list_roles))
        .routes(routes!(handlers::role::list_role_permissions))
        .routes(routes!(handlers::role::grant_permission_to_role))
        .routes(routes!(handlers::role::revoke_permission_from_role))
}

fn admin_routes() -> OpenApiRouter<AppState> {
    let upload = OpenApiRouter::new()
        .routes(routes!(
            handlers::admin::upload_plugin,
            handlers::admin::get_plugin_upload_policy
        ))
        .routes(routes!(handlers::admin::inspect_plugin_archive))
        .body_limit(handlers::admin::upload_body_limit());

    OpenApiRouter::new()
        .routes(routes!(handlers::admin::list_all_plugins))
        .routes(routes!(handlers::admin::get_plugin_details))
        .routes(routes!(handlers::admin::enable_plugin))
        .routes(routes!(handlers::admin::disable_plugin))
        .routes(routes!(handlers::admin::get_plugin_dependents))
        .routes(routes!(handlers::admin::reload_plugin))
        .routes(routes!(handlers::admin::reload_all_plugins))
        .routes(routes!(handlers::admin::remove_plugin_upload))
        .routes(routes!(handlers::submission::admin_fan_out_submission))
        .routes(routes!(handlers::settings::set_registration_status))
        .merge(upload)
        .nest("/plugins/{id}/config", plugin_global_config_routes())
        .nest("/system", system_routes())
}

fn system_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::system::list_workers))
        .routes(routes!(handlers::system::list_queues))
        .routes(routes!(handlers::system::system_overview))
}

fn plugin_global_config_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::plugin_config::list_plugin_global_config))
        .routes(routes!(
            handlers::plugin_config::get_plugin_global_config,
            handlers::plugin_config::upsert_plugin_global_config,
            handlers::plugin_config::delete_plugin_global_config,
        ))
}

fn config_upload_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::config_upload::upload_config_blob))
        .body_limit(handlers::config_upload::config_upload_body_limit())
}

fn plugin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::plugin::list_registries))
        .routes(routes!(handlers::plugin::list_active_plugins))
}

fn proxy_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(
        handlers::proxy::get_plugin_request,
        handlers::proxy::post_plugin_request,
        handlers::proxy::put_plugin_request,
        handlers::proxy::delete_plugin_request,
        handlers::proxy::patch_plugin_request,
        handlers::proxy::options_plugin_request,
        handlers::proxy::head_plugin_request,
        handlers::proxy::trace_plugin_request,
    ))
}

fn i18n_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::i18n::get_locales))
        .routes(routes!(handlers::i18n::get_translations))
}

fn problem_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::problem::list_problems,
            handlers::problem::create_problem,
        ))
        .routes(routes!(
            handlers::problem::get_problem,
            handlers::problem::update_problem,
            handlers::problem::delete_problem,
        ))
        .nest("/{id}/test-cases", test_case_routes())
        .nest("/{id}/attachments", attachment_routes())
        .nest("/{id}/additional-files", additional_file_routes())
        .nest("/{id}/checker-source", checker_source_routes())
        .nest("/{id}/config", problem_config_routes())
        .nest(
            "/{id}/submissions",
            problem_submission_routes(submission_max_size),
        )
        .nest(
            "/{id}/code-runs",
            problem_code_run_routes(submission_max_size),
        )
}

fn problem_submission_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::submission::create_submission))
        .body_limit(handlers::submission::submission_body_limit(
            submission_max_size,
        ))
}

fn checker_source_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::problem::get_checker_source,
            handlers::problem::upload_checker_source,
            handlers::problem::delete_checker_source,
        ))
        .body_limit(handlers::problem::checker_source_body_limit())
}

fn test_case_routes() -> OpenApiRouter<AppState> {
    let crud = OpenApiRouter::new()
        .routes(routes!(
            handlers::problem::list_test_cases,
            handlers::problem::create_test_case,
        ))
        .routes(routes!(handlers::problem::reorder_test_cases))
        .routes(routes!(handlers::problem::bulk_delete_test_cases))
        .routes(routes!(
            handlers::problem::get_test_case,
            handlers::problem::update_test_case,
            handlers::problem::delete_test_case,
        ))
        .body_limit(handlers::problem::test_case_body_limit());

    let upload = OpenApiRouter::new()
        .routes(routes!(handlers::problem::upload_test_cases))
        .body_limit(handlers::problem::upload_body_limit());

    crud.merge(upload)
}

fn attachment_routes() -> OpenApiRouter<AppState> {
    let read_delete = OpenApiRouter::new()
        .routes(routes!(handlers::attachment::list_attachments))
        .routes(routes!(
            handlers::attachment::download_attachment,
            handlers::attachment::delete_attachment,
        ));

    let upload = OpenApiRouter::new()
        .routes(routes!(handlers::attachment::upload_attachment))
        .body_limit(handlers::attachment::attachment_upload_body_limit());

    read_delete.merge(upload)
}

fn additional_file_routes() -> OpenApiRouter<AppState> {
    let read_delete = OpenApiRouter::new()
        .routes(routes!(handlers::additional_file::list_additional_files))
        .routes(routes!(
            handlers::additional_file::download_additional_file,
            handlers::additional_file::delete_additional_file,
        ));

    let upload = OpenApiRouter::new()
        .routes(routes!(handlers::additional_file::upload_additional_file))
        .body_limit(handlers::additional_file::additional_file_upload_body_limit());

    read_delete.merge(upload)
}

fn contest_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::contest::list_contests,
            handlers::contest::create_contest,
        ))
        .routes(routes!(
            handlers::contest::get_contest,
            handlers::contest::update_contest,
            handlers::contest::delete_contest,
        ))
        .routes(routes!(handlers::contest::get_contest_my_info))
        .routes(routes!(
            handlers::warm::prewarm_contest,
            handlers::warm::prewarm_status,
        ))
        .nest(
            "/{id}/problems",
            contest_problem_routes(submission_max_size),
        )
        .nest("/{id}/participants", contest_participant_routes())
        .nest("/{id}/submissions", contest_submission_routes())
        .nest("/{id}/config", contest_config_routes())
        .nest("/{id}/clarifications", clarification_routes())
        .routes(routes!(
            handlers::contest::register_for_contest,
            handlers::contest::unregister_from_contest,
        ))
}

fn clarification_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::clarification::list_clarifications,
            handlers::clarification::create_clarification,
        ))
        .routes(routes!(handlers::clarification::reply_clarification))
        .routes(routes!(handlers::clarification::toggle_reply_public))
        .routes(routes!(handlers::clarification::resolve_clarification))
}

fn contest_submission_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(handlers::submission::list_contest_submissions))
}

fn contest_problem_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::contest::list_contest_problems,
            handlers::contest::add_contest_problem,
        ))
        .routes(routes!(handlers::contest::reorder_contest_problems))
        .routes(routes!(handlers::contest::bulk_delete_contest_problems))
        .routes(routes!(
            handlers::problem::get_contest_problem,
            handlers::contest::update_contest_problem,
            handlers::contest::remove_contest_problem,
        ))
        .routes(routes!(handlers::contest::get_contest_problem_samples))
        .routes(routes!(handlers::problem::get_contest_problem_sample))
        .routes(routes!(
            handlers::attachment::list_contest_problem_attachments
        ))
        .routes(routes!(
            handlers::attachment::download_contest_problem_attachment
        ))
        .nest("/{problem_id}/config", contest_problem_config_routes())
        .nest(
            "/{problem_id}/submissions",
            contest_problem_submission_routes(submission_max_size),
        )
        .nest(
            "/{problem_id}/code-runs",
            contest_problem_code_run_routes(submission_max_size),
        )
}

fn contest_problem_submission_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::submission::create_contest_submission))
        .body_limit(handlers::submission::submission_body_limit(
            submission_max_size,
        ))
}

fn contest_participant_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::contest::list_participants,
            handlers::contest::add_participant,
        ))
        .routes(routes!(handlers::contest::bulk_add_participants))
        .routes(routes!(handlers::contest::remove_participant))
}

fn submission_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::submission::list_submissions))
        .routes(routes!(handlers::submission::bulk_rejudge_submissions))
        .routes(routes!(handlers::submission::get_submission))
        .routes(routes!(handlers::submission::list_submission_judgements))
        .routes(routes!(handlers::submission::get_submission_judgement))
        .routes(routes!(handlers::submission::apply_submission_judgement))
        .routes(routes!(handlers::submission::discard_submission_judgement))
        .routes(routes!(handlers::submission::rejudge_submission))
}

fn problem_config_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::plugin_config::list_problem_config))
        .routes(routes!(
            handlers::plugin_config::get_problem_config,
            handlers::plugin_config::upsert_problem_config,
            handlers::plugin_config::delete_problem_config,
        ))
}

fn contest_config_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::plugin_config::list_contest_config))
        .routes(routes!(
            handlers::plugin_config::get_contest_config,
            handlers::plugin_config::upsert_contest_config,
            handlers::plugin_config::delete_contest_config,
        ))
}

fn contest_problem_config_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            handlers::plugin_config::list_contest_problem_config
        ))
        .routes(routes!(
            handlers::plugin_config::get_contest_problem_config,
            handlers::plugin_config::upsert_contest_problem_config,
            handlers::plugin_config::delete_contest_problem_config,
        ))
}

fn code_run_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(handlers::code_run::get_code_run))
}

fn problem_code_run_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::code_run::run_code))
        .body_limit(handlers::code_run::code_run_body_limit(submission_max_size))
}

fn contest_problem_code_run_routes(submission_max_size: usize) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::code_run::run_contest_code))
        .body_limit(handlers::code_run::code_run_body_limit(submission_max_size))
}

fn dlq_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::dlq::list_dlq_messages))
        .routes(routes!(handlers::dlq::get_dlq_stats))
        .routes(routes!(handlers::dlq::bulk_retry_dlq))
        .routes(routes!(handlers::dlq::bulk_delete_dlq))
        .routes(routes!(
            handlers::dlq::get_dlq_message,
            handlers::dlq::delete_dlq_message,
        ))
        .routes(routes!(handlers::dlq::retry_dlq_message))
}
