//! Canonical catalog of permission identifiers.
//!
//! Every authorization gate in the system -- the server's `require_permission`
//! / `has_permission` checks, the role seed table, and the contest plugins'
//! [`PluginHttpRequest::has_permission`](crate::types::PluginHttpRequest::has_permission)
//! calls -- references a constant from this module instead of a bare string
//! literal, so the set of valid permissions lives in exactly one place and
//! cannot drift between the server, the SDK, and the plugins.
//!
//! The web frontend consumes the same catalog through the generated file
//! `packages/web-sdk/src/permissions/index.ts` (exported as
//! `@broccoli/web-sdk/permissions`). The `permissions_ts_is_in_sync` test below
//! fails CI if that file drifts from [`ALL`]; regenerate it with
//! `REGEN_PERMISSIONS_TS=1 cargo test -p broccoli-types permissions`.

/// Declares each permission constant and the [`ALL`] table from one list, so a
/// new permission is added in exactly one place (its constant, wire value, and
/// membership in `ALL` / the generated TS all follow from a single line here).
macro_rules! permissions {
    ($( $(#[$doc:meta])* $name:ident => $value:literal ),* $(,)?) => {
        $( $(#[$doc])* pub const $name: &str = $value; )*

        /// Every permission as a `(constant name, wire value)` pair, in
        /// declaration order. Drives [`is_valid`] and the TS code generation.
        pub const ALL: &[(&str, &str)] = &[ $( (stringify!($name), $value) ),* ];
    };
}

permissions! {
    /// Submit solutions to problems.
    SUBMISSION_SUBMIT => "submission:submit",
    /// View every user's submissions, not only one's own.
    SUBMISSION_VIEW_ALL => "submission:view_all",
    /// Trigger a rejudge of existing submissions.
    SUBMISSION_REJUDGE => "submission:rejudge",
    /// Create problems.
    PROBLEM_CREATE => "problem:create",
    /// Edit existing problems.
    PROBLEM_EDIT => "problem:edit",
    /// Delete problems.
    PROBLEM_DELETE => "problem:delete",
    /// Create contests.
    CONTEST_CREATE => "contest:create",
    /// Manage a contest: settings, participants, clarifications, freeze, etc.
    CONTEST_MANAGE => "contest:manage",
    /// Delete contests.
    CONTEST_DELETE => "contest:delete",
    /// Manage users.
    USER_MANAGE => "user:manage",
    /// Manage roles and their permission grants.
    ROLE_MANAGE => "role:manage",
    /// Manage plugins: install, configure, enable/disable.
    PLUGIN_MANAGE => "plugin:manage",
    /// Schedule one-shot callbacks via the plugin timer.
    TIMER => "timer",
    /// Manage the dead-letter queue.
    DLQ_MANAGE => "dlq:manage",
    /// View system status and diagnostics.
    SYSTEM_VIEW => "system:view",
    /// Full system administration.
    SYSTEM_ADMIN => "system:admin",
}

/// The permissions that make their holder contest staff. Either one alone
/// shows a viewer every contestant's submissions as the host stores them: the
/// host never asks a visibility plugin about staff, and a contest plugin shows
/// staff what the host does by deciding staff through [`is_staff`].
pub const STAFF: &[&str] = &[CONTEST_MANAGE, SUBMISSION_VIEW_ALL];

/// Whether a viewer holding `held` is contest staff (see [`STAFF`]).
pub fn is_staff<S: AsRef<str>>(held: &[S]) -> bool {
    held.iter().any(|p| STAFF.contains(&p.as_ref()))
}

/// Returns `true` if `permission` is a known permission identifier.
pub fn is_valid(permission: &str) -> bool {
    ALL.iter().any(|(_, value)| *value == permission)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path to the generated TS mirror, relative to this crate's manifest dir.
    const TS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../web-sdk/src/permissions/index.ts"
    );

    /// Canonical TS content generated from [`ALL`] and [`STAFF`], formatted
    /// the way the repo's prettier config writes it, so the formatter leaves
    /// the generated file as it is.
    fn generate_ts() -> String {
        let mut out = String::new();
        out.push_str(
            "// Code generated from packages/broccoli-types/src/permissions.rs. DO NOT EDIT.\n",
        );
        out.push_str(
            "// Regenerate: `REGEN_PERMISSIONS_TS=1 cargo test -p broccoli-types permissions`.\n\n",
        );
        for (name, value) in ALL {
            out.push_str(&format!("export const {name} = '{value}';\n"));
        }
        out.push_str("\nexport const ALL_PERMISSIONS = [\n");
        for (name, _) in ALL {
            out.push_str(&format!("  {name},\n"));
        }
        out.push_str("] as const;\n\n");
        out.push_str("export type Permission = (typeof ALL_PERMISSIONS)[number];\n\n");
        out.push_str("/** The permissions that make their holder contest staff. */\n");
        out.push_str("export const STAFF_PERMISSIONS: readonly Permission[] = [\n");
        for value in STAFF {
            let (name, _) = ALL
                .iter()
                .find(|(_, v)| v == value)
                .expect("a staff permission is in ALL");
            out.push_str(&format!("  {name},\n"));
        }
        out.push_str("];\n\n");
        out.push_str(
            "/** Whether a viewer holding `permissions` is contest staff, by the host's rule. */\n",
        );
        out.push_str("export function isStaff(permissions: readonly string[]): boolean {\n");
        out.push_str("  return STAFF_PERMISSIONS.some((p) => permissions.includes(p));\n");
        out.push_str("}\n");
        out
    }

    #[test]
    fn all_is_complete_and_unique() {
        // No duplicate constant names or wire values.
        let mut names: Vec<&str> = ALL.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len(), "duplicate permission constant name");

        let mut values: Vec<&str> = ALL.iter().map(|(_, v)| *v).collect();
        values.sort_unstable();
        values.dedup();
        assert_eq!(values.len(), ALL.len(), "duplicate permission wire value");

        for (_, value) in ALL {
            assert!(is_valid(value));
        }
        assert!(!is_valid("contest:mange"));
    }

    #[test]
    fn permissions_ts_is_in_sync() {
        if std::env::var_os("REGEN_PERMISSIONS_TS").is_some() {
            std::fs::write(TS_PATH, generate_ts()).expect("write web-sdk permissions/index.ts");
            return;
        }

        let contents = std::fs::read_to_string(TS_PATH).expect(
            "packages/web-sdk/src/permissions/index.ts is missing; regenerate with \
             `REGEN_PERMISSIONS_TS=1 cargo test -p broccoli-types permissions`",
        );

        assert_eq!(
            contents,
            generate_ts(),
            "packages/web-sdk/src/permissions/index.ts is out of sync with the Rust catalog; \
             regenerate with `REGEN_PERMISSIONS_TS=1 cargo test -p broccoli-types permissions`"
        );
    }
}
