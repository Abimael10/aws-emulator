// SPDX-License-Identifier: CC0-1.0

//! How CloudWatchOmni words the refusal of a name the addressed SPACE already holds, read
//! off the wire as a real client reads it.
//!
//! `ConflictException` is modelled with a required `message` and no fixed text, so the
//! wording of each conflict class is the service's own recorded behavior and a requirement
//! of the refusal. The class covered here is the one where a name identifies a resource
//! only WITHIN a space: the sentence names the resource KIND and states that the space
//! already holds one under that name, and it has no variable part at all — neither the
//! `name` the request carried nor the `spaceId` it addressed appears, and there is no
//! trailing period.
//!
//! The sentences are written out as LITERALS here rather than read from the crate, so this
//! suite pins the wire text itself: a template that drifted back to interpolating the
//! request or the store fails here even if every caller still reaches it through one
//! constant.
//!
//! The rows span what the rule generalises over rather than one recorded exchange:
//!
//! * the operation that reports the conflict — a create (`CreateAccessProfile`,
//!   `CreateAccessGrant`) and a rename (`UpdateAccessProfile`);
//! * the resource kind, which is the sentence's only difference between two of them, as
//!   [`the_two_kinds_differ_only_in_the_kind_they_name`] asserts over the two refusals the
//!   service actually returns;
//! * the submitted name, over the whole `@length` and character class the model admits for
//!   each kind (`AccessProfileName` is `@length(1..256)`, `^[a-zA-Z0-9 _.:/=+@-]+$`;
//!   `CreateAccessGrantInput.name` is `@length(1..64)`, `^[a-zA-Z0-9_-]+$`);
//! * the space addressed, because two different spaces earn the same sentence.
//!
//! Every call goes through the emulator the access-profile suite builds
//! ([`super::access_profile_lifecycle::Omni`]), so the plumbing that signs, dispatches and
//! shares one engine is stated once for the crate rather than copied here.
//!
//! Every assertion names its row and the actual value.

use aws_smithy_schema::serde::SerializableStruct;
use aws_smithy_schema::Schema;

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::types::{
    AccessGrantPrincipal, CreateAccessGrantInput, CreateAccessProfileInput,
    UpdateAccessProfileInput, CREATE_ACCESS_GRANT_INPUT_SCHEMA, CREATE_ACCESS_PROFILE_INPUT_SCHEMA,
    UPDATE_ACCESS_PROFILE_INPUT_SCHEMA,
};

/// One emulator: one shared engine, the service mounted on a router, and the request
/// metadata a handler reads — so the resources the setup creates are the ones the refused
/// request collides with.
use super::access_profile_lifecycle::Omni;
/// The `__type` and `message` of an rpcv2Cbor error envelope, decoded as CBOR, so the
/// message asserted on is the WHOLE message.
use super::validation_message_wording::error_envelope;

/// The error code the model declares for a conflict, with `@httpError 409`.
const CONFLICT_EXCEPTION: &str = "ConflictException";

/// The status that error carries.
const CONFLICT_STATUS: u16 = 409;

/// The WHOLE message a refused access-profile name earns, as a recorded refusal of this
/// service words it: fixed text, with neither the requested name nor the space in it.
const PROFILE_NAME_TAKEN: &str = "An access profile with that name already exists in this space";

/// The same sentence for the other kind a space holds by name.
const GRANT_NAME_TAKEN: &str = "An access grant with that name already exists in this space";

/// How each kind is named at the head of its sentence.
const ACCESS_PROFILE_KIND: &str = "An access profile";
const ACCESS_GRANT_KIND: &str = "An access grant";

/// `AccessProfileName` rows: the shortest name the model admits, one carrying every
/// punctuation character of its class, one whose punctuation is the separator a template
/// would most plausibly quote, and the longest name it admits.
fn profile_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "Read Only _.:/=+@- profile".to_string(),
        "profile.with.dots".to_string(),
        "n".repeat(256),
    ]
}

/// `CreateAccessGrantInput.name` rows: the shortest its `@length(1..64)` admits, one
/// carrying both punctuation characters of its narrower class, and the longest it admits.
fn grant_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "read_only-grant".to_string(),
        "g".repeat(64),
    ]
}

fn profile_create(space_id: &str, name: &str) -> CreateAccessProfileInput {
    CreateAccessProfileInput {
        space_id: space_id.to_string(),
        name: name.to_string(),
        ..Default::default()
    }
}

/// The smallest grant request the model admits for the parents it names.
fn grant_create(domain_id: &str, space_id: &str, name: &str) -> CreateAccessGrantInput {
    CreateAccessGrantInput {
        domain_id: domain_id.to_string(),
        space_id: space_id.to_string(),
        name: name.to_string(),
        principal: AccessGrantPrincipal {
            principal_type: "IAM_ROLE".to_string(),
            principal_id: Some("arn:aws:iam::000000000000:role/Reader".to_string()),
            principal_attributes: None,
        },
        permission: "READ".to_string(),
        ..Default::default()
    }
}

/// Require that `operation` refuses `input` with the modelled conflict, and return the
/// WHOLE message it stated.
async fn conflict_message<I: SerializableStruct>(
    omni: &Omni,
    case: &str,
    operation: &str,
    input_schema: &Schema,
    input: &I,
) -> String {
    let (status, body) = omni.over_the_wire(operation, input_schema, input).await;
    let (code, message) = error_envelope(case, &body);
    assert_eq!(
        (status, code.as_str()),
        (CONFLICT_STATUS, CONFLICT_EXCEPTION),
        "case {case:?}: expected {CONFLICT_STATUS} {CONFLICT_EXCEPTION}, got {status} {code:?}: \
         {message:?}"
    );
    message
}

/// Require that `operation` SERVES `input`, which is how the resource a later row collides
/// with is created when the suite does not need the record it returns.
async fn served<I: SerializableStruct>(
    omni: &Omni,
    case: &str,
    operation: &str,
    input_schema: &Schema,
    input: &I,
) {
    let (status, body) = omni.over_the_wire(operation, input_schema, input).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {operation} must serve the first request, got {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
}

/// Require that `message` reflects nothing the request carried: not the space it addressed,
/// and not the name it submitted.
///
/// A one- or two-character name can occur inside the sentence's own words, so a name is
/// required withheld only for the rows long enough to be recognisable.
fn withholds(case: &str, message: &str, space_id: &str, name: &str) {
    assert!(
        !message.contains(space_id),
        "case {case:?}: the refusal names the space ({space_id:?}): {message:?}"
    );
    assert!(
        name.len() <= 3 || !message.contains(name),
        "case {case:?}: the refusal names the submitted name ({name:?}): {message:?}"
    );
    assert!(
        !message.ends_with('.'),
        "case {case:?}: the refusal carries a trailing period: {message:?}"
    );
}

#[tokio::test]
async fn a_profile_name_a_space_already_holds_is_refused_with_the_recorded_sentence() {
    let omni = Omni::new();
    let spaces = [
        (
            "the first space",
            omni.space("profile wording", "wording-space-a").await,
        ),
        (
            "the second space",
            omni.space("profile wording", "wording-space-b").await,
        ),
    ];

    for name in profile_names() {
        for (label, space_id) in &spaces {
            let case = format!("{name:?} in {label}");
            omni.create_access_profile(&profile_create(space_id, &name))
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the first CreateAccessProfile failed: {error:?}")
                });
            let message = conflict_message(
                &omni,
                &case,
                "CreateAccessProfile",
                CREATE_ACCESS_PROFILE_INPUT_SCHEMA,
                &profile_create(space_id, &name),
            )
            .await;
            assert_eq!(
                message, PROFILE_NAME_TAKEN,
                "case {case:?}: expected {PROFILE_NAME_TAKEN:?}, got {message:?}"
            );
            withholds(&case, &message, space_id, &name);
        }
    }
}

#[tokio::test]
async fn a_rename_onto_a_name_the_space_already_holds_is_refused_with_the_same_sentence() {
    let omni = Omni::new();
    let spaces = [
        (
            "the first space",
            omni.space("rename wording", "rename-space-a").await,
        ),
        (
            "the second space",
            omni.space("rename wording", "rename-space-b").await,
        ),
    ];

    for name in profile_names() {
        for (label, space_id) in &spaces {
            let case = format!("renaming onto {name:?} in {label}");
            // The name the rename collides with, held by another profile of that space.
            omni.create_access_profile(&profile_create(space_id, &name))
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the colliding profile was not created: {error:?}")
                });
            // The profile being renamed, which is a different resource under a name of its
            // own, so the collision is with the other profile and not with itself.
            let renamed = omni
                .create_access_profile(&profile_create(space_id, &format!("other {}", name.len())))
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the profile to rename was not created: {error:?}")
                })
                .access_profile
                .profile_id;
            let message = conflict_message(
                &omni,
                &case,
                "UpdateAccessProfile",
                UPDATE_ACCESS_PROFILE_INPUT_SCHEMA,
                &UpdateAccessProfileInput {
                    space_id: space_id.to_string(),
                    profile_id: renamed.clone(),
                    name: Some(name.clone()),
                    description: None,
                },
            )
            .await;
            assert_eq!(
                message, PROFILE_NAME_TAKEN,
                "case {case:?}: expected {PROFILE_NAME_TAKEN:?}, got {message:?}"
            );
            withholds(&case, &message, space_id, &name);
            assert!(
                !message.contains(&renamed),
                "case {case:?}: the refusal names the profile the request addressed \
                 ({renamed:?}): {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_grant_name_a_space_already_holds_is_refused_with_its_own_kinds_sentence() {
    let omni = Omni::new();
    let domain_id = omni.domain("grant wording").await;
    let spaces = [
        (
            "the first space",
            omni.space("grant wording", "grant-space-a").await,
        ),
        (
            "the second space",
            omni.space("grant wording", "grant-space-b").await,
        ),
    ];

    for name in grant_names() {
        for (label, space_id) in &spaces {
            let case = format!("{name:?} in {label}");
            served(
                &omni,
                &case,
                "CreateAccessGrant",
                CREATE_ACCESS_GRANT_INPUT_SCHEMA,
                &grant_create(&domain_id, space_id, &name),
            )
            .await;
            let message = conflict_message(
                &omni,
                &case,
                "CreateAccessGrant",
                CREATE_ACCESS_GRANT_INPUT_SCHEMA,
                &grant_create(&domain_id, space_id, &name),
            )
            .await;
            assert_eq!(
                message, GRANT_NAME_TAKEN,
                "case {case:?}: expected {GRANT_NAME_TAKEN:?}, got {message:?}"
            );
            withholds(&case, &message, space_id, &name);
        }
    }
}

/// The invariant across the kinds, asserted on the two messages the service actually
/// RETURNS rather than on any text written here: strip the kind each refusal names and the
/// remainder is the SAME sentence, so one template words the class and a client can
/// recognise it whichever resource it addressed.
///
/// One name is submitted to both kinds — it satisfies both `AccessProfileName` and
/// `CreateAccessGrantInput.name` — so the two refusals differ in nothing but the kind.
#[tokio::test]
async fn the_two_kinds_differ_only_in_the_kind_they_name() {
    let omni = Omni::new();
    let domain_id = omni.domain("one template").await;
    let space_id = omni.space("one template", "template-space").await;
    let name = "shared-name-row";

    omni.create_access_profile(&profile_create(&space_id, name))
        .await
        .unwrap_or_else(|error| panic!("the colliding profile was not created: {error:?}"));
    served(
        &omni,
        "the grant holding the name",
        "CreateAccessGrant",
        CREATE_ACCESS_GRANT_INPUT_SCHEMA,
        &grant_create(&domain_id, &space_id, name),
    )
    .await;

    let profile_refusal = conflict_message(
        &omni,
        "a second profile under the name",
        "CreateAccessProfile",
        CREATE_ACCESS_PROFILE_INPUT_SCHEMA,
        &profile_create(&space_id, name),
    )
    .await;
    let grant_refusal = conflict_message(
        &omni,
        "a second grant under the name",
        "CreateAccessGrant",
        CREATE_ACCESS_GRANT_INPUT_SCHEMA,
        &grant_create(&domain_id, &space_id, name),
    )
    .await;

    assert_ne!(
        profile_refusal, grant_refusal,
        "the two kinds must be told apart, got {profile_refusal:?} for both"
    );
    let profile_tail = profile_refusal
        .strip_prefix(ACCESS_PROFILE_KIND)
        .unwrap_or_else(|| {
            panic!(
                "the profile refusal must open by naming its kind \
                 ({ACCESS_PROFILE_KIND:?}), got {profile_refusal:?}"
            )
        });
    let grant_tail = grant_refusal
        .strip_prefix(ACCESS_GRANT_KIND)
        .unwrap_or_else(|| {
            panic!(
                "the grant refusal must open by naming its kind ({ACCESS_GRANT_KIND:?}), got \
                 {grant_refusal:?}"
            )
        });
    assert_eq!(
        profile_tail, grant_tail,
        "the two refusals must differ only in the kind they name, got {profile_tail:?} and \
         {grant_tail:?}"
    );
    assert!(
        !profile_tail.is_empty(),
        "the sentence must say more than the kind, got {profile_refusal:?}"
    );
    for (case, message) in [
        ("the profile refusal", &profile_refusal),
        ("the grant refusal", &grant_refusal),
    ] {
        withholds(case, message, &space_id, name);
    }
}
