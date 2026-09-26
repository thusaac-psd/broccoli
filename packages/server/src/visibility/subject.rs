/// Who is asking. Mirrors the field naming already used by
/// `broccoli_types::FilterSubmissionInput` so the wire shape stays familiar.
#[derive(Debug, Clone)]
pub struct Subject {
    pub user_id: Option<i32>,
    pub authenticated: bool,
    pub permissions: Vec<String>,
    admin_override: bool,
}

impl Subject {
    pub fn from_auth_user(auth_user: &crate::extractors::auth::AuthUser) -> Self {
        Self {
            user_id: Some(auth_user.user_id),
            authenticated: true,
            permissions: auth_user.permissions.clone(),
            admin_override: false,
        }
    }

    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            authenticated: false,
            permissions: Vec::new(),
            admin_override: false,
        }
    }

    /// Skips `host_decide`/`query_plugins` entirely and resolves every
    /// `decide`/`decide_batch` call to `Allow` with zero DB queries and zero
    /// plugin calls (see `host_decide`'s `is_admin_override()` branch).
    /// Currently has NO production call sites - only the two tests that pin
    /// this short-circuit (`host_rules::admin_override_allows_everything_without_querying`,
    /// `visibility::admin_override_allows_everything_without_reaching_plugins`)
    /// construct one. Handlers that read entities outside a viewer's own
    /// reachability (e.g. `handlers/submission/rejudge.rs`) do NOT use this:
    /// a bare `Decision::Allow` can't express what they actually need (a
    /// locked row fetch, `SELECT ... FOR UPDATE`), so calling this here
    /// would be a no-op dressed up as an access check. Those call sites
    /// instead pair an ordinary `auth_user.require_permission(...)` guard
    /// with a `visibility-bypass-audited:` comment on the raw `entity::`
    /// import itself (grep that token across `handlers/`) - and still route
    /// their *response* through the kernel under the real
    /// `Subject::from_auth_user` (see `apply_filter_to_response_after_mutation`
    /// in `rejudge.rs`). Dispatcher and migration code never construct a
    /// `Subject` at all; they run outside any HTTP request/viewer context
    /// and have no kernel to bypass. Before adding a new call site, confirm
    /// a `Decision` is actually what's needed there.
    pub fn admin_override() -> Self {
        Self {
            user_id: None,
            authenticated: true,
            permissions: Vec::new(),
            admin_override: true,
        }
    }

    pub fn is_admin_override(&self) -> bool {
        self.admin_override
    }

    pub fn has_permission(&self, perm: &str) -> bool {
        self.permissions.iter().any(|p| p == perm)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Read,
    Submit,
    Clarify,
    Download,
}

impl Action {
    pub fn as_wire(&self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Submit => "submit",
            Action::Clarify => "clarify",
            Action::Download => "download",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    Contest(i32),
    Problem {
        contest_id: Option<i32>,
        problem_id: i32,
    },
    Sample {
        contest_id: Option<i32>,
        problem_id: i32,
    },
    // `attachment_id` is the REAL primary key (`problem_attachment.id`),
    // not a truncated wire id - there is no lossy identity anywhere inside
    // the host. `Uuid` is `Hash + Eq`, so `Resource` (and therefore the
    // kernel's `(Action, Resource)` per-request memo key) still works
    // unchanged. See `wire_id`'s doc comment for why the wire
    // representation must stay lossless too.
    Attachment {
        problem_id: i32,
        attachment_id: uuid::Uuid,
    },
    Submission(i32),
    Clarification(i32),
}

impl Resource {
    pub fn wire_kind(&self) -> &'static str {
        match self {
            Resource::Contest(_) => "contest",
            Resource::Problem { .. } => "problem",
            Resource::Sample { .. } => "sample",
            Resource::Attachment { .. } => "attachment",
            Resource::Submission(_) => "submission",
            Resource::Clarification(_) => "clarification",
        }
    }

    /// The resource's one authoritative wire identifier, always a lossless
    /// string (decimal for the i32-native kinds, hyphenated UUID for
    /// `Attachment`). Deliberately `String`, not `i32`: an earlier version
    /// of this method truncated `Attachment`'s `Uuid` into an `i32`
    /// (`id.as_u128() as i32`), which collided across the whole
    /// installation's attachment table at the 32-bit birthday bound. See
    /// `QueryResource::id`'s doc comment (`broccoli-types`) for the full
    /// account; that truncation helper has been deleted, not corrected.
    pub fn wire_id(&self) -> String {
        match self {
            Resource::Contest(id) | Resource::Submission(id) | Resource::Clarification(id) => {
                id.to_string()
            }
            Resource::Problem { problem_id, .. } | Resource::Sample { problem_id, .. } => {
                problem_id.to_string()
            }
            Resource::Attachment { attachment_id, .. } => attachment_id.to_string(),
        }
    }

    /// The parent problem for a `Problem`/`Sample`/`Attachment` resource,
    /// `None` for the three kinds that have no problem (`Contest`,
    /// `Submission`, `Clarification`). Exists so `QueryResource.problem_id`
    /// can be populated on the wire - without it, an attachment-visibility
    /// plugin would receive `contest_id: null` (an attachment's contest
    /// scope is never resolvable, see `resource_contest_id`'s doc comment
    /// in `visibility/mod.rs`) and nothing else to reason about at all.
    pub fn wire_problem_id(&self) -> Option<i32> {
        match self {
            Resource::Problem { problem_id, .. }
            | Resource::Sample { problem_id, .. }
            | Resource::Attachment { problem_id, .. } => Some(*problem_id),
            Resource::Contest(_) | Resource::Submission(_) | Resource::Clarification(_) => None,
        }
    }
}
