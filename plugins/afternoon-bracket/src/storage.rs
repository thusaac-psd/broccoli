//! Storage key schema and typed accessors for the 下午场 bracket plugin.
//!
//! No plugin-side transactions: `host.storage.begin()` deadlocks the
//! runtime under concurrent submissions. Concurrent match updates go through
//! `Storage::modify` (a compare-and-set retry loop), never a read-then-write
//! pair.

use broccoli_server_sdk::prelude::*;

use crate::model::MatchState;

/// Storage key for a contest's `/setup` round definitions.
pub fn setup_key(contest: i32) -> String {
    format!("setup:{contest}")
}

/// Storage key for the player id occupying bracket slot `(round, pos)`.
///
/// Slots are separate keys rather than fields of the next-round match
/// document, because two SIBLING matches in a round both feed the SAME
/// next-round match. If that next match were one document, each sibling's
/// winner would be written by reading the document, setting its own slot
/// field, and writing it back -- and the two siblings' winners can finish at
/// arbitrary times relative to each other. Even with `compare_and_set`
/// retries, that is a race between two DIFFERENT writers each trying to
/// fill a DIFFERENT field of the SAME document, which only guarantees no
/// corruption, not that both writes land: whichever writer's `modify` retry
/// loses hits `SdkError::Other("CAS retry limit exceeded")`, and a lost
/// retry silently drops a real winner. Per-slot keys make the collision
/// structurally impossible instead of merely unlikely: each sibling match
/// writes only its OWN slot key, and nothing else ever writes it.
pub fn slot_key(contest: i32, round: u8, pos: u8) -> String {
    format!("slot:{contest}:{round}:{pos}")
}

/// Storage key for a match's full state document, scoped by contest so two
/// concurrent afternoon sessions never collide.
pub fn match_key(contest: i32, id: u8) -> String {
    format!("match:{contest}:{id}")
}

/// Load a match's current state, if it has been created.
pub fn load_match(host: &Host, contest: i32, id: u8) -> Result<Option<MatchState>, SdkError> {
    let key = match_key(contest, id);
    match host.storage.get_one(&key)? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// Apply `f` to a match's state via a compare-and-set retry loop, so a
/// judging callback and a timer firing on the same match cannot clobber
/// each other's write.
pub fn update_match<F>(host: &Host, contest: i32, id: u8, f: F) -> Result<MatchState, SdkError>
where
    F: Fn(&mut MatchState) -> Result<(), SdkError>,
{
    let key = match_key(contest, id);
    host.storage.modify(&key, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_keys_are_distinct_per_position() {
        // The two feeders of one next-round match must not share a key.
        assert_ne!(slot_key(7, 2, 0), slot_key(7, 2, 1));
    }

    #[test]
    fn match_keys_are_scoped_by_contest() {
        // Two concurrent afternoon sessions must not collide.
        assert_ne!(match_key(7, 3), match_key(8, 3));
    }

    #[test]
    fn load_match_returns_none_for_an_uncreated_match() {
        let host = Host::mock();
        assert!(load_match(&host, 7, 0).unwrap().is_none());
    }

    #[test]
    fn update_match_persists_the_mutation_and_survives_a_concurrent_writer() {
        let host = Host::mock();
        let key = match_key(7, 0);
        host.storage
            .set(&[(
                key.as_str(),
                &serde_json::to_string(&MatchState {
                    round: 1,
                    pos: 0,
                    player_a: 10,
                    player_b: 20,
                    ..Default::default()
                })
                .unwrap(),
            )])
            .unwrap();

        let updated = update_match(&host, 7, 0, |m| {
            m.score_a += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(updated.score_a, 1);
        assert_eq!(updated.player_a, 10, "unrelated fields must be preserved");

        let reloaded = load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.score_a, 1, "the mutation must be durable");
    }
}
