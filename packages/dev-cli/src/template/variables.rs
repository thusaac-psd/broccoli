use anyhow::{Result, bail};

pub struct TemplateVars {
    pub plugin_name: String,
    pub plugin_name_snake: String,
    pub plugin_name_pascal: String,
    pub server_sdk_dep: String,
    pub web_sdk_dep: String,
    pub web_root: String,
}

/// Check `name` as a new plugin's id: the one plugin id rule the server
/// applies (`check_plugin_id`), so a scaffolded plugin always loads, narrowed
/// to lowercase letters and digits in words joined by single hyphens, since
/// its crate and npm package are named after it. Both refusals read as the
/// id rule's, naming the id and the rule it breaks.
pub fn validate_plugin_name(name: &str) -> Result<()> {
    broccoli_api_types::plugin_id::check_plugin_id(name)?;
    let conventional = name.split('-').all(|word| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if !conventional {
        bail!(
            "Invalid plugin ID '{name}': a new plugin's ID must be lowercase letters and digits \
             in words joined by single hyphens, since its crate and npm package are named after it"
        );
    }
    Ok(())
}

pub fn to_snake_case(name: &str) -> String {
    name.replace('-', "_")
}

pub fn to_pascal_case(name: &str) -> String {
    name.split('-')
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + chars.as_str()
                }
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_plugin_name_valid() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("plugin123").is_ok());
        assert!(validate_plugin_name("a").is_ok());
        assert!(validate_plugin_name("ioi-contest").is_ok());
    }

    #[test]
    fn test_validate_plugin_name_invalid() {
        assert!(validate_plugin_name("").is_err());
        assert!(validate_plugin_name("-leading").is_err());
        assert!(validate_plugin_name("trailing-").is_err());
        assert!(validate_plugin_name("double--hyphen").is_err());
        assert!(validate_plugin_name("Upper").is_err());
        assert!(validate_plugin_name("has space").is_err());
        assert!(validate_plugin_name("has_underscore").is_err());
    }

    /// Every refusal of a plugin name speaks as the one plugin id rule does,
    /// naming the id and the rule, whether the shared rule or the scaffold's
    /// narrower convention refused it.
    #[test]
    fn every_refusal_reads_as_the_plugin_id_rule() {
        for name in [
            "",
            "-leading",
            "trailing-",
            "double--hyphen",
            "Upper",
            "has space",
            "has_underscore",
            "users",
        ] {
            let error = validate_plugin_name(name).expect_err(name).to_string();
            assert!(
                error.starts_with(&format!("Invalid plugin ID '{name}': ")),
                "{name:?}: {error}"
            );
        }
    }

    /// A scaffolded plugin's id passes the server's one id rule: a host
    /// message namespace is refused here as the server refuses it.
    #[test]
    fn a_host_namespace_is_not_a_plugin_name() {
        for host in ["users", "toast", "plugin", "config", "identifier"] {
            let error = validate_plugin_name(host).expect_err(host).to_string();
            assert!(error.contains("namespace"), "{host}: {error}");
        }
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("my-plugin"), "my_plugin");
        assert_eq!(to_snake_case("ioi-contest"), "ioi_contest");
        assert_eq!(to_snake_case("simple"), "simple");
    }

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("my-plugin"), "MyPlugin");
        assert_eq!(to_pascal_case("ioi-contest"), "IoiContest");
        assert_eq!(to_pascal_case("simple"), "Simple");
        assert_eq!(to_pascal_case("a-b-c"), "ABC");
    }
}
