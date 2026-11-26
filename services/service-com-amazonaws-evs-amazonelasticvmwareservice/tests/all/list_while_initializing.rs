// SPDX-License-Identifier: CC0-1.0

//! A list addressed to an Amazon EVS environment that is STILL INITIALIZING.
//!
//! The rule under test: an operation that LISTS the resources of one environment is
//! refused with `ValidationException` — HTTP 400 — for exactly as long as that
//! environment is still initializing, and answered in every other state the list
//! addresses the environment in.
//!
//! "The input fails to satisfy the specified constraints. You will see this exception
//! if invalid inputs are provided for any of the Amazon EVS environment operations, or
//! if a list operation is performed on an environment resource that is still
//! initializing." (`ValidationException` doc, `smithy_query(query="shape",
//! shape="ValidationException")`.) An environment is still initializing while its
//! `EnvironmentState` is `CREATING`; the other four values the model declares —
//! `CREATED`, `DELETING`, `DELETED`, `CREATE_FAILED` (`smithy_query(query="shape",
//! shape="EnvironmentState")`) — are past initialization.
//!
//! This refusal is the LATER half of what a list resolves: whether the request addresses
//! the environment at all is decided first, by the one addressing rule every operation
//! that acts on an environment shares, so a record that has reached `DELETED` is reported
//! absent rather than reaching this rule (`tests/all/deleted_environment_lists.rs` and
//! `tests/all/environment_addressing_rule.rs` own that half).
//!
//! The rule is about the CLASS of list operations addressed to an environment resource,
//! not about one of them, so every table here spans all four
//! ([`list_requests`]) — and the state table spans every published
//! `EnvironmentState`, including the ones no create produces on its own.
//!
//! The refusal is also all a refused list does: it is a read, so once the environment
//! has settled the SAME request reports the whole inventory, unchanged by having been
//! refused.
//!
//! The harness (`emulator`, `call`, `call_in_region`, `create`, `settle`) is the one
//! `environment_lifecycle` already owns, so every test drives the service the way a
//! client does: `POST /` with `X-Amz-Target: AmazonElasticVMwareService.<Operation>`.

use serde_json::{json, Value};
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;

use crate::entitlement_connector::connector_in;
use crate::environment_lifecycle::{
    call, call_in_region, create, emulator, error_type, settle, Emulator, REGION,
};

/// A well-formed `connectorId` no connector was ever minted at: it satisfies the
/// modelled `^(cnctr-[a-zA-Z0-9]{10})$`, which is all a row needs where the environment
/// is decided before the connector handle is resolved — the environment's state and its
/// absence are both answered first. A row that expects the list to be ANSWERED carries
/// a connector the environment really holds instead
/// (`tests/all/list_vm_entitlements_connector.rs` owns the connector-handle rule).
const UNKNOWN_CONNECTOR_ID: &str = "cnctr-0123456789";

/// The type of connector the `ListVmEntitlements` rows are listed through — any
/// modelled type resolves, so this picks the one an entitlement is created through.
const VCENTER: &str = "VCENTER";

/// The only `EntitlementType` the model declares
/// (`smithy_query(query="shape", shape="EntitlementType")`).
const WINDOWS_SERVER: &str = "WINDOWS_SERVER";

/// What a list addressed to an environment in one state answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The `ValidationException` of the rule this suite owns: the environment is still
    /// initializing.
    RefusedAsInitializing,
    /// The `ResourceNotFoundException` of the ADDRESSING rule, which decides whether a
    /// list resolves the environment at all before this suite's rule is reached: a record
    /// that has reached `DELETED` is addressed by no list ("The resource might not be
    /// specified correctly, or it may have a `state` of `DELETED`",
    /// `ResourceNotFoundException` doc). `tests/all/deleted_environment_lists.rs` and
    /// `tests/all/environment_addressing_rule.rs` own that rule; this row is here so the
    /// state table stays complete.
    ReportedAbsent,
    /// The page itself.
    Page,
}

/// The `EnvironmentState` values the model declares
/// (`smithy_query(query="shape", shape="EnvironmentState")`), each paired with what a
/// list addressed to an environment in that state answers — the initializing refusal for
/// `CREATING` and nothing else, and a page for every state that is both past
/// initialization and still addressable.
const ENVIRONMENT_STATES: [(&str, Answer); 5] = [
    ("CREATING", Answer::RefusedAsInitializing),
    ("CREATED", Answer::Page),
    ("DELETING", Answer::Page),
    ("DELETED", Answer::ReportedAbsent),
    ("CREATE_FAILED", Answer::Page),
];

/// A Region other than [`REGION`], so a request signed for it reaches another endpoint
/// of this service — one that holds no record of an environment created in [`REGION`],
/// whatever state that record has reached.
const OTHER_REGION: &str = "eu-central-1";

/// A well-formed `environmentId` no environment was ever minted at: it satisfies the
/// modelled `^(env-[a-zA-Z0-9]{10})$`, so it reaches the handler's own lookup rather
/// than the boundary validator.
const ABSENT_ENVIRONMENT_ID: &str = "env-0000000000";

/// The `resourceType` this service's not-found refusal names for an environment.
const ENVIRONMENT_RESOURCE_TYPE: &str = "environment";

/// The number of VLAN subnets an environment is built with: `InitialVlans` declares ten
/// members, so a complete `ListEnvironmentVlans` answer reports ten.
const VLAN_COUNT: usize = 10;

/// Every LIST operation this service binds to one environment resource, with a request
/// that addresses `environment`, and the output member each one pages over.
///
/// These are the four operations whose input is an `environmentId` and whose output is a
/// page of that environment's resources
/// (`smithy_query(query="digest")`): the class the `ValidationException` doc's "a list
/// operation is performed on an environment resource" covers. `ListEnvironments` is not
/// among them — it addresses the account and Region, not one environment resource — and
/// neither is `ListTagsForResource`, which addresses a resource by ARN and declares no
/// `ValidationException` at all.
fn list_requests(environment: &str, connector_id: &str) -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "ListEnvironmentVlans",
            json!({ "environmentId": environment }),
            "environmentVlans",
        ),
        (
            "ListEnvironmentHosts",
            json!({ "environmentId": environment }),
            "environmentHosts",
        ),
        (
            "ListEnvironmentConnectors",
            json!({ "environmentId": environment }),
            "connectors",
        ),
        (
            "ListVmEntitlements",
            json!({
                "environmentId": environment,
                "connectorId": connector_id,
                "entitlementType": WINDOWS_SERVER,
            }),
            "entitlements",
        ),
    ]
}

/// Put one stored environment into `state` and drop the transition it owed, so a state
/// the lifecycle does not pass through on its own — or one it would leave while the test
/// runs — can be exercised too.
pub(crate) fn force_state(router: &Emulator, environment_id: &str, state: &str) {
    let store = evs::state::service_state(&*router.engine)
        .expect("the in-memory engine carries this service's store");
    let mut store = store.lock();
    let environment = store
        .environments
        .get_mut(environment_id)
        .unwrap_or_else(|| panic!("the store holds the environment {environment_id}"));
    environment.state = state.to_string();
    environment.pending = None;
}

/// Assert that `body` is this service's refusal of a list against an initializing
/// environment: a `ValidationException` naming `environmentId`.
fn assert_initializing_refusal(case: &str, status: u16, body: &Value) {
    // Every modeled CLIENT error of this awsJson1_0 service goes on the wire as a 400 —
    // see `tests/all/error_wire_status.rs`, which owns that rule.
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {body}"
    );
    assert_eq!(
        error_type(body).rsplit(['#', '/']).next().unwrap_or(""),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {}",
        error_type(body)
    );
    let field_names: Vec<&str> = body["fieldList"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| field["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        field_names,
        vec!["environmentId"],
        "case {case:?}: the refusal must name the member that carried the environment, got {body}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("still initializing"),
        "case {case:?}: the refusal must say why the list was not performed, got {message:?}"
    );
}

/// Assert that `body` is this service's answer for an environment the request addresses
/// no record of: a `ResourceNotFoundException` naming the environment that was asked
/// for.
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
        body["resourceId"].as_str().unwrap_or_default(),
        environment_id,
        "case {case:?}: resourceId must echo the environment the request named, got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str().unwrap_or_default(),
        ENVIRONMENT_RESOURCE_TYPE,
        "case {case:?}: unexpected resourceType, got {body}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(environment_id),
        "case {case:?}: the refusal must name the environment asked for, got {message:?}"
    );
}

#[tokio::test]
async fn every_environment_scoped_list_resolves_its_environment_the_same_way() {
    // The lookup and the initializing refusal are ONE step every list addressed to an
    // environment applies, so the four operations answer alike at each of its outcomes:
    // the record is addressed at the endpoint of its own Region and nowhere else, an id
    // that names no record is reported absent, an addressed record that is still
    // initializing refuses the list, and a settled one is reported.
    let router = emulator();
    let id = create(&router, Some("shared-lookup-env")).await;
    // A connector of that environment, so the `ListVmEntitlements` rows carry a handle
    // that resolves and the outcome under test is the environment's.
    let connector = connector_in(&router, "shared-lookup-env", &id, VCENTER).await;

    for (operation, request, _) in list_requests(&id, &connector) {
        let case = format!("{operation} while CREATING, at the environment's own endpoint");
        let (status, body) = call_in_region(&router, REGION, operation, &request).await;
        assert_initializing_refusal(&case, status, &body);

        // At another Region's endpoint the lookup half answers first: there is no record
        // there to be initializing.
        let case = format!("{operation} while CREATING, at {OTHER_REGION}");
        let (status, body) = call_in_region(&router, OTHER_REGION, operation, &request).await;
        assert_environment_not_found(&case, status, &body, &id);
    }

    settle(&router);

    for (operation, request, member) in list_requests(&id, &connector) {
        let case = format!("{operation} once CREATED, at the environment's own endpoint");
        let (status, body) = call_in_region(&router, REGION, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: both halves of the step are satisfied, so the page is \
             reported; got {status} {body}"
        );
        assert!(
            body[member].is_array(),
            "case {case:?}: expected a page in {member}, got {body}"
        );

        // The Region half is not relaxed by the environment having settled.
        let case = format!("{operation} once CREATED, at {OTHER_REGION}");
        let (status, body) = call_in_region(&router, OTHER_REGION, operation, &request).await;
        assert_environment_not_found(&case, status, &body, &id);
    }

    // An id this service holds no record of at any endpoint, with the store non-empty:
    // the same not-found answer from all four.
    for (operation, request, _) in list_requests(ABSENT_ENVIRONMENT_ID, UNKNOWN_CONNECTOR_ID) {
        let case = format!("{operation} against an id no environment was minted at");
        let (status, body) = call_in_region(&router, REGION, operation, &request).await;
        assert_environment_not_found(&case, status, &body, ABSENT_ENVIRONMENT_ID);
    }
}

#[tokio::test]
async fn every_list_of_an_environment_is_refused_while_it_is_initializing() {
    let router = emulator();
    // NOT settled: a freshly created environment is CREATING, which is the initializing
    // state.
    let id = create(&router, Some("initializing-env")).await;
    // A connector of that environment, so the `ListVmEntitlements` rows carry a handle
    // that resolves: the refusal under test must be the environment's state and not the
    // connector lookup. A connector can be created while the environment initializes.
    let connector = connector_in(&router, "initializing-env", &id, VCENTER).await;

    for (operation, request, _) in list_requests(&id, &connector) {
        let case = format!("{operation} while CREATING");
        let (status, body) = call(&router, operation, &request).await;
        assert_initializing_refusal(&case, status, &body);
    }

    // The same four requests, once the transition the clock owed the environment has
    // come due: the refusal is the initializing state's and not the operation's answer
    // to everything.
    settle(&router);
    for (operation, request, member) in list_requests(&id, &connector) {
        let case = format!("{operation} once CREATED");
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body}"
        );
        assert!(
            body[member].is_array(),
            "case {case:?}: expected a page in {member}, got {body}"
        );
    }
}

#[tokio::test]
async fn a_list_is_refused_in_the_initializing_state_and_answered_in_every_other_one() {
    let router = emulator();
    let id = create(&router, Some("state-table-env")).await;
    let connector = connector_in(&router, "state-table-env", &id, VCENTER).await;
    // Settled first, so the state each row forces is the state the list sees: an
    // environment that still owes a transition would leave the forced state behind as
    // soon as that transition came due.
    settle(&router);

    for (state, answer) in ENVIRONMENT_STATES {
        force_state(&router, &id, state);
        for (operation, request, member) in list_requests(&id, &connector) {
            let case = format!("{operation} against an environment in {state}");
            let (status, body) = call(&router, operation, &request).await;
            match answer {
                Answer::RefusedAsInitializing => assert_initializing_refusal(&case, status, &body),
                Answer::ReportedAbsent => {
                    assert_environment_not_found(&case, status, &body, &id)
                }
                Answer::Page => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: {state} is past initialization and still addressable, \
                         so the list is answered; got {status} {body}"
                    );
                    assert!(
                        body[member].is_array(),
                        "case {case:?}: expected a page in {member}, got {body}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_refused_list_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let id = create(&router, Some("refused-then-listed")).await;

    // A list refused while the environment initializes, repeated: a read consumes
    // nothing, so the second refusal is the same as the first.
    for attempt in 0..3 {
        let (status, body) = call(
            &router,
            "ListEnvironmentVlans",
            &json!({ "environmentId": id }),
        )
        .await;
        assert_initializing_refusal(&format!("attempt {attempt}"), status, &body);
    }

    // And once the environment has settled, the whole inventory is there: nothing the
    // refusals touched was spent.
    settle(&router);
    let (status, body) = call(
        &router,
        "ListEnvironmentVlans",
        &json!({ "environmentId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "after settling: expected 200, got {status} {body}"
    );
    let vlans = body["environmentVlans"]
        .as_array()
        .unwrap_or_else(|| panic!("after settling: no environmentVlans in {body}"));
    assert_eq!(
        vlans.len(),
        VLAN_COUNT,
        "after settling: the refused lists must not have consumed any VLAN, expected \
         {VLAN_COUNT}, got {}",
        vlans.len()
    );
}

#[tokio::test]
async fn an_absent_environment_is_still_reported_absent_rather_than_initializing() {
    // The refusal is about a resource this service HOLDS: an id that names nothing is
    // still the not-found answer, whichever order a reader might expect the two checks
    // in.
    let router = emulator();
    let absent = "env-0000000000";
    // One real, initializing environment, so the store is not empty.
    let _initializing = create(&router, Some("present-but-initializing")).await;

    for (operation, request, _) in list_requests(absent, UNKNOWN_CONNECTOR_ID) {
        let case = format!("{operation} against an absent environment");
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            error_type(&body).rsplit(['#', '/']).next().unwrap_or(""),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {}",
            error_type(&body)
        );
    }
}
