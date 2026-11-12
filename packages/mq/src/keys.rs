//! Every Redis key the queue uses, derived from one base name.
//!
//! Node ids never contain `:` (see `common::node_identity::is_valid_node_id`),
//! so each key below parses one way.

/// Consumer group of the work streams. Each worker consumes under its id.
pub const WORKERS_GROUP: &str = "workers";
/// Consumer group of the dead-letter stream. Each server consumes under its id.
pub const SERVERS_GROUP: &str = "servers";

/// The key layout under the base name `mq.operation_queue_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    base: String,
}

/// What a stream is for, by key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRole {
    /// The shared operation task stream.
    Shared,
    /// A worker's private stream of pinned tasks.
    Private,
    /// A server's reply stream.
    Reply,
    /// The dead-letter stream.
    DeadLetter,
}

impl StreamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "operation",
            Self::Private => "operation_private",
            Self::Reply => "operation_reply",
            Self::DeadLetter => "operation_dlq",
        }
    }
}

impl Topology {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// The shared operation task stream.
    pub fn shared(&self) -> String {
        self.base.clone()
    }

    /// Worker `worker_id`'s private stream of pinned tasks.
    pub fn private(&self, worker_id: &str) -> String {
        format!("{}:worker:{worker_id}", self.base)
    }

    /// The set of worker ids that have had a private stream.
    pub fn pinned_set(&self) -> String {
        format!("{}:pinned", self.base)
    }

    /// Server `server_id`'s reply stream.
    pub fn reply(&self, server_id: &str) -> String {
        format!("{}:reply:{server_id}", self.base)
    }

    /// The dead-letter stream.
    pub fn dead_letter(&self) -> String {
        format!("{}:dlq", self.base)
    }

    /// What `key` is, or `None` for a key outside this topology.
    pub fn role_of(&self, key: &str) -> Option<StreamRole> {
        let rest = key.strip_prefix(&self.base)?;
        if rest.is_empty() {
            return Some(StreamRole::Shared);
        }
        if rest == ":dlq" {
            return Some(StreamRole::DeadLetter);
        }
        if rest.strip_prefix(":worker:").is_some_and(is_id) {
            return Some(StreamRole::Private);
        }
        if rest.strip_prefix(":reply:").is_some_and(is_id) {
            return Some(StreamRole::Reply);
        }
        None
    }
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && !s.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_maps_back_to_its_role() {
        let t = Topology::new("ops");
        assert_eq!(t.role_of(&t.shared()), Some(StreamRole::Shared));
        assert_eq!(t.role_of(&t.private("w1")), Some(StreamRole::Private));
        assert_eq!(t.role_of(&t.reply("s1")), Some(StreamRole::Reply));
        assert_eq!(t.role_of(&t.dead_letter()), Some(StreamRole::DeadLetter));
        assert_eq!(t.role_of(&t.pinned_set()), None);
        assert_eq!(t.role_of("ops:worker:"), None);
        assert_eq!(t.role_of("other"), None);
    }
}
