use anyhow::{Result, bail};

pub struct TemplateVars {
    pub plugin_name: String,
    pub plugin_name_snake: String,
    pub plugin_name_pascal: String,
    pub server_sdk_dep: String,
    pub web_sdk_dep: String,
    pub web_root: String,
}

pub fn validate_plugin_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Plugin name cannot be empty");
    }
    // The one plugin id rule the server applies, so a scaffolded plugin
    // always loads; the scaffold's own naming convention narrows it below.
    broccoli_api_types::plugin_id::check_plugin_id(name)?;

    if name.starts_with('-') || name.ends_with('-') {
        bail!("Plugin name cannot start or end with a hyphen");
    }

    if name.contains("--") {
        bail!("Plugin name cannot contain consecutive hyphens");
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("Plugin name must contain only lowercase letters, digits, and hyphens");
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
