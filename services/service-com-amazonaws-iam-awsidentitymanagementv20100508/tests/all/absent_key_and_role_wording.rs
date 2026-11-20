// SPDX-License-Identifier: CC0-1.0

//! The two `NoSuchEntity` sentences this service words an absent SSH PUBLIC KEY and an
//! absent ROLE with, end to end over the awsQuery wire.
//!
//! Two rules, each over a FAMILY of operations rather than over one call:
//!
//! 1. every operation addressed by an `SSHPublicKeyId` refuses an id no key is reachable
//!    under — the account never minted it, another user holds it, a delete already took it
//!    away — with HTTP 404, the `NoSuchEntity` code and the one sentence AWS words for that
//!    entity kind: the fixed text `The Public Key with id ` and ` cannot be found.` around
//!    the id the request supplied. The entity is named `Public Key`, with nothing of SSH in
//!    the sentence, and the only variable part is the echoed id (`publicKeyIdType`,
//!    len 20..128, pattern `^[\w]+$`, from `smithy_query(query="operation",
//!    operation="GetSSHPublicKey")`).
//! 2. an operation addressed by a `RoleName` the account holds no role under refuses it with
//!    HTTP 404, `NoSuchEntity` and the one sentence AWS words for a missing role: the fixed
//!    text `The role with name ` and ` cannot be found.` around the name the request
//!    supplied (`roleNameType`, len 1..64, pattern `^[\w+=,.@-]+$`) — the instance-profile
//!    BINDING calls included, where it is a different absence from the profile's own
//!    (`Instance Profile <name> cannot be found.`) and from a binding the profile does not
//!    carry (`Role <role> in Instance Profile <profile> cannot be found.`).
//!
//! Each suite is a TABLE whose rows differ along what its rule generalises over: which
//! operation carries the id or the name, HOW the request misses a held entity (never minted,
//! held by someone else, already removed, outside the account's roles), and the FORM of the
//! value echoed back (at each published length bound, in the published character class, in
//! another case). No single request is the specification, and every assertion compares the
//! WHOLE message.
//!
//! A refusal is also a report of state rather than a side effect of asking: each suite reads
//! back what it addressed and finds it exactly as it was.
//!
//! The material fixtures (`modulus`, `openssh_line`), the upload and the user fixture are
//! the ones `tests/all/ssh_public_keys.rs` owns, the `GetSSHPublicKey` call is
//! `tests/all/ssh_public_key_reads.rs`'s, and the awsQuery harness is
//! `tests/all/user_lifecycle.rs`'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{create_user, modulus, openssh_line, upload, PUBLISHED_MINIMUM_BITS};
use crate::user_lifecycle::{call, element, elements, error_code, router};

/// A trust policy with a single statement, which a fixture role needs.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The code IAM answers a request that "referenced a resource entity that does not exist"
/// with, as the awsQuery error of `NoSuchEntityException` names it.
const ABSENT_ENTITY_CODE: &str = "NoSuchEntity";

/// RULE 1's sentence, with the id the request supplied in place of the variable part.
fn public_key_not_found(ssh_public_key_id: &str) -> String {
    format!("The Public Key with id {ssh_public_key_id} cannot be found.")
}

/// RULE 2's sentence, with the name the request supplied in place of the variable part.
fn role_not_found(role_name: &str) -> String {
    format!("The role with name {role_name} cannot be found.")
}

/// The sentence a missing INSTANCE PROFILE keeps, which rule 2 must stay distinct from.
fn instance_profile_not_found(instance_profile_name: &str) -> String {
    format!("Instance Profile {instance_profile_name} cannot be found.")
}

/// The sentence a binding the profile does not carry keeps, which rule 2 must stay distinct
/// from.
fn binding_not_found(role_name: &str, instance_profile_name: &str) -> String {
    format!("Role {role_name} in Instance Profile {instance_profile_name} cannot be found.")
}

/// Assert `action` with `params` is refused HTTP 404 `NoSuchEntity` carrying exactly
/// `expected`.
async fn assert_absent(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
    expected: &str,
) {
    let (status, body) = call(router, action, params).await;
    assert_absent_answer(case, action, status, &body, expected);
}

/// Assert one answer already taken off the wire is the absent-entity refusal `expected`.
fn assert_absent_answer(case: &str, action: &str, status: u16, body: &str, expected: &str) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected {action} to answer HTTP 404, got {status}: {body}"
    );
    let code = error_code(body);
    assert_eq!(
        code, ABSENT_ENTITY_CODE,
        "case {case:?}: expected {action} to answer {ABSENT_ENTITY_CODE}, got {code:?}: {body}"
    );
    let message = element(body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected {action} to answer {expected:?}, got {message:?}"
    );
}

/// RULE 1 over every operation addressed by an `SSHPublicKeyId` and every way a request can
/// name no key the user holds.
#[tokio::test]
async fn an_absent_ssh_public_key_is_worded_alike_by_every_call_addressing_one() {
    let router = router();
    create_user(&router, "absent keys", "keyholder").await;
    create_user(&router, "absent keys", "keybystander").await;

    // A key the OTHER user holds, and a key this user held until it was deleted: two ids the
    // account really minted, so a row is not keyed on the shape of an id.
    let held_by_another = mint(&router, "keybystander", 161).await;
    let already_removed = mint(&router, "keyholder", 162).await;
    let (status, removed) = call(
        &router,
        "DeleteSSHPublicKey",
        &[
            ("UserName", "keyholder"),
            ("SSHPublicKeyId", &already_removed),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture delete must be served, got {status}: {removed}"
    );
    // And a key the user DOES hold, which every row must leave untouched.
    let held = mint(&router, "keyholder", 163).await;

    // Ids satisfying `publicKeyIdType` (len 20..128, pattern `^[\w]+$`) at each published
    // length bound, plus a minted id spelled in another case — key ids are not names, so a
    // different case is a different id.
    let shortest_unminted = "APKANEVERMINTED00001".to_string();
    assert_eq!(shortest_unminted.len(), 20, "the published minimum length");
    let longest_unminted = format!("APKA{}", "0".repeat(124));
    assert_eq!(longest_unminted.len(), 128, "the published maximum length");

    // (case, the id the request names)
    let rows: Vec<(&str, String)> = vec![
        (
            "an id the account never minted, at the published minimum length",
            shortest_unminted,
        ),
        (
            "an id the account never minted, at the published maximum length",
            longest_unminted,
        ),
        ("an id another user holds", held_by_another.clone()),
        ("an id a delete already took away", already_removed.clone()),
        (
            "a held id spelled in another case",
            held.to_lowercase(),
        ),
    ];

    for (case, ssh_public_key_id) in rows {
        let expected = public_key_not_found(&ssh_public_key_id);

        // The read, in each encoding it can be asked for: the absence is answered before any
        // key is written out, so the encoding cannot move the sentence.
        for encoding in ["SSH", "PEM"] {
            let (status, body) = get(&router, "keyholder", &ssh_public_key_id, encoding).await;
            assert_absent_answer(
                &format!("{case} read as {encoding}"),
                "GetSSHPublicKey",
                status,
                &body,
                &expected,
            );
        }

        // The status update, in each direction the status can be moved.
        for status_value in ["Active", "Inactive"] {
            assert_absent(
                &router,
                &format!("{case} updated to {status_value}"),
                "UpdateSSHPublicKey",
                &[
                    ("UserName", "keyholder"),
                    ("SSHPublicKeyId", &ssh_public_key_id),
                    ("Status", status_value),
                ],
                &expected,
            )
            .await;
        }

        // The delete, twice: a refusal removes nothing, so asking again answers alike.
        for attempt in 1..=2 {
            assert_absent(
                &router,
                &format!("{case} deleted, attempt {attempt}"),
                "DeleteSSHPublicKey",
                &[
                    ("UserName", "keyholder"),
                    ("SSHPublicKeyId", &ssh_public_key_id),
                ],
                &expected,
            )
            .await;
        }
    }

    // Every refusal above reported nothing of the key this user holds and changed nothing:
    // it is still the one key listed, still Active, still readable.
    let (status, listed) = call(
        &router,
        "ListSSHPublicKeys",
        &[("UserName", "keyholder")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the list must be served, got {status}: {listed}"
    );
    assert_eq!(
        elements(&listed, "SSHPublicKeyId"),
        vec![held.clone()],
        "expected the refusals to leave exactly the held key, got {listed}"
    );
    assert_eq!(
        elements(&listed, "Status"),
        vec!["Active".to_string()],
        "expected the refusals to leave the held status, got {listed}"
    );
    let (status, read) = get(&router, "keyholder", &held, "SSH").await;
    assert_eq!(
        status, 200,
        "the held key must still be served, got {status}: {read}"
    );
    // And the key another user holds is untouched by this user's refused calls.
    let (status, read) = get(&router, "keybystander", &held_by_another, "SSH").await;
    assert_eq!(
        status, 200,
        "the other user's key must still be served, got {status}: {read}"
    );
}

/// RULE 2 over the two instance-profile binding calls and every form a `RoleName` the
/// account holds no role under can take.
#[tokio::test]
async fn an_absent_role_is_worded_alike_by_the_instance_profile_binding_calls() {
    let router = router();
    let (status, body) = call(
        &router,
        "CreateInstanceProfile",
        &[("InstanceProfileName", "held-profile")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the profile fixture must succeed, got {status}: {body}"
    );

    // Names satisfying `roleNameType` (len 1..64, pattern `^[\w+=,.@-]+$`) that the account
    // holds no role under: at each published length bound, spelling every character the
    // class admits, and a plain name in the middle of the range.
    let shortest = "r".to_string();
    let longest = "g".repeat(64);
    assert_eq!(longest.len(), 64, "the published maximum length");
    let punctuated = "ghost+role=,.@-_name".to_string();

    // (case, the role name the request names)
    let rows: Vec<(&str, String)> = vec![
        ("a name at the published minimum length", shortest),
        ("a name at the published maximum length", longest),
        ("a name spelling the whole published class", punctuated),
        ("a plain name no role carries", "ghost-role".to_string()),
    ];

    for (case, role_name) in rows {
        let expected = role_not_found(&role_name);
        for action in ["AddRoleToInstanceProfile", "RemoveRoleFromInstanceProfile"] {
            assert_absent(
                &router,
                &format!("{case} through {action}"),
                action,
                &[
                    ("InstanceProfileName", "held-profile"),
                    ("RoleName", &role_name),
                ],
                &expected,
            )
            .await;
        }
    }

    // The profile is resolved first, so a request missing BOTH entities names the profile:
    // the two absences are different sentences, which is what lets a client tell them apart.
    for action in ["AddRoleToInstanceProfile", "RemoveRoleFromInstanceProfile"] {
        assert_absent(
            &router,
            &format!("neither entity held, through {action}"),
            action,
            &[
                ("InstanceProfileName", "no-such-profile"),
                ("RoleName", "ghost-role"),
            ],
            &instance_profile_not_found("no-such-profile"),
        )
        .await;
    }

    // And a role the account DOES hold, which the profile simply does not carry, keeps the
    // binding sentence: the missing BINDING is a third absence, distinct from both.
    let (status, body) = call(
        &router,
        "CreateRole",
        &[
            ("RoleName", "unbound-role"),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the role fixture must succeed, got {status}: {body}"
    );
    assert_absent(
        &router,
        "a held role the profile does not carry",
        "RemoveRoleFromInstanceProfile",
        &[
            ("InstanceProfileName", "held-profile"),
            ("RoleName", "unbound-role"),
        ],
        &binding_not_found("unbound-role", "held-profile"),
    )
    .await;

    // Every refusal above wrote nothing: the profile carries no role, and the binding the
    // held role CAN take is still available to it.
    let (status, read) = call(
        &router,
        "GetInstanceProfile",
        &[("InstanceProfileName", "held-profile")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the profile must still be served, got {status}: {read}"
    );
    assert!(
        elements(&read, "RoleName").is_empty(),
        "expected the refusals to bind no role, got {read}"
    );
    let (status, bound) = call(
        &router,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", "held-profile"),
            ("RoleName", "unbound-role"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a held role must still be bindable, got {status}: {bound}"
    );
    let (status, read) = call(
        &router,
        "GetInstanceProfile",
        &[("InstanceProfileName", "held-profile")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the profile must still be served, got {status}: {read}"
    );
    assert_eq!(
        elements(&read, "RoleName"),
        vec!["unbound-role".to_string()],
        "expected the bound role to be reported, got {read}"
    );
}

/// Upload one key for `user_name` and answer the id the account minted for it.
async fn mint(router: &MultiServiceRouter, user_name: &str, seed: u8) -> String {
    let body = openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, seed), "");
    let (status, uploaded) = upload(router, user_name, &body).await;
    assert_eq!(
        status, 200,
        "the fixture upload for {user_name} must be served, got {status}: {uploaded}"
    );
    element(&uploaded, "SSHPublicKeyId")
}
