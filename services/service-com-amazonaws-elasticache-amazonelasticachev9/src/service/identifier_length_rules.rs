// SPDX-License-Identifier: CC0-1.0

//! The identifier-length rules this service applies BEFORE it resolves the resource an
//! identifier names, for the two members more than one handler validates.
//!
//! A value longer than the bound the service documents for a member names no resource the
//! service could ever have stored, so such a request is refused as the bad parameter value
//! it is rather than as a resource that does not exist: a caller that sent a malformed
//! identifier has to be told WHICH member it got wrong — a not-found reads as a retryable
//! absent resource and sends create-if-missing logic down the wrong branch.
//!
//! Each rule is stated here once rather than in the handlers that answer it, because the
//! SENTENCE is the whole difference a client sees and the service words one member's bound
//! the same way wherever it states it. The only variable parts of each template are the
//! member's name and its bound, both fixed by the rule — no value from the request appears
//! in either, so an over-long identifier is never echoed back.
//!
//! The templates the service words with the PROSE name of a member ("<Member> cannot be
//! longer than <n> characters.") are formatted by [`crate::state::identifier_refusal`],
//! which the handlers whose own wording is that template call with the bound their
//! operation states — except for the member whose bound more than one of them states in
//! that wording, which is stated here too so the NUMBER cannot drift between them.

/// Why `replication_group_id` cannot name a replication group, or `None` when it can.
///
/// The bound is the one `ReplicationGroupId` documents ("A name must contain from 1 to 40
/// alphanumeric characters or hyphens", [`crate::state::REPLICATION_GROUP_ID_MAX_LEN`]) —
/// the same bound a create enforces, so an id past it can name no stored group and is
/// refused before any lookup. An id within the bound still has to satisfy the FORM every
/// identifier of this service shares, which is the one rule
/// [`crate::state::identifier_refusal`] states.
///
/// The length is measured the way that rule measures it, so one over-long value is answered
/// by one sentence rather than by whichever of the two it reaches first. For a value that
/// could name a resource at all the two measures agree: the form admits ASCII letters,
/// digits and hyphens only.
pub fn replication_group_id_refusal(replication_group_id: &str) -> Option<String> {
    if replication_group_id.len() > crate::state::REPLICATION_GROUP_ID_MAX_LEN {
        return Some(format!(
            "The parameter ReplicationGroupId is not a valid identifier because it is longer \
             than {} characters.",
            crate::state::REPLICATION_GROUP_ID_MAX_LEN
        ));
    }
    // Within the bound the two wordings have nothing to disagree about: what is left to
    // judge is the FORM every identifier of this service shares, so it is asked for through
    // the sibling rule rather than stated a second time here — which is also what keeps the
    // bound the two measure against one number.
    replication_group_id_prose_refusal(replication_group_id)
}

/// Why `replication_group_id` cannot name a replication group for an operation that words
/// the bound with the PROSE name of the member ("Replication Group ID cannot be longer than
/// <n> characters."), or `None` when it can.
///
/// The BOUND is the same one [`replication_group_id_refusal`] applies and the same one a
/// create enforces — the 40-character maximum a replication-group name may have
/// ([`crate::state::REPLICATION_GROUP_ID_MAX_LEN`]) — so an id past it names no group this
/// service could ever have stored whichever operation was asked, and no operation may
/// advertise a wider ceiling than the one a create accepts. The two functions differ only
/// in the SENTENCE: the service words this member's bound one way on some operations and
/// another way on others, and each handler calls the one it states.
///
/// Stated once so the operations that word it this way cannot drift apart about the number,
/// which is the whole difference a client that trims its id to the advertised limit sees.
pub fn replication_group_id_prose_refusal(replication_group_id: &str) -> Option<String> {
    crate::state::identifier_refusal(
        "Replication Group ID",
        replication_group_id,
        crate::state::REPLICATION_GROUP_ID_MAX_LEN,
    )
}

/// The LONGEST `UserGroupId` the service accepts: ElastiCache documents an RBAC user group
/// id as at most 40 characters, so an id of exactly this length is ACCEPTED and only a
/// longer one is refused — the bound its sibling member `UserId` carries
/// ([`crate::state::MAX_USER_ID_LEN`]). The model constrains `UserGroupId` with neither
/// `@length` nor `@pattern`, so the bound is enforced here.
///
/// The refusal carries the service's own wording, which states the bound loosely; the BOUND
/// is the documented maximum rather than that sentence read literally.
pub const MAX_USER_GROUP_ID_LEN: usize = 40;

/// Why `user_group_id` cannot name a user group, or `None` when it can.
///
/// Stated once so the create that STORES a group id, the modify and the delete that resolve
/// one cannot drift about which ids the service accepts: a create that stored an id the
/// delete refuses would leave a group nothing could remove.
///
/// WHERE each operation applies it differs, because this bound is the service's own and not
/// the model's: an operation that RESOLVES an id applies it before the lookup, there being no
/// other rule for it to reach, while the create applies it LAST — after every rule the model
/// states for its request, so a refusal the request earns under a modelled rule (a membership
/// that names no `default` user, say) is answered as that rule rather than as this one.
pub fn user_group_id_refusal(user_group_id: &str) -> Option<String> {
    (user_group_id.chars().count() > MAX_USER_GROUP_ID_LEN)
        .then(|| format!("User group id length must be less than {MAX_USER_GROUP_ID_LEN}."))
}
