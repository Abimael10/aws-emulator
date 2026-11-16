// SPDX-License-Identifier: CC0-1.0

//! The ACCELERATOR level of the Global Accelerator API-family separation, over the wire.
//!
//! RULE under test: an accelerator the store holds in the custom routing family is a
//! resource that exists, so a STANDARD accelerator operation addressed at its
//! `AcceleratorArn` is refused by stating the API-family separation and naming the ARN
//! the request carried — never by reporting the accelerator as absent. Which family's
//! map holds the ARN is the whole of the decision, so the refusal is independent of that
//! accelerator's own members, of what else the store holds and of how often it is asked;
//! an ARN no family holds keeps the absent-accelerator wording; and because these calls
//! only resolve an ARN, none of them may change the accelerator they refused.
//!
//! The table below therefore spans the `CreateCustomRoutingAccelerator` members that
//! shape the stored record (`IpAddressType`, `IpAddresses`, `Enabled`, `Tags`) and the
//! length of its name, crossed with every standard accelerator operation this crate
//! implements that resolves an `AcceleratorArn`. Standard accelerators are created before
//! and after the custom ones, so the standard map is never empty while a crossing is
//! asked.
//!
//! Every call goes through the real router, using the harness in
//! [`super::accelerator_lifecycle`], so routing, deserialization, the handler and the
//! error envelope are all exercised.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::accelerator_lifecycle::{arn_of, call, create_accelerator, emulator, error_code, ok};
use super::input_refusals::error_message;

/// The sentence a standard accelerator operation states for an ARN the custom routing
/// family holds: fixed text, with the request's `AcceleratorArn` as its only variable.
fn api_separation(arn: &str) -> String {
    format!(
        "Custom Routing resources must use the Custom Routing APIs; got custom routing \
         resource: {arn}"
    )
}

/// The sentence a standard accelerator lookup states for an ARN no family holds.
fn absent(arn: &str) -> String {
    format!("Accelerator {arn} not found.")
}

/// One custom routing accelerator to build: a case label and the create members beyond
/// the required `Name` and `IdempotencyToken`.
///
/// Members and bounds come from `CreateCustomRoutingAccelerator`'s model entry
/// (smithy_query query="operation" operation="CreateCustomRoutingAccelerator"): optional
/// `IpAddressType`, `IpAddresses` (0..2), `Enabled`, `Tags`. `IpAddressType` has a single
/// value this kind accepts — "For a custom routing accelerator, the value must be IPV4"
/// (`CustomRoutingAccelerator$IpAddressType`) — so a row either names it or leaves it
/// out. The name bound is the one `CreateCustomRoutingAccelerator$Name` documents: at
/// most 64 characters, alphanumeric or hyphens, never starting or ending with a hyphen.
fn records() -> Vec<(&'static str, String, Value)> {
    vec![
        ("defaults", "cr-defaults".to_string(), json!({})),
        ("disabled", "cr-disabled".to_string(), json!({ "Enabled": false })),
        (
            "explicit-ipv4",
            "cr-explicit-ipv4".to_string(),
            json!({ "IpAddressType": "IPV4" }),
        ),
        (
            "one-byoip-address",
            "cr-byoip-one".to_string(),
            json!({ "IpAddresses": ["198.51.100.7"] }),
        ),
        (
            "two-byoip-addresses",
            "cr-byoip-two".to_string(),
            json!({ "IpAddresses": ["203.0.113.4", "203.0.113.9"] }),
        ),
        (
            "tagged",
            "cr-tagged".to_string(),
            json!({ "Tags": [{ "Key": "team", "Value": "edge" }] }),
        ),
        (
            "name-at-the-64-character-maximum",
            format!("cr-{}", "a".repeat(61)),
            json!({ "Enabled": true, "IpAddressType": "IPV4" }),
        ),
    ]
}

/// Every standard accelerator operation this crate implements that resolves an
/// `AcceleratorArn`, with the other members each one needs. They share one lookup, so
/// they owe one answer.
fn standard_calls(arn: &str, token: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeAccelerator", json!({ "AcceleratorArn": arn })),
        (
            "UpdateAccelerator",
            json!({ "AcceleratorArn": arn, "Enabled": false }),
        ),
        ("DeleteAccelerator", json!({ "AcceleratorArn": arn })),
        (
            "DescribeAcceleratorAttributes",
            json!({ "AcceleratorArn": arn }),
        ),
        (
            "UpdateAcceleratorAttributes",
            json!({ "AcceleratorArn": arn, "FlowLogsEnabled": true }),
        ),
        ("ListListeners", json!({ "AcceleratorArn": arn })),
        (
            "CreateListener",
            json!({
                "AcceleratorArn": arn,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "Protocol": "TCP",
                "IdempotencyToken": token,
            }),
        ),
    ]
}

/// Drive one call and assert the status, the modelled error code and the message.
async fn assert_refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
    expected: &str,
) {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (400, "AcceleratorNotFoundException"),
        "case {case:?}/{operation}: expected HTTP 400 AcceleratorNotFoundException, got \
         {status} with {answer}"
    );
    let message = error_message(&answer);
    assert_eq!(
        message, expected,
        "case {case:?}/{operation}: expected {expected:?}, got {message:?}"
    );
}

/// An accelerator ARN of the same shape whose identifier was never minted: the ARN's
/// own text cannot be what decides the diagnosis.
fn unminted_sibling(arn: &str, index: usize) -> String {
    let (prefix, _) = arn
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("an accelerator ARN ends in its identifier, got {arn:?}"));
    format!("{prefix}/00000000-0000-4000-8000-00000000{index:04}")
}

/// Every member of a reported accelerator except `Status`, which the stored deployment
/// due instant advances on its own and which therefore says nothing about whether a
/// refusal wrote.
fn without_status(accelerator: &Value) -> Value {
    let mut copy = accelerator.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("Status");
    }
    copy
}

/// RULE: a standard accelerator operation addressed at a stored CUSTOM ROUTING
/// accelerator states the API-family separation and names the request's ARN, whatever
/// that accelerator's own members are and however many times it is asked; an ARN no
/// family holds keeps the absent wording; and the refusals leave every accelerator
/// readable and unchanged through its own family's API.
#[tokio::test]
async fn a_standard_accelerator_api_refuses_a_custom_routing_arn_as_a_custom_routing_resource() {
    let router = emulator();

    // A standard accelerator already in the store, so the standard map a crossing misses
    // in is populated rather than empty.
    let first_standard = arn_of(&create_accelerator(&router, "std-before", "std-before").await);

    let mut built: Vec<(&'static str, String, Value)> = Vec::new();
    for (case, name, extra) in records() {
        let mut body = json!({ "Name": name, "IdempotencyToken": format!("token-{case}") });
        for (key, value) in extra.as_object().expect("the extra members are an object") {
            body[key] = value.clone();
        }
        let created = ok(&router, "CreateCustomRoutingAccelerator", body).await["Accelerator"]
            .clone();
        let arn = arn_of(&created);
        assert!(
            !arn.is_empty(),
            "case {case:?}: CreateCustomRoutingAccelerator answered no AcceleratorArn"
        );
        built.push((case, arn, created));
    }

    // And one created after them, so the standard map changes between crossings too.
    let second_standard = arn_of(&create_accelerator(&router, "std-after", "std-after").await);

    for (round, suffix) in [(0usize, "first"), (1usize, "second")] {
        for (case, arn, _) in &built {
            for (operation, body) in standard_calls(arn, &format!("{case}-{suffix}-listener")) {
                let labelled = format!("{case}/round-{round}");
                assert_refusal(&router, &labelled, operation, body, &api_separation(arn)).await;
            }
        }
    }

    // The other half of the rule: an ARN of neither family is absent, so the two
    // diagnoses stay distinguishable even though both answer the same error code.
    for (index, (case, arn, _)) in built.iter().enumerate() {
        let unminted = unminted_sibling(arn, index);
        for operation in ["DescribeAccelerator", "DeleteAccelerator"] {
            assert_refusal(
                &router,
                &format!("{case}/unminted"),
                operation,
                json!({ "AcceleratorArn": unminted }),
                &absent(&unminted),
            )
            .await;
        }
    }

    // Nothing above wrote: every custom routing accelerator still reads back through its
    // own family's describe, member for member as its create reported it.
    for (case, arn, created) in &built {
        let read = ok(
            &router,
            "DescribeCustomRoutingAccelerator",
            json!({ "AcceleratorArn": arn }),
        )
        .await["Accelerator"]
            .clone();
        assert_eq!(
            without_status(&read),
            without_status(created),
            "case {case:?}: expected {arn} to read back unchanged, got {read}"
        );
    }

    // ... and the standard accelerators the crossings missed in are still served by the
    // standard API, so the crossing check did not shadow the family it belongs to.
    for (case, arn) in [
        ("std-before", &first_standard),
        ("std-after", &second_standard),
    ] {
        let read = ok(
            &router,
            "DescribeAccelerator",
            json!({ "AcceleratorArn": arn }),
        )
        .await["Accelerator"]
            .clone();
        assert_eq!(
            arn_of(&read),
            *arn,
            "case {case:?}: expected {arn} to still be served by DescribeAccelerator, got {read}"
        );
    }
}
