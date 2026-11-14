// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what a DRS request gets on an account nobody has
//! called `InitializeService` for, and for the two refusals that were hidden behind
//! that call.
//!
//! The harness (router, `call`, `create_body`) is the one
//! `replication_configuration_template_lifecycle` builds: both suites are modules of
//! the same test target, so there is one assembly of the service, not two.
//!
//! The rules under test, each asserted over a TABLE of inputs:
//!
//! 1. The emulated account is onboarded to the service, so NO operation refuses a
//!    request as an uninitialized account: a fresh emulator serves every operation on
//!    its own terms, with `InitializeService` never called.
//! 2. A create that leaves out a member a replication template cannot be created
//!    without is a field-validation failure (400 `ValidationException`, reason
//!    `FIELD_VALIDATION_FAILED`) whose message names exactly the absent members, in
//!    alphabetical order — and stores nothing.
//! 3. An id-keyed template operation — one that acts on that one template — on an id
//!    the store does not hold answers 404 `ResourceNotFoundException` carrying the
//!    service's name for the resource type, the id the request named verbatim, and a
//!    message built from the two.

use serde_json::{json, Value};

use super::replication_configuration_template_lifecycle::{
    call, create, create_body, drs, initialize, items, Answer,
};

/// The create-input members the service requires that the model marks optional.
///
/// Taken from the handler's own list
/// (`ops::create_replication_configuration_template::SERVICE_REQUIRED_MEMBERS`), so
/// the table below cannot drift from the rule it tests. The four members it leaves out
/// (`autoReplicateNewDisks`, `ebsEncryptionKeyArn`, `internetProtocol`, `tags`) are
/// genuinely optional, which the last row of
/// [`missing_required_create_members_are_a_field_validation_failure`] pins.
const SERVICE_REQUIRED_OPTIONAL_MEMBERS: [&str; 6] =
    service_com_amazonaws_drs_elasticdisasterrecoveryservice::ops::create_replication_configuration_template::SERVICE_REQUIRED_MEMBERS;

/// A valid create body with `omitted` removed from it.
fn create_body_without(omitted: &[&str]) -> Value {
    let mut body = create_body("subnet-0123456789abcdef0", json!({}));
    let object = body.as_object_mut().expect("an object body");
    for member in omitted {
        object.remove(*member);
    }
    body
}

/// The message AWS words a field-validation failure with: the fixed text, then the
/// absent member names in alphabetical order.
fn missing_members_message(omitted: &[&str]) -> String {
    let mut names: Vec<&str> = omitted.to_vec();
    names.sort_unstable();
    format!("Missing required parameters in input: {}", names.join(", "))
}

/// The `message` member of an answer, as a string.
fn message(answer: &Answer) -> String {
    answer
        .member("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a message member, got {answer:?}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// Rule 1 — an account nobody initialized is served
// ---------------------------------------------------------------------------

/// Every operation this slice implements is served on a FRESH emulator, with
/// `InitializeService` never called: none answers `UninitializedAccountException`,
/// and each answers on its own terms (a create 201, a read 200, an id-keyed mutation
/// on an id nothing minted 404). The rows span every operation that modelled that
/// refusal, with request shapes that differ (no members, a full create, id-only, a
/// filtered read).
#[tokio::test]
async fn no_operation_refuses_an_account_that_was_never_initialized() {
    let absent_id = "rct-00000000000000000";
    let cases: Vec<(&str, Value, u16)> = vec![
        ("DescribeReplicationConfigurationTemplates", json!({}), 200),
        (
            "CreateReplicationConfigurationTemplate",
            create_body("subnet-0123456789abcdef0", json!({})),
            201,
        ),
        (
            "UpdateReplicationConfigurationTemplate",
            json!({"replicationConfigurationTemplateID": absent_id, "bandwidthThrottling": 10}),
            404,
        ),
        (
            "DeleteReplicationConfigurationTemplate",
            json!({"replicationConfigurationTemplateID": absent_id}),
            404,
        ),
        ("DescribeSourceServers", json!({}), 200),
        (
            "DescribeSourceServers",
            json!({"filters": {"sourceServerIDs": ["s-0123456789abcdef0"]}}),
            200,
        ),
    ];

    let router = drs();
    for (operation, body, expected) in &cases {
        let answer = call(&router, operation, body).await;
        assert_eq!(
            answer.status, *expected,
            "case {operation} {body}: expected {expected} without InitializeService, got {answer:?}"
        );
        assert_ne!(
            answer.error_code.as_deref(),
            Some("UninitializedAccountException"),
            "case {operation} {body}: refused as an uninitialized account: {answer:?}"
        );
    }
}

/// The read-only inventory call distinguishes "nothing registered" from "refused" on
/// a fresh account: 200 with `items` PRESENT and empty, never an absent member, and
/// no `nextToken`.
#[tokio::test]
async fn describe_source_servers_serves_an_empty_inventory_without_initialization() {
    let cases: Vec<(&str, Value)> = vec![
        ("an empty body", json!({})),
        ("an empty filters structure", json!({"filters": {}})),
        (
            "a filter on an id nothing registered",
            json!({"filters": {"sourceServerIDs": ["s-0123456789abcdef0"]}}),
        ),
        ("a page size", json!({"maxResults": 1})),
    ];

    let router = drs();
    for (case, body) in &cases {
        let answer = call(&router, "DescribeSourceServers", body).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {answer:?}"
        );
        assert_eq!(
            answer.member("items"),
            Some(&json!([])),
            "case {case}: expected items present and empty, got {answer:?}"
        );
        assert!(
            answer.member("nextToken").is_none(),
            "case {case}: a complete page carries no nextToken: {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — a create missing required members is a field-validation failure
// ---------------------------------------------------------------------------

/// A create that omits members the service requires is refused with 400
/// `ValidationException`, reason `FIELD_VALIDATION_FAILED`, and a message naming
/// exactly the absent members in alphabetical order. The rows are each required
/// member on its own, a pair, the whole set, a set whose alphabetical order differs
/// from the order the members are listed in, and — as the boundary — a body that
/// omits only the genuinely optional members, which must be CREATED.
#[tokio::test]
async fn missing_required_create_members_are_a_field_validation_failure() {
    let mut cases: Vec<Vec<&str>> = SERVICE_REQUIRED_OPTIONAL_MEMBERS
        .iter()
        .map(|member| vec![*member])
        .collect();
    cases.push(vec!["createPublicIP", "associateDefaultSecurityGroup"]);
    cases.push(vec![
        "useDedicatedReplicationServer",
        "dataPlaneRouting",
        "replicationServerInstanceType",
    ]);
    cases.push(SERVICE_REQUIRED_OPTIONAL_MEMBERS.to_vec());

    let router = drs();
    for omitted in &cases {
        let body = create_body_without(omitted);
        let answer = call(&router, "CreateReplicationConfigurationTemplate", &body).await;
        assert_eq!(
            answer.status, 400,
            "case omitting {omitted:?}: expected 400, got {answer:?}"
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some("ValidationException"),
            "case omitting {omitted:?}: expected ValidationException, got {answer:?}"
        );
        assert_eq!(
            message(&answer),
            missing_members_message(omitted),
            "case omitting {omitted:?}: wrong message in {answer:?}"
        );
        assert_eq!(
            answer.member("reason").and_then(Value::as_str),
            Some("FIELD_VALIDATION_FAILED"),
            "case omitting {omitted:?}: wrong reason in {answer:?}"
        );
    }

    // The boundary: a body that carries every required member and NONE of the
    // optional ones is created, so the rule refuses only what it must.
    let optional = ["autoReplicateNewDisks", "ebsEncryptionKeyArn", "internetProtocol", "tags"];
    let answer = call(
        &router,
        "CreateReplicationConfigurationTemplate",
        &create_body_without(&optional),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case omitting only {optional:?}: expected 201, got {answer:?}"
    );

    // Every refusal stored nothing: the one create that succeeded is the only
    // template in the store.
    let listed = call(
        &router,
        "DescribeReplicationConfigurationTemplates",
        &json!({}),
    )
    .await;
    assert_eq!(
        items(&listed).len(),
        1,
        "a refused create must store nothing: {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 3 — how a missing template is reported
// ---------------------------------------------------------------------------

/// Every template operation that acts on ONE named template reports an id the store
/// does not hold the same way: 404, `resourceType` the service's name for the
/// resource, `resourceId` the id the request named verbatim, and the message built
/// from both. The rows cross the two such operations with ids that are absent for
/// different reasons (never minted, deleted, a different id shape inside the modelled
/// pattern).
#[tokio::test]
async fn an_unknown_template_id_reports_the_resource_and_the_id() {
    let router = drs();
    // A template exists, so "not found" is about the id and not about an empty store.
    let present = create(&router, &create_body("subnet-0123456789abcdef0", json!({}))).await;
    let deleted = create(&router, &create_body("subnet-00112233445566778", json!({}))).await;
    let gone = call(
        &router,
        "DeleteReplicationConfigurationTemplate",
        &json!({"replicationConfigurationTemplateID": deleted}),
    )
    .await;
    assert_eq!(gone.status, 204, "delete: expected 204, got {gone:?}");

    // Each id matches the modelled `^rct-[0-9a-zA-Z]{17}$`.
    let ids = [
        ("never minted, all digits", "rct-00000000000000000".to_string()),
        ("never minted, mixed case", "rct-aBcDeFgHiJkLmNoP0".to_string()),
        ("deleted", deleted.clone()),
    ];

    for (case, id) in &ids {
        for operation in [
            "DeleteReplicationConfigurationTemplate",
            "UpdateReplicationConfigurationTemplate",
        ] {
            let body = json!({"replicationConfigurationTemplateID": id});
            let answer = call(&router, operation, &body).await;
            assert_eq!(
                answer.status, 404,
                "case {operation} / {case}: expected 404, got {answer:?}"
            );
            assert_eq!(
                answer.error_code.as_deref(),
                Some("ResourceNotFoundException"),
                "case {operation} / {case}: expected ResourceNotFoundException, got {answer:?}"
            );
            assert_eq!(
                answer.member("resourceType").and_then(Value::as_str),
                Some("Replication Configuration Template"),
                "case {operation} / {case}: wrong resourceType in {answer:?}"
            );
            assert_eq!(
                answer.member("resourceId").and_then(Value::as_str),
                Some(id.as_str()),
                "case {operation} / {case}: resourceId must be the requested id, got {answer:?}"
            );
            assert_eq!(
                message(&answer),
                format!("Replication Configuration Template with Id: '{id}' was not found"),
                "case {operation} / {case}: wrong message in {answer:?}"
            );
        }
    }

    // The template that WAS minted is still readable: the refusals touched nothing.
    let described = call(
        &router,
        "DescribeReplicationConfigurationTemplates",
        &json!({"replicationConfigurationTemplateIDs": [present]}),
    )
    .await;
    assert_eq!(
        items(&described).len(),
        1,
        "the surviving template must still be readable: {described:?}"
    );

    // And none of this depends on the account having been initialized: the same
    // refusal is what an initialized account gets.
    initialize(&router).await;
    let after = call(
        &router,
        "DeleteReplicationConfigurationTemplate",
        &json!({"replicationConfigurationTemplateID": "rct-00000000000000000"}),
    )
    .await;
    assert_eq!(
        after.status, 404,
        "after InitializeService: expected the same 404, got {after:?}"
    );
    assert_eq!(
        message(&after),
        "Replication Configuration Template with Id: 'rct-00000000000000000' was not found",
        "after InitializeService: wrong message in {after:?}"
    );
}
