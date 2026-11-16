//! The codes this plugin refuses a request with.

broccoli_server_sdk::refusal_codes! {
    /// The codes the print routes refuse a request with.
    pub enum Refusal {
        /// A print station sends no token.
        StationTokenMissing = "STATION_TOKEN_MISSING",
        /// No station of the deployment or the named contest has the token.
        StationTokenInvalid = "STATION_TOKEN_INVALID",
        /// The request body is missing or malformed.
        InvalidRequestBody = "INVALID_REQUEST_BODY",
        /// The status filter names no job status.
        InvalidStatusFilter = "INVALID_STATUS_FILTER",
        /// The print job does not exist.
        JobNotFound = "JOB_NOT_FOUND",
        /// The state of the job does not allow the change.
        JobStateConflict = "JOB_STATE_CONFLICT",
        /// The contest has printing switched off.
        PrintingDisabled = "PRINTING_DISABLED",
        /// The contest does not print pasted code.
        PastedCodeDisabled = "PASTED_CODE_DISABLED",
        /// Only the contestants of a contest print in it.
        NotAContestant = "NOT_A_CONTESTANT",
        /// The submission does not exist, or is not the caller's.
        SubmissionNotFound = "SUBMISSION_NOT_FOUND",
        /// The document or submission has nothing to print.
        NothingToPrint = "NOTHING_TO_PRINT",
        /// A document is larger than any printout may be.
        DocumentTooLarge = "DOCUMENT_TOO_LARGE",
        /// A document is longer than the page limit of the contest.
        TooManyPages = "TOO_MANY_PAGES",
        /// The contestant used every print job the contest allows.
        PrintLimitReached = "PRINT_LIMIT_REACHED",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_refusal_code_has_a_text_in_every_pack() {
        broccoli_server_sdk::testing::assert_refusal_codes_translated::<super::Refusal>();
    }
}
