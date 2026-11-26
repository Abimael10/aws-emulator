// SPDX-License-Identifier: CC0-1.0

//! The same CodeCommit reference-event chain, driven from the position a client POINTED at
//! this emulator is in: the started binary addressed at the authority it answers on
//! (`Host: 127.0.0.1:<port>`), the way a stock SDK configured with an `endpoint_url`
//! addresses it, reading the queue by the url the emulator minted for that client and
//! waiting for the envelope with the long poll such a consumer makes.
//!
//! The RULE under test is the one
//! [`crate::codecommit_repository_state_change_on_a_started_emulator`] states and owns the
//! legs of: **an accepted CodeCommit write that leaves a branch reference pointing somewhere
//! new publishes one event onto the account's default EventBridge bus as `source`
//! `aws.codecommit` / `detail-type` `CodeCommit Repository State Change`, naming the
//! repository's ARN in `resources` and, in `detail`, the class of the write, the reference
//! and the commit the write's own answer reported; a call the service refuses moves no
//! reference and publishes nothing.**
//!
//! Why the addressing is a row of its own: a client handed an endpoint url carries no Region
//! in its `Host`, so the Region of the repository, of the rule, of the queue and of the
//! publication is read off the SigV4 credential scope alone, and the queue url it is handed
//! names this emulator's authority rather than Amazon SQS's — the address it then polls. A
//! chain that holds for a caller naming AWS's own hosts and breaks for one pointed at the
//! emulator is a failure a test reports rather than one only a client sees.

use crate::codecommit_repository_state_change_on_a_started_emulator::{
    reference_writes_reach_their_queue, Reached, LONG_POLL_SECONDS,
};

/// RULE: for a client pointed at this emulator's own endpoint, every accepted CodeCommit
/// write that moves a reference is delivered to the SQS target of a rule matching it, and a
/// write refused for a stale parent delivers nothing.
#[test]
fn a_reference_write_reaches_the_queue_a_pointed_client_was_handed() {
    reference_writes_reach_their_queue(Reached::EmulatorAuthority, LONG_POLL_SECONDS);
}
