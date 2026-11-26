// SPDX-License-Identifier: CC0-1.0

//! A list addressed to one Amazon EVS environment that has reached `DELETED`.
//!
//! The rule under test, one level above the example: a request that acts on an
//! environment — a LIST of that environment's resources no less than a call that changes
//! it — addresses only a record this service holds at the endpoint of the Region the
//! request was signed for whose `EnvironmentState` has not reached `DELETED`, and
//! anything else is reported absent: "A service resource associated with the request
//! could not be found. The resource might not be specified correctly, or it may have a
//! `state` of `DELETED`" (`ResourceNotFoundException` doc, `smithy_query(query="shape",
//! shape="ResourceNotFoundException")`), which every one of the four lists declares
//! (`smithy_query(query="operation",
//! operation="ListEnvironmentVlans,ListEnvironmentHosts,ListEnvironmentConnectors,ListVmEntitlements")`).
//! So one deleted environment cannot be gone for one read of it and present for another:
//! the only readers outside the rule are the two that report a record for its own sake,
//! `GetEnvironment` and `ListEnvironments`.
//!
//! The rows differ along what the rule generalises over — which of the four lists is
//! asked (every one of them), what the deleted environment RETAINED to be reported
//! (nothing but its VLANs, one connector with entitlements through it, several
//! connectors), how the record reached `DELETED` (the deletion the clock made due, and a
//! record put straight into that state), and the endpoint the list is sent to.
//!
//! Why the page a `DELETED` environment used to answer was incoherent as well:
//! `DeleteEnvironment` RELEASES the environment's ten VLAN subnets from the account's own
//! Amazon EC2 store on the call that accepts the deletion ("Environment deletion also
//! deletes the associated Amazon EVS VLAN subnets", `DeleteEnvironment` doc), so a page
//! of `Vlan$subnetId`s served afterwards names subnets no other service holds —
//! [`no_list_reports_a_vlan_subnet_the_deletion_released`] pins both halves against the
//! shared store.
//!
//! The refusal of a read is also all it does: a repeat answers the same, and the record
//! behind it stands exactly as the settled deletion left it.
//!
//! The harness (`emulator`, `call`, `call_in_region`, `create_settled`, `settle`) is the
//! one `environment_lifecycle` already owns, so every test drives the service the way a
//! client does: `POST /` with `X-Amz-Target: AmazonElasticVMwareService.<Operation>`.

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;

use crate::entitlement_connector::connector_in;
use crate::environment_lifecycle::{
    call, call_in_region, create_settled, emulator, error_type, settle, Emulator, ACCOUNT, REGION,
};
use crate::list_while_initializing::force_state;

/// The connector type an entitlement is created through
/// (`smithy_query(query="shape", shape="ConnectorType")`).
const VCENTER: &str = "VCENTER";

/// A second modelled connector type, so a row can retain more than one connector record.
const SDDC_MANAGER: &str = "SDDC_MANAGER";

/// The only `EntitlementType` the model declares
/// (`smithy_query(query="shape", shape="EntitlementType")`).
const WINDOWS_SERVER: &str = "WINDOWS_SERVER";

/// A well-formed `connectorId` no connector was ever minted at: it satisfies the modelled
/// `^(cnctr-[a-zA-Z0-9]{10})$`, which is all a row needs where the ENVIRONMENT is decided
/// before the connector handle is resolved.
const UNKNOWN_CONNECTOR_ID: &str = "cnctr-0123456789";

/// The `resourceType` this service's not-found refusal names for an environment.
const ENVIRONMENT_RESOURCE_TYPE: &str = "environment";

/// A Region other than [`REGION`], so a request signed for it reaches another endpoint of
/// this service.
const OTHER_REGION: &str = "eu-central-1";

/// The number of VLAN subnets an environment is built with: `InitialVlans` declares ten
/// members.
const VLAN_COUNT: usize = 10;

/// Every LIST operation this service binds to ONE environment resource, with a request
/// that addresses `environment` through the connector handle `connector_id`.
///
/// The four operations whose input is an `environmentId` and whose output is a page of
/// that environment's resources (`smithy_query(query="digest")`).
fn list_requests(environment: &str, connector_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("ListEnvironmentVlans", json!({ "environmentId": environment })),
        ("ListEnvironmentHosts", json!({ "environmentId": environment })),
        (
            "ListEnvironmentConnectors",
            json!({ "environmentId": environment }),
        ),
        (
            "ListVmEntitlements",
            json!({
                "environmentId": environment,
                "connectorId": connector_id,
                "entitlementType": WINDOWS_SERVER,
            }),
        ),
    ]
}

/// Assert that `body` is this service's answer for an environment the request addresses
/// no record of: the whole refusal, so another not-found of the same operation (a
/// connector's, say) is not mistaken for it.
fn assert_environment_not_found(case: &str, status: u16, body: &Value, environment_id: &str) {
    // Every modeled CLIENT error of this awsJson1_0 service goes on the wire as a 400 —
    // see `tests/all/error_wire_status.rs`, which owns that rule.
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    assert_eq!(
        error_type(body).rsplit(['#', '/']).next().unwrap_or(""),
        "ResourceNotFoundException",
        "case {case:?}: expected a ResourceNotFoundException, got {}",
        error_type(body)
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        format!("Environment {environment_id} was not found"),
        "case {case:?}: the refusal must name the environment the request asked for, got {body}"
    );
    assert_eq!(
        body["resourceId"].as_str().unwrap_or_default(),
        environment_id,
        "case {case:?}: resourceId must echo the environment the request named, got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str().unwrap_or_default(),
        ENVIRONMENT_RESOURCE_TYPE,
        "case {case:?}: unexpected resourceType, got {body}"
    );
}

/// The `environmentState` `GetEnvironment` reports, with its `modifiedAt` — the reader
/// that is outside the rule, so it answers for the record itself however far it has got.
async fn stored_record(router: &Emulator, case: &str, environment_id: &str) -> (String, f64) {
    let (status, body) = call_in_region(
        router,
        REGION,
        "GetEnvironment",
        &json!({ "environmentId": environment_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetEnvironment expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let state = body["environment"]["environmentState"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no environmentState in {body}"))
        .to_string();
    let modified_at = body["environment"]["modifiedAt"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: no modifiedAt in {body}"));
    (state, modified_at)
}

/// Delete `environment_id` and let the deletion the clock owes it come due, so the record
/// has reached `DELETED` when the call returns.
async fn delete_and_settle(router: &Emulator, case: &str, environment_id: &str) {
    let (status, body) = call_in_region(
        router,
        REGION,
        "DeleteEnvironment",
        &json!({ "environmentId": environment_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteEnvironment expected 200, got {status} ({}) {body}",
        error_type(&body)
    );
    settle(router);
    let (state, _) = stored_record(router, case, environment_id).await;
    assert_eq!(
        state, "DELETED",
        "case {case:?}: the record must have settled on DELETED, got {state:?}"
    );
}

/// How the row's environment reached `DELETED`.
#[derive(Debug, Clone, Copy)]
enum Path {
    /// `DeleteEnvironment`, with the deletion the clock owes it come due.
    TheDeletion,
    /// The record put straight into that state, so the rule is judged on the STATE the
    /// record carries and not on the call that got it there.
    ForcedState,
}

#[tokio::test]
async fn every_environment_scoped_list_reports_a_deleted_environment_absent() {
    let router = emulator();

    // A control environment, deleted by nothing: every refusal below is the addressing
    // rule's answer about one record and not this service's answer to everything.
    let control = create_settled(&router, Some("deleted-lists-control")).await;
    let control_connector = connector_in(&router, "the control environment", &control, VCENTER).await;
    settle(&router);

    // Each row: the case, the connector types the environment holds, the VMs entitled
    // through its vCenter connector, and how the record reached DELETED. The rows differ
    // along what the deleted environment RETAINED for a list to report and along the path
    // into the state — neither of which is what decides whether a list addresses it.
    let rows: Vec<(&str, Vec<&str>, Vec<&str>, Path)> = vec![
        (
            "nothing but its VLANs",
            Vec::new(),
            Vec::new(),
            Path::TheDeletion,
        ),
        (
            "one vCenter connector with two entitlements through it",
            vec![VCENTER],
            vec!["vm-1", "vm-22"],
            Path::TheDeletion,
        ),
        (
            "two connectors of different types",
            vec![VCENTER, SDDC_MANAGER],
            Vec::new(),
            Path::TheDeletion,
        ),
        (
            "a record put straight into DELETED",
            vec![VCENTER],
            vec!["vm-333"],
            Path::ForcedState,
        ),
    ];

    for (case, connector_types, vm_ids, path) in rows {
        let environment = create_settled(&router, None).await;
        let mut vcenter: Option<String> = None;
        for connector_type in &connector_types {
            let connector_id = connector_in(&router, case, &environment, connector_type).await;
            if *connector_type == VCENTER {
                vcenter = Some(connector_id);
            }
        }
        settle(&router);
        if let Some(vcenter) = &vcenter {
            if !vm_ids.is_empty() {
                let (status, body) = call(
                    &router,
                    "CreateEntitlement",
                    &json!({
                        "environmentId": environment,
                        "connectorId": vcenter,
                        "entitlementType": WINDOWS_SERVER,
                        "vmIds": vm_ids,
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the entitlement create expected 200, got {status} ({}) {body}",
                    error_type(&body)
                );
                settle(&router);
            }
        }
        // The connector handle the `ListVmEntitlements` row carries: the environment's own
        // vCenter connector where it has one, and otherwise an id no connector was minted
        // at — the environment is decided first either way, which is what makes the four
        // answers agree.
        let handle = vcenter.clone().unwrap_or(UNKNOWN_CONNECTOR_ID.to_string());

        // While the environment is addressable every list answers a page: that is what
        // reaching DELETED has to change. A row that holds no connector has no handle for
        // the `ListVmEntitlements` request to resolve, so that row's page is asserted
        // through the other three.
        for (operation, request) in list_requests(&environment, &handle) {
            if operation == "ListVmEntitlements" && vcenter.is_none() {
                continue;
            }
            let before = format!("{case}: {operation} before the delete");
            let (status, body) = call_in_region(&router, REGION, operation, &request).await;
            assert_eq!(
                status, 200,
                "case {before:?}: expected HTTP 200, got {status} ({}) {body}",
                error_type(&body)
            );
        }

        match path {
            Path::TheDeletion => delete_and_settle(&router, case, &environment).await,
            Path::ForcedState => force_state(&router, &environment, evs::state::STATE_DELETED),
        }

        // THE POINT: the environment has reached DELETED, so no list addresses it — all
        // four answer the one refusal this service words for an environment a request
        // cannot address, whatever each of them would have had to report.
        for (operation, request) in list_requests(&environment, &handle) {
            let refused = format!("{case}: {operation}");
            let (status, body) = call_in_region(&router, REGION, operation, &request).await;
            assert_environment_not_found(&refused, status, &body, &environment);

            // And at another Region's endpoint, where the record never resolved anyway.
            let elsewhere = format!("{case}: {operation} at {OTHER_REGION}");
            let (status, body) = call_in_region(&router, OTHER_REGION, operation, &request).await;
            assert_environment_not_found(&elsewhere, status, &body, &environment);
        }

        // The record is still THERE — `GetEnvironment` reports it, whatever state it has
        // reached — so each refusal above is the addressing rule's answer and not an
        // emptied store.
        let (state, _) = stored_record(&router, case, &environment).await;
        assert_eq!(
            state, "DELETED",
            "case {case:?}: the record must still be reported in DELETED, got {state:?}"
        );

        // And the control environment answers every list as it always did.
        for (operation, request) in list_requests(&control, &control_connector) {
            let control_case = format!("{case}: {operation} against the control environment");
            let (status, body) = call_in_region(&router, REGION, operation, &request).await;
            assert_eq!(
                status, 200,
                "case {control_case:?}: expected HTTP 200, got {status} ({}) {body}",
                error_type(&body)
            );
        }
    }
}

#[tokio::test]
async fn a_deleted_environment_is_gone_for_every_read_that_acts_on_it_at_once() {
    // The symptom the rule rules out: one and the same deleted environment reported gone
    // by one read of it and present by another. Every operation that acts on the
    // environment refuses it — a list as much as the readers that always did — while the
    // two readers of the record itself still report it.
    let router = emulator();
    let environment = create_settled(&router, Some("deleted-lists-agreement")).await;
    let connector = connector_in(&router, "agreement", &environment, VCENTER).await;
    settle(&router);
    let arn = format!("arn:aws:evs:{REGION}:{ACCOUNT}:environment/{environment}");

    delete_and_settle(&router, "agreement", &environment).await;

    // The four lists, beside two readers that were already refusing it: one addressed by
    // id and one addressed by ARN.
    let mut refused = list_requests(&environment, &connector);
    refused.push(("GetDepotUrl", json!({ "environmentId": environment })));
    for (operation, request) in refused {
        let case = format!("agreement: {operation}");
        let (status, body) = call_in_region(&router, REGION, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            error_type(&body).rsplit(['#', '/']).next().unwrap_or(""),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {} {body}",
            error_type(&body)
        );
    }
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ListTagsForResource",
        &json!({ "resourceArn": arn }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"agreement: ListTagsForResource\": expected HTTP 400, got {status} {body}"
    );

    // The two readers outside the rule still report the record: `GetEnvironment`
    // describes it in the state it reached, and `ListEnvironments` finds it when asked
    // for that state.
    let (state, _) = stored_record(&router, "agreement", &environment).await;
    assert_eq!(
        state, "DELETED",
        "case \"agreement\": GetEnvironment reports the record whatever state it reached, \
         got {state:?}"
    );
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ListEnvironments",
        &json!({ "state": ["DELETED"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"agreement: ListEnvironments\": expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let listed: Vec<&str> = body["environmentSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"agreement\": no environmentSummaries in {body}"))
        .iter()
        .filter_map(|summary| summary["environmentId"].as_str())
        .collect();
    assert!(
        listed.contains(&environment.as_str()),
        "case \"agreement\": ListEnvironments must still report the deleted record, got \
         {listed:?}"
    );
}

#[tokio::test]
async fn the_refusal_changes_nothing_a_later_caller_sees() {
    // A refused list is a read: repeating it answers the same, and the record behind it
    // stands exactly as the settled deletion left it.
    let router = emulator();
    let environment = create_settled(&router, Some("deleted-lists-read-only")).await;
    delete_and_settle(&router, "read-only", &environment).await;
    let (settled_state, settled_modified_at) = stored_record(&router, "read-only", &environment).await;

    for attempt in 0..3 {
        for (operation, request) in list_requests(&environment, UNKNOWN_CONNECTOR_ID) {
            let case = format!("read-only: {operation}, attempt {attempt}");
            let (status, body) = call_in_region(&router, REGION, operation, &request).await;
            assert_environment_not_found(&case, status, &body, &environment);
        }
    }

    let (state, modified_at) = stored_record(&router, "read-only, afterwards", &environment).await;
    assert_eq!(
        state, settled_state,
        "case \"read-only, afterwards\": the refused lists must leave the record in \
         {settled_state:?}, got {state:?}"
    );
    assert_eq!(
        modified_at, settled_modified_at,
        "case \"read-only, afterwards\": the refused lists must not move modifiedAt from \
         {settled_modified_at}, got {modified_at}"
    );
}

/// Every subnet id the account's own Amazon EC2 store holds, read from the shared store
/// the two services both use.
fn subnets_in_ec2(router: &Emulator) -> Vec<String> {
    let store = ec2::state::service_state(&*router.engine)
        .expect("the in-memory engine carries the account's EC2 store");
    let store = store.lock();
    store.subnets.keys().cloned().collect()
}

#[tokio::test]
async fn no_list_reports_a_vlan_subnet_the_deletion_released() {
    // Why the page a deleted environment used to answer could not be right: the VLAN
    // subnets it reports are REAL subnets of the account's Amazon EC2 store, and the
    // deletion removes them from it. So once the environment is gone there is nothing
    // left for a page of `subnetId`s to name.
    let router = emulator();
    let environment = create_settled(&router, Some("deleted-lists-subnets")).await;

    let (status, body) = call_in_region(
        &router,
        REGION,
        "ListEnvironmentVlans",
        &json!({ "environmentId": environment }),
    )
    .await;
    assert_eq!(
        status, 200,
        "before the delete: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let subnet_ids: Vec<String> = body["environmentVlans"]
        .as_array()
        .unwrap_or_else(|| panic!("before the delete: no environmentVlans in {body}"))
        .iter()
        .filter_map(|vlan| vlan["subnetId"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        subnet_ids.len(),
        VLAN_COUNT,
        "before the delete: every VLAN names its subnet, got {subnet_ids:?}"
    );
    let held = subnets_in_ec2(&router);
    for subnet_id in &subnet_ids {
        assert!(
            held.contains(subnet_id),
            "before the delete: Amazon EC2 must hold the VLAN subnet {subnet_id}, got {held:?}"
        );
    }

    delete_and_settle(&router, "subnets", &environment).await;

    // The deletion released every one of them from the EC2 store...
    let left = subnets_in_ec2(&router);
    for subnet_id in &subnet_ids {
        assert!(
            !left.contains(subnet_id),
            "after the delete: Amazon EC2 must hold no VLAN subnet {subnet_id}, got {left:?}"
        );
    }
    // ...and no list of this service names one of them either.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ListEnvironmentVlans",
        &json!({ "environmentId": environment }),
    )
    .await;
    assert_environment_not_found("subnets: ListEnvironmentVlans", status, &body, &environment);
    for subnet_id in &subnet_ids {
        assert!(
            !body.to_string().contains(subnet_id.as_str()),
            "after the delete: the refusal must name no released subnet, got {subnet_id} in \
             {body}"
        );
    }
}
