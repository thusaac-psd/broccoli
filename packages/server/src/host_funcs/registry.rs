use crate::registry::{
    CheckerStageHandlers, CheckerStageRegistry, ContestTypeHandlers, ContestTypeRegistry,
    EvaluatorFunctions, EvaluatorHandler, EvaluatorRegistry, LanguageResolverEntry,
    LanguageResolverRegistry,
};
use extism::{Function, UserData, Val, ValType};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct RegisterContestTypeInput {
    #[serde(rename = "type")]
    contest_type: String,
    submission_handler: String,
    code_run_handler: String,
    #[serde(default)]
    submission_admission: bool,
}

#[derive(Deserialize)]
struct RegisterEvaluatorInput {
    #[serde(rename = "type")]
    problem_type: String,
    #[serde(flatten)]
    functions: RegisterEvaluatorFunctions,
}

/// Either one direct `handler`, or a staged `prepare_handler` +
/// `interpret_handler` pair (see [`EvaluatorFunctions`]).
#[derive(Deserialize)]
#[serde(untagged)]
enum RegisterEvaluatorFunctions {
    Staged {
        prepare_handler: String,
        interpret_handler: String,
    },
    Direct {
        handler: String,
    },
}

impl RegisterEvaluatorFunctions {
    fn into_functions(self) -> Result<EvaluatorFunctions, extism::Error> {
        let functions = match self {
            Self::Staged {
                prepare_handler,
                interpret_handler,
            } => EvaluatorFunctions::Staged {
                prepare: prepare_handler,
                interpret: interpret_handler,
            },
            Self::Direct { handler } => EvaluatorFunctions::Direct { function: handler },
        };
        let names: &[&String] = match &functions {
            EvaluatorFunctions::Direct { function } => &[function],
            EvaluatorFunctions::Staged { prepare, interpret } => &[prepare, interpret],
        };
        if names.iter().any(|name| name.is_empty()) {
            return Err(extism::Error::msg(
                "evaluator handler names must not be empty",
            ));
        }
        Ok(functions)
    }
}

#[derive(Deserialize)]
struct RegisterCheckerResolverInput {
    #[serde(rename = "format")]
    checker_format: String,
    resolve_handler: String,
    interpret_handler: String,
}

#[derive(Deserialize)]
struct RegisterLanguageResolverInput {
    language_id: String,
    function_name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default = "default_source_filename")]
    default_filename: String,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    template: String,
}

fn default_source_filename() -> String {
    "solution.txt".to_string()
}

struct RegistryContext {
    plugin_id: String,
    contest_type_registry: ContestTypeRegistry,
    evaluator_registry: EvaluatorRegistry,
    checker_stage_registry: CheckerStageRegistry,
    language_resolver_registry: LanguageResolverRegistry,
}

impl super::HostFnContext for RegistryContext {
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}

type RegistryUserData = UserData<Arc<RegistryContext>>;

pub fn create_registry_functions(
    plugin_id: String,
    contest_type_registry: ContestTypeRegistry,
    evaluator_registry: EvaluatorRegistry,
    checker_stage_registry: CheckerStageRegistry,
    language_resolver_registry: LanguageResolverRegistry,
) -> Vec<Function> {
    let user_data = super::host_data(RegistryContext {
        plugin_id,
        contest_type_registry,
        evaluator_registry,
        checker_stage_registry,
        language_resolver_registry,
    });

    vec![
        Function::new(
            "register_contest_type",
            [ValType::I64],
            [],
            user_data.clone(),
            register_contest_type_fn,
        ),
        Function::new(
            "register_evaluator",
            [ValType::I64],
            [],
            user_data.clone(),
            register_evaluator_fn,
        ),
        Function::new(
            "register_checker_resolver",
            [ValType::I64],
            [],
            user_data.clone(),
            register_checker_resolver_fn,
        ),
        Function::new(
            "register_language_resolver",
            [ValType::I64],
            [],
            user_data,
            register_language_resolver_fn,
        ),
    ]
}

fn register_contest_type_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: RegistryUserData,
) -> Result<(), extism::Error> {
    let call = super::HostCall::enter(&user_data, "register_contest_type")?;
    let (plugin_id, registry) = (&call.plugin_id, &call.contest_type_registry);

    let input: RegisterContestTypeInput = super::read_json(plugin, inputs, "input")?;

    let key = input.contest_type;
    validate_registry_id(&key, "contest_type")?;
    if input.submission_handler.is_empty() || input.code_run_handler.is_empty() {
        return Err(extism::Error::msg(
            "submission_handler and code_run_handler must not be empty",
        ));
    }

    tokio::runtime::Handle::current().block_on(async {
        let mut registry = registry.write().await;
        registry.insert(
            key.clone(),
            ContestTypeHandlers {
                plugin_id: plugin_id.to_string(),
                submission_fn: input.submission_handler.clone(),
                code_run_fn: input.code_run_handler.clone(),
                submission_admission: input.submission_admission,
            },
        );
        tracing::info!(
            plugin_id = %plugin_id,
            key = %key,
            submission_fn = %input.submission_handler,
            code_run_fn = %input.code_run_handler,
            "Contest type registered"
        );
    });

    Ok(())
}

fn register_evaluator_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: RegistryUserData,
) -> Result<(), extism::Error> {
    let call = super::HostCall::enter(&user_data, "register_evaluator")?;
    let (plugin_id, registry) = (&call.plugin_id, &call.evaluator_registry);
    let input: RegisterEvaluatorInput = super::read_json(plugin, inputs, "input")?;
    validate_registry_id(&input.problem_type, "problem_type")?;
    let functions = input.functions.into_functions()?;

    tokio::runtime::Handle::current().block_on(async {
        tracing::info!(
            plugin_id = %plugin_id,
            problem_type = %input.problem_type,
            functions = ?functions,
            "Evaluator registered"
        );
        registry.write().await.insert(
            input.problem_type,
            EvaluatorHandler {
                plugin_id: plugin_id.to_string(),
                functions,
            },
        );
    });
    Ok(())
}

fn register_checker_resolver_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: RegistryUserData,
) -> Result<(), extism::Error> {
    let call = super::HostCall::enter(&user_data, "register_checker_resolver")?;
    let (plugin_id, registry) = (&call.plugin_id, &call.checker_stage_registry);

    let input: RegisterCheckerResolverInput = super::read_json(plugin, inputs, "input")?;

    validate_registry_id(&input.checker_format, "checker_format")?;
    if input.resolve_handler.is_empty() || input.interpret_handler.is_empty() {
        return Err(extism::Error::msg(
            "resolve_handler and interpret_handler must not be empty",
        ));
    }

    tokio::runtime::Handle::current().block_on(async {
        let mut registry = registry.write().await;
        registry.insert(
            input.checker_format.clone(),
            CheckerStageHandlers {
                plugin_id: plugin_id.to_string(),
                resolve_fn: input.resolve_handler.clone(),
                interpret_fn: input.interpret_handler.clone(),
            },
        );
        tracing::info!(
            plugin_id = %plugin_id,
            checker_format = %input.checker_format,
            resolve_fn = %input.resolve_handler,
            interpret_fn = %input.interpret_handler,
            "Checker resolver registered"
        );
    });

    Ok(())
}

fn validate_registry_id(id: &str, field_name: &str) -> Result<(), extism::Error> {
    if id.is_empty() || id.len() > 128 {
        return Err(extism::Error::msg(format!(
            "{field_name} must be 1-128 characters"
        )));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(extism::Error::msg(format!(
            "{field_name} must contain only letters, digits, hyphens, and underscores"
        )));
    }
    Ok(())
}

fn register_language_resolver_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: RegistryUserData,
) -> Result<(), extism::Error> {
    let call = super::HostCall::enter(&user_data, "register_language_resolver")?;
    let (plugin_id, registry) = (&call.plugin_id, &call.language_resolver_registry);

    let input: RegisterLanguageResolverInput = super::read_json(plugin, inputs, "input")?;

    validate_registry_id(&input.language_id, "language_id")?;
    if input.function_name.is_empty() {
        return Err(extism::Error::msg("function_name must not be empty"));
    }

    let display_name = if input.display_name.is_empty() {
        input.language_id.clone()
    } else {
        input.display_name
    };

    let extensions: Vec<String> = input
        .extensions
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();

    tokio::runtime::Handle::current().block_on(async {
        let mut registry = registry.write().await;
        registry.insert(
            input.language_id.clone(),
            LanguageResolverEntry {
                plugin_id: plugin_id.to_string(),
                function_name: input.function_name.clone(),
                display_name,
                default_filename: input.default_filename,
                extensions,
                template: input.template,
            },
        );
        tracing::info!(
            plugin_id = %plugin_id,
            language_id = %input.language_id,
            handler = %input.function_name,
            "Language resolver registered"
        );
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> Result<EvaluatorFunctions, extism::Error> {
        serde_json::from_value::<RegisterEvaluatorInput>(json)
            .unwrap()
            .functions
            .into_functions()
    }

    #[test]
    fn evaluator_registration_declares_direct_or_staged_functions() {
        assert_eq!(
            parse(serde_json::json!({"type": "t", "handler": "eval"})).unwrap(),
            EvaluatorFunctions::Direct {
                function: "eval".into()
            }
        );
        assert_eq!(
            parse(serde_json::json!({
                "type": "t",
                "prepare_handler": "prep",
                "interpret_handler": "interp",
            }))
            .unwrap(),
            EvaluatorFunctions::Staged {
                prepare: "prep".into(),
                interpret: "interp".into(),
            }
        );
    }

    #[test]
    fn evaluator_registration_rejects_empty_function_names() {
        assert!(parse(serde_json::json!({"type": "t", "handler": ""})).is_err());
        assert!(
            parse(serde_json::json!({
                "type": "t",
                "prepare_handler": "prep",
                "interpret_handler": "",
            }))
            .is_err()
        );
    }
}
