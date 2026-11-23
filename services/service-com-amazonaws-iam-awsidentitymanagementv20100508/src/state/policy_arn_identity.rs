// SPDX-License-Identifier: CC0-1.0

//! WHICH managed policy an ARN names.
//!
//! Every operation addressed by a `PolicyArn` has to decide whether two spellings of an ARN
//! are one policy: the store resolving the record, the catalog resolving a published entry,
//! the counts a reader derives, and the attachment sets an identity carries. The decision is
//! stated once here, so no two of them can read one ARN differently.

#[allow(unused_imports)]
use super::*;

/// Whether `left` and `right` name the SAME managed policy.
///
/// IAM entity names "are not distinguished by case" ([`name_key`]), and a managed policy's
/// name is the LAST segment of its ARN, so an ARN spelling a policy's name in another case
/// names that policy — the name the account enforces uniqueness on is the name every reader
/// resolves by. Every other part of the ARN is compared as written: the partition, the
/// account that owns the policy, the resource type and the PATH it is filed under are part
/// of WHICH policy is named rather than of how it is spelled, which is why only the segment
/// after the last `/` is folded.
///
/// A value that is not a managed-policy ARN is not this question's business, so two such
/// values are compared literally: an operation taking a `PolicyArn` refuses a value it
/// cannot read as one before it asks.
pub fn same_policy_arn(left: &str, right: &str) -> bool {
    if parse_policy_arn(left).is_none() || parse_policy_arn(right).is_none() {
        return left == right;
    }
    match (left.rfind('/'), right.rfind('/')) {
        (Some(left_slash), Some(right_slash)) => {
            left[..=left_slash] == right[..=right_slash]
                && name_key(&left[left_slash + 1..]) == name_key(&right[right_slash + 1..])
        }
        _ => left == right,
    }
}

/// The ARN the attachment set `attached` holds for the policy `arn` names — the spelling the
/// attach that wrote it carried — or `None` when the set carries no attachment of it.
///
/// An attachment is held under the ARN the attach named, and `ListAttachedUserPolicies` and
/// its peers report it back as it was written; which POLICY it is, though, is
/// [`same_policy_arn`]'s question, so one policy can never be attached twice under two
/// spellings of its ARN.
pub fn attached_policy_arn<'a>(
    attached: &'a ::std::collections::BTreeSet<String>,
    arn: &str,
) -> Option<&'a String> {
    attached.iter().find(|held| same_policy_arn(held, arn))
}

/// Whether the attachment set `attached` carries the policy `arn` names, however the ARN it
/// was attached under is spelled ([`attached_policy_arn`]).
pub fn holds_policy_arn(attached: &::std::collections::BTreeSet<String>, arn: &str) -> bool {
    attached_policy_arn(attached, arn).is_some()
}
