// SPDX-License-Identifier: CC0-1.0

//! One IAM entity as a response reports it.
//!
//! Answering a read is not the same as copying out the record the account holds: a stored
//! record is this crate's own shape, while a response carries the entity's MODELLED
//! members, in the form the model and IAM's own documentation say each of them carries. Two
//! differences are properties of the MEMBER rather than of any one operation, so they live
//! here once and every reader goes through this module:
//!
//! * a member whose target is a policy document carries the ENCODED form
//!   ([`super::policy_document_reports::reported_policy_document`]);
//! * a member that reports a SET the entity holds — the tags attached to it, the last use
//!   recorded against a role — is REPORTED, empty when the entity holds nothing in it.
//!
//! The second is why a full read of an untagged policy still answers with a `Tags` member:
//! the member is the report of the entity's tag set, and the empty set has a report. Leaving
//! it out would make the member a three-valued answer — these tags, no tags, or not
//! reported — which a caller cannot read, since it could not tell an entity carrying no tags
//! from one whose tags were withheld. The store spells the same emptiness as an absent value
//! (a tag set every key was removed from is held as none at all, the one spelling a write of
//! an empty set leaves behind), so the two spellings meet here rather than in each handler.
//!
//! Those set members belong to the reader OF THAT ENTITY. Where one entity's report CONTAINS
//! another — the roles an instance profile holds — the contained entity carries the members
//! the containing report is about ([`reported_contained_role`]), and its own tag set is read
//! through its own reader, which is the same division IAM draws for the entity LISTINGS:
//! "IAM resource-listing operations return a subset of the available attributes for the
//! resource. This operation does not return the following attributes, even though they are an
//! attribute of the returned object" (`ListRoles`). That subset is [`super::listed_role`] and
//! the helpers beside it, which a listing applies to the report this module builds — so a
//! listing's own answer keeps nothing this module filled in.

use crate::ops::types::{InstanceProfile, Policy, PolicyVersion, Role, RoleLastUsed, Tag, User};

use super::policy_document_reports::reported_policy_document;

/// The `Tags` of an entity a full read reports: the tags the record holds, and the EMPTY
/// list for a record holding none.
///
/// `Tag` lists are the member every tagged IAM entity carries (`tagListType`, length
/// 0..50), and a reader of one entity reports it: "a list of tags that are attached to the
/// specified IAM OIDC provider" (`GetOpenIDConnectProviderResponse$Tags`) is the set, so an
/// entity attached to no tag is reported with the empty set rather than with no member.
#[must_use]
pub fn reported_tags(held: Option<Vec<Tag>>) -> Option<Vec<Tag>> {
    Some(held.unwrap_or_default())
}

/// The `RoleLastUsed` a full read of a role reports: what the account knows about the
/// role's last use, and the EMPTY detail for a role nothing has been recorded against.
///
/// "Contains information about the last time that an IAM role was used" (`Role$RoleLastUsed`)
/// — both of its members are optional, so the detail of a role that has never been assumed
/// is the one carrying neither a date nor a region, which is what a reader of such a role
/// reports. No date is invented for it: a role that was never used has no last-used instant,
/// and the empty detail says exactly that.
#[must_use]
pub fn reported_role_last_used(held: Option<RoleLastUsed>) -> Option<RoleLastUsed> {
    Some(held.unwrap_or_default())
}

/// One role as a response reports it: its trust policy encoded
/// ([`reported_policy_document`]), its tags and its last-use detail reported
/// ([`reported_tags`], [`reported_role_last_used`]), and every other member as the account
/// holds it.
///
/// `Role$AssumeRolePolicyDocument` targets `policyDocumentType`, so it is encoded wherever a
/// `Role` is written out — a role read on its own, a page of roles, and the roles embedded in
/// an instance profile ([`reported_contained_role`], which is that encode step alone).
#[must_use]
pub fn reported_role(role: Role) -> Role {
    let contained = reported_contained_role(role);
    Role {
        tags: reported_tags(contained.tags),
        role_last_used: reported_role_last_used(contained.role_last_used),
        ..contained
    }
}

/// One role as the CREATE that minted it answers: its trust policy as the REQUEST wrote it,
/// percent-encoded ([`reported_policy_document`]) like every reported document, and every other
/// member as the created record holds it ([`reported_role`]).
///
/// A create answers with the document it was HANDED, not with the rendering the account keeps
/// for it ([`super::trust_policy::stored_trust_policy`]): the two differ wherever the caller
/// wrote a document IAM renders differently — whitespace between the JSON tokens, a one-element
/// string-or-array element, an account principal named by its id alone — and the create's answer
/// restates the caller's own text, so a client that compares what it sent with what came back
/// sees the policy it sent. What the account HOLDS is the rendering, which is what every later
/// read of the role reports ([`reported_role`], and so `GetRole`, `ListRoles`, the
/// instance-profile readers and `UpdateRoleDescription`), so a client reads the rendering from
/// the moment it asks the store rather than the request back.
#[must_use]
pub fn created_role_report(role: Role, submitted_document: &str) -> Role {
    Role {
        assume_role_policy_document: Some(reported_policy_document(submitted_document)),
        ..reported_role(role)
    }
}

/// One role as the report of ANOTHER entity carries it: its trust policy encoded
/// ([`reported_policy_document`]) and every other member as the account holds it.
///
/// A role inside an instance profile's `Roles` is reported because the PROFILE was read, not
/// because the role was: what the account knows about that role's tags and its last use is
/// answered by a reader of the role itself, the way IAM answers it for a role a listing
/// reported ([`super::listed_role`]).
#[must_use]
pub fn reported_contained_role(role: Role) -> Role {
    Role {
        assume_role_policy_document: role
            .assume_role_policy_document
            .as_deref()
            .map(reported_policy_document),
        ..role
    }
}

/// One instance profile as a response reports it: its own tags ([`reported_tags`]) and each
/// role it contains as [`reported_contained_role`] reports one, since `InstanceProfile$Roles`
/// carries the ROLE and therefore that role's trust policy.
#[must_use]
pub fn reported_instance_profile(instance_profile: InstanceProfile) -> InstanceProfile {
    InstanceProfile {
        roles: instance_profile
            .roles
            .into_iter()
            .map(reported_contained_role)
            .collect(),
        tags: reported_tags(instance_profile.tags),
        ..instance_profile
    }
}

/// One managed policy as a response reports it: its tags ([`reported_tags`]) and its
/// metadata as it stands.
///
/// The `Policy` shape carries no document — the document a version holds is reported by
/// [`reported_policy_version`] — so the encoding rule does not reach this one.
#[must_use]
pub fn reported_policy(policy: Policy) -> Policy {
    Policy {
        tags: reported_tags(policy.tags),
        ..policy
    }
}

/// One user as a response reports it: its tags ([`reported_tags`]) and every other member as
/// the account holds it.
#[must_use]
pub fn reported_user(user: User) -> User {
    User {
        tags: reported_tags(user.tags),
        ..user
    }
}

/// One managed-policy version as a response reports it: its `Document`
/// ([`reported_policy_document`]) when the reader carries one at all, and its metadata as it
/// stands.
#[must_use]
pub fn reported_policy_version(version: PolicyVersion) -> PolicyVersion {
    PolicyVersion {
        document: version.document.as_deref().map(reported_policy_document),
        ..version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a reported tag set is the set the record holds, and the empty set for a record
    /// holding none — never an absent member.
    #[test]
    fn a_reported_tag_set_is_present_however_many_tags_the_record_holds() {
        // The rows differ along what the rule generalises over: how the record spells what it
        // holds (absent, an empty list, one tag, several).
        let rows: [(&str, Option<Vec<Tag>>, usize); 4] = [
            ("a record holding no tag set at all", None, 0),
            ("a record holding an empty tag set", Some(Vec::new()), 0),
            (
                "a record holding one tag",
                Some(vec![Tag {
                    key: "team".to_string(),
                    value: "payments".to_string(),
                }]),
                1,
            ),
            (
                "a record holding several tags",
                Some(vec![
                    Tag {
                        key: "env".to_string(),
                        value: "test".to_string(),
                    },
                    Tag {
                        key: "team".to_string(),
                        value: "payments".to_string(),
                    },
                ]),
                2,
            ),
        ];

        for (case, held, expected) in rows {
            let reported = reported_tags(held.clone());
            let actual = reported
                .as_ref()
                .map(|tags| tags.len())
                .unwrap_or_else(|| panic!("case {case:?}: the member must be reported, got None"));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected} reported tag(s), got {actual}"
            );
            if let Some(held) = held {
                let reported = reported.expect("reported above");
                assert_eq!(
                    reported.len(),
                    held.len(),
                    "case {case:?}: every held tag must be reported, got {reported:?}"
                );
            }
        }
    }

    /// RULE: a role's last-use detail is reported whether or not anything is recorded against
    /// it, and nothing is invented for a role that has never been used.
    #[test]
    fn a_role_last_used_detail_is_reported_for_a_role_that_was_never_used() {
        let reported = reported_role_last_used(None)
            .expect("the member must be reported for a role that was never used");
        assert!(
            reported.last_used_date.is_none() && reported.region.is_none(),
            "expected an empty detail, got {reported:?}"
        );

        let recorded = RoleLastUsed {
            last_used_date: Some(::aws_smithy_types::DateTime::from_secs(1_700_000_000)),
            region: Some("us-west-2".to_string()),
        };
        let reported = reported_role_last_used(Some(recorded.clone()))
            .expect("the member must be reported for a role that was used");
        assert_eq!(
            reported.region, recorded.region,
            "expected the recorded region, got {reported:?}"
        );
        assert_eq!(
            reported.last_used_date, recorded.last_used_date,
            "expected the recorded instant, got {reported:?}"
        );
    }
}
