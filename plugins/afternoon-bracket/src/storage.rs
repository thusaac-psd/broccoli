//! Storage key schema and typed accessors for the 下午场 bracket plugin.
//!
//! No plugin-side transactions: `host.storage.begin()` deadlocks the
//! runtime under concurrent submissions. Concurrent match updates go through
//! `Storage::modify` (a compare-and-set retry loop), never a read-then-write
//! pair.

use broccoli_server_sdk::prelude::*;

use crate::model::{MatchState, Setup};

/// A single-elimination bracket of 16 players has exactly 15 matches
/// (8 + 4 + 2 + 1), independent of whatever match-id numbering scheme
/// round-advancement assigns. Used to bound the scan in
/// [`load_all_matches`] instead of guessing a range.
pub const MATCH_COUNT: u8 = 15;

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

/// Load a contest's `/setup` round definitions, if `/setup` has run.
pub fn load_setup(host: &Host, contest: i32) -> Result<Option<Setup>, SdkError> {
    let key = setup_key(contest);
    match host.storage.get_one(&key)? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// Load every match that has been created for `contest`, in ONE batched
/// `host.storage.get` call across all [`MATCH_COUNT`] possible match ids
/// -- never one `get_one` per id, which would be one host-fn crossing per
/// match instead of one for the whole bracket. Matches that have not been
/// created yet (no round-advancement has written them) are simply absent
/// from the result, not an error.
pub fn load_all_matches(host: &Host, contest: i32) -> Result<Vec<(u8, MatchState)>, SdkError> {
    let keys: Vec<String> = (0..MATCH_COUNT).map(|id| match_key(contest, id)).collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let raw = host.storage.get(&key_refs)?;

    let mut matches = Vec::new();
    for (id, key) in (0..MATCH_COUNT).zip(keys.iter()) {
        if let Some(value) = raw.get(key) {
            matches.push((id, serde_json::from_str(value)?));
        }
    }
    Ok(matches)
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

    #[test]
    fn load_setup_returns_none_before_setup_has_run() {
        let host = Host::mock();
        assert!(load_setup(&host, 7).unwrap().is_none());
    }

    #[test]
    fn load_setup_returns_the_persisted_document() {
        let host = Host::mock();
        let setup = Setup {
            xiaoju_seconds: 1_800,
            ..Default::default()
        };
        host.storage
            .set(&[(
                setup_key(7).as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            )])
            .unwrap();
        assert_eq!(load_setup(&host, 7).unwrap(), Some(setup));
    }

    #[test]
    fn load_all_matches_skips_uncreated_ids_and_reads_the_bracket_in_one_call() {
        let host = Host::mock();
        host.storage
            .set(&[
                (
                    match_key(7, 0).as_str(),
                    serde_json::to_string(&MatchState {
                        round: 1,
                        pos: 0,
                        player_a: 10,
                        player_b: 20,
                        ..Default::default()
                    })
                    .unwrap()
                    .as_str(),
                ),
                (
                    match_key(7, 5).as_str(),
                    serde_json::to_string(&MatchState {
                        round: 2,
                        pos: 0,
                        player_a: 30,
                        player_b: 40,
                        ..Default::default()
                    })
                    .unwrap()
                    .as_str(),
                ),
            ])
            .unwrap();

        // A second contest's match must not leak into contest 7's scan.
        host.storage
            .set(&[(
                match_key(8, 0).as_str(),
                serde_json::to_string(&MatchState::default())
                    .unwrap()
                    .as_str(),
            )])
            .unwrap();

        let before = host.storage.get_call_count();
        let matches = load_all_matches(&host, 7).unwrap();
        assert_eq!(
            host.storage.get_call_count(),
            before + 1,
            "the whole bracket must be one batched read"
        );

        let ids: Vec<u8> = matches.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 5]);
        assert_eq!(matches[0].1.player_a, 10);
        assert_eq!(matches[1].1.player_a, 30);
    }
}
