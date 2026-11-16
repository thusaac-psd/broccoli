//! Plugin config schemas: the `[config.<namespace>]` declarations of a
//! manifest are the one description of a plugin's configuration. The host
//! checks every write against them ([`validate`]) and hands a plugin its
//! config with every declared default filled in
//! ([`ConfigNamespace::resolve`]), so a plugin never invents defaults of its
//! own and never sees a value its schema forbids.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::manifest::{ConfigNamespace, PluginManifest, SchemaProperty};

pub use broccoli_api_types::plugin::FieldError;

/// The value types a schema property can declare.
const TYPES: &[&str] = &["string", "integer", "number", "boolean", "array", "object"];

/// Checks `config` against the namespace's schema. A namespace that declares
/// no properties accepts any object; otherwise every field must be declared.
pub fn validate(ns: &ConfigNamespace, config: &Value) -> Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    match config {
        Value::Object(fields) if !ns.properties.is_empty() => {
            validate_fields(&ns.properties, None, false, fields, "", &mut errors);
        }
        Value::Object(_) => {}
        other => errors.push(FieldError {
            path: String::new(),
            message: format!("expected an object, got {}", kind(other)),
        }),
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Checks that every namespace of a manifest is a schema the host can
/// enforce: known types, consistent bounds, patterns that compile, and
/// defaults and enum values that satisfy their own property. A schema that
/// fails this would reject its own defaults, so the plugin does not load.
pub fn check_manifest(manifest: &PluginManifest) -> Result<(), String> {
    for (name, ns) in &manifest.config {
        for (field, prop) in &ns.properties {
            check_property(prop, field)
                .map_err(|e| format!("config namespace '{name}', field {e}"))?;
        }
        validate(ns, &ns.defaults()).map_err(|errors| {
            format!(
                "config namespace '{name}': its defaults break its schema: {}",
                describe(&errors)
            )
        })?;
    }
    Ok(())
}

/// The errors as one line, for logs and messages.
pub fn describe(errors: &[FieldError]) -> String {
    errors
        .iter()
        .map(|e| {
            if e.path.is_empty() {
                e.message.clone()
            } else {
                format!("{}: {}", e.path, e.message)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn check_property(prop: &SchemaProperty, path: &str) -> Result<(), String> {
    if !TYPES.contains(&prop.schema_type.as_str()) {
        return Err(format!(
            "'{path}': unknown type '{}' (expected one of {})",
            prop.schema_type,
            TYPES.join(", ")
        ));
    }
    if let (Some(min), Some(max)) = (prop.min, prop.max)
        && min > max
    {
        return Err(format!("'{path}': min {min} exceeds max {max}"));
    }
    if let (Some(min), Some(max)) = (prop.min_length, prop.max_length)
        && min > max
    {
        return Err(format!(
            "'{path}': min_length {min} exceeds max_length {max}"
        ));
    }
    if let Some(pattern) = &prop.pattern {
        regex::Regex::new(pattern).map_err(|e| format!("'{path}': invalid pattern: {e}"))?;
    }
    if let Some(required) = &prop.required
        && let Some(missing) = required.iter().find(|r| !prop.properties.contains_key(*r))
    {
        return Err(format!("'{path}': requires undeclared field '{missing}'"));
    }
    let mut errors = Vec::new();
    if let Some(default) = &prop.default {
        validate_value(prop, default, path, &mut errors);
    }
    for value in prop.enum_values.iter().flatten() {
        let mut probe = prop.clone();
        probe.enum_values = None;
        validate_value(&probe, value, path, &mut errors);
    }
    if !errors.is_empty() {
        return Err(format!("'{path}': {}", describe(&errors)));
    }
    if let Some(items) = &prop.items {
        check_property(items, &format!("{path}[]"))?;
    }
    for (name, child) in &prop.properties {
        check_property(child, &join(path, name))?;
    }
    Ok(())
}

fn validate_fields(
    properties: &HashMap<String, SchemaProperty>,
    required: Option<&[String]>,
    additional: bool,
    fields: &Map<String, Value>,
    path: &str,
    errors: &mut Vec<FieldError>,
) {
    let mut names: Vec<&String> = fields.keys().collect();
    names.sort();
    for name in names {
        let field_path = join(path, name);
        match properties.get(name) {
            Some(prop) => validate_value(prop, &fields[name], &field_path, errors),
            None if additional => {}
            None => errors.push(FieldError {
                path: field_path,
                message: "unknown field".into(),
            }),
        }
    }
    for name in required.into_iter().flatten() {
        if !fields.contains_key(name) {
            errors.push(FieldError {
                path: join(path, name),
                message: "required field is missing".into(),
            });
        }
    }
}

fn validate_value(prop: &SchemaProperty, value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let mut fail = |message: String| {
        errors.push(FieldError {
            path: path.to_string(),
            message,
        })
    };
    let type_matches = match prop.schema_type.as_str() {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    };
    if !type_matches {
        fail(format!(
            "expected {}, got {}",
            prop.schema_type,
            kind(value)
        ));
        return;
    }
    if let Some(allowed) = &prop.enum_values
        && !allowed.contains(value)
    {
        let allowed: Vec<String> = allowed.iter().map(Value::to_string).collect();
        fail(format!("must be one of {}", allowed.join(", ")));
        return;
    }
    match value {
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = prop.min
                && n < min
            {
                fail(format!("must be at least {min}"));
            }
            if let Some(max) = prop.max
                && n > max
            {
                fail(format!("must be at most {max}"));
            }
        }
        Value::String(s) => {
            let len = s.chars().count();
            if let Some(min) = prop.min_length
                && len < min as usize
            {
                fail(format!("must be at least {min} characters"));
            }
            if let Some(max) = prop.max_length
                && len > max as usize
            {
                fail(format!("must be at most {max} characters"));
            }
            if let Some(pattern) = &prop.pattern
                && !regex::Regex::new(pattern).is_ok_and(|re| re.is_match(s))
            {
                fail(format!("must match the pattern {pattern}"));
            }
        }
        Value::Array(items) => {
            if let Some(item) = &prop.items {
                for (i, v) in items.iter().enumerate() {
                    validate_value(item, v, &format!("{path}[{i}]"), errors);
                }
            }
        }
        Value::Object(fields) if !prop.properties.is_empty() => validate_fields(
            &prop.properties,
            prop.required.as_deref(),
            prop.additional_properties == Some(true),
            fields,
            path,
            errors,
        ),
        _ => {}
    }
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ns(toml_src: &str) -> ConfigNamespace {
        toml::from_str(toml_src).expect("namespace parses")
    }

    fn icpc() -> ConfigNamespace {
        ns(r#"
            scopes = ["contest"]
            [properties.penalty_minutes]
            type = "integer"
            default = 20
            min = 0
            [properties.public_standings]
            type = "boolean"
            default = false
            [properties.freeze_minutes]
            type = "integer"
            default = 0
            min = 0
        "#)
    }

    fn paths(result: Result<(), Vec<FieldError>>) -> Vec<(String, String)> {
        result
            .expect_err("invalid")
            .into_iter()
            .map(|e| (e.path, e.message))
            .collect()
    }

    #[test]
    fn a_value_of_the_wrong_type_is_reported_at_its_path() {
        let errors = paths(validate(
            &icpc(),
            &json!({"freeze_minutes": 15, "penalty_minutes": "20"}),
        ));
        assert_eq!(
            errors,
            [(
                "penalty_minutes".into(),
                "expected integer, got string".into()
            )]
        );
    }

    #[test]
    fn bounds_enums_and_unknown_fields_are_enforced() {
        let errors = paths(validate(
            &icpc(),
            &json!({"penalty_minutes": -1, "bogus": 1}),
        ));
        assert_eq!(
            errors,
            [
                ("bogus".into(), "unknown field".into()),
                ("penalty_minutes".into(), "must be at least 0".into()),
            ]
        );

        let modes = ns(r#"
            [properties.scoring_mode]
            type = "string"
            enum = ["max_submission", "sum_best_subtask"]
            default = "max_submission"
        "#);
        let errors = paths(validate(&modes, &json!({"scoring_mode": "bogus"})));
        assert_eq!(errors[0].0, "scoring_mode");
        assert!(validate(&modes, &json!({"scoring_mode": "sum_best_subtask"})).is_ok());
    }

    #[test]
    fn nested_objects_and_array_items_are_checked_with_their_paths() {
        let task = ns(r#"
            [properties.subtasks]
            type = "array"
            [properties.subtasks.items]
            type = "object"
            [properties.subtasks.items.properties.max_score]
            type = "number"
            min = 0
            [properties.subtasks.items.properties.name]
            type = "string"
            default = ""
        "#);
        let errors = paths(validate(
            &task,
            &json!({"subtasks": [{"max_score": 10}, {"max_score": -5, "extra": true}]}),
        ));
        assert_eq!(
            errors,
            [
                ("subtasks[1].extra".into(), "unknown field".into()),
                ("subtasks[1].max_score".into(), "must be at least 0".into()),
            ]
        );
    }

    /// `step` is the form's increment, not a constraint: a score of 12.5 on
    /// a field stepping by 1 is fine.
    #[test]
    fn integers_reject_fractions_and_numbers_accept_both() {
        let schema = ns(r#"
            [properties.count]
            type = "integer"
            [properties.ratio]
            type = "number"
            step = 1.0
        "#);
        assert!(validate(&schema, &json!({"count": 3, "ratio": 12.5})).is_ok());
        assert!(validate(&schema, &json!({"ratio": 2})).is_ok());
        assert_eq!(
            paths(validate(&schema, &json!({"count": 1.5})))[0].0,
            "count"
        );
    }

    #[test]
    fn a_non_object_config_is_rejected() {
        assert_eq!(paths(validate(&icpc(), &json!([1])))[0].0, "");
        assert!(validate(&ns("scopes = []"), &json!({"anything": 1})).is_ok());
    }

    #[test]
    fn resolve_fills_every_default_the_stored_config_leaves_out() {
        assert_eq!(
            icpc().resolve(Some(&json!({"freeze_minutes": 15}))),
            json!({"freeze_minutes": 15, "penalty_minutes": 20, "public_standings": false})
        );
        assert_eq!(
            icpc().resolve(None),
            json!({"freeze_minutes": 0, "penalty_minutes": 20, "public_standings": false})
        );
    }

    #[test]
    fn resolve_fills_defaults_inside_nested_objects_and_array_items() {
        let schema = ns(r#"
            [properties.tokens]
            type = "object"
            [properties.tokens.properties.mode]
            type = "string"
            default = "none"
            [properties.tokens.properties.initial]
            type = "integer"
            default = 0
            [properties.subtasks]
            type = "array"
            default = []
            [properties.subtasks.items]
            type = "object"
            [properties.subtasks.items.properties.scoring_method]
            type = "string"
            default = "group_min"
        "#);
        assert_eq!(
            schema.resolve(Some(
                &json!({"tokens": {"initial": 2}, "subtasks": [{}, {"scoring_method": "sum"}]})
            )),
            json!({
                "tokens": {"mode": "none", "initial": 2},
                "subtasks": [{"scoring_method": "group_min"}, {"scoring_method": "sum"}],
            })
        );
    }

    #[test]
    fn a_manifest_whose_defaults_break_its_schema_is_refused() {
        let manifest: PluginManifest = toml::from_str(
            r#"
            name = "p"
            version = "1.0.0"
            [config.limits]
            scopes = ["contest"]
            [config.limits.properties.max]
            type = "integer"
            default = 5
            min = 10
        "#,
        )
        .unwrap();
        let err = check_manifest(&manifest).expect_err("default below min");
        assert!(
            err.contains("'max'") && err.contains("at least 10"),
            "{err}"
        );
    }

    #[test]
    fn a_manifest_with_an_unknown_type_is_refused() {
        let manifest: PluginManifest = toml::from_str(
            r#"
            name = "p"
            version = "1.0.0"
            [config.limits.properties.max]
            type = "int"
        "#,
        )
        .unwrap();
        let err = check_manifest(&manifest).expect_err("unknown type");
        assert!(err.contains("unknown type 'int'"), "{err}");
    }
}
