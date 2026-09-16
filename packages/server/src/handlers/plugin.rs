use axum::{Json, extract::State};
use plugin_core::registry::PluginStatus;
use tracing::instrument;

use crate::error::AppError;
use crate::models::plugin::{
    ActivePluginResponse, ContestTypeEntry, EvaluatorEntry, HookEntryInfo, LanguageRegistryItem,
    RegistriesResponse,
};
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "/registries",
    tag = "Plugins",
    operation_id = "listRegistries",
    summary = "List available registry values",
    description = "Returns the currently registered problem types, checker formats, and contest types. These values are populated by loaded plugins.",
    responses(
        (status = 200, description = "Available registry values", body = RegistriesResponse),
    ),
)]
#[instrument(skip(state))]
pub async fn list_registries(
    State(state): State<AppState>,
) -> Result<Json<RegistriesResponse>, AppError> {
    let mut evaluators: Vec<EvaluatorEntry> = {
        let reg = state.registries.evaluator_registry.read().await;
        reg.iter()
            .map(|(problem_type, h)| EvaluatorEntry {
                problem_type: problem_type.clone(),
                plugin_id: h.plugin_id.clone(),
                function_name: h.function_name.clone(),
            })
            .collect()
    };
    evaluators.sort_by(|a, b| a.problem_type.cmp(&b.problem_type));
    let mut problem_types: Vec<String> =
        evaluators.iter().map(|e| e.problem_type.clone()).collect();
    problem_types.sort();

    // Selectable checker formats come from the fused checker-stage registry
    // (exact/lines/tokens*/testlib/none) - the legacy checker_format_registry was
    // removed with checker fusion.
    let mut checker_formats: Vec<String> = {
        let reg = state.registries.checker_stage_registry.read().await;
        reg.keys().cloned().collect()
    };
    checker_formats.sort();

    let mut contest_type_handlers: Vec<ContestTypeEntry> = {
        let reg = state.registries.contest_type_registry.read().await;
        reg.iter()
            .map(|(contest_type, h)| ContestTypeEntry {
                contest_type: contest_type.clone(),
                plugin_id: h.plugin_id.clone(),
                submission_fn: h.submission_fn.clone(),
                code_run_fn: h.code_run_fn.clone(),
            })
            .collect()
    };
    contest_type_handlers.sort_by(|a, b| a.contest_type.cmp(&b.contest_type));
    let mut contest_types: Vec<String> = contest_type_handlers
        .iter()
        .map(|e| e.contest_type.clone())
        .collect();
    contest_types.sort();

    let mut hooks: Vec<HookEntryInfo> = {
        let reg = state.registries.hook_registry.read().await;
        reg.iter_summaries()
            .into_iter()
            .map(|(topic, plugin_id, scope, mode)| HookEntryInfo {
                topic,
                plugin_id,
                scope: format!("{:?}", scope).to_lowercase(),
                mode: format!("{:?}", mode).to_lowercase(),
            })
            .collect()
    };
    hooks.sort_by(|a, b| {
        a.topic
            .cmp(&b.topic)
            .then_with(|| a.plugin_id.cmp(&b.plugin_id))
    });

    let mut languages: Vec<LanguageRegistryItem> = state
        .registries
        .language_resolver_registry
        .read()
        .await
        .iter()
        .map(|(id, entry)| LanguageRegistryItem {
            id: id.clone(),
            name: entry.display_name.clone(),
            default_filename: entry.default_filename.clone(),
            extensions: entry.extensions.clone(),
            template: entry.template.clone(),
        })
        .collect();
    languages.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Json(RegistriesResponse {
        problem_types,
        checker_formats,
        contest_types,
        languages,
        evaluators,
        contest_type_handlers,
        hooks,
    }))
}

#[utoipa::path(
    get,
    path = "/active",
    tag = "Plugins",
    operation_id = "listActivePlugins",
    summary = "List active plugins with web components",
    description = "Returns a list of currently active (loaded) plugins that have web (frontend) components. This is used by the frontend to discover which plugins are available for rendering UI.",
    responses(
        (status = 200, description = "List of active plugins", body = Vec<ActivePluginResponse>),
    ),
)]
#[instrument(skip(state))]
pub async fn list_active_plugins(
    State(state): State<AppState>,
) -> Result<Json<Vec<ActivePluginResponse>>, AppError> {
    let active_plugins = state
        .plugins
        .list_plugins()
        .map_err(AppError::from)?
        .into_iter()
        .filter(|p| p.status == PluginStatus::Loaded && p.manifest.has_web())
        .map(ActivePluginResponse::from)
        .collect();

    Ok(Json(active_plugins))
}
