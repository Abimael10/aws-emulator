// SPDX-License-Identifier: CC0-1.0

//! What a Studio SESSION MAPPING request has to satisfy, and how the Studio backend words
//! each refusal when it does not.
//!
//! One concern of this service's state, beside [`super::studio_identities`]: that module
//! answers WHICH identity of IAM Identity Center a request names, and this one answers what
//! the four session-mapping operations require of a request and what they say when it falls
//! short — the pair of members that name the identity, the identity being one the Studio's
//! directory holds, the session policy being an ARN, and the mapping itself being present (or
//! absent) as the operation needs.
//!
//! Every sentence here is stated by the backend that fronts EMR Studio, so each one is built
//! through that backend's wrapper ([`super::editors_refusal_message`]): the sentence, then the
//! parenthesised detail naming the backend, the status, the error code and the response's own
//! request id. Nothing of the request is interpolated into any of them — the Studio id, the
//! identity and the session policy a caller sent are not part of the text — so a client
//! matching a sentence sees the same text for every instance of its class.

use super::*;

/// The member a request names the identity by its GUID under, as the model spells it.
pub const IDENTITY_ID_MEMBER: &str = "IdentityId";

/// The member a request names the identity by its name under, as the model spells it.
pub const IDENTITY_NAME_MEMBER: &str = "IdentityName";

/// How the Studio backend refuses a request that does not name EXACTLY one identity.
///
/// "Either `IdentityName` or `IdentityId` must be specified, but not both"
/// (`CreateStudioSessionMapping$IdentityId` doc) is the rule; this is the whole sentence the
/// backend states it with, which says both halves of it — that one of the two must be given,
/// and that only one of them may be — so a caller learns which of the two input errors it
/// made.
pub fn identity_members_message() -> String {
    editors_refusal_message(
        "Either IdentityName or IdentityId must be specified and only one of them must be \
         specified.",
    )
}

/// How the Studio backend refuses a request addressing a session mapping the Studio does not
/// hold: the statement that the mapping does not exist.
///
/// Recorded behavior for the text, shared by `GetStudioSessionMapping`,
/// `UpdateStudioSessionMapping` and `DeleteStudioSessionMapping` — those operations model no
/// not-found error shape, so an absent mapping is reported as the one client error they model
/// and it is the SENTENCE that tells a caller the mapping is gone. Neither the Studio id nor
/// the identity is echoed.
pub fn absent_session_mapping_message() -> String {
    editors_refusal_message("Studio session mapping does not exist.")
}

/// How the Studio backend refuses a create for an identity the Studio is already mapped to:
/// the statement that the mapping already exists.
///
/// Recorded behavior for the text. It carries no remediation clause and echoes neither the
/// Studio id nor the identity, so a client that treats "already exists" as benign — an
/// idempotent re-apply — recognises the whole class by this one sentence.
pub fn existing_session_mapping_message() -> String {
    editors_refusal_message("Studio session mapping already exists.")
}

/// How the Studio backend refuses an identity member that names no identity of the kind the
/// request asked for: the member, as the model spells it, and that it is invalid.
///
/// Recorded behavior for the text on `IdentityId`, and it is the MEMBER that varies: a request
/// that named the identity by its name is told about `IdentityName`, because that is the member
/// that carried the value the directory could not resolve. The value itself is not echoed.
pub fn invalid_identity_message(member: &str) -> String {
    editors_refusal_message(&format!("{member} is invalid."))
}

/// How the Studio backend refuses a `SessionPolicyArn` that is not the ARN of an IAM policy:
/// the statement that the member is malformed.
///
/// Recorded behavior for the text. The rejected value is not echoed, so every malformed
/// session policy is refused with the one sentence.
pub fn malformed_session_policy_arn_message() -> String {
    editors_refusal_message("SessionPolicyArn is malformed.")
}

/// What `SessionPolicyArn` has to name: "the Amazon Resource Name (ARN) for the session policy
/// that will be applied to the user or group … not the ARN of your user role"
/// (`CreateStudioSessionMapping$SessionPolicyArn` doc) — an IAM POLICY, so the ARN's vendor is
/// `iam` and its resource type is `policy`.
///
/// Read against the workspace's ONE ARN parse ([`::service_common::arn`]), so what counts as a
/// well-formed ARN here is what counts as one anywhere else in this emulator.
const SESSION_POLICY: ::service_common::arn::Requirement = ::service_common::arn::Requirement {
    vendor: "iam",
    resource_type: Some(POLICY_RESOURCE_TYPE),
    label: "an IAM policy",
};

/// Whether `value` names a session policy at all: the ARN of an IAM policy
/// ([`SESSION_POLICY`]), naming a policy inside it.
///
/// The member is `XmlStringMaxLen256`, which admits any text, so the FORM is checked here:
/// a value that is not that ARN — or that is one naming no policy — names no policy the
/// Studio's users could ever be given.
#[must_use]
pub fn names_a_session_policy(value: &str) -> bool {
    SESSION_POLICY.admits(value)
        && ::service_common::arn::resource_id_of_type(value, POLICY_RESOURCE_TYPE)
            .is_some_and(|policy| !policy.trim().is_empty())
}

/// The resource type an IAM policy ARN names, as [`SESSION_POLICY`] requires it.
const POLICY_RESOURCE_TYPE: &str = "policy";

/// Whether this Studio maps identities out of an IAM Identity Center DIRECTORY.
///
/// "The ARN of the IAM Identity Center instance the Studio application belongs to"
/// (`Studio$IdcInstanceArn` doc): a Studio bound to such an instance maps the users and groups
/// of that instance's identity store, which is where EMR resolves the identity a session
/// mapping names. A Studio bound to none names no directory at all, so there is nothing to
/// resolve an identity in and nothing to refuse one against.
#[must_use]
pub fn studio_names_identity_center(studio: &Studio) -> bool {
    studio
        .idc_instance_arn
        .as_deref()
        .is_some_and(|arn| !arn.trim().is_empty())
}

/// Whether the identity a session-mapping request names is one the Studio's own IAM Identity
/// Center directory REFUSES: the Studio is bound to such an instance
/// ([`studio_names_identity_center`]) and that directory holds no user or group of the kind
/// the request asked for ([`look_up_identity`]).
///
/// A mapping to an identity the directory has no record of would grant its holder nothing and
/// could never be used, so a Studio that has a directory holds its mappings to it. A Studio
/// bound to none names no directory at all, so there is nothing to hold them to and the
/// request's own members stand as they arrived.
#[must_use]
pub fn directory_refuses_identity(studio: &Studio, lookup: &IdentityLookup) -> bool {
    studio_names_identity_center(studio) && !matches!(lookup, IdentityLookup::Held(_))
}

/// Whether that directory holds the value as the OTHER kind of identity
/// ([`IdentityLookup::WrongType`]).
///
/// A mapping is keyed by its identity type as well as by the identity, so a request that names
/// a group's GUID as a user names no identity of the kind it asked for: what is wrong is the
/// member that carried the value, and the mapping the identity really has under its own kind is
/// untouched. Answered only for a Studio with a directory to be wrong against.
#[must_use]
pub fn directory_holds_other_type(studio: &Studio, lookup: &IdentityLookup) -> bool {
    studio_names_identity_center(studio) && *lookup == IdentityLookup::WrongType
}

/// Which member of `requested` named the identity: the GUID when the request carried one, and
/// the name otherwise.
///
/// Exactly one of the two is ever carried ([`session_mapping_identity`] refuses a request that
/// names neither or both), so this is the member a refusal about the identity states.
#[must_use]
pub fn identity_member_of(requested: &MappedIdentity) -> &'static str {
    if requested.identity_id.is_some() {
        IDENTITY_ID_MEMBER
    } else {
        IDENTITY_NAME_MEMBER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope every sentence of this plane is stated inside, as a caller reads it: the
    /// sentence, then the backend detail. Only the request id varies, so a case asserts the
    /// text around it.
    fn envelope(sentence: &str, message: &str) -> bool {
        let opening = format!(
            "{sentence} (Service: AWSEditors; Status Code: 400; Error Code: \
             InvalidRequestException; Request ID: "
        );
        message.starts_with(&opening)
            && message.ends_with("; Proxy: null)")
            && message.len() > opening.len() + "; Proxy: null)".len()
    }

    /// Every session-mapping refusal states its own sentence inside the backend's envelope,
    /// and interpolates nothing of the request.
    #[test]
    fn each_refusal_states_its_sentence_inside_the_backend_envelope() {
        let cases: Vec<(&str, String)> = vec![
            (
                "Studio session mapping does not exist.",
                absent_session_mapping_message(),
            ),
            (
                "Studio session mapping already exists.",
                existing_session_mapping_message(),
            ),
            (
                "Either IdentityName or IdentityId must be specified and only one of them \
                 must be specified.",
                identity_members_message(),
            ),
            (
                "IdentityId is invalid.",
                invalid_identity_message(IDENTITY_ID_MEMBER),
            ),
            (
                "IdentityName is invalid.",
                invalid_identity_message(IDENTITY_NAME_MEMBER),
            ),
            (
                "SessionPolicyArn is malformed.",
                malformed_session_policy_arn_message(),
            ),
        ];
        for (sentence, message) in &cases {
            assert!(
                envelope(sentence, message),
                "case {sentence:?}: expected the sentence inside the backend envelope, \
                 got {message:?}"
            );
        }
    }

    /// Which values name a session policy: the ARN of an IAM policy, and nothing else. The
    /// rows span what the rule generalises over — a policy ARN with and without an account,
    /// a value that is no ARN at all, an ARN of another vendor, and an IAM ARN of another
    /// resource type (a role, which the member's own documentation rules out).
    #[test]
    fn a_session_policy_is_the_arn_of_an_iam_policy() {
        let cases = [
            ("arn:aws:iam::aws:policy/EMRStudioBasic", true),
            (
                "arn:aws:iam::000000000000:policy/service-role/EMRStudioUser",
                true,
            ),
            ("arn:aws-us-gov:iam::000000000000:policy/Studio", true),
            ("not-an-arn", false),
            ("", false),
            ("arn:aws:iam::000000000000:role/EMR_Studio_UserRole", false),
            ("arn:aws:s3:::bucket/policy", false),
            ("arn:aws:iam::000000000000:policy", false),
        ];
        for (value, expected) in cases {
            let answered = names_a_session_policy(value);
            assert_eq!(
                answered, expected,
                "case {value:?}: expected {expected}, got {answered}"
            );
        }
    }

    /// Which member a refusal about the identity names: the one the request carried.
    #[test]
    fn the_refusal_names_the_member_the_request_carried() {
        let cases = [
            (
                MappedIdentity {
                    identity_id: Some("6ae6d4e8-7061-70e2-6b42-example".to_string()),
                    identity_name: None,
                },
                IDENTITY_ID_MEMBER,
            ),
            (
                MappedIdentity {
                    identity_id: None,
                    identity_name: Some("analyst@example.com".to_string()),
                },
                IDENTITY_NAME_MEMBER,
            ),
        ];
        for (requested, expected) in &cases {
            let answered = identity_member_of(requested);
            assert_eq!(
                answered, *expected,
                "case {requested:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
