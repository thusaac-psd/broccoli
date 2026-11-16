//! The codes this plugin refuses a request with.

broccoli_server_sdk::refusal_codes! {
    /// The codes the cooldown refuses a submission with.
    pub enum Refusal {
        /// The problem's cooldown since the last submission has not run out.
        CooldownActive = "COOLDOWN_ACTIVE",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_refusal_code_has_a_text_in_every_pack() {
        broccoli_server_sdk::testing::assert_refusal_codes_translated::<super::Refusal>();
    }
}
