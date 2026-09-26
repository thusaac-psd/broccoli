#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::types::ResolveLanguageInput;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde::Deserialize;

pub mod resolve;

/// Config namespace for compiler paths and flags (cascades at all scopes).
#[cfg(target_arch = "wasm32")]
const CONFIG_COMPILATION: &str = "compilation";
/// Config namespace for per-problem, per-language entry point overrides.
#[cfg(target_arch = "wasm32")]
const CONFIG_ENTRY_POINTS: &str = "entry-points";

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();

    for lang in resolve::LANGUAGES {
        host.registry.register_language_resolver(
            lang.id,
            "resolve_standard_language",
            lang.display_name,
            lang.default_filename,
            lang.extensions,
            lang.template,
        )?;
    }

    host.log.info("Standard languages plugin initialized")?;
    Ok("ok".into())
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn resolve_standard_language(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: ResolveLanguageInput = serde_json::from_str(&input)?;

    // Config cascade returns schema defaults when nothing is explicitly set.
    let lang_config = load_lang_config(&host, &req.language_id, req.problem_id, req.contest_id);

    // Explicit overrides from the caller (e.g. standard-checkers) take highest priority.
    let overrides: CompilerOverrides = req
        .overrides
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let entry_point_config = load_entry_point_config(&host, req.problem_id, &req.language_id);
    let extra_compile_flags = entry_point_config
        .as_ref()
        .and_then(|c| c.extra_compile_flags.as_deref())
        .unwrap_or(&[]);

    let compiler = overrides
        .compiler
        .as_deref()
        .or(lang_config.compiler.as_deref())
        .unwrap_or("");
    let flags = overrides
        .flags
        .as_deref()
        .or(lang_config.flags.as_deref())
        .unwrap_or(&[]);

    let result = match req.language_id.as_str() {
        "c" => resolve::resolve_c(
            &req,
            entry_point_config.as_ref(),
            compiler,
            flags,
            extra_compile_flags,
        ),
        "cpp" => resolve::resolve_cpp(
            &req,
            entry_point_config.as_ref(),
            compiler,
            flags,
            extra_compile_flags,
        ),
        "python3" => resolve::resolve_python3(
            &req,
            entry_point_config.as_ref(),
            lang_config.interpreter.as_deref().unwrap_or(""),
        ),
        "java" => resolve::resolve_java(
            &req,
            entry_point_config.as_ref(),
            compiler,
            lang_config.runner.as_deref().unwrap_or(""),
            flags,
            extra_compile_flags,
        ),
        _ => {
            return Err(extism_pdk::Error::msg(format!(
                "Unsupported language: {}",
                req.language_id
            ))
            .into());
        }
    };

    Ok(serde_json::to_string(&result)?)
}

/// Load per-language compilation config from the cascade.
#[cfg(target_arch = "wasm32")]
fn load_lang_config(
    host: &Host,
    language_id: &str,
    problem_id: Option<i32>,
    contest_id: Option<i32>,
) -> LanguageCompilationConfig {
    let config_value = if let Some(pid) = problem_id {
        host.config
            .get_effective(CONFIG_COMPILATION, pid, contest_id)
            .ok()
            .filter(|r| r.source != broccoli_server_sdk::types::ConfigSource::Disabled)
            .map(|r| r.config)
    } else {
        host.config
            .get_global(CONFIG_COMPILATION)
            .ok()
            .map(|r| r.config)
    };

    config_value
        .as_ref()
        .and_then(|c| c.get(language_id))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Load per-language entry-point config (problem scope only).
#[cfg(target_arch = "wasm32")]
fn load_entry_point_config(
    host: &Host,
    problem_id: Option<i32>,
    language_id: &str,
) -> Option<EntryPointConfig> {
    let problem_id = problem_id?;
    let result = host
        .config
        .get_problem(problem_id, CONFIG_ENTRY_POINTS)
        .ok()?;
    if result.is_default {
        return None;
    }
    let config = result.config.as_object()?;
    let lang_value = config.get(language_id)?;
    serde_json::from_value(lang_value.clone()).ok()
}

/// Per-language compilation config from the admin UI / plugin.toml schema defaults.
/// Only constructed by the wasm32-gated `load_lang_config`/`resolve_standard_language`
/// entry points; gated the same way so a native (test/clippy) build doesn't see it
/// as dead code.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize, Default)]
struct LanguageCompilationConfig {
    compiler: Option<String>,
    interpreter: Option<String>,
    runner: Option<String>,
    flags: Option<Vec<String>>,
}

/// Per-problem, per-language entry point config.
#[derive(Deserialize, Default)]
pub struct EntryPointConfig {
    pub entry_point: Option<String>,
    pub extra_compile_flags: Option<Vec<String>>,
}

/// Override schema accepted by this resolver in `ResolveLanguageInput.overrides`.
/// Only constructed by the wasm32-gated `resolve_standard_language` entry point;
/// gated the same way so a native (test/clippy) build doesn't see it as dead code.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize, Default)]
struct CompilerOverrides {
    compiler: Option<String>,
    flags: Option<Vec<String>>,
}

#[cfg(test)]
mod tests;
