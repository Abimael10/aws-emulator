// SPDX-License-Identifier: CC0-1.0

//! The ONE rule by which every operation that ACTS ON an Amazon EVS environment
//! resolves the environment its request addresses.
//!
//! The rule under test: an environment is a regional resource that ceases to exist once
//! its `EnvironmentState` reaches `DELETED`, so a request that acts on an environment
//! resolves it only to a record this service holds at the endpoint of the Region the
//! request was signed for whose state has not reached `DELETED` — "A service resource
//! associated with the request could not be found. The resource might not be specified
//! correctly, or it may have a `state` of `DELETED`" (`ResourceNotFoundException` doc,
//! `smithy_query(query="shape",shape="ResourceNotFoundException")`). Any other record
//! is addressed by no request: the operation answers its own not-found refusal and
//! writes nothing. A record that is only on its way out (`DELETING`) is still
//! addressable, which is where the rule draws its line.
//!
//! Every operation of this service that acts on an environment applies that one rule
//! through the state module's `live_environment_in_region(_mut)` /
//! `live_environment_by_arn(_mut)`, so the table below spans ALL of them — the
//! environment-id-addressed ones, the four lists addressed to one environment (which
//! reach the rule through the state module's `listed_environment`) and the three
//! ARN-addressed tagging ones — rather than
//! one. What the rows differ along is the operation and the way it addresses the
//! environment (by `environmentId`, or by `resourceArn`), and what each axis varies is
//! the state of the record (`CREATED`, `DELETING`, `DELETED`) and the Region the request
//! is signed for.
//!
//! `DeleteEnvironment` applies the same rule and is pinned here too, by cases of its own
//! ([`delete_row`]) rather than by a row of that table: it is the operation that MOVES a
//! record through the states the table is judged against.
//!
//! The two readers that deliberately report a finished environment, `GetEnvironment` and
//! `ListEnvironments`, are NOT part of this rule and are pinned by
//! `tests/all/environment_lifecycle.rs`.
//!
//! The harness (`emulator`, `call_in_region`, `settle`) is the one
//! `environment_lifecycle` already owns, so every test drives the service the way a
//! client does: `POST /` with `X-Amz-Target: AmazonElasticVMwareService.<Operation>`.

use serde_json::{json, Value};
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;

use crate::environment_lifecycle::{
    call_in_region, emulator, error_type, settle, Emulator, ACCOUNT, REGION,
};
use crate::hcx_eip_associations::{create_public_hcx, register_elastic_ip};

/// The connector type an entitlement is created through
/// (`smithy_query(query="shape",shape="ConnectorType")`).
const VCENTER: &str = "VCENTER";

/// The only `EntitlementType` the model declares
/// (`smithy_query(query="shape",shape="EntitlementType")`).
const WINDOWS_SERVER: &str = "WINDOWS_SERVER";

/// The appliance the fixture's connector reaches, and the one a repeated create has to
/// name for that create to be the idempotent repeat of the stored record.
const APPLIANCE_FQDN: &str = "vcenter.evs.example.com";

/// The appliance an update moves the connector to — a different name of the same form.
const UPDATED_APPLIANCE_FQDN: &str = "vcenter-2.evs.example.com";

/// The credential secret the fixture's connector is created with, given as a bare name
/// (one of the two forms `secretIdentifier` accepts).
const SECRET_IDENTIFIER: &str = "evs/vcf-appliance-credentials";

/// The virtual machine the fixture's entitlement is created for.
const VM_ID: &str = "vm-0123456789";

/// The host the fixture's environment holds, and the one a delete names.
const HOST_NAME: &str = "esx-1";

/// The host a create adds to the fixture's environment.
const NEW_HOST_NAME: &str = "esx-2";

/// A Region other than [`REGION`], so a request signed for it reaches another endpoint
/// of this service and resolves no record this one holds.
const OTHER_REGION: &str = "eu-central-1";

/// How one operation reports an environment its request cannot address — the three
/// not-found refusals this service's operations declare for that case.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The `ResourceNotFoundException` an `environmentId`-addressed operation answers,
    /// naming the environment the request asked for.
    EnvironmentNotFound,
    /// The `ValidationException` `CreateEnvironmentHost` answers, that operation
    /// declaring no `ResourceNotFoundException`
    /// (`smithy_query(query="operation",operation="CreateEnvironmentHost")`).
    EnvironmentNotFoundValidation,
    /// The tagging surface's own refusal: the fixed resource-agnostic message with the
    /// request's `resourceArn` echoed (`tests/all/tagging_not_found.rs` owns its body).
    TaggingResourceNotFound,
}

/// One operation that acts on an environment, with a request that asks it about the
/// fixture's environment and the refusal it answers when that environment resolves to
/// nothing.
struct Row {
    /// The operation name on the wire.
    operation: &'static str,
    /// The request body.
    request: Value,
    /// The not-found refusal this operation declares.
    refusal: Refusal,
}

/// An environment with one of everything the operations under test act on: a tag, a
/// vCenter Server connector, an entitlement through it, an Elastic IP association on its
/// public HCX VLAN, a spare allocation to associate, and (when `with_host`) one ESX
/// host.
///
/// `with_host` is false for a fixture that is then DELETED: "Amazon EVS environments
/// will only be enabled for deletion once the hosts are deleted"
/// (`DeleteEnvironment` doc).
struct Fixture {
    /// The `environmentId` every id-addressed row carries.
    environment: String,
    /// The `resourceArn` every ARN-addressed row carries.
    arn: String,
    /// The connector every connector-addressed row carries.
    connector: String,
    /// The association `DisassociateEipFromVlan` removes.
    association: String,
    /// The allocation `AssociateEipToVlan` associates, held by the account and used by
    /// no VLAN yet.
    free_allocation: String,
}

/// Build the fixture at [`REGION`]'s endpoint. `allocations` are the two Elastic IP
/// allocation ids this fixture owns — one associated here, one left free — and must
/// differ per fixture, because one allocation carries one association at a time.
async fn fixture(
    router: &Emulator,
    name: &str,
    allocations: (&str, &str),
    with_host: bool,
) -> Fixture {
    let environment = create_public_hcx(router, name).await;
    let arn = format!("arn:aws:evs:{REGION}:{ACCOUNT}:environment/{environment}");

    let (status, body) = call_in_region(
        router,
        REGION,
        "TagResource",
        &json!({ "resourceArn": arn, "tags": { "owner": "platform" } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture {name:?}: tagging expected 200, got {status} ({}) {body}",
        error_type(&body)
    );

    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateEnvironmentConnector",
        &json!({
            "environmentId": environment,
            "type": VCENTER,
            "applianceFqdn": APPLIANCE_FQDN,
            "secretIdentifier": SECRET_IDENTIFIER,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture {name:?}: the connector create expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let connector = body["connector"]["connectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name:?}: no connectorId in {body}"))
        .to_string();
    settle(router);

    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateEntitlement",
        &json!({
            "environmentId": environment,
            "connectorId": connector,
            "entitlementType": WINDOWS_SERVER,
            "vmIds": [VM_ID],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture {name:?}: the entitlement create expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    settle(router);

    let (held, free) = allocations;
    register_elastic_ip(&router.engine, held, Some("198.51.100.10"));
    register_elastic_ip(&router.engine, free, Some("198.51.100.11"));
    let (status, body) = call_in_region(
        router,
        REGION,
        "AssociateEipToVlan",
        &json!({
            "environmentId": environment,
            "vlanName": "hcx",
            "allocationId": held,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture {name:?}: the association expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let association = body["vlan"]["eipAssociations"]
        .as_array()
        .and_then(|associations| associations.last())
        .and_then(|association| association["associationId"].as_str())
        .unwrap_or_else(|| panic!("fixture {name:?}: no associationId in {body}"))
        .to_string();

    if with_host {
        let (status, body) = call_in_region(
            router,
            REGION,
            "CreateEnvironmentHost",
            &json!({
                "environmentId": environment,
                "host": {
                    "hostName": HOST_NAME,
                    "keyName": "evs-key",
                    "instanceType": "i4i.metal",
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "fixture {name:?}: the host create expected 200, got {status} ({}) {body}",
            error_type(&body)
        );
        settle(router);
    }

    Fixture {
        environment,
        arn,
        connector,
        association,
        free_allocation: free.to_string(),
    }
}

/// Every operation of this service that acts on an environment, each asking about
/// `fixture`'s environment.
///
/// The order is the one a run against a LIVE environment needs: an operation whose
/// precondition another row satisfies comes after it (an entitlement is removed before
/// the connector it was created through is deleted, "Before deleting a connector, you
/// must remove all entitlements that are associated with the same vCenter",
/// `DeleteEnvironmentConnector` doc). Against an environment no request can address,
/// every row is refused before it looks at anything else, so the order does not matter
/// there.
fn rows(fixture: &Fixture) -> Vec<Row> {
    let Fixture {
        environment,
        arn,
        connector,
        association,
        free_allocation,
    } = fixture;
    vec![
        Row {
            operation: "GetDepotUrl",
            request: json!({ "environmentId": environment }),
            refusal: Refusal::EnvironmentNotFound,
        },
        // The four lists addressed to ONE environment are part of this rule too: a list
        // acts on the environment it names, so it resolves the same records its siblings
        // do. The state each one is refused in beyond this rule — the environment still
        // initializing — is `tests/all/list_while_initializing.rs`.
        Row {
            operation: "ListEnvironmentVlans",
            request: json!({ "environmentId": environment }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "ListEnvironmentHosts",
            request: json!({ "environmentId": environment }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "ListEnvironmentConnectors",
            request: json!({ "environmentId": environment }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "ListVmEntitlements",
            request: json!({
                "environmentId": environment,
                "connectorId": connector,
                "entitlementType": WINDOWS_SERVER,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "ListTagsForResource",
            request: json!({ "resourceArn": arn }),
            refusal: Refusal::TaggingResourceNotFound,
        },
        Row {
            operation: "TagResource",
            request: json!({ "resourceArn": arn, "tags": { "team": "vcf" } }),
            refusal: Refusal::TaggingResourceNotFound,
        },
        Row {
            operation: "UntagResource",
            request: json!({ "resourceArn": arn, "tagKeys": ["owner"] }),
            refusal: Refusal::TaggingResourceNotFound,
        },
        Row {
            operation: "CreateEnvironmentConnector",
            request: json!({
                "environmentId": environment,
                "type": VCENTER,
                "applianceFqdn": APPLIANCE_FQDN,
                "secretIdentifier": SECRET_IDENTIFIER,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "UpdateEnvironmentConnector",
            request: json!({
                "environmentId": environment,
                "connectorId": connector,
                "applianceFqdn": UPDATED_APPLIANCE_FQDN,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "CreateEntitlement",
            request: json!({
                "environmentId": environment,
                "connectorId": connector,
                "entitlementType": WINDOWS_SERVER,
                "vmIds": [VM_ID],
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "DeleteEntitlement",
            request: json!({
                "environmentId": environment,
                "connectorId": connector,
                "entitlementType": WINDOWS_SERVER,
                "vmIds": [VM_ID],
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "DeleteEnvironmentConnector",
            request: json!({
                "environmentId": environment,
                "connectorId": connector,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "AssociateEipToVlan",
            request: json!({
                "environmentId": environment,
                "vlanName": "hcx",
                "allocationId": free_allocation,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "DisassociateEipFromVlan",
            request: json!({
                "environmentId": environment,
                "vlanName": "hcx",
                "associationId": association,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
        Row {
            operation: "CreateEnvironmentHost",
            request: json!({
                "environmentId": environment,
                "host": {
                    "hostName": NEW_HOST_NAME,
                    "keyName": "evs-key",
                    "instanceType": "i4i.metal",
                },
            }),
            refusal: Refusal::EnvironmentNotFoundValidation,
        },
        Row {
            operation: "DeleteEnvironmentHost",
            request: json!({
                "environmentId": environment,
                "hostName": HOST_NAME,
            }),
            refusal: Refusal::EnvironmentNotFound,
        },
    ]
}

/// The `__type` of an error body, without its namespace.
fn error_code(body: &Value) -> String {
    error_type(body)
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// A member of a body, as a string, for a comparison or an assertion message.
fn member(body: &Value, name: &str) -> String {
    body[name].as_str().unwrap_or("<absent>").to_string()
}

/// Whether `body` is the refusal `row` answers for an environment its request cannot
/// address — the whole refusal, message and members included, so another not-found of
/// the same operation (a connector, a host, an allocation) is not mistaken for it.
fn is_addressing_refusal(row: &Row, body: &Value, fixture: &Fixture) -> bool {
    match row.refusal {
        Refusal::EnvironmentNotFound => {
            error_code(body) == "ResourceNotFoundException"
                && member(body, "message")
                    == format!("Environment {} was not found", fixture.environment)
                && member(body, "resourceId") == fixture.environment
                && member(body, "resourceType") == "environment"
        }
        Refusal::EnvironmentNotFoundValidation => {
            error_code(body) == "ValidationException"
                && member(body, "message") == format!("{} was not found", fixture.environment)
        }
        Refusal::TaggingResourceNotFound => {
            error_code(body) == "ResourceNotFoundException"
                && member(body, "message")
                    == evs::scaffolded::TAGGING_RESOURCE_NOT_FOUND_MESSAGE
                && member(body, "resourceArn") == fixture.arn
        }
    }
}

/// Assert that `row`, sent to `region`'s endpoint, is answered with the refusal its
/// operation declares for an environment no request can address.
async fn assert_refused(router: &Emulator, case: &str, region: &str, row: &Row, fixture: &Fixture) {
    let (status, body) = call_in_region(router, region, row.operation, &row.request).await;
    let case = format!("{case}: {}", row.operation);
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    assert!(
        is_addressing_refusal(row, &body, fixture),
        "case {case:?}: expected the {:?} refusal naming {}, got {body}",
        row.refusal,
        fixture.environment
    );
}

#[tokio::test]
async fn every_operation_that_acts_on_an_environment_serves_a_live_one() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-live",
        ("eipalloc-live1", "eipalloc-live2"),
        true,
    )
    .await;

    // Each row in turn against the one live environment: the record resolves, so the
    // operation does its own work and answers 200. The transitions each call starts are
    // settled after it, so the next row judges a store the clock is caught up with.
    for row in rows(&fixture) {
        let (status, body) = call_in_region(&router, REGION, row.operation, &row.request).await;
        assert_eq!(
            status, 200,
            "case a CREATED environment: {}: expected HTTP 200, got {status} ({}) {body}",
            row.operation,
            error_type(&body)
        );
        settle(&router);
    }
}

#[tokio::test]
async fn no_operation_that_acts_on_an_environment_addresses_one_that_reached_deleted() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-deleted",
        ("eipalloc-gone1", "eipalloc-gone2"),
        false,
    )
    .await;

    let (status, body) = call_in_region(
        &router,
        REGION,
        "DeleteEnvironment",
        &json!({ "environmentId": fixture.environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the delete: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    // Once the deletion the clock owes comes due the record is DELETED, and the
    // environment it stood for is gone for every request that would act on it.
    settle(&router);

    for row in rows(&fixture) {
        assert_refused(&router, "a DELETED environment", REGION, &row, &fixture).await;
    }

    // The record is still THERE — `GetEnvironment` reports it — so each refusal above is
    // the addressing rule's answer and not an empty store.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "GetEnvironment",
        &json!({ "environmentId": fixture.environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the deleted record: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    assert_eq!(
        body["environment"]["environmentState"],
        json!("DELETED"),
        "case the deleted record: expected DELETED, got {}",
        body["environment"]["environmentState"]
    );
}

#[tokio::test]
async fn no_operation_that_acts_on_an_environment_addresses_one_of_another_region() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-region",
        ("eipalloc-far1", "eipalloc-far2"),
        true,
    )
    .await;

    // The same live environment, asked about at another Region's endpoint: the Region
    // half of the one addressing rule, which every row applies too.
    for row in rows(&fixture) {
        assert_refused(
            &router,
            "another Region's endpoint",
            OTHER_REGION,
            &row,
            &fixture,
        )
        .await;
    }
}

/// The `DeleteEnvironment` request that asks about `fixture`'s environment, and the
/// refusal it answers for an environment no request can address.
///
/// `DeleteEnvironment` acts on an environment and applies the one rule above, but it
/// cannot be a row of [`rows`]: it is the operation that MOVES a record through the
/// states the other rows are judged against, so a run of the table would take the
/// environment the next row addresses away with it. Its cases vary the same two axes the
/// table's tests do — the state the record has reached and the Region the request is
/// signed for.
fn delete_row(fixture: &Fixture) -> Row {
    Row {
        operation: "DeleteEnvironment",
        request: json!({ "environmentId": fixture.environment }),
        refusal: Refusal::EnvironmentNotFound,
    }
}

/// The state `GetEnvironment` reports for `fixture`'s environment, with its `modifiedAt`
/// stamp — the reader that is outside the rule, so it answers for the record itself and
/// not for what addresses it.
async fn stored_record(router: &Emulator, case: &str, fixture: &Fixture) -> (String, f64) {
    let (status, body) = call_in_region(
        router,
        REGION,
        "GetEnvironment",
        &json!({ "environmentId": fixture.environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetEnvironment expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let state = member(&body["environment"], "environmentState");
    let modified_at = body["environment"]["modifiedAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: no modifiedAt in {body}"));
    (state, modified_at)
}

#[tokio::test]
async fn delete_environment_addresses_a_record_until_it_has_reached_deleted() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-delete-states",
        ("eipalloc-del1", "eipalloc-del2"),
        false,
    )
    .await;
    let row = delete_row(&fixture);

    // CREATED: the record resolves, so the delete does its own work and accepts the
    // deletion.
    let (status, body) = call_in_region(&router, REGION, row.operation, &row.request).await;
    assert_eq!(
        status, 200,
        "case a CREATED environment: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    assert_eq!(
        body["environment"]["environmentState"],
        json!("DELETING"),
        "case a CREATED environment: expected DELETING, got {}",
        body["environment"]["environmentState"]
    );
    let deleting_modified_at = body["environment"]["modifiedAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case a CREATED environment: no modifiedAt in {body}"));

    // DELETING: the record is on its way out but still addressed, which is where the
    // rule draws its line — the `@idempotent` repeat reports it as it stands rather than
    // refusing it.
    let (status, body) = call_in_region(&router, REGION, row.operation, &row.request).await;
    assert_eq!(
        status, 200,
        "case a DELETING environment: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    assert_eq!(
        body["environment"]["environmentState"],
        json!("DELETING"),
        "case a DELETING environment: expected DELETING, got {}",
        body["environment"]["environmentState"]
    );
    assert_eq!(
        body["environment"]["modifiedAt"].as_f64(),
        Some(deleting_modified_at),
        "case a DELETING environment: the repeat must not move modifiedAt from \
         {deleting_modified_at}, got {}",
        body["environment"]["modifiedAt"]
    );

    // DELETED: once the transition the clock owes comes due the environment is gone, so
    // no request that acts on it addresses it — this one no more than its siblings.
    settle(&router);
    let (settled_state, settled_modified_at) =
        stored_record(&router, "a DELETED environment", &fixture).await;
    assert_eq!(
        settled_state, "DELETED",
        "case a DELETED environment: expected the record to have settled on DELETED, got \
         {settled_state:?}"
    );
    assert_refused(&router, "a DELETED environment", REGION, &row, &fixture).await;

    // And the refused delete wrote nothing: the record still stands exactly as the
    // settled transition left it.
    let (state, modified_at) =
        stored_record(&router, "a DELETED environment, afterwards", &fixture).await;
    assert_eq!(
        state, settled_state,
        "case a DELETED environment, afterwards: the refused delete must leave the record \
         in {settled_state:?}, got {state:?}"
    );
    assert_eq!(
        modified_at, settled_modified_at,
        "case a DELETED environment, afterwards: the refused delete must not move \
         modifiedAt from {settled_modified_at}, got {modified_at}"
    );
}

#[tokio::test]
async fn delete_environment_addresses_no_record_of_another_region() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-delete-region",
        ("eipalloc-delfar1", "eipalloc-delfar2"),
        false,
    )
    .await;
    let row = delete_row(&fixture);

    // The Region half of the same rule: a live record asked about at another endpoint of
    // this service resolves to nothing there, and nothing about it changes.
    assert_refused(
        &router,
        "another Region's endpoint, a CREATED environment",
        OTHER_REGION,
        &row,
        &fixture,
    )
    .await;
    let (state, _) = stored_record(
        &router,
        "another Region's endpoint, a CREATED environment",
        &fixture,
    )
    .await;
    assert_eq!(
        state, "CREATED",
        "case another Region's endpoint, a CREATED environment: the refused delete must \
         leave the record in \"CREATED\", got {state:?}"
    );

    // The same at each state the record goes through: the deletion is accepted at the
    // endpoint the environment lives at, and the other endpoint addresses neither the
    // DELETING record nor the DELETED one.
    let (status, body) = call_in_region(&router, REGION, row.operation, &row.request).await;
    assert_eq!(
        status, 200,
        "case the delete at its own endpoint: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    // Each pass: the state the record has reached by then, and the case naming it. What
    // the record is is read at its own endpoint, so each pass is judged against the
    // state it says it is about.
    for (expected_state, case) in [
        ("DELETING", "another Region's endpoint, a DELETING environment"),
        ("DELETED", "another Region's endpoint, a DELETED environment"),
    ] {
        let (state, _) = stored_record(&router, case, &fixture).await;
        assert_eq!(
            state, expected_state,
            "case {case:?}: expected the record to be {expected_state:?}, got {state:?}"
        );
        assert_refused(&router, case, OTHER_REGION, &row, &fixture).await;
        // Which settles the deletion, so the next pass judges the DELETED record.
        settle(&router);
    }
}

#[tokio::test]
async fn an_environment_whose_deletion_has_only_started_is_still_addressed() {
    let router = emulator();
    let fixture = fixture(
        &router,
        "addressing-deleting",
        ("eipalloc-going1", "eipalloc-going2"),
        false,
    )
    .await;

    let (status, body) = call_in_region(
        &router,
        REGION,
        "DeleteEnvironment",
        &json!({ "environmentId": fixture.environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the delete: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    assert_eq!(
        body["environment"]["environmentState"],
        json!("DELETING"),
        "case the delete: expected DELETING, got {}",
        body["environment"]["environmentState"]
    );

    // Not settled: the deletion is accepted but not yet due, so the record is DELETING —
    // the state where the rule still resolves it. Each operation then answers its own
    // way (serving the request, or refusing it for a reason of its own, a resource the
    // deletion took with it included), but none of them answers the refusal it keeps for
    // an environment it cannot address.
    for row in rows(&fixture) {
        let (status, body) = call_in_region(&router, REGION, row.operation, &row.request).await;
        assert!(
            !is_addressing_refusal(&row, &body, &fixture),
            "case a DELETING environment: {}: the record is still addressable, got \
             {status} {body}",
            row.operation
        );
    }
}
