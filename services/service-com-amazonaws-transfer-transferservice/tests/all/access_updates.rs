// SPDX-License-Identifier: CC0-1.0

//! What `UpdateAccess` writes onto a stored directory access grant, and which requests
//! it refuses.
//!
//! Five rules, each asserted over a TABLE whose rows differ along what the rule
//! generalises over — which members the request names, which identity provider the
//! server has, which identifier the request addresses:
//!
//! 1. An update assigns the properties the request NAMES to the addressed grant and
//!    leaves every property it does not name as the grant already had it; the answer
//!    echoes the grant's own identity, and `DescribeAccess` reports the result.
//! 2. The grant is addressed by `(ServerId, ExternalId)`: a server the store does not
//!    hold and an `ExternalId` the server holds no grant for are each reported not
//!    found, and one update writes only the grant it names.
//! 3. An access grant is a group in the SERVER'S directory, so an update addressed at a
//!    server whose `IdentityProviderType` is not the directory-backed one is refused
//!    with the modelled `InvalidRequestException` (HTTP 400) naming that server's own
//!    type.
//! 4. The configuration an update would LEAVE has to satisfy the documented
//!    `HomeDirectoryType`/`HomeDirectoryMappings` pairing, and a refused update leaves
//!    the stored grant exactly as it was.
//! 5. A `Role` in another account is refused with `AccessDeniedException` (HTTP 403),
//!    and a caller whose identity policy explicitly denies `transfer:UpdateAccess` is
//!    refused the same way — both before anything is written.
//!
//! Every request goes over the real wire path (`POST /` with
//! `X-Amz-Target: TransferService.<Op>`) through the shared [`crate::harness`], so what
//! is asserted is what an SDK sees.

use serde_json::{json, Value};

use crate::harness::{
    assert_members, member_of, Emulator, LAMBDA_IDP, ROLE, SHORT_SID, UNMINTED_SERVER_ID,
};

/// A group SID satisfying `ExternalId`'s modelled `len 1..256`, pattern `^S-1-[\d-]+$`.
const GROUP_SID: &str = "S-1-5-21-1111111111-2222222222-3333333333-4444";

/// A second role ARN of THIS account, satisfying `Role`'s modelled pattern
/// `^arn:.*role/\S+$` and `len 20..2048`.
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/transfer-efs";

/// The same role name in ANOTHER account: what the pass-role account check refuses.
const FOREIGN_ROLE: &str = "arn:aws:iam::111122223333:role/transfer-s3";

/// Create one access grant, asserted to have been served.
async fn create_access(emulator: &Emulator, server_id: &str, external_id: &str, extra: &Value) {
    let mut request = json!({"ServerId": server_id, "ExternalId": external_id, "Role": ROLE});
    let object = request.as_object_mut().expect("the request is an object");
    for (member, value) in extra.as_object().expect("the extra members are an object") {
        object.insert(member.clone(), value.clone());
    }
    let answer = emulator.call("CreateAccess", request).await;
    assert_eq!(
        answer.status, 200,
        "CreateAccess {external_id}: expected 200, got {} with body {}",
        answer.status, answer.body
    );
}

/// Send one `UpdateAccess` and assert it was served, answering the grant's own
/// identity.
async fn update_access(emulator: &Emulator, server_id: &str, external_id: &str, extra: &Value) {
    let mut request = json!({"ServerId": server_id, "ExternalId": external_id});
    let object = request.as_object_mut().expect("the request is an object");
    for (member, value) in extra.as_object().expect("the extra members are an object") {
        object.insert(member.clone(), value.clone());
    }
    let answer = emulator.call("UpdateAccess", request).await;
    assert_eq!(
        answer.status, 200,
        "UpdateAccess {external_id}: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.get("ServerId").as_str(),
        Some(server_id),
        "UpdateAccess {external_id}: expected the addressed ServerId back, got {}",
        answer.body
    );
    assert_eq!(
        answer.get("ExternalId").as_str(),
        Some(external_id),
        "UpdateAccess {external_id}: expected the addressed ExternalId back, got {}",
        answer.body
    );
}

/// RULE 1: an update assigns the properties the request names and leaves the rest.
///
/// Every grant starts from one and the same stored configuration, so each row's
/// expectation is the start configuration with only the named members replaced. The
/// rows differ along which member is named (the landing directory, the session policy,
/// the POSIX profile, the role, the directory type together with its mappings, several
/// at once, and none at all) and along the shape of the `ExternalId` they address.
#[tokio::test]
async fn an_update_replaces_the_members_it_names_and_keeps_the_rest() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    let start_policy = r#"{"Version":"2012-10-17","Statement":[]}"#;
    let start_posix = json!({"Uid": 1000, "Gid": 1000});
    // The configuration every row starts from: a PATH grant with every optional
    // member set, so a row that names one member can be seen to leave the others.
    let start = json!({
        "HomeDirectoryType": "PATH",
        "HomeDirectory": "/bucket_name/home/start",
        "Policy": start_policy,
        "PosixProfile": start_posix,
    });
    let start_described = json!({
        "ExternalId": null, // filled in per row below
        "Role": ROLE,
        "HomeDirectoryType": "PATH",
        "HomeDirectory": "/bucket_name/home/start",
        "Policy": start_policy,
        "PosixProfile": start_posix,
        "HomeDirectoryMappings": null,
    });

    let mappings = json!([
        {"Entry": "/directory1", "Target": "/bucket_name/home/mydirectory"},
        {"Entry": "/directory2", "Target": "/bucket_name/home/other"},
    ]);
    let new_posix = json!({"Uid": 2000, "Gid": 2000, "SecondaryGids": [3000]});
    let new_policy = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Action":"*","Resource":"*"}]}"#;

    // (case, the ExternalId the row addresses, the members the update names, the
    // members that must then differ from the start configuration)
    let cases: Vec<(&str, &str, Value, Value)> = vec![
        (
            "only the landing directory",
            GROUP_SID,
            json!({"HomeDirectory": "/bucket_name/home/moved"}),
            json!({"HomeDirectory": "/bucket_name/home/moved"}),
        ),
        (
            "only the session policy",
            SHORT_SID,
            json!({"Policy": new_policy}),
            json!({"Policy": new_policy}),
        ),
        (
            "only the POSIX profile",
            "S-1-5-32-544",
            json!({"PosixProfile": new_posix}),
            json!({"PosixProfile": new_posix}),
        ),
        (
            "only the role",
            "S-1-5-21-0-0-0-1",
            json!({"Role": OTHER_ROLE}),
            json!({"Role": OTHER_ROLE}),
        ),
        (
            "the directory type and its two mappings",
            "S-1-5-21-0-0-0-2",
            json!({"HomeDirectoryType": "LOGICAL", "HomeDirectoryMappings": mappings}),
            json!({"HomeDirectoryType": "LOGICAL", "HomeDirectoryMappings": mappings}),
        ),
        (
            "the directory type and a single chroot mapping",
            "S-1-5-21-0-0-0-3",
            json!({
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket_name/home/root"}],
            }),
            json!({
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket_name/home/root"}],
            }),
        ),
        (
            "the landing directory, the policy, the profile and the role at once",
            "S-1-5-21-0-0-0-4",
            json!({
                "HomeDirectory": "/bucket_name/home/all",
                "Policy": new_policy,
                "PosixProfile": new_posix,
                "Role": OTHER_ROLE,
            }),
            json!({
                "HomeDirectory": "/bucket_name/home/all",
                "Policy": new_policy,
                "PosixProfile": new_posix,
                "Role": OTHER_ROLE,
            }),
        ),
        (
            "no update parameter at all",
            "S-1-5-21-0-0-0-5",
            json!({}),
            json!({}),
        ),
        (
            "an EMPTY landing directory, the shortest HomeDirectory len 0..1024 admits",
            "S-1-5-21-0-0-0-6",
            json!({"HomeDirectory": ""}),
            json!({"HomeDirectory": ""}),
        ),
    ];

    for (case, external_id, named, changed) in &cases {
        create_access(&emulator, &server_id, external_id, &start).await;
        update_access(&emulator, &server_id, external_id, named).await;

        // The expectation is the start configuration with the named members replaced:
        // everything else has to read back as the create left it.
        let mut expected = start_described.clone();
        let object = expected.as_object_mut().expect("the expectation is an object");
        object.insert("ExternalId".to_string(), json!(external_id));
        for (member, value) in changed.as_object().expect("the changes are an object") {
            object.insert(member.clone(), value.clone());
        }

        let access = emulator.described_access(&server_id, external_id).await;
        assert_members(case, &access, &expected);
    }

    // Every grant of the table still reads back the same way after the whole table
    // ran: one update writes one grant, so the later rows did not disturb the earlier.
    for (case, external_id, _, changed) in &cases {
        let mut expected = start_described.clone();
        let object = expected.as_object_mut().expect("the expectation is an object");
        object.insert("ExternalId".to_string(), json!(external_id));
        for (member, value) in changed.as_object().expect("the changes are an object") {
            object.insert(member.clone(), value.clone());
        }
        let access = emulator.described_access(&server_id, external_id).await;
        assert_members(&format!("{case} after the whole table"), &access, &expected);
    }
}

/// RULE 2: the grant an update writes is the one `(ServerId, ExternalId)` names.
///
/// The rows differ along which part of the pair the store does not hold, and along
/// which of two servers holds the identically named grant.
#[tokio::test]
async fn an_update_addresses_one_grant_by_its_server_and_external_id() {
    let emulator = Emulator::new();
    let first = emulator.directory_server().await;
    let second = emulator.directory_server().await;

    // A server the store does not hold: the SERVER is the missing resource, reported
    // with this service's not-found answer (HTTP 400 `ResourceNotFoundException`).
    let answer = emulator
        .call(
            "UpdateAccess",
            json!({
                "ServerId": UNMINTED_SERVER_ID,
                "ExternalId": GROUP_SID,
                "HomeDirectory": "/bucket/nowhere",
            }),
        )
        .await;
    for (member, want) in [("ResourceType", "Server"), ("Resource", UNMINTED_SERVER_ID)] {
        assert_eq!(
            member_of(&answer.body, member).as_str(),
            Some(want),
            "case \"UpdateAccess on an unminted server\": expected {member} {want:?}, got {}",
            answer.body
        );
    }
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case \"UpdateAccess on an unminted server\": expected ResourceNotFoundException, got {}",
        answer.body
    );
    assert_eq!(
        answer.status, 400,
        "case \"UpdateAccess on an unminted server\": expected HTTP 400, got {} with body {}",
        answer.status, answer.body
    );

    // An `ExternalId` the resolved server holds no grant for: the GRANT is the missing
    // resource.
    for external_id in [GROUP_SID, SHORT_SID] {
        let answer = emulator
            .call(
                "UpdateAccess",
                json!({
                    "ServerId": &first,
                    "ExternalId": external_id,
                    "HomeDirectory": "/bucket/never-granted",
                }),
            )
            .await;
        let case = format!("UpdateAccess for the never-granted {external_id}");
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {}",
            answer.body
        );
        for (member, want) in [("ResourceType", "Access"), ("Resource", external_id)] {
            assert_eq!(
                member_of(&answer.body, member).as_str(),
                Some(want),
                "case {case:?}: expected {member} {want:?}, got {}",
                answer.body
            );
        }
    }

    // The same `ExternalId` granted on two servers is two independent records: an
    // update addressed at one leaves the other alone.
    for server_id in [&first, &second] {
        create_access(
            &emulator,
            server_id,
            GROUP_SID,
            &json!({"HomeDirectory": "/bucket/start"}),
        )
        .await;
    }
    update_access(
        &emulator,
        &first,
        GROUP_SID,
        &json!({"HomeDirectory": "/bucket/updated"}),
    )
    .await;
    for (case, server_id, home) in [
        ("the updated server's grant", &first, "/bucket/updated"),
        ("the other server's grant", &second, "/bucket/start"),
    ] {
        let access = emulator.described_access(server_id, GROUP_SID).await;
        assert_members(case, &access, &json!({"HomeDirectory": home}));
    }
}

/// RULE 3: an update addressed at a server with no directory is refused 400
/// `InvalidRequestException` whose message names this operation's action and the
/// server's own `IdentityProviderType`, carrying nothing the caller sent.
///
/// The rows span the whole non-directory half of the `IdentityProviderType` value set
/// (`[SERVICE_MANAGED, API_GATEWAY, AWS_DIRECTORY_SERVICE, AWS_LAMBDA]`, per
/// `smithy_query(query="shape",shape="IdentityProviderType")`), including the absent
/// member that resolves to `SERVICE_MANAGED`, each with the `IdentityProviderDetails`
/// its type requires.
#[tokio::test]
async fn an_update_is_served_only_on_a_directory_backed_server() {
    let emulator = Emulator::new();

    // (case, the CreateServer request, the type the server resolves to)
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "no IdentityProviderType at all, which resolves to SERVICE_MANAGED",
            json!({}),
            "SERVICE_MANAGED",
        ),
        (
            "SERVICE_MANAGED",
            json!({"IdentityProviderType": "SERVICE_MANAGED"}),
            "SERVICE_MANAGED",
        ),
        (
            "API_GATEWAY",
            json!({
                "IdentityProviderType": "API_GATEWAY",
                "IdentityProviderDetails": {"Url": "https://example.invalid/auth"},
            }),
            "API_GATEWAY",
        ),
        (
            "AWS_LAMBDA",
            json!({
                "IdentityProviderType": "AWS_LAMBDA",
                "IdentityProviderDetails": {"Function": LAMBDA_IDP},
            }),
            "AWS_LAMBDA",
        ),
    ];

    for (case, server_request, idp_type) in cases {
        let server_id = emulator.create_server(server_request).await;
        for external_id in [GROUP_SID, SHORT_SID] {
            let case = format!("{case} for {external_id}");
            let answer = emulator
                .call(
                    "UpdateAccess",
                    json!({
                        "ServerId": &server_id,
                        "ExternalId": external_id,
                        "HomeDirectory": "/bucket/refused",
                    }),
                )
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with body {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException, got {}",
                answer.body
            );
            let message = member_of(&answer.body, "Message")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                message,
                format!("Cannot update access on server with IdP type: {idp_type}"),
                "case {case:?}: expected the fixed directory-provider refusal for this \
                 operation, got {message:?}"
            );
            for sent in [server_id.as_str(), external_id] {
                assert!(
                    !message.contains(sent),
                    "case {case:?}: expected no request data in the refusal, got {message:?} \
                     carrying {sent:?}"
                );
            }
        }
    }

    // The same update on a directory-backed server is served, so the refusal above is
    // the server's type and not the operation being unavailable.
    let directory = emulator.directory_server().await;
    create_access(&emulator, &directory, GROUP_SID, &json!({})).await;
    update_access(
        &emulator,
        &directory,
        GROUP_SID,
        &json!({"HomeDirectory": "/bucket/served"}),
    )
    .await;
}

/// RULE 4: the configuration the update would LEAVE has to satisfy the documented
/// `HomeDirectoryType`/`HomeDirectoryMappings` pairing, and a refused update leaves the
/// stored grant exactly as it was.
///
/// The rows differ along which side of the pairing the resulting configuration breaks,
/// whether the broken side comes from the request or from the stored grant, and how
/// many mappings are involved.
#[tokio::test]
async fn an_update_holds_the_resulting_home_directory_configuration_to_the_pairing() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    let mappings = json!([{"Entry": "/", "Target": "/bucket_name/home/root"}]);

    // (case, the stored grant's configuration, the update's members, the fragments the
    // refusal must name)
    let cases: Vec<(&str, Value, Value, Vec<&str>)> = vec![
        (
            "turning a PATH grant LOGICAL without naming mappings",
            json!({"HomeDirectoryType": "PATH", "HomeDirectory": "/bucket/kept"}),
            json!({"HomeDirectoryType": "LOGICAL"}),
            // This direction states `HomeDirectoryMappings`' own modelled minimum
            // (`len 1..50000`), which is what an absent or empty list violates.
            vec!["HomeDirectoryMappings"],
        ),
        (
            "naming mappings on a grant that stays PATH",
            json!({"HomeDirectoryType": "PATH", "HomeDirectory": "/bucket/kept"}),
            json!({"HomeDirectoryMappings": mappings}),
            // The opposite direction has no modelled constraint of its own, so it names
            // the condition the value may be sent under.
            vec!["HomeDirectoryMappings", "LOGICAL"],
        ),
        (
            "naming two mappings together with PATH",
            json!({"HomeDirectoryType": "PATH", "HomeDirectory": "/bucket/kept"}),
            json!({
                "HomeDirectoryType": "PATH",
                "HomeDirectoryMappings": [
                    {"Entry": "/a", "Target": "/bucket/a"},
                    {"Entry": "/b", "Target": "/bucket/b"},
                ],
            }),
            vec!["HomeDirectoryMappings", "LOGICAL"],
        ),
        (
            "turning a LOGICAL grant PATH while its mappings stay",
            json!({"HomeDirectoryType": "LOGICAL", "HomeDirectoryMappings": mappings}),
            json!({"HomeDirectoryType": "PATH"}),
            vec!["HomeDirectoryMappings", "LOGICAL"],
        ),
    ];

    for (index, (case, stored, named, fragments)) in cases.iter().enumerate() {
        // One grant per row, so each row's read-back reports its own stored record.
        let external_id = format!("S-1-5-21-9-{index}");
        create_access(&emulator, &server_id, &external_id, stored).await;
        let before = emulator.described_access(&server_id, &external_id).await;

        let mut request = json!({"ServerId": &server_id, "ExternalId": &external_id});
        let object = request.as_object_mut().expect("the request is an object");
        for (member, value) in named.as_object().expect("the update is an object") {
            object.insert(member.clone(), value.clone());
        }
        let answer = emulator.call("UpdateAccess", request).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {}",
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        for fragment in fragments {
            assert!(
                message.contains(fragment),
                "case {case:?}: expected the refusal to name {fragment:?}, got {message:?}"
            );
        }

        let after = emulator.described_access(&server_id, &external_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must leave the stored grant alone, it read \
             back as {after} instead of {before}"
        );
    }
}

/// RULE 5: an update is refused before it writes anything when the role it passes
/// belongs to another account, and when the calling principal's identity policy
/// explicitly denies `transfer:UpdateAccess`.
///
/// The rows differ along which guard refuses the request and along whether the grant
/// the request addresses exists at all — the account check runs before the resource is
/// resolved, so it answers the same either way.
#[tokio::test]
async fn an_update_is_refused_before_it_writes_for_a_foreign_role_or_a_denied_caller() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;
    create_access(
        &emulator,
        &server_id,
        GROUP_SID,
        &json!({"HomeDirectory": "/bucket/kept"}),
    )
    .await;

    // (case, the ServerId the request addresses)
    for (case, addressed) in [
        ("a stored grant", server_id.as_str()),
        ("a server nothing minted", UNMINTED_SERVER_ID),
    ] {
        let answer = emulator
            .call(
                "UpdateAccess",
                json!({
                    "ServerId": addressed,
                    "ExternalId": GROUP_SID,
                    "Role": FOREIGN_ROLE,
                    "HomeDirectory": "/bucket/foreign",
                }),
            )
            .await;
        assert_eq!(
            answer.status, 403,
            "case {case:?}: expected HTTP 403, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {}",
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains("Cross-account pass role is not allowed."),
            "case {case:?}: expected the pass-role refusal, got {message:?}"
        );
    }

    let access = emulator.described_access(&server_id, GROUP_SID).await;
    assert_members(
        "the stored grant after a refused foreign-role update",
        &access,
        &json!({"HomeDirectory": "/bucket/kept", "Role": ROLE}),
    );

    // A caller the account's identity policy denies this one action is refused too,
    // while the same principal's other actions are unaffected.
    emulator.seed_calling_principal(
        "gated-user",
        r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"transfer:*","Resource":"*"},
            {"Effect":"Deny","Action":"transfer:UpdateAccess","Resource":"*"}]}"#,
    );
    let answer = emulator
        .call(
            "UpdateAccess",
            json!({
                "ServerId": &server_id,
                "ExternalId": GROUP_SID,
                "HomeDirectory": "/bucket/denied",
            }),
        )
        .await;
    assert_eq!(
        answer.status, 403,
        "case \"a denied caller\": expected HTTP 403, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "AccessDeniedException",
        "case \"a denied caller\": expected AccessDeniedException, got {}",
        answer.body
    );
    let message = member_of(&answer.body, "Message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    for fragment in ["transfer:UpdateAccess", "explicit deny"] {
        assert!(
            message.contains(fragment),
            "case \"a denied caller\": expected the refusal to name {fragment:?}, got {message:?}"
        );
    }
    let access = emulator.described_access(&server_id, GROUP_SID).await;
    assert_members(
        "the stored grant after a refused denied-caller update",
        &access,
        &json!({"HomeDirectory": "/bucket/kept"}),
    );
}
