// SPDX-License-Identifier: CC0-1.0

//! The account state a suite needs when its sends are not about the Amazon SES SANDBOX.
//!
//! A new SES account is in the sandbox, where it "can only send email to verified
//! identities": every address such an account's message is mailed to has to be an identity
//! it has verified, or a mailbox of the mailbox simulator. That rule is pinned where it
//! belongs, beside the two surfaces that serve a send
//! (`services/service-com-amazonaws-sesv2-simpleemailservice-v2/tests/all/send_sandbox_destination_gate.rs`
//! and
//! `services/service-com-amazonaws-ses-simpleemailservice/tests/all/ses_sandbox_destination_gate.rs`).
//!
//! A cross-service suite about what a send then PRODUCES — the events it publishes, the
//! message the account receives, the report a handler mails — gives the account production
//! access first, so its sends are weighed on their sending identity alone and may be
//! addressed to any mailbox the suite's rows need.
//!
//! Both SES surfaces keep their own account record, so the state is written on both: a
//! suite that sends through either one reads one account.

/// Give the account production access on `engine`, on both SES surfaces.
///
/// Written on the account records rather than asked for over the wire because asking opens
/// an account review that falls due later, which is a span of its own and not what a suite
/// calling this is about.
pub(crate) fn given_production_access(engine: &aws_emulator_router::InMemoryEngine) {
    engine
        .state::<service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State>()
        .lock()
        .account
        .production_access_enabled = true;
    engine
        .state::<service_com_amazonaws_ses_simpleemailservice::state::ComAmazonawsSesSimpleemailserviceState>()
        .lock()
        .account
        .production_access_granted = true;
}
