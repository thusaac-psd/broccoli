//! The codes this plugin refuses a request with.

broccoli_server_sdk::refusal_codes! {
    /// The codes the ICPC routes refuse a request with.
    pub enum Refusal {
        /// The board is revealed before the contest ends.
        RevealBeforeEnd = "REVEAL_BEFORE_END",
        /// The contest has no scoreboard freeze to reveal.
        NoFreezeToReveal = "NO_FREEZE_TO_REVEAL",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_refusal_code_has_a_text_in_every_pack() {
        broccoli_server_sdk::testing::assert_refusal_codes_translated::<super::Refusal>();
    }
}
