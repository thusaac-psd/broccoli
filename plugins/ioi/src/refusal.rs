//! The codes this plugin refuses a request with.

broccoli_server_sdk::refusal_codes! {
    /// The codes the IOI routes refuse a request with.
    pub enum Refusal {
        /// The submission does not exist in this contest.
        SubmissionNotFound = "SUBMISSION_NOT_FOUND",
        /// The submission is another contestant's.
        NotYourSubmission = "NOT_YOUR_SUBMISSION",
        /// The submission belongs to another contest.
        SubmissionNotInContest = "SUBMISSION_NOT_IN_CONTEST",
        /// A token is spent on a submission that is not judged.
        SubmissionNotJudged = "SUBMISSION_NOT_JUDGED",
        /// A token is spent while the contest is not running.
        ContestNotRunning = "CONTEST_NOT_RUNNING",
        /// The contest hands out no tokens.
        TokensDisabled = "TOKENS_DISABLED",
        /// The contestant has no token left.
        NoTokensAvailable = "NO_TOKENS_AVAILABLE",
        /// The submission already has a token.
        AlreadyTokened = "ALREADY_TOKENED",
        /// An anonymous viewer asks for scores before the contest ends.
        SignInToSeeScores = "SIGN_IN_TO_SEE_SCORES",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_refusal_code_has_a_text_in_every_pack() {
        broccoli_server_sdk::testing::assert_refusal_codes_translated::<super::Refusal>();
    }
}
