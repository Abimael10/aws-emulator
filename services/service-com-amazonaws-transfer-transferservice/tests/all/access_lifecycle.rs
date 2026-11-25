// SPDX-License-Identifier: CC0-1.0

//! A server's ACCESSES — the directory groups allowed to transfer files — over the
//! whole wire path.
//!
//! Three rules, each asserted over a TABLE whose rows differ along what the rule
//! generalises over:
//!
//! 1. An access names a group in the SERVER'S DIRECTORY, so `CreateAccess` is served
//!    only for a server whose `IdentityProviderType` is the directory-backed one
//!    (`AWS_DIRECTORY_SERVICE`) and refused for every other one with the modelled
//!    `InvalidRequestException` (HTTP 400) naming that server's own type — and the
//!    refusal persists nothing.
//! 2. The properties a create stores are what `DescribeAccess` reports back, keyed by
//!    `(ServerId, ExternalId)`: `HomeDirectoryType` resolved once by the write, the
//!    landing directory, the session policy and the POSIX profile as sent, and
//!    `HomeDirectoryMappings` absent rather than empty when the access has none.
//! 3. An access record is addressed by its own identifier: a server the store does not
//!    hold, an `ExternalId` it has no access for, and a second create of the same pair
//!    are each refused with the modelled error for that case, and a server's accesses
//!    go with the server when it is deleted.
//! 4. `ListAccesses` reports exactly the grants stored for the server it names, each
//!    entry projected from the SAME record `DescribeAccess` reports, in `ExternalId`
//!    order, at most `MaxResults` per page with a `NextToken` only while a further page
//!    remains; a server with no directory, a server the store does not hold and a token
//!    the service did not mint are each refused with the modelled error for that case.
//!
//! Every request goes over the real wire path (`POST /` with
//! `X-Amz-Target: TransferService.<Op>`) through the shared [`crate::harness`], so what
//! is asserted is what an SDK sees.

use serde_json::{json, Value};

use crate::harness::{
    assert_members, drain_pages, member_of, Emulator, DIRECTORY_ID, LAMBDA_IDP, ROLE, SHORT_SID,
    UNMINTED_SERVER_ID,
};

/// A group SID satisfying `ExternalId`'s modelled `len 1..256`, pattern
/// `^S-1-[\d-]+$` — the form `Get-ADGroup` reports, per the member's documentation.
const GROUP_SID: &str = "S-1-1-12-1234567890-123456789-1234567890-1234";

/// A second IAM role ARN satisfying `Role`'s modelled `len 20..2048`, pattern
/// `^arn:.*role/\S+$`, so a row can differ from [`ROLE`] in the role it grants.
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/transfer-efs";

/// The members `ListedAccess` declares — source:
/// `smithy_query(query="shape",shape="ListedAccess")`. A listing entry carries these
/// and nothing else: the mappings, the session policy and the POSIX profile are
/// `DescribeAccess`'s to report.
const LISTED_ACCESS_MEMBERS: [&str; 4] = [
    "ExternalId",
    "HomeDirectory",
    "HomeDirectoryType",
    "Role",
];


/// RULE 1: `CreateAccess` is served only on a directory-backed server, and refused on
/// every other identity provider type with HTTP 400 `InvalidRequestException` naming
/// that server's type.
///
/// The rows span the WHOLE `IdentityProviderType` value set — `[SERVICE_MANAGED,
/// API_GATEWAY, AWS_DIRECTORY_SERVICE, AWS_LAMBDA]`, per
/// `smithy_query(query="shape",shape="IdentityProviderType")` — plus the absent member
/// that resolves to `SERVICE_MANAGED`, each with the `IdentityProviderDetails` that
/// type requires.
#[tokio::test]
async fn create_access_is_served_only_on_a_directory_backed_server() {
    let emulator = Emulator::new();

    // (case, the CreateServer request, the IdP type the server resolves to, whether an
    // access may be created on it)
    let cases: Vec<(&str, Value, &str, bool)> = vec![
        (
            "no IdentityProviderType at all, which resolves to SERVICE_MANAGED",
            json!({}),
            "SERVICE_MANAGED",
            false,
        ),
        (
            "SERVICE_MANAGED",
            json!({"IdentityProviderType": "SERVICE_MANAGED"}),
            "SERVICE_MANAGED",
            false,
        ),
        (
            "API_GATEWAY",
            json!({
                "IdentityProviderType": "API_GATEWAY",
                "IdentityProviderDetails": {"Url": "https://example.invalid/auth"},
            }),
            "API_GATEWAY",
            false,
        ),
        (
            "AWS_LAMBDA",
            json!({
                "IdentityProviderType": "AWS_LAMBDA",
                "IdentityProviderDetails": {"Function": LAMBDA_IDP},
            }),
            "AWS_LAMBDA",
            false,
        ),
        (
            "AWS_DIRECTORY_SERVICE",
            json!({
                "IdentityProviderType": "AWS_DIRECTORY_SERVICE",
                "IdentityProviderDetails": {"DirectoryId": DIRECTORY_ID},
            }),
            "AWS_DIRECTORY_SERVICE",
            true,
        ),
    ];

    for (case, server_request, idp_type, servable) in cases {
        let server_id = emulator.create_server(server_request).await;
        let answer = emulator
            .call(
                "CreateAccess",
                json!({"ServerId": server_id, "ExternalId": GROUP_SID, "Role": ROLE}),
            )
            .await;

        if servable {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the create to be served, got {} with body {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.get("ExternalId").as_str(),
                Some(GROUP_SID),
                "case {case:?}: expected the created ExternalId back, got {}",
                answer.body
            );
            assert_eq!(
                answer.get("ServerId").as_str(),
                Some(server_id.as_str()),
                "case {case:?}: expected the server id back, got {}",
                answer.body
            );
            continue;
        }

        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got body {}",
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        // The refusal is the ONE rule every access operation shares
        // (`state::accesses_require_directory_provider`, which the implemented
        // `DescribeAccess` and `DeleteAccess` apply too): a text fixed for this
        // operation and for the type THIS server is configured with, carrying nothing
        // the caller sent. The per-operation wording itself is pinned, across the
        // operations and the whole type set, in `refusal_message_rules.rs`.
        assert_eq!(
            message,
            format!("Cannot create access on server with IdP type: {idp_type}"),
            "case {case:?}: expected the fixed directory-provider refusal, got {message:?}"
        );
        for sent in [server_id.as_str(), GROUP_SID] {
            assert!(
                !message.contains(sent),
                "case {case:?}: expected no request data in the refusal, got {message:?} \
                 carrying {sent:?}"
            );
        }

        // The refusal persisted nothing: the store holds no grant for the pair the
        // request named. (A read-back cannot report that here — `DescribeAccess` is
        // refused by the same gate on this server.)
        assert!(
            !emulator
                .store()
                .lock()
                .accesses
                .contains_key(&(server_id.clone(), GROUP_SID.to_string())),
            "case {case:?}: a refused create must persist nothing, the store holds a grant \
             for {GROUP_SID}"
        );
    }
}

/// RULE 2: every property a create stores is what `DescribeAccess` reports back for
/// that `(ServerId, ExternalId)`.
///
/// The rows differ along which optional members the create carried, the
/// `HomeDirectoryType` it resolves to (sent `PATH`, sent `LOGICAL` with mappings, or
/// absent, which resolves to `PATH`), how many mappings it holds, and the shape and
/// length of the `ExternalId`.
#[tokio::test]
async fn a_created_access_reads_back_through_describe_access() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    let posix = json!({"Uid": 1000, "Gid": 1000, "SecondaryGids": [2000, 2001]});
    let mappings = json!([
        {"Entry": "/directory1", "Target": "/bucket_name/home/mydirectory"},
        {"Entry": "/directory2", "Target": "/bucket_name/home/other"},
    ]);

    // (case, the extra members of the CreateAccess request, the DescribedAccess the
    // reader must report — a null expectation means the member must be absent)
    let cases: Vec<(&str, &str, Value, Value)> = vec![
        (
            "only the required members",
            GROUP_SID,
            json!({}),
            json!({
                "ExternalId": GROUP_SID,
                "Role": ROLE,
                "HomeDirectoryType": "PATH",
                "HomeDirectory": null,
                "HomeDirectoryMappings": null,
                "Policy": null,
                "PosixProfile": null,
            }),
        ),
        (
            "a PATH landing directory and a session policy",
            SHORT_SID,
            json!({
                "HomeDirectoryType": "PATH",
                "HomeDirectory": "/bucket_name/home/short",
                "Policy": "{\"Version\":\"2012-10-17\",\"Statement\":[]}",
            }),
            json!({
                "ExternalId": SHORT_SID,
                "HomeDirectoryType": "PATH",
                "HomeDirectory": "/bucket_name/home/short",
                "Policy": "{\"Version\":\"2012-10-17\",\"Statement\":[]}",
                "HomeDirectoryMappings": null,
            }),
        ),
        (
            "LOGICAL with two mappings and a POSIX profile",
            "S-1-5-21-1111111111-2222222222-3333333333-4444",
            json!({
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": mappings,
                "PosixProfile": posix,
            }),
            json!({
                "ExternalId": "S-1-5-21-1111111111-2222222222-3333333333-4444",
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": mappings,
                "PosixProfile": posix,
                "HomeDirectory": null,
            }),
        ),
        (
            "LOGICAL with a single chroot mapping",
            "S-1-5-32-544",
            json!({
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket_name/home/root"}],
            }),
            json!({
                "ExternalId": "S-1-5-32-544",
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket_name/home/root"}],
            }),
        ),
    ];

    for (case, external_id, extra, expected) in &cases {
        let mut request = json!({"ServerId": server_id, "ExternalId": external_id, "Role": ROLE});
        let object = request.as_object_mut().expect("the request is an object");
        for (member, value) in extra.as_object().expect("the extra members are an object") {
            object.insert(member.clone(), value.clone());
        }

        let answer = emulator.call("CreateAccess", request).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with body {}",
            answer.status, answer.body
        );

        let access = emulator.described_access(&server_id, external_id).await;
        assert_members(case, &access, expected);
    }

    // Every access of the table is still readable after the later creates, each under
    // its own `ExternalId`: the records do not overwrite one another.
    for (case, external_id, _, expected) in &cases {
        let access = emulator.described_access(&server_id, external_id).await;
        assert_members(&format!("{case} after the whole table"), &access, expected);
    }
}

/// RULE 2 (the other half of the documented pairing): a create whose
/// `HomeDirectoryType` and `HomeDirectoryMappings` break the documented pairing is
/// refused with `InvalidRequestException` and stores nothing.
///
/// The rows differ along which side of the pairing is unsatisfied and how many
/// mappings were sent.
#[tokio::test]
async fn create_access_enforces_the_home_directory_pairing() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    // Each row carries what the shared rule's refusal must NAME: the member always,
    // plus the condition in the direction whose rule states one.
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "LOGICAL with no mappings",
            json!({"HomeDirectoryType": "LOGICAL"}),
            // This direction states `HomeDirectoryMappings`' own modelled minimum
            // (`len 1..50000`), which is what an absent or empty list violates.
            vec!["HomeDirectoryMappings"],
        ),
        (
            "mappings with no type at all",
            json!({"HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket/home"}]}),
            // The opposite direction has no modelled constraint of its own, so it
            // names the condition the value may be sent under.
            vec!["HomeDirectoryMappings", "LOGICAL"],
        ),
        (
            "two mappings with HomeDirectoryType PATH",
            json!({
                "HomeDirectoryType": "PATH",
                "HomeDirectoryMappings": [
                    {"Entry": "/a", "Target": "/bucket/a"},
                    {"Entry": "/b", "Target": "/bucket/b"},
                ],
            }),
            vec!["HomeDirectoryMappings", "LOGICAL"],
        ),
    ];

    for (case, extra, named) in cases {
        let mut request = json!({"ServerId": server_id, "ExternalId": GROUP_SID, "Role": ROLE});
        let object = request.as_object_mut().expect("the request is an object");
        for (member, value) in extra.as_object().expect("the extra members are an object") {
            object.insert(member.clone(), value.clone());
        }

        let answer = emulator.call("CreateAccess", request).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got body {}",
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        for fragment in &named {
            assert!(
                message.contains(fragment),
                "case {case:?}: expected the refusal to name {fragment:?}, got {message:?}"
            );
        }

        let read_back = emulator
            .call(
                "DescribeAccess",
                json!({"ServerId": server_id, "ExternalId": GROUP_SID}),
            )
            .await;
        assert_eq!(
            read_back.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: a refused create must persist nothing, got {}",
            read_back.body
        );
    }
}

/// RULE 3: an access is addressed by `(ServerId, ExternalId)` — a pair the store does
/// not hold is refused rather than invented, the same pair twice is refused rather
/// than replaced, and the same `ExternalId` on two servers is two independent records.
#[tokio::test]
async fn an_access_is_addressed_by_its_server_and_external_id() {
    let emulator = Emulator::new();
    let first = emulator.directory_server().await;
    let second = emulator.directory_server().await;

    // A server the store does not hold: refused with the service's not-found answer
    // (HTTP 400 `ResourceNotFoundException`, `ResourceType` `Server`), for both the
    // create and the read.
    for (case, op, request) in [
        (
            "CreateAccess on an unminted server",
            "CreateAccess",
            json!({"ServerId": UNMINTED_SERVER_ID, "ExternalId": GROUP_SID, "Role": ROLE}),
        ),
        (
            "DescribeAccess on an unminted server",
            "DescribeAccess",
            json!({"ServerId": UNMINTED_SERVER_ID, "ExternalId": GROUP_SID}),
        ),
    ] {
        let answer = emulator.call(op, request).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {}",
            answer.body
        );
        assert_eq!(
            member_of(&answer.body, "ResourceType").as_str(),
            Some("Server"),
            "case {case:?}: expected ResourceType Server, got {}",
            answer.body
        );
    }

    // An `ExternalId` the server has no access for: the access itself is the missing
    // resource.
    let answer = emulator
        .call(
            "DescribeAccess",
            json!({"ServerId": first, "ExternalId": GROUP_SID}),
        )
        .await;
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case \"DescribeAccess for an ExternalId with no access\": expected \
         ResourceNotFoundException, got {}",
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "ResourceType").as_str(),
        Some("Access"),
        "case \"DescribeAccess for an ExternalId with no access\": expected ResourceType \
         Access, got {}",
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "Resource").as_str(),
        Some(GROUP_SID),
        "case \"DescribeAccess for an ExternalId with no access\": expected the requested \
         ExternalId in Resource, got {}",
        answer.body
    );

    // The same `ExternalId` on two servers: two records, each with its own properties.
    for (server_id, home) in [(&first, "/bucket/first"), (&second, "/bucket/second")] {
        let answer = emulator
            .call(
                "CreateAccess",
                json!({
                    "ServerId": server_id,
                    "ExternalId": GROUP_SID,
                    "Role": ROLE,
                    "HomeDirectory": home,
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case \"the same ExternalId on {server_id}\": expected 200, got {} with body {}",
            answer.status, answer.body
        );
    }
    for (server_id, home) in [(&first, "/bucket/first"), (&second, "/bucket/second")] {
        let access = emulator.described_access(server_id, GROUP_SID).await;
        assert_members(
            &format!("the access of {server_id}"),
            &access,
            &json!({"HomeDirectory": home}),
        );
    }

    // The same pair twice: refused with the modelled `ResourceExistsException`
    // (HTTP 409), and the stored record is untouched.
    let answer = emulator
        .call(
            "CreateAccess",
            json!({
                "ServerId": first,
                "ExternalId": GROUP_SID,
                "Role": ROLE,
                "HomeDirectory": "/bucket/replacement",
            }),
        )
        .await;
    assert_eq!(
        answer.status, 409,
        "case \"the same pair twice\": expected HTTP 409, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ResourceExistsException",
        "case \"the same pair twice\": expected ResourceExistsException, got {}",
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "Resource").as_str(),
        Some(GROUP_SID),
        "case \"the same pair twice\": expected the ExternalId in Resource, got {}",
        answer.body
    );
    let access = emulator.described_access(&first, GROUP_SID).await;
    assert_members(
        "the stored record after a refused duplicate",
        &access,
        &json!({"HomeDirectory": "/bucket/first"}),
    );

    // The accesses belong to the server: deleting it takes them with it, while the
    // other server's identically named access stays readable.
    let deleted = emulator
        .call("DeleteServer", json!({"ServerId": first}))
        .await;
    assert_eq!(
        deleted.status, 200,
        "case \"DeleteServer\": expected 200, got {} with body {}",
        deleted.status, deleted.body
    );
    let answer = emulator
        .call(
            "DescribeAccess",
            json!({"ServerId": first, "ExternalId": GROUP_SID}),
        )
        .await;
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case \"DescribeAccess after DeleteServer\": expected ResourceNotFoundException, got {}",
        answer.body
    );
    let access = emulator.described_access(&second, GROUP_SID).await;
    assert_members(
        "the surviving server's access",
        &access,
        &json!({"HomeDirectory": "/bucket/second"}),
    );
}

/// RULE 3 (the bound on the store): a server's access set is bounded — past
/// `MAX_ACCESSES_PER_SERVER` a create is refused with the modelled
/// `ServiceUnavailableException` instead of growing, and the refusal does not disturb
/// the stored records.
#[tokio::test]
async fn a_servers_access_set_is_bounded() {
    use service_com_amazonaws_transfer_transferservice::state::MAX_ACCESSES_PER_SERVER;

    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    for n in 0..MAX_ACCESSES_PER_SERVER {
        let answer = emulator
            .call(
                "CreateAccess",
                json!({
                    "ServerId": server_id,
                    "ExternalId": format!("S-1-5-21-{n}"),
                    "Role": ROLE,
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case \"access {n} of {MAX_ACCESSES_PER_SERVER}\": expected 200 up to the bound, \
             got {} with body {}",
            answer.status, answer.body
        );
    }

    let answer = emulator
        .call(
            "CreateAccess",
            json!({
                "ServerId": server_id,
                "ExternalId": format!("S-1-5-21-{MAX_ACCESSES_PER_SERVER}"),
                "Role": ROLE,
            }),
        )
        .await;
    assert_eq!(
        answer.status, 503,
        "case \"one access past the bound\": expected HTTP 503, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ServiceUnavailableException",
        "case \"one access past the bound\": expected ServiceUnavailableException, got {}",
        answer.body
    );

    // The bound refused the new record and kept every stored one.
    let access = emulator.described_access(&server_id, "S-1-5-21-0").await;
    assert_members(
        "the first access after the bound was hit",
        &access,
        &json!({"ExternalId": "S-1-5-21-0", "Role": ROLE}),
    );
    let answer = emulator
        .call(
            "DescribeAccess",
            json!({
                "ServerId": server_id,
                "ExternalId": format!("S-1-5-21-{MAX_ACCESSES_PER_SERVER}"),
            }),
        )
        .await;
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case \"the access the bound refused\": expected it to be absent, got {}",
        answer.body
    );
}

/// One `ListAccesses` call that must be served, with the whole answer body.
async fn list_accesses(emulator: &Emulator, case: &str, request: Value) -> Value {
    let answer = emulator.call("ListAccesses", request).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected ListAccesses 200, got {} with body {}",
        answer.status, answer.body
    );
    answer.body
}

/// The `Accesses` of one answer. The member is modelled required, so it is an array
/// even for a server that holds no grant.
fn accesses_of(case: &str, body: &Value) -> Vec<Value> {
    member_of(body, "Accesses")
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Accesses is modelled required, got {body}"))
        .clone()
}

/// RULE 4: `ListAccesses` reports exactly the grants stored for the server it names,
/// each entry projected from the same record `DescribeAccess` reports, in `ExternalId`
/// order.
///
/// The rows differ along which optional members the create carried, the
/// `HomeDirectoryType` the write resolved, the role granted and the shape and length of
/// the `ExternalId`; the servers differ along how many grants they hold (none, one,
/// several), and one grant is deleted to show the listing follows the store.
#[tokio::test]
async fn list_accesses_reports_the_grants_stored_for_the_server_it_names() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;
    let neighbour = emulator.directory_server().await;
    let empty_server = emulator.directory_server().await;

    // A server that holds no grant: the modelled-required list comes back empty rather
    // than absent, with the `ServerId` echoed and no page token.
    let case = "a directory server holding no grant";
    let body = list_accesses(&emulator, case, json!({"ServerId": &empty_server})).await;
    assert!(
        accesses_of(case, &body).is_empty(),
        "case {case:?}: expected no listed grant, got {body}"
    );
    assert_eq!(
        member_of(&body, "ServerId").as_str(),
        Some(empty_server.as_str()),
        "case {case:?}: expected the ServerId back, got {body}"
    );
    assert!(
        member_of(&body, "NextToken").is_null(),
        "case {case:?}: expected no NextToken with no further page, got {body}"
    );

    // (case, ExternalId, the extra CreateAccess members, the ListedAccess entry the
    // listing must report — a null expectation means the member must be absent)
    let cases: Vec<(&str, &str, Value, Value)> = vec![
        (
            "only the required members",
            GROUP_SID,
            json!({}),
            json!({
                "ExternalId": GROUP_SID,
                "Role": ROLE,
                "HomeDirectoryType": "PATH",
                "HomeDirectory": null,
            }),
        ),
        (
            "the shortest ExternalId, with a PATH landing directory",
            SHORT_SID,
            json!({
                "HomeDirectoryType": "PATH",
                "HomeDirectory": "/bucket_name/home/short",
            }),
            json!({
                "ExternalId": SHORT_SID,
                "Role": ROLE,
                "HomeDirectoryType": "PATH",
                "HomeDirectory": "/bucket_name/home/short",
            }),
        ),
        (
            "LOGICAL mappings and a session policy, under another role",
            "S-1-5-21-1111111111-2222222222-3333333333-4444",
            json!({
                "Role": OTHER_ROLE,
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [
                    {"Entry": "/directory1", "Target": "/bucket_name/home/mydirectory"},
                    {"Entry": "/directory2", "Target": "/bucket_name/home/other"},
                ],
                "Policy": "{\"Version\":\"2012-10-17\",\"Statement\":[]}",
            }),
            json!({
                "ExternalId": "S-1-5-21-1111111111-2222222222-3333333333-4444",
                "Role": OTHER_ROLE,
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectory": null,
            }),
        ),
        (
            "a POSIX profile and a single chroot mapping",
            "S-1-5-32-544",
            json!({
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectoryMappings": [{"Entry": "/", "Target": "/bucket_name/home/root"}],
                "PosixProfile": {"Uid": 1000, "Gid": 1000},
            }),
            json!({
                "ExternalId": "S-1-5-32-544",
                "Role": ROLE,
                "HomeDirectoryType": "LOGICAL",
                "HomeDirectory": null,
            }),
        ),
    ];

    for (case, external_id, extra, _) in &cases {
        let mut request = json!({"ServerId": &server_id, "ExternalId": external_id, "Role": ROLE});
        let object = request.as_object_mut().expect("the request is an object");
        for (member, value) in extra.as_object().expect("the extra members are an object") {
            object.insert(member.clone(), value.clone());
        }
        let answer = emulator.call("CreateAccess", request).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected CreateAccess 200, got {} with body {}",
            answer.status, answer.body
        );
    }

    // The neighbouring server holds a grant under an `ExternalId` this one also has, so
    // a listing that reached past its own server would show up here.
    let answer = emulator
        .call(
            "CreateAccess",
            json!({
                "ServerId": &neighbour,
                "ExternalId": GROUP_SID,
                "Role": ROLE,
                "HomeDirectory": "/bucket/neighbour",
            }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case \"the neighbour's grant\": expected 200, got {} with body {}",
        answer.status, answer.body
    );

    let case = "the whole set of this server's grants";
    let body = list_accesses(&emulator, case, json!({"ServerId": &server_id})).await;
    let listed = accesses_of(case, &body);
    assert_eq!(
        listed.len(),
        cases.len(),
        "case {case:?}: expected {} listed grants, got {body}",
        cases.len()
    );
    assert!(
        member_of(&body, "NextToken").is_null(),
        "case {case:?}: expected no NextToken when one page holds the set, got {body}"
    );

    // `ExternalId` order, which is the order the store keys the grants in.
    let keys: Vec<String> = listed
        .iter()
        .map(|entry| {
            member_of(entry, "ExternalId")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "case {case:?}: expected the grants in ExternalId order, got {keys:?}"
    );

    for (case, external_id, _, expected) in &cases {
        let entry = listed
            .iter()
            .find(|entry| member_of(entry, "ExternalId").as_str() == Some(*external_id))
            .unwrap_or_else(|| panic!("case {case:?}: {external_id} is not listed, got {body}"));
        assert_members(case, entry, expected);

        // The entry is a projection of the record the reader reports: every member
        // `ListedAccess` declares reads the same through both operations, and the entry
        // carries no member the shape does not declare.
        let described = emulator.described_access(&server_id, external_id).await;
        for member in LISTED_ACCESS_MEMBERS {
            assert_eq!(
                member_of(entry, member),
                member_of(&described, member),
                "case {case:?}: expected the listed {member} to match DescribeAccess, got \
                 {entry} against {described}"
            );
        }
        let members: Vec<&String> = entry
            .as_object()
            .expect("a listed access is an object")
            .keys()
            .filter(|member| !LISTED_ACCESS_MEMBERS.contains(&member.as_str()))
            .collect();
        assert!(
            members.is_empty(),
            "case {case:?}: expected only the members ListedAccess declares, got {members:?}"
        );
    }

    // The neighbour's listing is its own, even though the two servers share an
    // `ExternalId`.
    let case = "the neighbouring server's own listing";
    let body = list_accesses(&emulator, case, json!({"ServerId": &neighbour})).await;
    let listed = accesses_of(case, &body);
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected the neighbour's single grant, got {body}"
    );
    assert_members(
        case,
        &listed[0],
        &json!({"ExternalId": GROUP_SID, "HomeDirectory": "/bucket/neighbour"}),
    );

    // A grant that is deleted stops being listed, and the rest of the set is untouched.
    let deleted = emulator
        .call(
            "DeleteAccess",
            json!({"ServerId": &server_id, "ExternalId": SHORT_SID}),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case \"DeleteAccess\": expected 200, got {} with body {}",
        deleted.status, deleted.body
    );
    let case = "the set after one grant was deleted";
    let body = list_accesses(&emulator, case, json!({"ServerId": &server_id})).await;
    let keys: Vec<String> = accesses_of(case, &body)
        .iter()
        .map(|entry| {
            member_of(entry, "ExternalId")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut expected: Vec<String> = cases
        .iter()
        .map(|(_, external_id, _, _)| (*external_id).to_string())
        .filter(|external_id| external_id != SHORT_SID)
        .collect();
    expected.sort();
    assert_eq!(
        keys, expected,
        "case {case:?}: expected {expected:?} still listed, got {keys:?}"
    );
}

/// RULE 4 (the paging half): a page carries at most `MaxResults` grants and a
/// `NextToken` only while a further page remains, and the pages concatenate to the
/// whole set, in order, exactly once.
///
/// The rows are the page bounds: no bound at all, both published bounds of
/// `MaxResults`' modelled `range 1..1000`, and bounds below, equal to and above the
/// stored count.
#[tokio::test]
async fn list_accesses_pages_by_max_results() {
    let emulator = Emulator::new();
    let server_id = emulator.directory_server().await;

    let stored = 5;
    let mut all: Vec<String> = Vec::new();
    for n in 0..stored {
        let external_id = format!("S-1-5-21-{n}");
        let answer = emulator
            .call(
                "CreateAccess",
                json!({"ServerId": &server_id, "ExternalId": &external_id, "Role": ROLE}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case \"grant {n}\": expected 200, got {} with body {}",
            answer.status, answer.body
        );
        all.push(external_id);
    }
    all.sort();

    // No bound, both published bounds of `MaxResults`' range 1..1000, and bounds below,
    // at and above the stored count.
    for max_results in [None, Some(1), Some(2), Some(5), Some(6), Some(1000)] {
        let case = format!("MaxResults={max_results:?}");
        let (page_sizes, keys) = drain_pages(
            &emulator,
            "ListAccesses",
            &json!({"ServerId": &server_id}),
            "Accesses",
            "ExternalId",
            max_results,
        )
        .await;
        if let Some(limit) = max_results {
            let limit = usize::try_from(limit).expect("a positive page bound");
            for size in &page_sizes {
                assert!(
                    *size <= limit,
                    "case {case:?}: a page carried {size} items, above the bound {limit}"
                );
            }
            let want_pages = all.len().div_ceil(limit);
            assert_eq!(
                page_sizes.len(),
                want_pages,
                "case {case:?}: expected {want_pages} page(s), got {page_sizes:?}"
            );
        } else {
            assert_eq!(
                page_sizes,
                vec![all.len()],
                "case {case:?}: expected a single unbounded page, got {page_sizes:?}"
            );
        }
        assert_eq!(
            keys, all,
            "case {case:?}: expected every grant exactly once in ExternalId order, got {keys:?}"
        );
    }

    // The token resumes exactly where the page stopped, and it is opaque: it is not the
    // key it resumes after.
    let case = "resuming from an issued NextToken";
    let first = list_accesses(
        &emulator,
        case,
        json!({"ServerId": &server_id, "MaxResults": 2}),
    )
    .await;
    let token = member_of(&first, "NextToken")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a NextToken, got {first}"))
        .to_string();
    assert!(
        !all.contains(&token),
        "case {case:?}: expected an opaque token, got the key {token:?}"
    );
    let second = list_accesses(
        &emulator,
        case,
        json!({"ServerId": &server_id, "MaxResults": 2, "NextToken": token}),
    )
    .await;
    let keys: Vec<String> = accesses_of(case, &second)
        .iter()
        .map(|entry| {
            member_of(entry, "ExternalId")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        keys,
        all[2..4].to_vec(),
        "case {case:?}: expected the page after the first two, got {keys:?}"
    );

    // One PAST each published bound of `MaxResults`' modelled range 1..1000 is refused,
    // while each bound itself is served.
    for bad in [0, 1001] {
        let answer = emulator
            .call(
                "ListAccesses",
                json!({"ServerId": &server_id, "MaxResults": bad}),
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case \"MaxResults={bad}\": expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case \"MaxResults={bad}\": expected ValidationException, got {}",
            answer.body
        );
    }
    for good in [1, 1000] {
        let case = format!("MaxResults={good}");
        let body = list_accesses(
            &emulator,
            &case,
            json!({"ServerId": &server_id, "MaxResults": good}),
        )
        .await;
        assert!(
            !accesses_of(&case, &body).is_empty(),
            "case {case:?}: expected a page of grants, got {body}"
        );
    }
}

/// RULE 4 (the refusals): a listing is refused for a server with no directory to hold
/// groups, for a server the store does not hold, and for a `NextToken` the service did
/// not mint — each with the modelled error for that case, and the directory refusal
/// with the text fixed for this operation and the server's own type.
///
/// The rows span the whole non-directory half of the `IdentityProviderType` value set
/// (`smithy_query(query="shape",shape="IdentityProviderType")`), including the absent
/// member that resolves to `SERVICE_MANAGED`, and the shapes a token that decodes to
/// nothing can take.
#[tokio::test]
async fn list_accesses_refuses_a_server_or_token_it_cannot_resolve() {
    let emulator = Emulator::new();

    // A server the store does not hold: the service's not-found answer, naming the
    // server as the missing resource.
    let answer = emulator
        .call("ListAccesses", json!({"ServerId": UNMINTED_SERVER_ID}))
        .await;
    assert_eq!(
        answer.status, 400,
        "case \"an unminted server\": expected HTTP 400, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case \"an unminted server\": expected ResourceNotFoundException, got {}",
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "ResourceType").as_str(),
        Some("Server"),
        "case \"an unminted server\": expected ResourceType Server, got {}",
        answer.body
    );

    // (case, the CreateServer request, the IdP type the server resolves to)
    let non_directory: Vec<(&str, Value, &str)> = vec![
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

    for (case, server_request, idp_type) in non_directory {
        let server_id = emulator.create_server(server_request).await;
        let answer = emulator
            .call("ListAccesses", json!({"ServerId": &server_id}))
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
            format!("Cannot list access on server with IdP type: {idp_type}"),
            "case {case:?}: expected the fixed directory-provider refusal, got {message:?}"
        );
        assert!(
            !message.contains(&server_id),
            "case {case:?}: expected no request data in the refusal, got {message:?}"
        );
    }

    // A `NextToken` this service never handed out resumes nothing, so it is refused
    // rather than silently restarting the listing.
    let server_id = emulator.directory_server().await;
    let answer = emulator
        .call(
            "CreateAccess",
            json!({"ServerId": &server_id, "ExternalId": GROUP_SID, "Role": ROLE}),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case \"a grant to page over\": expected 200, got {} with body {}",
        answer.status, answer.body
    );

    for token in ["not-a-token", GROUP_SID, "zz", "0"] {
        let answer = emulator
            .call(
                "ListAccesses",
                json!({"ServerId": &server_id, "NextToken": token}),
            )
            .await;
        assert_eq!(
            answer.status, 400,
            "case \"NextToken={token}\": expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidNextTokenException",
            "case \"NextToken={token}\": expected InvalidNextTokenException, got {}",
            answer.body
        );
        assert_eq!(
            member_of(&answer.body, "Message").as_str(),
            Some("Invalid nextToken"),
            "case \"NextToken={token}\": expected the service's token diagnostic, got {}",
            answer.body
        );
    }
}
