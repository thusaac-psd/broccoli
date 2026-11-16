//! The codes this plugin refuses a request with.

broccoli_server_sdk::refusal_codes! {
    /// The codes the bracket refuses a submission or a request with.
    pub enum Refusal {
        /// The problem is not the current one of the submitter in the game.
        NotYourProblem = "NOT_YOUR_PROBLEM",
        /// The game is decided or outside its window.
        XiaojuClosed = "XIAOJU_CLOSED",
        /// The caller plays in no match of the problem or route.
        NotInMatch = "NOT_IN_MATCH",
        /// No game of the match is open.
        MatchNotStarted = "MATCH_NOT_STARTED",
        /// The previous submission of the player has no result yet.
        SubmissionInFlight = "SUBMISSION_IN_FLIGHT",
        /// The submission window of the game has closed.
        SubmissionTooLate = "SUBMISSION_TOO_LATE",
        /// A problem a bracket holds is submitted outside its bracket contest.
        ContestSubmissionRequired = "CONTEST_SUBMISSION_REQUIRED",
        /// The request body is missing or malformed.
        InvalidRequestBody = "INVALID_REQUEST_BODY",
        /// The bracket of the contest has no setup yet.
        BracketNotSetUp = "BRACKET_NOT_SET_UP",
        /// The match does not exist.
        MatchNotFound = "MATCH_NOT_FOUND",
        /// The contest has not started.
        ContestNotStarted = "CONTEST_NOT_STARTED",
        /// The contest has ended.
        ContestEnded = "CONTEST_ENDED",
        /// The stage of the match has not opened yet.
        StageNotOpen = "STAGE_NOT_OPEN",
        /// The match follows its shared stage schedule.
        FollowsStageSchedule = "FOLLOWS_STAGE_SCHEDULE",
        /// The ordering window of the match has closed.
        OrderingClosed = "ORDERING_CLOSED",
        /// The ranking is not an order of the problem group of the opponent.
        InvalidOrder = "INVALID_ORDER",
        /// The match has already been decided.
        MatchDecided = "MATCH_DECIDED",
        /// The named player does not play in the match.
        NotAPlayer = "NOT_A_PLAYER",
        /// The match is not waiting to be started.
        NotAwaitingStart = "NOT_AWAITING_START",
        /// The match has no game index left.
        NoGameLeft = "NO_GAME_LEFT",
        /// A time the action computes is out of range.
        TimeOutOfRange = "TIME_OUT_OF_RANGE",
        /// The bracket setup is not valid.
        InvalidSetup = "INVALID_SETUP",
        /// A stage starts outside the contest window.
        StageOutsideContest = "STAGE_OUTSIDE_CONTEST",
        /// A new stage start time is not in the future.
        StageStartInPast = "STAGE_START_IN_PAST",
        /// The edit changes what the progress of the bracket fixes.
        SetupEditRefused = "SETUP_EDIT_REFUSED",
        /// Another edit of the setup landed first.
        SetupChanged = "SETUP_CHANGED",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_refusal_code_has_a_text_in_every_pack() {
        broccoli_server_sdk::testing::assert_refusal_codes_translated::<super::Refusal>();
    }
}
