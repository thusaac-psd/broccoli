#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::types::ResolveLanguageInput;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde::Deserialize;

pub mod resolve;

/// Config namespace for compiler paths and flags (cascades at all scopes).
#[cfg(any(target_arch = "wasm32", test))]
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

    let Some(language) = resolve::language(&req.language_id) else {
        return Err(
            extism_pdk::Error::msg(format!("Unsupported language: {}", req.language_id)).into(),
        );
    };
    let lang_config = load_lang_config(&host, &req.language_id, req.problem_id, req.contest_id)?;

    // Explicit overrides from the caller (e.g. standard-checkers) take highest priority.
    let overrides: CompilerOverrides = req
        .overrides
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let entry_point_config = load_entry_point_config(&host, req.problem_id, &req.language_id)?;
    let extra_flags = entry_point_config
        .as_ref()
        .and_then(|c| c.extra_compile_flags.as_deref())
        .unwrap_or(&[]);

    let tools = lang_config.tools(&overrides, extra_flags);
    let result = language.resolve(&req, entry_point_config.as_ref(), &tools);

    Ok(serde_json::to_string(&result)?)
}

/// The compilation config of `language_id` where it is judged: the
/// namespace's effective config at the problem (see
/// `broccoli_server_sdk::types::ConfigSource`), or the deployment's when no
/// problem is named. A language the namespace has no entry for, or a config
/// that does not parse, is an error, never a silent fallback.
#[cfg(any(target_arch = "wasm32", test))]
fn load_lang_config(
    host: &Host,
    language_id: &str,
    problem_id: Option<i32>,
    contest_id: Option<i32>,
) -> Result<LanguageCompilationConfig, SdkError> {
    let config = match problem_id {
        Some(pid) => {
            host.config
                .get_effective(CONFIG_COMPILATION, pid, contest_id)?
                .config
        }
        None => host.config.get_global(CONFIG_COMPILATION)?.config,
    };
    parse_language_entry(&config, language_id)?.ok_or_else(|| {
        SdkError::Serialization(format!(
            "the {CONFIG_COMPILATION} config has no entry for language '{language_id}'"
        ))
    })
}

/// Load per-language entry-point config (problem scope only).
#[cfg(target_arch = "wasm32")]
fn load_entry_point_config(
    host: &Host,
    problem_id: Option<i32>,
    language_id: &str,
) -> Result<Option<EntryPointConfig>, SdkError> {
    let Some(problem_id) = problem_id else {
        return Ok(None);
    };
    let result = host.config.get_problem(problem_id, CONFIG_ENTRY_POINTS)?;
    if result.is_default {
        return Ok(None);
    }
    parse_language_entry(&result.config, language_id)
}

/// The entry for `language_id` in a per-language config map, if it has one.
#[cfg(any(target_arch = "wasm32", test))]
fn parse_language_entry<T: serde::de::DeserializeOwned>(
    config: &serde_json::Value,
    language_id: &str,
) -> Result<Option<T>, SdkError> {
    config
        .get(language_id)
        .map(|entry| {
            T::deserialize(entry).map_err(|e| {
                SdkError::Serialization(format!("config for language '{language_id}': {e}"))
            })
        })
        .transpose()
}

/// Per-language compilation config from the admin UI / plugin.toml schema defaults.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Deserialize, Default)]
struct LanguageCompilationConfig {
    compiler: Option<String>,
    interpreter: Option<String>,
    runner: Option<String>,
    flags: Option<Vec<String>>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl LanguageCompilationConfig {
    /// The tools a language resolves with: a caller's explicit compiler and
    /// flags win over the configured ones.
    fn tools<'a>(
        &'a self,
        overrides: &'a CompilerOverrides,
        extra_flags: &'a [String],
    ) -> resolve::Tools<'a> {
        resolve::Tools {
            tool: overrides
                .compiler
                .as_deref()
                .or(self.compiler.as_deref())
                .or(self.interpreter.as_deref())
                .unwrap_or(""),
            runner: self.runner.as_deref().unwrap_or(""),
            flags: overrides
                .flags
                .as_deref()
                .or(self.flags.as_deref())
                .unwrap_or(&[]),
            extra_flags,
        }
    }
}

/// Per-problem, per-language entry point config.
#[derive(Deserialize, Default)]
pub struct EntryPointConfig {
    pub entry_point: Option<String>,
    pub extra_compile_flags: Option<Vec<String>>,
}

/// Override schema accepted by this resolver in `ResolveLanguageInput.overrides`.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Deserialize, Default)]
struct CompilerOverrides {
    compiler: Option<String>,
    flags: Option<Vec<String>>,
}

#[cfg(test)]
mod tests;
