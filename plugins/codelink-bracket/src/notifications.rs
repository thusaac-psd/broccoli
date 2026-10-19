//! Only settled 小局 produce notifications; match-level staff overrides and
//! local deadlines are not small-game results. Called after the match CAS.
use broccoli_server_sdk::prelude::*;
use serde_json::json;

use crate::model::MatchState;

pub fn publish_games(
    host: &Host,
    contest: i32,
    match_id: u8,
    m: &MatchState,
) -> Result<(), SdkError> {
    let Some(first) = m.notifications_from_game else {
        return Ok(());
    };
    let (mut score_a, mut score_b) = (0_u16, 0_u16);
    for game in &m.xiaoju {
        if !game.decided {
            continue;
        }
        score_a += u16::from(game.winner == Some(m.player_a));
        score_b += u16::from(game.winner == Some(m.player_b));
        if game.index < first {
            continue;
        }
        let tiebreak = !m.is_regular_game(game.index);
        let number = if tiebreak {
            game.index as usize - m.group_a.len() + 1
        } else {
            game.index as usize + 1
        };
        // Replaying every settled game repairs a failed post-commit publish.
        // The host preserves the first record per key. Prefix scores keep the
        // payload stable even when a later callback has already settled more games.
        host.notifications.publish(&PublishNotification {
            contest_id: contest,
            user_ids: vec![m.player_a, m.player_b],
            key: format!("game:{match_id}:{}:{}", game.index, game.opened_at_ms),
            kind: "game-ended".into(),
            payload: json!({
                "match_id": match_id,
                "round": m.round,
                "game_index": game.index,
                "game_number": number,
                "tiebreak": tiebreak,
                "winner": game.winner,
                "player_a": m.player_a,
                "player_b": m.player_b,
                "score_a": score_a,
                "score_b": score_b,
            }),
        })?;
    }
    Ok(())
}
