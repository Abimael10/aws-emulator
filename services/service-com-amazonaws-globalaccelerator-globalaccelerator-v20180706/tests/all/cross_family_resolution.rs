// SPDX-License-Identifier: CC0-1.0

//! How this service answers an ARN that names a real resource of the OTHER API family.
//!
//! RULE under test: Global Accelerator serves standard and custom routing resources
//! through two separate API families — the model gives each kind its own operations and
//! its own output shapes (`Accelerator` vs `CustomRoutingAccelerator`, `Listener` vs
//! `CustomRoutingListener`) — so an operation of one family addressed at an ARN that
//! names an existing resource of the other family refuses the call by stating the family
//! separation and naming the ARN the request carried, instead of reporting the resource
//! as absent. An ARN that names a resource of NEITHER family keeps the absent-resource
//! wording.
//!
//! It is a property of the family-scoped lookup, not of one operation, so the table below
//! spans both directions, both resource levels (accelerator and listener), and every
//! operation this crate implements that resolves an ARN inside one family: reads,
//! updates, deletes, a list of children and a create's parent check.
//!
//! Every call goes through the real router, using the harness in
//! [`super::accelerator_lifecycle`], so routing, deserialization, the handler and the
//! error envelope are all exercised.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::accelerator_lifecycle::{arn_of, call, create_accelerator, emulator, error_code, ok};
use super::input_refusals::error_message;

/// The sentence a STANDARD operation refuses a custom routing ARN with.
fn custom_routing_api_required(arn: &str) -> String {
    format!(
        "Custom Routing resources must use the Custom Routing APIs; got custom routing \
         resource: {arn}"
    )
}

/// The sentence a CUSTOM ROUTING operation refuses a standard ARN with.
fn standard_resource_not_supported(arn: &str) -> String {
    format!(
        "Only Custom Routing resources are supported by this custom routing API; got \
         standard accelerator: {arn}"
    )
}

/// One accelerator and one listener of each family, each ARN read back out of the
/// create's own answer.
struct Families {
    standard_accelerator: String,
    standard_listener: String,
    custom_accelerator: String,
    custom_listener: String,
}

/// Build one hierarchy per family, labelled so several can live in one emulator.
async fn families(router: &MultiServiceRouter, label: &str) -> Families {
    let standard_accelerator = arn_of(
        &create_accelerator(router, &format!("std-{label}"), &format!("{label}-std-acc")).await,
    );
    let standard_listener = ok(
        router,
        "CreateListener",
        json!({
            "AcceleratorArn": standard_accelerator,
            "PortRanges": [{ "FromPort": 80, "ToPort": 81 }],
            "Protocol": "TCP",
            "IdempotencyToken": format!("{label}-std-lis"),
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("CreateListener answers a ListenerArn")
        .to_string();

    let custom_accelerator = arn_of(
        &ok(
            router,
            "CreateCustomRoutingAccelerator",
            json!({ "Name": format!("crf-{label}"), "IdempotencyToken": format!("{label}-cr-acc") }),
        )
        .await["Accelerator"],
    );
    let custom_listener = ok(
        router,
        "CreateCustomRoutingListener",
        json!({
            "AcceleratorArn": custom_accelerator,
            "PortRanges": [{ "FromPort": 443, "ToPort": 443 }],
            "IdempotencyToken": format!("{label}-cr-lis"),
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("CreateCustomRoutingListener answers a ListenerArn")
        .to_string();

    Families {
        standard_accelerator,
        standard_listener,
        custom_accelerator,
        custom_listener,
    }
}

/// One row of the table: the operation called, the body it carries, the modelled error
/// code it earns and the message it must state.
struct Row {
    case: String,
    operation: &'static str,
    body: Value,
    code: &'static str,
    message: String,
}

/// Every cross-family call this crate can make, in both directions, at both resource
/// levels. Two hierarchies are built so each message is exercised against two different
/// ARNs: the ARN is a variable the request supplies, not part of the wording.
fn crossing_rows(label: &str, families: &Families) -> Vec<Row> {
    let custom_accelerator = &families.custom_accelerator;
    let custom_listener = &families.custom_listener;
    let standard_accelerator = &families.standard_accelerator;
    let standard_listener = &families.standard_listener;

    // The standard APIs, addressed at a custom routing resource.
    let mut rows = vec![
        Row {
            case: format!("{label}/DescribeAccelerator"),
            operation: "DescribeAccelerator",
            body: json!({ "AcceleratorArn": custom_accelerator }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/UpdateAccelerator"),
            operation: "UpdateAccelerator",
            body: json!({ "AcceleratorArn": custom_accelerator, "Enabled": false }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/DeleteAccelerator"),
            operation: "DeleteAccelerator",
            body: json!({ "AcceleratorArn": custom_accelerator }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/DescribeAcceleratorAttributes"),
            operation: "DescribeAcceleratorAttributes",
            body: json!({ "AcceleratorArn": custom_accelerator }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/UpdateAcceleratorAttributes"),
            operation: "UpdateAcceleratorAttributes",
            body: json!({ "AcceleratorArn": custom_accelerator, "FlowLogsEnabled": false }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/ListListeners"),
            operation: "ListListeners",
            body: json!({ "AcceleratorArn": custom_accelerator }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/CreateListener"),
            operation: "CreateListener",
            body: json!({
                "AcceleratorArn": custom_accelerator,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "Protocol": "TCP",
                "IdempotencyToken": format!("{label}-cross-create-listener"),
            }),
            code: "AcceleratorNotFoundException",
            message: custom_routing_api_required(custom_accelerator),
        },
        Row {
            case: format!("{label}/DescribeListener"),
            operation: "DescribeListener",
            body: json!({ "ListenerArn": custom_listener }),
            code: "ListenerNotFoundException",
            message: custom_routing_api_required(custom_listener),
        },
        Row {
            case: format!("{label}/UpdateListener"),
            operation: "UpdateListener",
            body: json!({ "ListenerArn": custom_listener, "Protocol": "UDP" }),
            code: "ListenerNotFoundException",
            message: custom_routing_api_required(custom_listener),
        },
        Row {
            case: format!("{label}/DeleteListener"),
            operation: "DeleteListener",
            body: json!({ "ListenerArn": custom_listener }),
            code: "ListenerNotFoundException",
            message: custom_routing_api_required(custom_listener),
        },
        Row {
            case: format!("{label}/CreateEndpointGroup"),
            operation: "CreateEndpointGroup",
            body: json!({
                "ListenerArn": custom_listener,
                "EndpointGroupRegion": "eu-west-1",
                "IdempotencyToken": format!("{label}-cross-create-group"),
            }),
            code: "ListenerNotFoundException",
            message: custom_routing_api_required(custom_listener),
        },
    ];

    // The custom routing APIs, addressed at a standard resource.
    rows.extend([
        Row {
            case: format!("{label}/DescribeCustomRoutingAccelerator"),
            operation: "DescribeCustomRoutingAccelerator",
            body: json!({ "AcceleratorArn": standard_accelerator }),
            code: "AcceleratorNotFoundException",
            message: standard_resource_not_supported(standard_accelerator),
        },
        Row {
            case: format!("{label}/CreateCustomRoutingListener"),
            operation: "CreateCustomRoutingListener",
            body: json!({
                "AcceleratorArn": standard_accelerator,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "IdempotencyToken": format!("{label}-cross-create-cr-listener"),
            }),
            code: "AcceleratorNotFoundException",
            message: standard_resource_not_supported(standard_accelerator),
        },
        Row {
            case: format!("{label}/DescribeCustomRoutingListener"),
            operation: "DescribeCustomRoutingListener",
            body: json!({ "ListenerArn": standard_listener }),
            code: "ListenerNotFoundException",
            message: standard_resource_not_supported(standard_listener),
        },
    ]);

    rows
}

/// Drive one row and assert the status, the modelled error code and the message.
async fn assert_row(router: &MultiServiceRouter, row: &Row) {
    let (status, body) = call(router, row.operation, row.body.clone()).await;
    let case = &row.case;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, row.code),
        "case {case:?}: expected HTTP 400 {code}, got {status} with {body}",
        code = row.code
    );
    let message = error_message(&body);
    assert_eq!(
        message, row.message,
        "case {case:?}: expected {expected:?}, got {message:?}",
        expected = row.message
    );
}

/// RULE: an operation of one API family addressed at an ARN that names an existing
/// resource of the other family states the family separation and names that ARN —
/// whichever direction, whichever resource level, whichever operation.
#[tokio::test]
async fn a_cross_family_arn_is_refused_as_the_other_family_s_resource() {
    let router = emulator();
    // Two independent hierarchies, so every sentence is checked against two different
    // ARNs rather than one recorded value.
    let first = families(&router, "one").await;
    let second = families(&router, "two").await;

    for row in crossing_rows("one", &first)
        .iter()
        .chain(crossing_rows("two", &second).iter())
    {
        assert_row(&router, row).await;
    }

    // None of those refusals touched the store: every resource still reads back through
    // its own family's describe, which is what makes "absent" the wrong diagnosis.
    for (case, operation, member, arn) in [
        (
            "standard accelerator",
            "DescribeAccelerator",
            "AcceleratorArn",
            &first.standard_accelerator,
        ),
        (
            "custom routing accelerator",
            "DescribeCustomRoutingAccelerator",
            "AcceleratorArn",
            &first.custom_accelerator,
        ),
        (
            "standard listener",
            "DescribeListener",
            "ListenerArn",
            &first.standard_listener,
        ),
        (
            "custom routing listener",
            "DescribeCustomRoutingListener",
            "ListenerArn",
            &first.custom_listener,
        ),
    ] {
        let read = ok(&router, operation, json!({ member: arn })).await;
        let reported = read
            .as_object()
            .and_then(|body| body.values().next())
            .and_then(|resource| resource.get(member))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported, arn,
            "case {case:?}: expected {arn} to still read back, got {read}"
        );
    }
}

/// RULE (the other half): the family-separation wording belongs to a CROSSING. An ARN
/// that names a resource of neither family keeps this service's absent-resource wording,
/// so the two diagnoses stay distinguishable.
#[tokio::test]
async fn an_arn_of_neither_family_keeps_the_absent_resource_wording() {
    let router = emulator();
    let built = families(&router, "absent").await;

    // Rows: ARNs of the right shape that were never minted, and the ARNs of resources of
    // each family that really existed and were then deleted.
    let unminted_accelerator =
        "arn:aws:globalaccelerator::000000000000:accelerator/00000000-0000-4000-8000-000000000000";
    let unminted_listener = format!("{unminted_accelerator}/listener/deadbeef");
    ok(
        &router,
        "DeleteListener",
        json!({ "ListenerArn": built.standard_listener }),
    )
    .await;
    ok(
        &router,
        "UpdateAccelerator",
        json!({ "AcceleratorArn": built.standard_accelerator, "Enabled": false }),
    )
    .await;
    ok(
        &router,
        "DeleteAccelerator",
        json!({ "AcceleratorArn": built.standard_accelerator }),
    )
    .await;

    let deleted_accelerator = built.standard_accelerator.clone();
    let deleted_listener = built.standard_listener.clone();
    let rows: Vec<Row> = vec![
        Row {
            case: "unminted/DescribeAccelerator".to_string(),
            operation: "DescribeAccelerator",
            body: json!({ "AcceleratorArn": unminted_accelerator }),
            code: "AcceleratorNotFoundException",
            message: format!("Accelerator {unminted_accelerator} not found."),
        },
        Row {
            case: "deleted/DescribeAccelerator".to_string(),
            operation: "DescribeAccelerator",
            body: json!({ "AcceleratorArn": deleted_accelerator }),
            code: "AcceleratorNotFoundException",
            message: format!("Accelerator {deleted_accelerator} not found."),
        },
        Row {
            case: "unminted/DescribeCustomRoutingAccelerator".to_string(),
            operation: "DescribeCustomRoutingAccelerator",
            body: json!({ "AcceleratorArn": unminted_accelerator }),
            code: "AcceleratorNotFoundException",
            message: format!("Accelerator {unminted_accelerator} not found."),
        },
        Row {
            case: "deleted/DescribeCustomRoutingAccelerator".to_string(),
            operation: "DescribeCustomRoutingAccelerator",
            body: json!({ "AcceleratorArn": deleted_accelerator }),
            code: "AcceleratorNotFoundException",
            message: format!("Accelerator {deleted_accelerator} not found."),
        },
        Row {
            case: "unminted/DescribeListener".to_string(),
            operation: "DescribeListener",
            body: json!({ "ListenerArn": unminted_listener }),
            code: "ListenerNotFoundException",
            message: format!("Listener {unminted_listener} not found."),
        },
        Row {
            case: "deleted/DescribeListener".to_string(),
            operation: "DescribeListener",
            body: json!({ "ListenerArn": deleted_listener }),
            code: "ListenerNotFoundException",
            message: format!("Listener {deleted_listener} not found."),
        },
        Row {
            case: "unminted/DescribeCustomRoutingListener".to_string(),
            operation: "DescribeCustomRoutingListener",
            body: json!({ "ListenerArn": unminted_listener }),
            code: "ListenerNotFoundException",
            message: format!("Listener {unminted_listener} not found."),
        },
        Row {
            case: "deleted/DescribeCustomRoutingListener".to_string(),
            operation: "DescribeCustomRoutingListener",
            body: json!({ "ListenerArn": deleted_listener }),
            code: "ListenerNotFoundException",
            message: format!("Listener {deleted_listener} not found."),
        },
        // A create's parent lookup states its own sentence, with no variable part, for a
        // parent that is of neither family.
        Row {
            case: "unminted/CreateListener".to_string(),
            operation: "CreateListener",
            body: json!({
                "AcceleratorArn": unminted_accelerator,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "Protocol": "TCP",
                "IdempotencyToken": "absent-parent-standard",
            }),
            code: "AcceleratorNotFoundException",
            message: "Could not find accelerator.".to_string(),
        },
        Row {
            case: "unminted/CreateCustomRoutingListener".to_string(),
            operation: "CreateCustomRoutingListener",
            body: json!({
                "AcceleratorArn": unminted_accelerator,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "IdempotencyToken": "absent-parent-custom",
            }),
            code: "AcceleratorNotFoundException",
            message: "Could not find accelerator.".to_string(),
        },
    ];

    for row in &rows {
        assert_row(&router, row).await;
    }

    // The custom routing hierarchy is untouched by the deletes above, and a crossing
    // addressed at it still states the separation rather than the absent wording — the
    // two diagnoses are decided by the store, not by the operation.
    let still_crossing = Row {
        case: "surviving-custom/DescribeListener".to_string(),
        operation: "DescribeListener",
        body: json!({ "ListenerArn": built.custom_listener }),
        code: "ListenerNotFoundException",
        message: custom_routing_api_required(&built.custom_listener),
    };
    assert_row(&router, &still_crossing).await;
}
