// SPDX-License-Identifier: CC0-1.0

//! How many values one `Filters` entry of `DescribeInstancePatches` may carry, end to end.
//!
//! The rule under test: this read narrows the patches of ONE managed node by one value of the
//! patch property a filter's key names ("Each element in the array is a structure containing
//! a key-value pair. Supported keys for `DescribeInstancePatches` include the following:
//! Classification, KBId, Severity, State",
//! `smithy_query(query="doc", shape="DescribeInstancePatchesRequest", member="Filters")`), so
//! an entry whose `Values` list does not hold exactly ONE value is refused HTTP 400 with the
//! value-size sentence — fixed text that states the rule and carries nothing of the request,
//! neither the key nor the values the entry held. An entry carrying exactly one value is
//! served, for every published key, up to the five filters the modelled list admits
//! (`PatchOrchestratorFilterList` @length 0..5).
//!
//! The sibling catalogue read `DescribeAvailablePatches` searches on its own keys and is NOT
//! held to one value per entry; a row below pins that boundary, so the rule is not widened to
//! every patch filter of the service.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router.
//!
//! The rows span what the rule generalises over: which published key the entry names, how
//! many values it carries (none at all, an empty list, two, three, five), where in the
//! `Filters` list the offending entry sits, and whether the node id names a node this
//! endpoint holds a registration of.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::managed_node_lifecycle::register;
use crate::parameter_store::{call, error_code, error_message, router_with_engine, REGION};

/// The operation under test.
const OPERATION: &str = "DescribeInstancePatches";

/// Every key this read publishes, in the order its `Filters` doc lists them. Spelled out
/// here rather than read from the crate, so the vocabulary itself is what the assertions
/// check.
const FILTER_KEYS: &[&str] = &["Classification", "KBId", "Severity", "State"];

/// What a `Filters` entry whose `Values` list does not hold exactly one value is refused
/// with: the whole sentence is fixed text, with no part of the request in it.
const VALUE_SIZE_MESSAGE: &str = "Invalid filter value size. A filter can only contain one value.";

/// A registered EC2 node id (`InstanceId`, pattern `^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// A well-formed node id nothing registered, so a case can pin that the filter is refused
/// before the node id is resolved.
const UNREGISTERED: &str = "i-0dead999";

/// The modelled ceiling of the `Filters` list (`PatchOrchestratorFilterList` @length 0..5).
const MAX_FILTERS: usize = 5;

/// A router with this service mounted and one registered managed node, which is what the
/// served cases read the patches of.
fn service() -> MultiServiceRouter {
    let (router, engine): (MultiServiceRouter, Arc<engine::InMemoryEngine>) = router_with_engine();
    register(
        &engine,
        REGION,
        NODE,
        service_com_amazonaws_ssm_amazonssm::state::RESOURCE_TYPE_EC2_INSTANCE,
        "Linux",
    );
    router
}

/// One `Filters` entry carrying `count` distinct values under `key`.
fn entry(key: &str, count: usize) -> Value {
    let values: Vec<String> = (0..count).map(|index| format!("value-{index}")).collect();
    json!({ "Key": key, "Values": values })
}

/// A `Values` list that does not hold exactly one value is refused with the value-size
/// sentence, whichever published key it names, however many values it carries and wherever
/// in the `Filters` list it sits — and whether or not the node id names a registered node.
#[tokio::test]
async fn a_filter_that_does_not_carry_exactly_one_value_is_refused() {
    let router = service();

    // (case, the Filters list)
    let mut rows: Vec<(String, Value)> = Vec::new();
    for key in FILTER_KEYS {
        rows.push((
            format!("{key} with no Values member at all"),
            json!([{ "Key": key }]),
        ));
        rows.push((
            format!("{key} with an empty Values list"),
            json!([entry(key, 0)]),
        ));
        rows.push((format!("{key} with two values"), json!([entry(key, 2)])));
        rows.push((format!("{key} with three values"), json!([entry(key, 3)])));
        rows.push((
            format!("{key} with five values"),
            json!([entry(key, MAX_FILTERS)]),
        ));
    }
    // The offending entry behind an entry that is fine, so the refusal is not an artefact of
    // it being the first thing read.
    rows.push((
        "a well-formed entry followed by a two-value one".to_string(),
        json!([entry("State", 1), entry("Severity", 2)]),
    ));
    rows.push((
        "the fifth entry of a full Filters list".to_string(),
        json!([
            entry("State", 1),
            entry("Severity", 1),
            entry("KBId", 1),
            entry("Classification", 1),
            entry("State", 2),
        ]),
    ));

    for (case, filters) in &rows {
        for node_id in [NODE, UNREGISTERED] {
            let (status, body) = call(
                &router,
                OPERATION,
                &json!({ "InstanceId": node_id, "Filters": filters }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?} for {node_id}: must be refused, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?} for {node_id}: expected ValidationException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                VALUE_SIZE_MESSAGE,
                "case {case:?} for {node_id}: expected {VALUE_SIZE_MESSAGE:?}, got {:?}",
                error_message(&body)
            );
        }
    }
}

/// The sentence states the RULE: every such refusal of this read is worded identically, so
/// nothing of the request — not the key, not the values — varies between two of them.
#[tokio::test]
async fn the_value_size_refusal_carries_nothing_of_the_request() {
    let router = service();

    let mut seen: Vec<(String, String)> = Vec::new();
    for key in FILTER_KEYS {
        for count in [0usize, 2, 4] {
            let case = format!("{key} with {count} values");
            let (status, body) = call(
                &router,
                OPERATION,
                &json!({ "InstanceId": NODE, "Filters": [entry(key, count)] }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: must be refused, got {status} with {body}"
            );
            seen.push((case, error_message(&body)));
        }
    }

    for (case, message) in &seen {
        assert_eq!(
            message, VALUE_SIZE_MESSAGE,
            "case {case:?}: every value-size refusal reads the same, got {message:?}"
        );
    }
}

/// An entry carrying exactly one value is served, for every published key and up to the five
/// filters the modelled list admits: the refusal is about the value SIZE and nothing else.
#[tokio::test]
async fn a_filter_carrying_exactly_one_value_is_served() {
    let router = service();

    // (case, the Filters list)
    let mut rows: Vec<(String, Value)> = Vec::new();
    rows.push(("no Filters member at all".to_string(), Value::Null));
    rows.push(("an empty Filters list".to_string(), json!([])));
    for key in FILTER_KEYS {
        rows.push((format!("{key} with one value"), json!([entry(key, 1)])));
    }
    rows.push((
        "five entries, each with one value".to_string(),
        json!([
            entry("State", 1),
            entry("Severity", 1),
            entry("KBId", 1),
            entry("Classification", 1),
            entry("State", 1),
        ]),
    ));

    for (case, filters) in &rows {
        let mut body = json!({ "InstanceId": NODE });
        if !filters.is_null() {
            body["Filters"] = filters.clone();
        }
        let (status, response) = call(&router, OPERATION, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: must be served, got {status} with {response}"
        );
        // Nothing of this model runs a patching operation on the node, so the node has
        // reported no patches and every narrowing of them is empty.
        assert_eq!(
            response["Patches"],
            json!([]),
            "case {case:?}: no patch has been reported for the node, got {response}"
        );
    }
}

/// The boundary of the rule: the catalogue read searches on its own keys and takes SEVERAL
/// values under one of them, so the one-value rule belongs to the node-patch read alone.
#[tokio::test]
async fn the_catalogue_read_still_takes_several_values_under_one_key() {
    let router = service();

    let (status, response) = call(
        &router,
        "DescribeAvailablePatches",
        &json!({
            "Filters": [{
                "Key": "PRODUCT",
                "Values": ["WindowsServer2019", "WindowsServer2022"],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a catalogue filter of two products must be served, got {status} with {response}"
    );
    assert!(
        response["Patches"].is_array(),
        "the catalogue read reports a Patches list, got {response}"
    );
}
