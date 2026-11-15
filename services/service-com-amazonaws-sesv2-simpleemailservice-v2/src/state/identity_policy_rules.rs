// SPDX-License-Identifier: CC0-1.0

//! The rules ONE sending-authorization policy of an email identity has to satisfy, and
//! the sentences this service refuses a policy write with.
//!
//! Every operation that writes one of an identity's policies —
//! `CreateEmailIdentityPolicy` and `UpdateEmailIdentityPolicy` — reads them from here,
//! so the two cannot come to disagree about what a policy name and a policy document
//! may be, or about how the refusal of a name that is already taken, or of one that
//! names no policy, is worded. Each operation wraps a sentence in the error its own
//! model binds; nothing here names an operation's error shape.

/// How this service words `AlreadyExistsException` for a sending-authorization policy
/// name an identity already carries.
///
/// This surface is RECORDED answering a duplicate `CreateEmailIdentityPolicy` with the
/// fixed text `Policy <` + `> already exists` — the policy name QUOTED in angle
/// brackets, no trailing period — and the `PolicyName` the request spelled is its only
/// variable part: the identity the policy hangs off is not named, because the name the
/// caller has to change is the policy's.
pub fn identity_policy_already_exists(policy_name: &str) -> String {
    format!("Policy <{policy_name}> already exists")
}

/// How this service words `NotFoundException` for a sending-authorization policy name
/// an identity carries no policy under.
///
/// The counterpart of [`identity_policy_already_exists`], and worded the same way: the
/// fixed text `Policy <` + `> does not exist` — the policy name QUOTED in angle
/// brackets, no trailing period — with the `PolicyName` the request spelled as its
/// only variable part, and the identity the policy would hang off unnamed, because the
/// resource the call could not find is the policy. This surface is RECORDED answering
/// `UpdateEmailIdentityPolicy` with it for a name the identity carries no policy
/// under.
pub fn identity_policy_not_found(policy_name: &str) -> String {
    format!("Policy <{policy_name}> does not exist")
}

/// The bytes ONE sending-authorization policy document may carry.
///
/// `Policy` is documented the same way on every operation that writes one — "The
/// text of the policy in JSON format. The policy cannot exceed 4 KB." — and the
/// model carries only that member's lower bound (`length 1..`), so the documented
/// ceiling is this service's to enforce; it is also what bounds one entry of an
/// identity's policy map.
pub const MAX_POLICY_BYTES: usize = 4 * 1024;

/// The sending-authorization policies ONE identity may hold at once, and so the
/// ceiling every policy write keeps on an identity's policy map.
///
/// A CHOSEN bound, informed by what `GetEmailIdentityPolicies` documents — it
/// returns the identity's policies and "you can retrieve a maximum of 20 policies
/// at a time", so 20 is the whole set a caller can read back in one call — rather
/// than a quota AWS states for the collection itself. A write that would carry an
/// identity past it is refused, so caller input cannot grow the store without end.
pub const MAX_IDENTITY_POLICIES: usize = 20;

/// What this service answers for a document that is not a sending authorization
/// policy.
///
/// The service reports ONE opaque phrase for that whole class — it does not name the
/// clause of the policy grammar the document broke — so a document that is not JSON
/// and one that is JSON but carries no statement are refused in the same words.
const NOT_A_POLICY: &str = "Unable to parse policy.";

/// How this service words the refusal of a policy whose resource element names
/// something that is not an ARN.
///
/// A sending authorization policy authorizes sending THROUGH one identity, and an
/// identity is named by its ARN, so the service reads the element as one and reports
/// the value it could not read as an ARN: the trailing token is that value, copied
/// from the document that was submitted.
fn invalid_resource_arn(value: &str) -> String {
    format!("Invalid ARN: ARNs must start with 'arn:': {value}")
}

/// The characters `PolicyName` admits, from its own documentation: "The name of the
/// policy. The policy name cannot exceed 64 characters and can only include
/// alphanumeric characters, dashes, and underscores." The length is modeled
/// (`length 1..64`) and so already refused at the boundary; the character set is
/// not, so the policy writes read it here.
fn is_policy_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Why this service will not hold `policy` under `policy_name` as one of an
/// identity's sending-authorization policies, as the sentence the refusal carries, or
/// `None` when both the name and the document are admissible.
///
/// The rules are the POLICY's rather than one operation's — the name's character set,
/// the document's published size ceiling, the IAM resource-policy grammar the
/// document has to satisfy, and the ARN its resource element has to name — so every
/// operation that writes one reads them from here and a client is told the same thing
/// in the same words whichever of them it called. Each operation wraps the sentence
/// in the error its own model binds for invalid input.
pub fn identity_policy_write_defect(policy_name: &str, policy: &str) -> Option<String> {
    if !is_policy_name(policy_name) {
        return Some(format!(
            "The policy name {policy_name} can only include alphanumeric characters, \
             dashes, and underscores."
        ));
    }

    // The document's ceiling: "The policy cannot exceed 4 KB." The model bounds
    // `Policy` only below, so a longer document reaches a handler. Its own sentence,
    // because the size rule is not the parse rule.
    if policy.len() > MAX_POLICY_BYTES {
        return Some(format!(
            "Policy exceeds the maximum size of {MAX_POLICY_BYTES} bytes."
        ));
    }

    // A sending authorization policy IS an IAM resource-based policy document — each
    // statement names the `Principal` it authorizes to send through the identity — so
    // whether the text is a policy at all is read through the ONE shared grammar
    // rather than by asking `serde_json` whether it parses, which would accept `5`,
    // `"hello"` and `{}` as policies and store them.
    if ::service_common::iam_policy::resource_policy_defect(policy).is_some() {
        return Some(NOT_A_POLICY.to_string());
    }

    // The resource a sending authorization policy names is the IDENTITY it authorizes
    // sending through, and an identity is named by its ARN. The IAM grammar above
    // admits a bare wildcard as a resource; this service does not, so a resource
    // element that is not an ARN is refused, and the refusal names the value that is
    // not one.
    ::service_common::iam_policy::non_arn_resource(policy)
        .map(|value| invalid_resource_arn(&value))
}
