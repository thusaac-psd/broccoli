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
        Self { user_id: None, authenticated: false, permissions: Vec::new(), admin_override: false }
    }

    /// Explicit, greppable bypass for system and admin paths that must read
    /// entities without a viewer decision (rejudge, dispatcher, migrations).
    /// Every call site needs its own test justifying it.
    pub fn admin_override() -> Self {
        Self { user_id: None, authenticated: true, permissions: Vec::new(), admin_override: true }
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
    Problem { contest_id: Option<i32>, problem_id: i32 },
    Sample { contest_id: Option<i32>, problem_id: i32 },
    Attachment { problem_id: i32, attachment_id: i32 },
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

    pub fn wire_id(&self) -> i32 {
        match self {
            Resource::Contest(id)
            | Resource::Submission(id)
            | Resource::Clarification(id) => *id,
            Resource::Problem { problem_id, .. } | Resource::Sample { problem_id, .. } => *problem_id,
            Resource::Attachment { attachment_id, .. } => *attachment_id,
        }
    }
}
