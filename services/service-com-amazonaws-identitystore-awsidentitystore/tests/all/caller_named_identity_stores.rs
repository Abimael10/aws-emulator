// SPDX-License-Identifier: CC0-1.0

//! The identity store a request names is the store its CALLER has, driven end to end
//! over HTTP through [`crate::harness`].
//!
//! The rule under test: this API models no operation that creates an identity store — in
//! AWS one comes with an IAM Identity Center instance, and a client reads its id out of
//! its own environment — so WHICH stores exist is Identity Center's answer:
//!
//! * while Identity Center has decided nothing about this account's enablement, every
//!   `IdentityStoreId` the modelled constraints admit names the store the caller's own
//!   environment gave it and is served, each store holding exactly what has been written
//!   to it;
//! * once Identity Center holds an instance, the stores the account has are those
//!   instances' identity stores, and any other well-formed id is refused with the
//!   modelled `ResourceNotFoundException` naming `IDENTITY_STORE` — having created
//!   nothing ([`an_identity_store_the_account_does_not_have_is_refused`]);
//! * an identifier that names a store this emulator cannot serve at all (an ARN carrying
//!   another account) is refused either way;
//! * and the same set is what `ListIdentityStores` REPORTS, each store as its id and the
//!   ARN derived from that id, paged by `MaxResults` with an opaque `NextToken`
//!   ([`list_identity_stores_reports_every_store_this_emulator_holds`]) — so a store the
//!   listing names is a store a request may be addressed to, and one it leaves out is one
//!   that resolution refuses.
//!
//! A value the modelled `@length`/`@pattern` refuse is a `ValidationException`, never a
//! not-found.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the FORM
//! of the identifier (`d-` + 10 hex or a UUID, bare or as the store's ARN, in either
//! partition spelling), its LENGTH (both published bounds of the modelled 1..93 and one
//! past the top), and WHICH store it names (the seeded one, a caller's own, another
//! account's). The recorded ids the issue cites are three rows among them.

use serde_json::{json, Value};
use service_com_amazonaws_identitystore_awsidentitystore as identitystore;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, emulator, ok, refused, string, MISSING_STORE, NOT_FOUND, STORE};

/// An account id that is NOT this emulator's, for the ARN form that names another
/// account's store (the modelled pattern admits any 12-digit account).
const OTHER_ACCOUNT: &str = "210987654321";

/// A `UUID`-form `IdentityStoreId`, the second form the modelled pattern admits
/// (`smithy_query(query="shape",shape="IdentityStoreId")`).
const UUID_FORM: &str = "a1b2c3d4-5678-90ab-cdef-111122223333";

/// The published bounds of the modelled `IdentityStoreId` length, from
/// `smithy_query(query="shape",shape="IdentityStoreId")` (length 1..93). The pattern
/// admits nothing shorter than `d-` + 10 hexadecimal digits, which is the shortest value
/// that can reach a handler at all; the longest is the ARN form at its maximum.
const ID_MIN_REACHABLE_LEN: usize = 12;
const ID_MAX_LEN: usize = 93;

/// The emulator's own account id, which the ARN form of a store it serves carries.
fn account_id() -> String {
    engine::emulator_account_id().to_string()
}

/// The store ARN form of `id` under `account`, in the `partition` spelling the modelled
/// pattern's `aws[a-z-]*` admits.
fn store_arn(partition: &str, account: &str, id: &str) -> String {
    format!("arn:{partition}:identitystore::{account}:identitystore/{id}")
}

/// `CreateGroup` in the store `identity_store_id` names.
async fn create_group(
    router: &MultiServiceRouter,
    identity_store_id: &str,
    display_name: &str,
    case: &str,
) -> Value {
    ok(
        router,
        "CreateGroup",
        json!({ "IdentityStoreId": identity_store_id, "DisplayName": display_name }),
        case,
    )
    .await
}

/// A router with this service AND IAM Identity Center mounted on ONE engine, so the
/// identity stores this service serves are the stores that service's own instances have.
fn emulator_with_identity_center() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the request still carries a SigV4
    // credential scope, which is what routes it to its service.
    router.set_request_authentication(false);
    router.register_service(identitystore::EmulatedService::new(engine.clone()));
    router.register_service(
        service_com_amazonaws_ssoadmin_swbexternalservice::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call to IAM Identity Center, which must answer 200, as its body.
///
/// Its own front door: the `sso` signing name in the credential scope is what routes the
/// request there, and `SWBExternalService` is its `X-Amz-Target` prefix.
async fn identity_center_call(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("SWBExternalService.{operation}"))
        .header("host", "sso.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDIDENTITYCENTER/20260101/us-east-1\
             /sso/aws4_request, SignedHeaders=host;x-amz-target, Signature=00",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    assert_eq!(
        status,
        200,
        "expected Identity Center's {operation} to answer 200, got {status} with {}",
        String::from_utf8_lossy(&payload)
    );
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// `ListGroups` in the store `identity_store_id` names, as the whole response body.
async fn list_groups(router: &MultiServiceRouter, identity_store_id: &str, case: &str) -> Value {
    ok(
        router,
        "ListGroups",
        json!({ "IdentityStoreId": identity_store_id }),
        case,
    )
    .await
}

/// Every store id form that names a store this emulator SERVES, paired with the id that
/// store reports for it (the bare id, for either form that names it).
fn served_rows() -> Vec<(&'static str, String, String)> {
    let longest_uuid_arn = store_arn("aws-us-gov", &account_id(), UUID_FORM);
    vec![
        // The three ids the recorded fixtures this issue was filed from use: each is a
        // store a client read out of its own environment, and none is the seeded value.
        (
            "a recorded client's store",
            "d-a2ede64c82".to_string(),
            "d-a2ede64c82".to_string(),
        ),
        (
            "a second recorded client's store",
            "d-e00797f110".to_string(),
            "d-e00797f110".to_string(),
        ),
        (
            "a third recorded client's store",
            "d-486a49b0e4".to_string(),
            "d-486a49b0e4".to_string(),
        ),
        // The store another service advertises, which is one row and not a privileged
        // case.
        (
            "the seeded store",
            STORE.to_string(),
            STORE.to_string(),
        ),
        (
            "the seeded store by ARN",
            store_arn("aws", &account_id(), STORE),
            STORE.to_string(),
        ),
        (
            "an id in UUID form",
            UUID_FORM.to_string(),
            UUID_FORM.to_string(),
        ),
        (
            "an id in UUID form, by ARN",
            store_arn("aws", &account_id(), UUID_FORM),
            UUID_FORM.to_string(),
        ),
        (
            // The partition is a wildcard in the modelled pattern, so a store ARN in
            // another partition names the same store.
            "a d- id by ARN in another partition",
            store_arn("aws-cn", &account_id(), "d-0f1e2d3c4b"),
            "d-0f1e2d3c4b".to_string(),
        ),
        (
            // The shortest value the modelled pattern admits: `d-` + 10 hex digits.
            "the shortest id the pattern admits",
            "d-0000000000".to_string(),
            "d-0000000000".to_string(),
        ),
        (
            // The longest the modelled length admits: the ARN form at 93 characters.
            "an id at the modelled maximum length",
            longest_uuid_arn,
            UUID_FORM.to_string(),
        ),
    ]
}

/// The rule for a WRITE: a create is served in whatever store the caller names, answers
/// the required members of its output, and what it stored is readable back from that
/// store — and from no other.
#[tokio::test]
async fn a_create_is_served_in_whatever_store_the_caller_names() {
    let router = emulator();
    let rows = served_rows();
    assert_eq!(
        rows[8].1.len(),
        ID_MIN_REACHABLE_LEN,
        "expected the shortest row to be the shortest id the pattern admits, got {:?}",
        rows[8].1
    );
    assert_eq!(
        rows[9].1.len(),
        ID_MAX_LEN,
        "expected the longest row to sit on the modelled maximum, got {:?}",
        rows[9].1
    );

    let mut minted: Vec<(String, String, String)> = Vec::new();
    for (case, identity_store_id, reported_id) in &rows {
        let display_name = format!("group-for-{}", case.replace(' ', "-"));
        let created = create_group(&router, identity_store_id, &display_name, case).await;
        // CreateGroup's output members are all required: the store it was addressed to,
        // the minted id and ARN, and the revision.
        assert_eq!(
            &string(&created, "IdentityStoreId", case),
            reported_id,
            "case {case:?}: expected the store the request named to be echoed, got {created}"
        );
        let group_id = string(&created, "GroupId", case);
        let group_arn = string(&created, "GroupArn", case);
        assert_eq!(
            group_arn,
            format!("arn:aws:identitystore:::group/{group_id}"),
            "case {case:?}: expected the group's ARN to carry its own id, got {created}"
        );
        assert_eq!(
            string(&created, "Revision", case),
            "1",
            "case {case:?}: expected a create to be revision 1, got {created}"
        );

        // Read back through the store the request named: a describe reports what was
        // stored, and the `Description` nothing supplied is ABSENT rather than empty.
        let described = ok(
            &router,
            "DescribeGroup",
            json!({ "IdentityStoreId": identity_store_id, "GroupId": group_id.clone() }),
            case,
        )
        .await;
        assert_eq!(
            string(&described, "DisplayName", case),
            display_name,
            "case {case:?}: expected the stored DisplayName, got {described}"
        );
        assert_eq!(
            &string(&described, "IdentityStoreId", case),
            reported_id,
            "case {case:?}: expected the describe to report that store, got {described}"
        );
        assert!(
            described.get("Description").is_none_or(Value::is_null),
            "case {case:?}: expected no Description on a group created without one, \
             got {described}"
        );
        minted.push((reported_id.clone(), group_id, display_name));
    }

    // Each store lists exactly the group created in it: a create addressed to one store
    // never lands in another, however the two ids are written.
    for (reported_id, group_id, display_name) in &minted {
        let case = format!("{reported_id} lists its own group");
        let listed = list_groups(&router, reported_id, &case).await;
        let groups = listed["Groups"].as_array().cloned().unwrap_or_default();
        let names: Vec<String> = groups
            .iter()
            .map(|group| group["DisplayName"].as_str().unwrap_or_default().to_string())
            .collect();
        // The seeded store is named by two rows, so it holds two groups; every other
        // store holds exactly the one created in it.
        assert!(
            names.contains(display_name),
            "case {case:?}: expected the group created in that store to be listed, \
             got {names:?}"
        );
        for group in &groups {
            assert_eq!(
                group["IdentityStoreId"].as_str().unwrap_or_default(),
                reported_id,
                "case {case:?}: expected every listed group to report its own store, \
                 got {group}"
            );
        }
        // A group minted in one store is not readable through another.
        let elsewhere = minted
            .iter()
            .find(|(other, _, _)| other != reported_id)
            .expect("the table names more than one store");
        let body = refused(
            &router,
            "DescribeGroup",
            json!({ "IdentityStoreId": elsewhere.0.clone(), "GroupId": group_id.clone() }),
            NOT_FOUND,
            "ResourceNotFoundException",
            &case,
        )
        .await;
        assert_eq!(
            body["ResourceType"].as_str().unwrap_or_default(),
            "GROUP",
            "case {case:?}: expected the GROUP to be what another store is missing, \
             got {body}"
        );
    }
}

/// The rule for a unique-attribute READ: `GetUserId` resolves the user of the store the
/// request names, in every form that names it.
#[tokio::test]
async fn a_user_resolves_by_unique_attribute_in_whatever_store_the_caller_names() {
    let router = emulator();
    for (case, identity_store_id, reported_id) in served_rows() {
        let user_name = format!("user-for-{}", case.replace(' ', "-"));
        let created = ok(
            &router,
            "CreateUser",
            json!({ "IdentityStoreId": identity_store_id, "UserName": user_name.clone() }),
            case,
        )
        .await;
        let user_id = string(&created, "UserId", case);
        let user_arn = string(&created, "UserArn", case);
        assert_eq!(
            string(&created, "IdentityStoreId", case),
            reported_id,
            "case {case:?}: expected the store the request named to be echoed, got {created}"
        );

        let resolved = ok(
            &router,
            "GetUserId",
            json!({
                "IdentityStoreId": identity_store_id,
                "AlternateIdentifier": {
                    "UniqueAttribute": {
                        "AttributePath": "userName",
                        "AttributeValue": user_name.clone(),
                    },
                },
            }),
            case,
        )
        .await;
        // GetUserId's output members are all required.
        assert_eq!(
            string(&resolved, "IdentityStoreId", case),
            reported_id,
            "case {case:?}: expected the lookup to report that store, got {resolved}"
        );
        assert_eq!(
            string(&resolved, "UserId", case),
            user_id,
            "case {case:?}: expected the created user's id, got {resolved}"
        );
        assert_eq!(
            string(&resolved, "UserArn", case),
            user_arn,
            "case {case:?}: expected the created user's ARN, got {resolved}"
        );
    }
}

/// The rule for a store nothing has written to: it is served, reports the nothing it
/// holds, and a read of it writes nothing.
#[tokio::test]
async fn a_store_nothing_has_written_to_is_served_and_holds_nothing() {
    let router = emulator();
    // Rows differ along the form of the id and whether anything was ever addressed to it.
    let rows: Vec<(&str, String)> = vec![
        ("a d- id", "d-abcdef0123".to_string()),
        ("a UUID id", UUID_FORM.to_string()),
        (
            "an ARN of this account",
            store_arn("aws", &account_id(), "d-abcdef0124"),
        ),
    ];

    for (case, identity_store_id) in &rows {
        for attempt in 0..2 {
            let case = format!("{case}, read {attempt}");
            let groups = list_groups(&router, identity_store_id, &case).await;
            assert_eq!(
                groups["Groups"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected no group in a store nothing has written to, \
                 got {groups}"
            );
            let users = ok(
                &router,
                "ListUsers",
                json!({ "IdentityStoreId": identity_store_id }),
                &case,
            )
            .await;
            assert_eq!(
                users["Users"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected no user in a store nothing has written to, \
                 got {users}"
            );
            // A resource read of such a store reports the RESOURCE as missing, never the
            // store.
            let body = refused(
                &router,
                "DescribeGroup",
                json!({
                    "IdentityStoreId": identity_store_id,
                    "GroupId": "a1b2c3d4-5678-90ab-cdef-999999999999",
                }),
                NOT_FOUND,
                "ResourceNotFoundException",
                &case,
            )
            .await;
            assert_eq!(
                body["ResourceType"].as_str().unwrap_or_default(),
                "GROUP",
                "case {case:?}: expected the GROUP to be the missing resource, got {body}"
            );
        }

        // The reads wrote nothing: the first create addressed to that store is still the
        // first group it holds.
        let created = create_group(&router, identity_store_id, "after the reads", case).await;
        let listed = list_groups(&router, identity_store_id, case).await;
        let groups = listed["Groups"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            groups.len(),
            1,
            "case {case:?}: expected exactly the one created group, got {listed}"
        );
        assert_eq!(
            groups[0]["GroupId"].as_str().unwrap_or_default(),
            string(&created, "GroupId", case),
            "case {case:?}: expected the listing to report the created group, got {listed}"
        );
    }
}

/// The rule once Identity Center HAS answered: the stores this account has are the
/// identity stores of the IAM Identity Center instances it holds, and any other
/// well-formed `IdentityStoreId` is refused with the modelled
/// `ResourceNotFoundException` naming `IDENTITY_STORE`, having created nothing.
///
/// Both services are mounted on ONE engine, so the store this suite serves is the store
/// `sso-admin`'s own `ListInstances` reports rather than a value restated here. Rows
/// differ along the FORM of the refused id (both forms the modelled pattern admits, bare
/// and as this account's ARN) and along which operation is addressed to it — a create, a
/// read, a listing, an update and a delete.
#[tokio::test]
async fn an_identity_store_the_account_does_not_have_is_refused() {
    let router = emulator_with_identity_center();

    // The account's own identity store, as a client reads it: Identity Center's
    // `ListInstances`. Asking it is also what settles the account's enablement, so from
    // here on the stores the account has are knowable.
    let instances = identity_center_call(&router, "ListInstances", json!({})).await;
    let listed = instances["Instances"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "expected Identity Center to report the account's one instance, got {instances}"
    );
    let account_store = listed[0]["IdentityStoreId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !account_store.is_empty(),
        "expected the instance to report its IdentityStoreId, got {instances}"
    );

    // The store Identity Center reports IS served, in both forms that name it: this is
    // the store a client has.
    for (case, identifier) in [
        ("the account's store", account_store.clone()),
        (
            "the account's store by ARN",
            store_arn("aws", &account_id(), &account_store),
        ),
    ] {
        let created = create_group(&router, &identifier, &format!("group-for-{case}"), case).await;
        assert_eq!(
            string(&created, "IdentityStoreId", case),
            account_store,
            "case {case:?}: expected the account's store to be echoed, got {created}"
        );
    }

    // Every other well-formed id names no identity store this account has.
    let absent: Vec<(&str, String)> = vec![
        ("a d- id of no instance", "d-9999999999".to_string()),
        (
            "a second d- id of no instance",
            "d-0123456789".to_string(),
        ),
        ("an id in UUID form", UUID_FORM.to_string()),
        (
            "an id of no instance, by this account's ARN",
            store_arn("aws", &account_id(), "d-abcdef0123"),
        ),
        (
            "the seeded value under another account",
            store_arn("aws", OTHER_ACCOUNT, &account_store),
        ),
    ];
    for (case, identity_store_id) in &absent {
        for operation in [
            "CreateGroup",
            "CreateUser",
            "DescribeUser",
            "ListGroups",
            "UpdateGroup",
            "DeleteUser",
        ] {
            let case = format!("{case} / {operation}");
            let unminted = "a1b2c3d4-5678-90ab-cdef-999999999999";
            let request = match operation {
                "CreateGroup" => json!({
                    "IdentityStoreId": identity_store_id,
                    "DisplayName": "never stored",
                }),
                "CreateUser" => json!({
                    "IdentityStoreId": identity_store_id,
                    "UserName": "never-stored",
                }),
                "DescribeUser" => {
                    json!({ "IdentityStoreId": identity_store_id, "UserId": unminted })
                }
                "UpdateGroup" => json!({
                    "IdentityStoreId": identity_store_id,
                    "GroupId": unminted,
                    "Operations": [{ "AttributePath": "description", "AttributeValue": "d" }],
                }),
                "DeleteUser" => {
                    json!({ "IdentityStoreId": identity_store_id, "UserId": unminted })
                }
                _ => json!({ "IdentityStoreId": identity_store_id }),
            };
            let body = refused(
                &router,
                operation,
                request,
                NOT_FOUND,
                "ResourceNotFoundException",
                &case,
            )
            .await;
            assert_eq!(
                body["ResourceType"].as_str().unwrap_or_default(),
                "IDENTITY_STORE",
                "case {case:?}: expected the identity store to be the missing resource, \
                 got {body}"
            );
            assert_eq!(
                crate::harness::error_message(&body),
                identitystore::state::identity_store_not_found_message(identity_store_id),
                "case {case:?}: expected the refusal to name the requested \
                 IdentityStoreId, got {body}"
            );
        }

        // The refused creates stored nothing: the store is still unknown, and the group
        // and user they named are nowhere — not in it, and not in the account's store.
        let case = format!("{case} / nothing stored");
        let listed = ok(
            &router,
            "ListGroups",
            json!({ "IdentityStoreId": account_store.clone() }),
            &case,
        )
        .await;
        let names: Vec<&str> = listed["Groups"]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|group| group["DisplayName"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !names.contains(&"never stored"),
            "case {case:?}: expected the refused create to have stored nothing, \
             got {names:?}"
        );
    }
}

/// The rule's NEGATIVE side: an identifier that names a store this emulator cannot serve
/// is the modelled not-found, and one the modelled constraints refuse is a
/// `ValidationException` — so a not-found is never the answer to a well-formed id of this
/// account while Identity Center has decided nothing.
#[tokio::test]
async fn only_a_store_outside_this_account_is_not_found_and_a_bad_value_is_a_400() {
    let router = emulator();

    // Another account's store, in every form that names one.
    let not_served: Vec<(&str, String)> = vec![
        ("another account's d- store", MISSING_STORE.to_string()),
        (
            "another account's store in UUID form",
            store_arn("aws", OTHER_ACCOUNT, UUID_FORM),
        ),
        (
            "another account's store in another partition",
            store_arn("aws-cn", OTHER_ACCOUNT, STORE),
        ),
    ];
    for (case, identity_store_id) in &not_served {
        for operation in ["CreateGroup", "CreateUser", "ListGroups", "GetUserId"] {
            let case = format!("{case} / {operation}");
            let request = match operation {
                "CreateGroup" => json!({
                    "IdentityStoreId": identity_store_id,
                    "DisplayName": "never stored",
                }),
                "CreateUser" => json!({
                    "IdentityStoreId": identity_store_id,
                    "UserName": "never-stored",
                }),
                "GetUserId" => json!({
                    "IdentityStoreId": identity_store_id,
                    "AlternateIdentifier": {
                        "UniqueAttribute": {
                            "AttributePath": "userName",
                            "AttributeValue": "never-stored",
                        },
                    },
                }),
                _ => json!({ "IdentityStoreId": identity_store_id }),
            };
            let body = refused(
                &router,
                operation,
                request,
                NOT_FOUND,
                "ResourceNotFoundException",
                &case,
            )
            .await;
            assert_eq!(
                body["ResourceType"].as_str().unwrap_or_default(),
                "IDENTITY_STORE",
                "case {case:?}: expected the identity store to be the missing resource, \
                 got {body}"
            );
        }
    }

    // A value the modelled constraints refuse never reaches resolution: it is the 400 the
    // boundary validator answers, one character past the modelled maximum length and for
    // a value outside the pattern.
    let too_long = store_arn("aws-us-govz", &account_id(), UUID_FORM);
    assert_eq!(
        too_long.len(),
        ID_MAX_LEN + 1,
        "expected the over-long row to sit one past the modelled maximum, got {too_long:?}"
    );
    for (case, identity_store_id) in [
        ("one character past the modelled maximum", too_long),
        ("not the modelled pattern", "d-NOTAVALIDST".to_string()),
        ("an id the pattern rejects for length", "d-abc".to_string()),
    ] {
        let (status, code, body) = call(
            &router,
            "ListGroups",
            json!({ "IdentityStoreId": identity_store_id }),
        )
        .await;
        assert_eq!(
            (status, code.ends_with("ValidationException")),
            (400, true),
            "case {case:?}: expected the modelled constraint to refuse it with 400 \
             ValidationException, got {status} {code} with {body}"
        );
    }
}

/// The RECORDS this emulator keeps are bounded: only a create registers one, never past
/// `MAX_IDENTITY_STORES`, and past the bound a create in a store that is already recorded
/// is still served.
#[tokio::test]
async fn the_number_of_recorded_identity_stores_is_bounded() {
    let router = emulator();
    // The seeded store is already one record, so that many MORE stores can be recorded.
    let headroom = identitystore::state::MAX_IDENTITY_STORES - 1;
    for index in 0..headroom {
        let store = format!("d-{index:010x}");
        let case = format!("store {index} of the headroom");
        create_group(&router, &store, &format!("group-{index}"), &case).await;
    }

    // One more NEW store does not fit: the create is refused with the modelled quota
    // error (HTTP 402), and nothing is stored for it.
    let over = format!("d-{headroom:010x}");
    let body = refused(
        &router,
        "CreateGroup",
        json!({ "IdentityStoreId": over.clone(), "DisplayName": "one store too many" }),
        402,
        "ServiceQuotaExceededException",
        "one store past the bound",
    )
    .await;
    assert!(
        body.get("Message")
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "case \"one store past the bound\": expected the quota refusal to carry a message, \
         got {body}"
    );
    // Rows differ along which operation addresses the store that was refused: none of
    // them sees it hold anything, because the refusal stored nothing.
    for (case, operation, request) in [
        (
            "the refused store lists nothing",
            "ListGroups",
            json!({ "IdentityStoreId": over.clone() }),
        ),
        (
            "and no user either",
            "ListUsers",
            json!({ "IdentityStoreId": over.clone() }),
        ),
    ] {
        let listed = ok(&router, operation, request, case).await;
        let items = listed["Groups"]
            .as_array()
            .or_else(|| listed["Users"].as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            items.is_empty(),
            "case {case:?}: expected the refused create to have stored nothing, got {listed}"
        );
    }

    // The bound is on RECORDS, not on writes: a create in a store that is already
    // recorded is still served at capacity.
    for (case, store) in [
        ("the seeded store", STORE.to_string()),
        ("a store recorded earlier", "d-0000000000".to_string()),
    ] {
        create_group(&router, &store, &format!("after capacity {case}"), case).await;
    }
}

/// A write that stores NO new resource registers no store record either, so it cannot
/// consume the bound: only a create does.
#[tokio::test]
async fn a_write_that_stores_nothing_records_no_identity_store() {
    let router = emulator();
    // Rows are the writes that only ever change what is already there, each addressed to
    // a store nothing has written to, and what each must answer there.
    let probes: Vec<(&str, Value, u16, &str)> = vec![
        (
            "UpdateUser",
            json!({
                "UserId": "a1b2c3d4-5678-90ab-cdef-999999999999",
                "Operations": [{ "AttributePath": "displayName", "AttributeValue": "Ada" }],
            }),
            NOT_FOUND,
            "USER",
        ),
        (
            "UpdateGroup",
            json!({
                "GroupId": "a1b2c3d4-5678-90ab-cdef-999999999999",
                "Operations": [{ "AttributePath": "description", "AttributeValue": "d" }],
            }),
            NOT_FOUND,
            "GROUP",
        ),
        (
            "CreateGroupMembership",
            json!({
                "GroupId": "a1b2c3d4-5678-90ab-cdef-999999999999",
                "MemberId": { "UserId": "a1b2c3d4-5678-90ab-cdef-888888888888" },
            }),
            NOT_FOUND,
            "GROUP",
        ),
    ];

    // Each probe is repeated enough times, each time against a store of its own, that a
    // registering write would have exhausted the bound several times over.
    let rounds = identitystore::state::MAX_IDENTITY_STORES;
    for (probe, (operation, body, status, resource_type)) in probes.iter().enumerate() {
        for round in 0..rounds {
            // `d-` + 10 hex digits, unique per (probe, round).
            let store = format!("d-f{probe:01x}0000{round:04x}");
            let case = format!("{operation} in {store}");
            let mut request = body.clone();
            request["IdentityStoreId"] = json!(store);
            let refusal = refused(
                &router,
                operation,
                request,
                *status,
                "ResourceNotFoundException",
                &case,
            )
            .await;
            assert_eq!(
                refusal["ResourceType"].as_str().unwrap_or_default(),
                *resource_type,
                "case {case:?}: expected {resource_type} to be the missing resource in a \
                 store that is served, got {refusal}"
            );
        }
    }

    // The delete family is `@idempotent`, so its answer in a store that holds nothing is
    // the empty success — which also stores nothing.
    let idempotent: [(&str, Value); 3] = [
        (
            "DeleteGroup",
            json!({ "GroupId": "a1b2c3d4-5678-90ab-cdef-999999999999" }),
        ),
        (
            "DeleteUser",
            json!({ "UserId": "a1b2c3d4-5678-90ab-cdef-999999999999" }),
        ),
        (
            "DeleteGroupMembership",
            json!({ "MembershipId": "a1b2c3d4-5678-90ab-cdef-999999999999" }),
        ),
    ];
    for (probe, (operation, body)) in idempotent.iter().enumerate() {
        for round in 0..rounds {
            let store = format!("d-e{probe:01x}0000{round:04x}");
            let case = format!("{operation} in {store}");
            let mut request = body.clone();
            request["IdentityStoreId"] = json!(store);
            let answered = ok(&router, operation, request, &case).await;
            assert_eq!(
                answered,
                json!({}),
                "case {case:?}: expected the idempotent delete's empty success, \
                 got {answered}"
            );
        }
    }

    // None of those writes recorded a store, so the whole bound is still free: a create
    // in each of that many fresh stores is served.
    for index in 0..(identitystore::state::MAX_IDENTITY_STORES - 1) {
        let store = format!("d-{index:010x}");
        let case = format!("create in fresh store {index}");
        create_group(&router, &store, &format!("group-{index}"), &case).await;
    }
}

// ---------------------------------------------------------------------------
// The same set of identity stores, REPORTED: `ListIdentityStores`.
// ---------------------------------------------------------------------------

/// The published bounds of the modelled `MaxResults` range this listing carries (range
/// 1..100, from `smithy_query(query="operation",operation="ListIdentityStores")`). The
/// operation's member declares no `@default`, so an absent one asks for this service's
/// default page size.
const LIST_STORES_MAX_RESULTS_MIN: i64 = 1;
const LIST_STORES_MAX_RESULTS_MAX: i64 = 100;

/// An `IdentityStoreId` the suites below write nothing to until they say so: it must not be
/// reported, which is what makes the listing a report of STATE rather than of the ids the
/// modelled pattern admits.
const UNWRITTEN_STORE: &str = "d-1111111111";

/// One page of `ListIdentityStores`: the `(IdentityStoreId, IdentityStoreArn)` pairs it
/// reported, in the order it reported them, and its `NextToken`.
async fn identity_stores_page(
    router: &MultiServiceRouter,
    request: Value,
    case: &str,
) -> (Vec<(String, String)>, Option<String>) {
    let answer = ok(router, "ListIdentityStores", request, case).await;
    let entries = answer["IdentityStores"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the required IdentityStores list, got {answer}")
        });
    let stores = entries
        .iter()
        .map(|entry| {
            (
                string(entry, "IdentityStoreId", case),
                string(entry, "IdentityStoreArn", case),
            )
        })
        .collect();
    (stores, answer["NextToken"].as_str().map(str::to_string))
}

/// Every identity store the listing reports, walked to its end at `max_results` per page.
///
/// Each page is judged as it arrives — no page carries more entries than it was asked for,
/// a `NextToken` only ever follows a FULL page, each entry's ARN is the one derived from
/// its own id, and the walk terminates — so this helper is also what pins pagination
/// itself.
async fn walk_identity_stores(
    router: &MultiServiceRouter,
    max_results: Option<i64>,
    case: &str,
) -> Vec<String> {
    let page_size = max_results.unwrap_or(LIST_STORES_MAX_RESULTS_MAX) as usize;
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // A page reports at least one store, so a walk needs no more pages than this emulator
    // records stores, plus one for the page that carries no cursor.
    let page_budget = identitystore::state::MAX_IDENTITY_STORES + 1;
    for page_number in 1..=page_budget {
        let mut request = json!({});
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        if let Some(token) = token.clone() {
            request["NextToken"] = json!(token);
        }
        let row = format!("{case}, page {page_number}");
        let (stores, next_token) = identity_stores_page(router, request, &row).await;
        assert!(
            stores.len() <= page_size,
            "case {row:?}: expected at most {page_size} stores on a page, got {}",
            stores.len()
        );
        for (identity_store_id, identity_store_arn) in &stores {
            // The ARN is DERIVED from the id, so one store's two identifiers cannot
            // disagree.
            assert_eq!(
                identity_store_arn,
                &store_arn("aws", &account_id(), identity_store_id),
                "case {row:?}: expected the ARN derived from {identity_store_id:?}, \
                 got {identity_store_arn:?}"
            );
            collected.push(identity_store_id.clone());
        }
        match next_token {
            None => return collected,
            Some(next) => {
                assert_eq!(
                    stores.len(),
                    page_size,
                    "case {row:?}: expected a NextToken only after a full page of \
                     {page_size}, got {} stores beside {next:?}",
                    stores.len()
                );
                token = Some(next);
            }
        }
    }
    panic!(
        "case {case:?}: the paged walk did not finish within {page_budget} pages: {collected:?}"
    )
}

/// The listing reports the identity stores this emulator HOLDS — each under its own id,
/// once, whatever form the caller addressed it in — and reports nothing it holds no record
/// of.
#[tokio::test]
async fn list_identity_stores_reports_every_store_this_emulator_holds() {
    let router = emulator();

    // Before anything is written, the one store this emulator holds is the one another
    // service advertises.
    let (stores, next_token) = identity_stores_page(&router, json!({}), "a fresh emulator").await;
    assert_eq!(
        stores,
        vec![(STORE.to_string(), store_arn("aws", &account_id(), STORE))],
        "case \"a fresh emulator\": expected the advertised store alone"
    );
    assert_eq!(
        next_token, None,
        "case \"a fresh emulator\": expected no NextToken for a listing that fits one page"
    );

    // The table every suite here shares: each row writes to the store its identifier
    // names, and the listing then reports that store under its own id — two rows that name
    // ONE store (bare and by its ARN) are one entry, never two.
    let mut expected: Vec<String> = vec![STORE.to_string()];
    for (case, identity_store_id, reported_id) in served_rows() {
        let display_name = format!("listed-group-for-{}", case.replace(' ', "-"));
        create_group(&router, &identity_store_id, &display_name, case).await;
        expected.push(reported_id.clone());
        expected.sort();
        expected.dedup();

        let listed = walk_identity_stores(&router, None, case).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected every store held so far, once each, in id order"
        );
        assert!(
            !listed.contains(&UNWRITTEN_STORE.to_string()),
            "case {case:?}: expected a store nothing has written to to be absent, \
             got {listed:?}"
        );
    }

    // A listing is a READ: repeating it answers the same stores, and it records none.
    let again = walk_identity_stores(&router, None, "a repeated listing").await;
    assert_eq!(
        again, expected,
        "case \"a repeated listing\": expected the same stores as before, got {again:?}"
    );

    // The store those listings did not report is still unrecorded rather than hidden: the
    // first create addressed to it is what puts it in the listing.
    let case = "a store the listings did not record";
    create_group(&router, UNWRITTEN_STORE, "after the listings", case).await;
    let listed = walk_identity_stores(&router, None, case).await;
    assert!(
        listed.contains(&UNWRITTEN_STORE.to_string()),
        "case {case:?}: expected the store just written to to be reported, got {listed:?}"
    );
}

/// Whatever the page size, the walk reports every store exactly once in one order: a page
/// resumed with a `NextToken` continues where the previous page stopped.
#[tokio::test]
async fn list_identity_stores_pages_every_store_exactly_once() {
    let router = emulator();

    // Six stores beside the advertised one, so pages of 1, 2, 3 and 7 split the set
    // differently.
    let written = [
        "d-0000000001",
        "d-0000000002",
        "d-00000000a3",
        "d-00000000b4",
        "d-00000000c5",
        "d-00000000d6",
    ];
    for identity_store_id in written {
        create_group(
            &router,
            identity_store_id,
            &format!("group-in-{identity_store_id}"),
            "pagination setup",
        )
        .await;
    }
    let mut expected: Vec<String> = written.iter().map(|id| (*id).to_string()).collect();
    expected.push(STORE.to_string());
    expected.sort();

    // (case, MaxResults): absent, both published bounds of the modelled range, and sizes
    // that split the set evenly and unevenly.
    let rows: [(&str, Option<i64>); 6] = [
        ("no MaxResults, the default page size", None),
        (
            "MaxResults at the published minimum",
            Some(LIST_STORES_MAX_RESULTS_MIN),
        ),
        ("MaxResults of 2, an uneven split", Some(2)),
        ("MaxResults of 3, an uneven split", Some(3)),
        (
            "MaxResults of exactly the number held",
            Some(expected.len() as i64),
        ),
        (
            "MaxResults at the published maximum",
            Some(LIST_STORES_MAX_RESULTS_MAX),
        ),
    ];
    for (case, max_results) in rows {
        let listed = walk_identity_stores(&router, max_results, case).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected every store once, in one order, got {listed:?}"
        );
    }
}

/// A page size outside the modelled range, and a `NextToken` this listing never issued,
/// are refused with the modelled `ValidationException` — while a cursor it DID issue
/// resumes the walk.
#[tokio::test]
async fn list_identity_stores_refuses_a_page_it_cannot_answer() {
    let router = emulator();
    let created = create_group(
        &router,
        "d-00000000e7",
        "group-for-token-rows",
        "refusal setup",
    )
    .await;
    let group_id = string(&created, "GroupId", "refusal setup");

    // The modelled range is 1..100: one past each published bound is refused…
    for (case, max_results) in [
        (
            "MaxResults one below the published minimum",
            LIST_STORES_MAX_RESULTS_MIN - 1,
        ),
        (
            "MaxResults one above the published maximum",
            LIST_STORES_MAX_RESULTS_MAX + 1,
        ),
    ] {
        refused(
            &router,
            "ListIdentityStores",
            json!({ "MaxResults": max_results }),
            400,
            "ValidationException",
            case,
        )
        .await;
    }
    // … and both bounds themselves are served.
    for (case, max_results) in [
        (
            "MaxResults at the published minimum",
            LIST_STORES_MAX_RESULTS_MIN,
        ),
        (
            "MaxResults at the published maximum",
            LIST_STORES_MAX_RESULTS_MAX,
        ),
    ] {
        let (stores, _) =
            identity_stores_page(&router, json!({ "MaxResults": max_results }), case).await;
        assert!(
            !stores.is_empty(),
            "case {case:?}: expected a served page to report at least one store"
        );
    }

    // A cursor this listing issued resumes the walk at the store it names.
    let (first, token) =
        identity_stores_page(&router, json!({ "MaxResults": 1 }), "the first page").await;
    let first = first
        .first()
        .expect("case \"the first page\": a page of one reports one store")
        .0
        .clone();
    let token = token.expect("case \"the first page\": two stores are held, so a cursor follows");
    let (second, _) = identity_stores_page(
        &router,
        json!({ "MaxResults": 1, "NextToken": token }),
        "the resumed page",
    )
    .await;
    let second = second
        .first()
        .expect("case \"the resumed page\": a page of one reports one store")
        .0
        .clone();
    assert_ne!(
        first, second,
        "case \"the resumed page\": expected the page after {first:?} to start at another \
         store"
    );

    // (case, a NextToken this listing never issued for this walk)
    let rows: [(&str, String); 4] = [
        (
            "a cursor naming a store this emulator holds no record of",
            hex::encode(UNWRITTEN_STORE.as_bytes()),
        ),
        (
            "a cursor of another listing of this service",
            hex::encode(group_id.as_bytes()),
        ),
        (
            "a token this service never minted",
            "not-a-cursor".to_string(),
        ),
        (
            // Inside the modelled pattern and length, so only the handler can refuse it.
            "a token of the right alphabet and nothing else",
            "abcdef".to_string(),
        ),
    ];
    for (case, next_token) in rows {
        refused(
            &router,
            "ListIdentityStores",
            json!({ "NextToken": next_token }),
            400,
            "ValidationException",
            case,
        )
        .await;
    }
}

/// Once Identity Center HAS answered, the listing is the set of its instances' identity
/// stores: a store a caller addressed while that answer was still open is no longer
/// reported.
///
/// The cross-service half — that the set FOLLOWS the instances that service holds, and that
/// every store the listing reports is one the resolution admits, through the assembled
/// router — is `core/router/tests/all/identitystore_identity_store_listing.rs`.
#[tokio::test]
async fn list_identity_stores_reports_the_stores_identity_center_answers() {
    let router = emulator_with_identity_center();

    // While nothing is decided, the store the caller's own environment names is served, so
    // this emulator comes to hold a record of it — and reports it.
    let caller_store = "d-00000000f8";
    create_group(
        &router,
        caller_store,
        "group-in-the-callers-store",
        "an undecided account",
    )
    .await;
    let listed = walk_identity_stores(&router, None, "an undecided account").await;
    assert!(
        listed.contains(&caller_store.to_string()),
        "case \"an undecided account\": expected the store just written to, got {listed:?}"
    );

    // Identity Center's own listing of its instances is where a client reads the identity
    // store it has; asking also settles the account's enablement.
    let instances = identity_center_call(&router, "ListInstances", json!({})).await;
    let mut account_stores: Vec<String> = instances["Instances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|instance| instance["IdentityStoreId"].as_str().map(str::to_string))
        .collect();
    account_stores.sort();
    assert!(
        !account_stores.is_empty(),
        "case \"a decided account\": expected Identity Center to report an instance, \
         got {instances}"
    );

    let listed = walk_identity_stores(&router, None, "a decided account").await;
    assert_eq!(
        listed, account_stores,
        "case \"a decided account\": expected the stores of the account's instances, \
         got {listed:?}"
    );
    assert!(
        !listed.contains(&caller_store.to_string()),
        "case \"a decided account\": expected a store the account does not have to be \
         absent, got {listed:?}"
    );
}
