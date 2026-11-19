// SPDX-License-Identifier: CC0-1.0

//! What the Studio backend SAYS when it refuses a session-mapping request, end to end over
//! the wire, with the IAM Identity Center identity store mounted beside Amazon EMR on ONE
//! engine (the harness `tests/all/studio_session_mapping_identity.rs` owns).
//!
//! THE RULES these cases assert, for the four session-mapping operations:
//!
//! * a request has to name EXACTLY one identity — "either `IdentityName` or `IdentityId` must
//!   be specified, but not both" (`CreateStudioSessionMapping$IdentityId` doc);
//! * a Studio bound to an IAM Identity Center instance (`Studio$IdcInstanceArn`) maps the
//!   users and groups of that instance's identity store, so the identity a request names is
//!   resolved there and one that resolves to no user or group of the `IdentityType` asked for
//!   is refused, naming the member that carried it — and nothing is mapped for it;
//! * `SessionPolicyArn` is "the Amazon Resource Name (ARN) for the session policy"
//!   (`CreateStudioSessionMapping$SessionPolicyArn` doc), so a value that is not the ARN of an
//!   IAM policy is refused and changes nothing;
//! * a mapping exists once per (Studio, `IdentityType`, identity), so a second create of one
//!   is refused as already existing and a read, an update or a delete of one that is not
//!   there is refused as not existing.
//!
//! Every one of those refusals is the Studio backend's: HTTP 400, the one client error these
//! operations model, and a MESSAGE that is the backend's own sentence followed by its service
//! detail — the backend, the status, the error code and the response's own request id. The
//! request id is the one variable part, so each case lifts the observed id into the expected
//! message before comparing it, exactly as a client classifying the refusal by its sentence
//! does.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::assert_refused;
use crate::refusal_messages::{message, wrapped_request_id};
use crate::studio_session_mapping_identity::{emulator, Emulator, SESSION_POLICY};

/// The IAM Identity Center instance the Studios below belong to: "the ARN of the IAM Identity
/// Center instance the Studio application belongs to" (`Studio$IdcInstanceArn` doc), which is
/// what gives a Studio a directory to resolve its identities in.
const IDC_INSTANCE_ARN: &str = "arn:aws:sso:::instance/ssoins-0123456789abcdef";

/// The members a `CreateStudio` of a Studio with a directory carries beside the rest.
fn bound_to_identity_center() -> Value {
    json!({ "IdcInstanceArn": IDC_INSTANCE_ARN })
}

/// Assert that `payload` is the Studio backend's refusal of `sentence`: HTTP 400, the modelled
/// client error, and the sentence inside the backend's service detail.
///
/// The request id the detail carries is the response's own, so it is read out of the message
/// and lifted into the expected text ([`wrapped_request_id`]): what the case pins is the
/// sentence and every label around it.
fn assert_backend_refusal(case: &str, status: u16, payload: &Value, sentence: &str) {
    assert_refused(case, status, payload, "InvalidRequestException");
    let said = message(case, payload);
    let request_id = wrapped_request_id(case, &said);
    let expected = format!(
        "{sentence} (Service: AWSEditors; Status Code: 400; Error Code: \
         InvalidRequestException; Request ID: {request_id}; Proxy: null)"
    );
    assert_eq!(
        said, expected,
        "case {case:?}: expected {expected:?}, got {said:?}"
    );
}

/// A create body for `studio_id` naming the identity `member` = `value`.
fn create_body(studio_id: &str, identity_type: &str, member: &str, value: &str) -> Value {
    let mut body = json!({
        "StudioId": studio_id,
        "IdentityType": identity_type,
        "SessionPolicyArn": SESSION_POLICY,
    });
    body[member] = json!(value);
    body
}

/// The same body without the session policy: what a read or a delete addresses a mapping with.
fn lookup_body(studio_id: &str, identity_type: &str, member: &str, value: &str) -> Value {
    let mut body = create_body(studio_id, identity_type, member, value);
    body.as_object_mut().expect("an object").remove("SessionPolicyArn");
    body
}

/// The mappings a Studio reports, as `(IdentityType, IdentityId, IdentityName)` triples.
async fn listed(emulator: &Emulator, case: &str, studio_id: &str) -> Vec<(String, String, String)> {
    let page = emulator
        .emr(case, "ListStudioSessionMappings", json!({ "StudioId": studio_id }))
        .await;
    page["SessionMappings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| {
            (
                row["IdentityType"].as_str().unwrap_or_default().to_string(),
                row["IdentityId"].as_str().unwrap_or_default().to_string(),
                row["IdentityName"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// THE RULE: a request naming neither or both of the two identity members is refused with the
/// backend's whole sentence for it, whichever operation was asked.
///
/// The rows differ along what the rule generalises over: how the pair is wrong (neither
/// member, both members, a member present but naming nothing) and which operation was asked.
#[tokio::test]
async fn a_request_must_name_exactly_one_identity_member() {
    let emulator = emulator();
    let studio_id = emulator
        .studio_with("members", bound_to_identity_center())
        .await;
    let user_id = emulator.user("members", "analyst@example.com").await;

    let identities = [
        ("neither member", json!({})),
        (
            "both members",
            json!({ "IdentityId": &user_id, "IdentityName": "analyst@example.com" }),
        ),
        ("a name naming nothing", json!({ "IdentityName": "" })),
        ("a GUID naming nothing", json!({ "IdentityId": "   " })),
    ];
    for (label, identity) in &identities {
        for operation in [
            "CreateStudioSessionMapping",
            "GetStudioSessionMapping",
            "UpdateStudioSessionMapping",
            "DeleteStudioSessionMapping",
        ] {
            let mut body = json!({
                "StudioId": &studio_id,
                "IdentityType": "USER",
                "SessionPolicyArn": SESSION_POLICY,
            });
            for (member, value) in identity.as_object().into_iter().flatten() {
                body[member] = value.clone();
            }
            let case = format!("{operation} with {label}");
            let (status, payload) = emulator.emr_call(operation, body).await;
            assert_backend_refusal(
                &case,
                status,
                &payload,
                "Either IdentityName or IdentityId must be specified and only one of them \
                 must be specified.",
            );
        }
    }

    // Nothing was mapped by any of those refusals.
    let mappings = listed(&emulator, "after the refusals", &studio_id).await;
    assert!(
        mappings.is_empty(),
        "case {:?}: expected no mapping, got {mappings:?}",
        "after the refusals"
    );
}

/// THE RULE: the identity a mapping names is resolved in the Studio's own Identity Center
/// directory, so one that resolves to no user or group of the `IdentityType` asked for is
/// refused — naming the member that carried it — and no mapping is left behind.
///
/// The rows differ along what the rule generalises over: which member named the identity, the
/// kind asked for, and HOW the value fails to name one of that kind — the directory holds no
/// record of it at all, or holds it as the other kind. The identities the directory does hold
/// are the positive control: each of them maps.
#[tokio::test]
async fn an_identity_the_studios_directory_does_not_hold_is_refused() {
    let emulator = emulator();
    let studio_id = emulator
        .studio_with("directory", bound_to_identity_center())
        .await;
    let user_name = "engineer@example.com";
    let group_name = "emr-studio-engineers";
    let user_id = emulator.user("the directory's user", user_name).await;
    let group_id = emulator.group("the directory's group", group_name).await;

    let rows: [(&str, &str, &str, &str, &str); 6] = [
        (
            "a GUID the directory has no record of",
            "USER",
            "IdentityId",
            "11111111-2222-3333-4444-555555555555",
            "IdentityId",
        ),
        (
            "a user name the directory has no record of",
            "USER",
            "IdentityName",
            "never-provisioned@example.com",
            "IdentityName",
        ),
        (
            "a group name the directory has no record of",
            "GROUP",
            "IdentityName",
            "never-provisioned-group",
            "IdentityName",
        ),
        (
            "a GUID the directory has no record of, asked for as a group",
            "GROUP",
            "IdentityId",
            "66666666-7777-8888-9999-aaaaaaaaaaaa",
            "IdentityId",
        ),
        (
            "the group's GUID, asked for as a user",
            "USER",
            "IdentityId",
            group_id.as_str(),
            "IdentityId",
        ),
        (
            "the user's name, asked for as a group",
            "GROUP",
            "IdentityName",
            user_name,
            "IdentityName",
        ),
    ];
    for (case, identity_type, member, value, refused_member) in &rows {
        let (status, payload) = emulator
            .emr_call(
                "CreateStudioSessionMapping",
                create_body(&studio_id, identity_type, member, value),
            )
            .await;
        assert_backend_refusal(
            case,
            status,
            &payload,
            &format!("{refused_member} is invalid."),
        );
    }

    // The positive control: the identities the directory HOLDS are mapped, by either member.
    for (case, identity_type, member, value) in [
        ("the user by its GUID", "USER", "IdentityId", user_id.as_str()),
        ("the group by its name", "GROUP", "IdentityName", group_name),
    ] {
        emulator
            .emr(
                case,
                "CreateStudioSessionMapping",
                create_body(&studio_id, identity_type, member, value),
            )
            .await;
    }

    // So the Studio holds exactly those two mappings: not one of the refused identities is
    // reported, and each mapped one carries both members of its identity.
    let mut mappings = listed(&emulator, "the whole table", &studio_id).await;
    mappings.sort();
    let mut expected = vec![
        (
            "USER".to_string(),
            user_id.clone(),
            user_name.to_string(),
        ),
        (
            "GROUP".to_string(),
            group_id.clone(),
            group_name.to_string(),
        ),
    ];
    expected.sort();
    assert_eq!(
        mappings, expected,
        "case {:?}: expected only the identities the directory holds, got {mappings:?}",
        "the whole table"
    );
}

/// THE OTHER HALF of that rule: the mapping an identity has under its OWN kind is untouched by
/// a request that asks for it as the other kind, and still reads back as it was created.
#[tokio::test]
async fn asking_for_an_identity_as_the_wrong_kind_leaves_its_mapping_alone() {
    let emulator = emulator();
    let studio_id = emulator
        .studio_with("wrong-kind", bound_to_identity_center())
        .await;
    let group_id = emulator.group("the mapped group", "emr-studio-analysts").await;
    emulator
        .emr(
            "the mapped group",
            "CreateStudioSessionMapping",
            create_body(&studio_id, "GROUP", "IdentityId", &group_id),
        )
        .await;

    // Every operation that addresses a mapping refuses the GUID as a USER: what is wrong is
    // the member, not a mapping that is missing.
    for operation in [
        "GetStudioSessionMapping",
        "UpdateStudioSessionMapping",
        "DeleteStudioSessionMapping",
    ] {
        let body = if operation == "UpdateStudioSessionMapping" {
            create_body(&studio_id, "USER", "IdentityId", &group_id)
        } else {
            lookup_body(&studio_id, "USER", "IdentityId", &group_id)
        };
        let case = format!("{operation} for the group's GUID as a USER");
        let (status, payload) = emulator.emr_call(operation, body).await;
        assert_backend_refusal(&case, status, &payload, "IdentityId is invalid.");
    }

    let mapping = emulator
        .emr(
            "the group's own mapping",
            "GetStudioSessionMapping",
            lookup_body(&studio_id, "GROUP", "IdentityId", &group_id),
        )
        .await["SessionMapping"]
        .clone();
    assert_eq!(
        mapping["SessionPolicyArn"].as_str(),
        Some(SESSION_POLICY),
        "case {:?}: the correctly-typed mapping must be untouched, got {mapping}",
        "the group's own mapping"
    );
    assert_eq!(
        mapping["IdentityType"].as_str(),
        Some("GROUP"),
        "case {:?}: the correctly-typed mapping must be untouched, got {mapping}",
        "the group's own mapping"
    );
}

/// THE RULE: `SessionPolicyArn` names an IAM policy, so a value that is not the ARN of one is
/// refused and the mapping is left exactly as it was.
///
/// The rows differ along what the rule generalises over: a value that is no ARN at all, one
/// that names nothing, an ARN of another vendor, an IAM ARN of another resource type (a role,
/// which the member's own documentation rules out) and an IAM policy ARN naming no policy.
#[tokio::test]
async fn a_session_policy_that_is_not_an_iam_policy_arn_is_refused() {
    let emulator = emulator();
    let studio_id = emulator
        .studio_with("policies", bound_to_identity_center())
        .await;
    let user_name = "analyst@example.com";
    let user_id = emulator.user("the mapped user", user_name).await;

    let malformed = [
        "not-an-arn",
        "",
        "arn:aws:s3:::emr-studio/policy.json",
        "arn:aws:iam::000000000000:role/EMR_Studio_UserRole",
        "arn:aws:iam::000000000000:policy",
    ];

    // A create carrying one of them maps nothing.
    for policy in malformed {
        let mut body = create_body(&studio_id, "USER", "IdentityId", &user_id);
        body["SessionPolicyArn"] = json!(policy);
        let case = format!("CreateStudioSessionMapping with the policy {policy:?}");
        let (status, payload) = emulator.emr_call("CreateStudioSessionMapping", body).await;
        assert_backend_refusal(&case, status, &payload, "SessionPolicyArn is malformed.");

        let (status, payload) = emulator
            .emr_call(
                "GetStudioSessionMapping",
                lookup_body(&studio_id, "USER", "IdentityId", &user_id),
            )
            .await;
        assert_backend_refusal(
            &format!("{case}, read back"),
            status,
            &payload,
            "Studio session mapping does not exist.",
        );
    }

    // And an update carrying one leaves the policy the mapping already had.
    emulator
        .emr(
            "the mapped user",
            "CreateStudioSessionMapping",
            create_body(&studio_id, "USER", "IdentityId", &user_id),
        )
        .await;
    for policy in malformed {
        let mut body = create_body(&studio_id, "USER", "IdentityId", &user_id);
        body["SessionPolicyArn"] = json!(policy);
        let case = format!("UpdateStudioSessionMapping with the policy {policy:?}");
        let (status, payload) = emulator.emr_call("UpdateStudioSessionMapping", body).await;
        assert_backend_refusal(&case, status, &payload, "SessionPolicyArn is malformed.");

        let mapping = emulator
            .emr(
                &case,
                "GetStudioSessionMapping",
                lookup_body(&studio_id, "USER", "IdentityId", &user_id),
            )
            .await["SessionMapping"]
            .clone();
        assert_eq!(
            mapping["SessionPolicyArn"].as_str(),
            Some(SESSION_POLICY),
            "case {case:?}: the stored policy must be untouched, got {mapping}"
        );
    }
}

/// THE RULE: one mapping per (Studio, `IdentityType`, identity) — so a second create of one is
/// refused as already existing, and a read, an update or a delete of one the Studio does not
/// hold is refused as not existing.
///
/// The rows differ along what the rule generalises over: the identity's kind, which member
/// addresses it (a mapping created by one is the same mapping addressed by the other) and
/// which operation asks for it.
#[tokio::test]
async fn the_duplicate_and_absent_mapping_refusals_are_the_backends_own_sentences() {
    let emulator = emulator();
    let studio_id = emulator
        .studio_with("lifecycle", bound_to_identity_center())
        .await;
    let user_name = "analyst@example.com";
    let group_name = "emr-studio-analysts";
    let user_id = emulator.user("the directory's user", user_name).await;
    let group_id = emulator.group("the directory's group", group_name).await;

    let rows: [(&str, &str, &str, &str, &str, &str); 2] = [
        (
            "a user mapped by its GUID",
            "USER",
            "IdentityId",
            user_id.as_str(),
            "IdentityName",
            user_name,
        ),
        (
            "a group mapped by its name",
            "GROUP",
            "IdentityName",
            group_name,
            "IdentityId",
            group_id.as_str(),
        ),
    ];

    for (case, identity_type, member, value, other_member, other_value) in &rows {
        emulator
            .emr(
                case,
                "CreateStudioSessionMapping",
                create_body(&studio_id, identity_type, member, value),
            )
            .await;

        // A second create is refused, whichever member addresses the identity.
        for (addressed, member, value) in [
            ("the same member", *member, *value),
            ("the other member", *other_member, *other_value),
        ] {
            let case = format!("{case}, created again by {addressed}");
            let (status, payload) = emulator
                .emr_call(
                    "CreateStudioSessionMapping",
                    create_body(&studio_id, identity_type, member, value),
                )
                .await;
            assert_backend_refusal(
                &case,
                status,
                &payload,
                "Studio session mapping already exists.",
            );
        }

        // Once deleted, every operation that addresses it reports that it does not exist —
        // again whichever member addresses it.
        emulator
            .emr(
                case,
                "DeleteStudioSessionMapping",
                lookup_body(&studio_id, identity_type, member, value),
            )
            .await;
        for (addressed, member, value) in [
            ("the same member", *member, *value),
            ("the other member", *other_member, *other_value),
        ] {
            for operation in [
                "GetStudioSessionMapping",
                "UpdateStudioSessionMapping",
                "DeleteStudioSessionMapping",
            ] {
                let body = if operation == "UpdateStudioSessionMapping" {
                    create_body(&studio_id, identity_type, member, value)
                } else {
                    lookup_body(&studio_id, identity_type, member, value)
                };
                let case = format!("{operation} after the delete, by {addressed}");
                let (status, payload) = emulator.emr_call(operation, body).await;
                assert_backend_refusal(
                    &case,
                    status,
                    &payload,
                    "Studio session mapping does not exist.",
                );
            }
        }
    }

    let mappings = listed(&emulator, "after every delete", &studio_id).await;
    assert!(
        mappings.is_empty(),
        "case {:?}: expected no mapping left, got {mappings:?}",
        "after every delete"
    );
}
