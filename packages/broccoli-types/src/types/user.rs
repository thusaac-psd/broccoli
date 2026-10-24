use serde::{Deserialize, Serialize};

/// Stable account identity with an optional human-readable name for legacy data.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserIdentity {
    pub user_id: i32,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl UserIdentity {
    /// Prefer the display name without changing the login name or ranking keys.
    pub fn display_label(&self) -> &str {
        self.display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.username)
    }
}

#[cfg(test)]
mod tests {
    use super::UserIdentity;
    use serde_json::json;

    #[test]
    fn labels_prefer_display_names_and_keep_legacy_accounts_readable() {
        for (display_name, expected) in [
            (json!("  选手甲  "), "选手甲"),
            (json!(null), "oj_01"),
            (json!(""), "oj_01"),
            (json!(" \t\n"), "oj_01"),
        ] {
            let user: UserIdentity = serde_json::from_value(json!({
                "user_id": 1, "username": "oj_01", "display_name": display_name,
            }))
            .unwrap();
            assert_eq!(user.display_label(), expected);
            assert_eq!(user.username, "oj_01");
        }
        let legacy: UserIdentity =
            serde_json::from_value(json!({"user_id": 1, "username": "oj_01"})).unwrap();
        assert_eq!(legacy.display_label(), "oj_01");
    }
}
