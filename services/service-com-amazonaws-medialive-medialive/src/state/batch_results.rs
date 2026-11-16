// SPDX-License-Identifier: CC0-1.0

//! The BATCH slice: the per-entry outcome machinery and the id-count bound that
//! every Batch operation of this service (`BatchDelete`, `BatchStart`,
//! `BatchStop`) answers by.
//!
//! One file for the whole slice, so the three operations cannot drift into three
//! readings of one rule. What they share is exactly this:
//!
//! * The per-entry report. Each of the three answers 200 with a
//!   `BatchSuccessfulResultModel` per accepted resource and a
//!   `BatchFailedResultModel` per refused one
//!   (`smithy_query(query="operation", operation="BatchDelete")`), filled in the
//!   order the request listed the resources, so one resource's refusal never
//!   hides another's accepted call. [`Results`], [`Outcome`], [`failure_code`],
//!   [`not_found`], [`refused`] and [`accepted`] are that report, once.
//! * The classification a failed entry's `Code` carries ("Error code for the
//!   failed operation"): the wire error code of the refusal the SINGLE-resource
//!   call for that resource answers with, which on this service is the modelled
//!   exception's shape name — so an id no resource of that kind has is NOT
//!   FOUND, and what an id whose delete has already been accepted is reported as
//!   is whatever that resource's own single-resource call answers for it: the
//!   channel family drops such an id out of resolution and reports it NOT FOUND
//!   before any state rule, while a multiplex's record stays resolvable and its
//!   start keeps the state conflict naming the tear-down state (see
//!   [`super::MultiplexStartRefusal`] for the recorded evidence).
//! * The id-count bound. The model targets every Batch request's id lists at
//!   plain `__listOf__string` with no length constraint, so nothing else bounds
//!   them, and each named id costs a derivation over the stores plus one entry
//!   in the response; [`within_id_cap`] counts them against the cap that
//!   operation's own constant sets ([`super::MAX_BATCH_DELETE_IDS`] and its
//!   siblings) and answers the message its refusal carries. The caller maps that
//!   message onto the modelled `BadRequestException` its own operation declares,
//!   exactly as the operations wrapping [`super::bounded_length`] and
//!   [`super::list_page_size`] do.
//!
//! What stays in each operation's own file is the part that is that operation's
//! own: which resource kinds it walks, the state rule each kind's single-resource
//! call follows, and the modelled error type its refusals are lifted into.

use crate::ops::types::{BatchFailedResultModel, BatchSuccessfulResultModel};

/// This service's state, named once so the per-resource helpers of the three
/// Batch operations read.
pub type State = super::ComAmazonawsMedialiveMedialiveState;

/// The two output lists, filled in the order the request listed the resources.
#[derive(Debug, Default)]
pub struct Results {
    /// The refused resources, one entry each — the operation's `Failed` member.
    pub failed: Vec<BatchFailedResultModel>,
    /// The accepted resources, one entry each — the operation's `Successful`
    /// member.
    pub successful: Vec<BatchSuccessfulResultModel>,
}

impl Results {
    /// File one resource's outcome in the list its kind belongs in.
    pub fn push(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Successful(result) => self.successful.push(result),
            Outcome::Failed(result) => self.failed.push(result),
        }
    }
}

/// What attempting one resource produced.
#[derive(Debug)]
pub enum Outcome {
    /// The call was accepted for that resource.
    Successful(BatchSuccessfulResultModel),
    /// The call was refused for that resource, with the code and message its
    /// single-resource form answers.
    Failed(BatchFailedResultModel),
}

/// The `Code` a failed entry carries ("Error code for the failed operation"):
/// the wire error code of the refusal the single-resource call for that resource
/// answers with, which on this service is the modelled exception's shape name
/// (`smithy_query(query="operation", operation="DeleteChannel")`).
pub mod failure_code {
    /// Neither an existing resource nor one a caller may still act on —
    /// `NotFoundException` (404), which every single-resource delete, start and
    /// stop in this service models: no resource of that kind has that id, or the
    /// one it had has been deleted and its family's delete takes the record out
    /// of resolution. For the channel family that check comes BEFORE any state
    /// rule, so a channel the account no longer holds is never reported as a
    /// state conflict; a multiplex's record is still resolvable after its delete
    /// has been accepted, so its own refusal is the [`CONFLICT`] its state rule
    /// makes.
    pub const NOT_FOUND: &str = "NotFoundException";
    /// The resource still exists but its current state does not allow the call —
    /// `ConflictException` (409), modelled by `DeleteChannel`, `DeleteInput`,
    /// `DeleteMultiplex` and by both single-resource starts and stops.
    pub const CONFLICT: &str = "ConflictException";
    /// The resource exists and is still in use by another one —
    /// `BadRequestException` (400). `DeleteInputSecurityGroup` is the one delete
    /// in this service that models no `ConflictException`
    /// (`smithy_query(query="operation", operation="DeleteInputSecurityGroup")`),
    /// so its refusal is the client error it does model.
    pub const BAD_REQUEST: &str = "BadRequestException";
}

/// A failed entry for an id no resource of that kind has. No `Arn`: there is no
/// resource to name one for.
pub fn not_found(resource_type: &str, id: &str) -> Outcome {
    Outcome::Failed(BatchFailedResultModel {
        arn: None,
        code: Some(failure_code::NOT_FOUND.to_string()),
        id: Some(id.to_string()),
        message: Some(super::not_found_message(resource_type, id)),
    })
}

/// A failed entry for a resource whose record the account still holds: it
/// carries that record's own ARN beside the code and the message of the refusal.
pub fn refused(id: &str, arn: Option<String>, code: &str, message: String) -> Outcome {
    Outcome::Failed(BatchFailedResultModel {
        arn,
        code: Some(code.to_string()),
        id: Some(id.to_string()),
        message: Some(message),
    })
}

/// A successful entry: the resource's id and ARN, and the state the accepted
/// call left it in, both read back out of the store.
///
/// The read-back is the operation's own view of what it wrote, so a description
/// that is suddenly missing is a server fault rather than an accepted call with
/// empty members: this answers `None` for it, and the caller lifts that into the
/// modelled server error its single-resource form answers for a store it cannot
/// reach.
pub fn accepted(
    id: &str,
    description: Option<(Option<String>, Option<String>)>,
) -> Option<Outcome> {
    let (arn, state) = description?;
    Some(Outcome::Successful(BatchSuccessfulResultModel {
        arn,
        id: Some(id.to_string()),
        state,
    }))
}

/// The message a Batch call naming a number of non-empty id lists other than one
/// is refused with.
///
/// FIXED text, with no variable part: the refusal states the request shape the
/// operation accepts — one id list, naming resources of ONE kind — and neither
/// the lists the request filled nor the ids in them change what has to be true.
/// The kinds it names are the four an id list of this family can name
/// (`smithy_query(query="operation", operation="BatchDelete")`: ChannelIds,
/// InputIds, InputSecurityGroupIds, MultiplexIds).
pub const ONE_ID_LIST_PER_CALL_MESSAGE: &str = "Exactly one non-empty list of channel, \
     multiplex, input, or input security group IDs is required";

/// Check that the request filled EXACTLY ONE of its id lists, answering the
/// message that refusal carries.
///
/// `BatchDelete` acts on resources of one kind per call: its four lists are four
/// alternatives rather than four parts of one request, so a call that fills two
/// of them asks for two batches at once and a call that fills none asks for
/// nothing. Neither is a request the operation serves, and the refusal is taken
/// BEFORE any resource is touched, so a mixed call deletes nothing instead of
/// performing both halves and leaving the caller without the resources the
/// one-kind call would have kept. It lives here, beside [`within_id_cap`],
/// because both are rules about the SHAPE of a batch request rather than about
/// any one resource; which lists a given operation passes is that operation's
/// own ([`crate::ops::batch_delete`]).
///
/// An EMPTY list is not a filled one: a member carrying no id names no resource,
/// so it is the same request as one that leaves the member out.
pub fn exactly_one_id_list(id_lists: &[&Option<Vec<String>>]) -> Result<(), String> {
    let filled = id_lists
        .iter()
        .filter(|ids| ids.as_ref().is_some_and(|ids| !ids.is_empty()))
        .count();
    if filled == 1 {
        return Ok(());
    }
    Err(ONE_ID_LIST_PER_CALL_MESSAGE.to_string())
}

/// Check the ids ONE batch call names against the cap that operation sets,
/// answering the message that refusal carries: the operation, the number of ids
/// named and the cap.
///
/// The count is over EVERY id list of the request, because the cap is on their
/// sum: the work done under the state guard and the length of the two result
/// lists are both that sum. Taken BEFORE the state guard, so the guard is never
/// held for an amount of work the caller chose.
pub fn within_id_cap(
    operation: &str,
    cap: usize,
    id_lists: &[&Option<Vec<String>>],
) -> Result<(), String> {
    let named: usize = id_lists
        .iter()
        .map(|ids| ids.as_ref().map_or(0, Vec::len))
        .sum();
    if named <= cap {
        return Ok(());
    }
    Err(format!(
        "{operation} names {named} resources, more than the {cap} one call may name"
    ))
}
