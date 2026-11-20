// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the custom routing listener and endpoint group slice, driven over
//! the wire.
//!
//! Every call goes through the real router, using the harness in
//! [`super::accelerator_lifecycle`] (`POST /` with
//! `X-Amz-Target: GlobalAccelerator_V20180706.<Operation>`, `awsJson1_1` body), so these
//! exercise routing, deserialization, constraint validation, the handler and the response
//! envelope — not just the handler function.
//!
//! The rules under test are stated once each, as a table or an invariant over a SET of
//! inputs; a recorded request is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::accelerator_lifecycle::{arn_of, call, create_accelerator, emulator, error_code, ok};
// The envelope's message, in either spelling, already has one reader in this crate.
use super::input_refusals::error_message;

/// One custom routing accelerator, returning its ARN.
async fn custom_routing_accelerator(router: &MultiServiceRouter, label: &str) -> String {
    arn_of(
        &ok(
            router,
            "CreateCustomRoutingAccelerator",
            json!({ "Name": format!("crl-{label}"), "IdempotencyToken": format!("{label}-acc") }),
        )
        .await["Accelerator"],
    )
}

/// `CreateCustomRoutingListener` with the port ranges a row names.
async fn create_listener(
    router: &MultiServiceRouter,
    accelerator_arn: &str,
    token: &str,
    port_ranges: Value,
) -> (u16, Value) {
    call(
        router,
        "CreateCustomRoutingListener",
        json!({
            "AcceleratorArn": accelerator_arn,
            "PortRanges": port_ranges,
            "IdempotencyToken": token,
        }),
    )
    .await
}

/// The created listener, requiring a 200.
async fn created_listener(
    router: &MultiServiceRouter,
    accelerator_arn: &str,
    token: &str,
    port_ranges: Value,
) -> Value {
    let (status, body) = create_listener(router, accelerator_arn, token, port_ranges).await;
    assert_eq!(
        status, 200,
        "case {token}: expected HTTP 200 from CreateCustomRoutingListener, got {status} \
         with {body}"
    );
    body["Listener"].clone()
}

/// `DescribeCustomRoutingListener` for one ARN.
async fn describe_listener(router: &MultiServiceRouter, listener_arn: &str) -> (u16, Value) {
    call(
        router,
        "DescribeCustomRoutingListener",
        json!({ "ListenerArn": listener_arn }),
    )
    .await
}

/// The ARN of a listener a create or a describe answered.
fn listener_arn_of(listener: &Value) -> String {
    listener["ListenerArn"]
        .as_str()
        .unwrap_or_else(|| panic!("ListenerArn missing from {listener}"))
        .to_string()
}

/// A listener ARN hangs off its parent accelerator's ARN, with an 8-hex-digit id: the
/// shape the service's own ARNs carry (`.../accelerator/<uuid>/listener/<8 hex>`).
fn assert_listener_arn(case: &str, accelerator_arn: &str, listener_arn: &str) {
    let id = listener_arn
        .strip_prefix(&format!("{accelerator_arn}/listener/"))
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected a listener ARN under {accelerator_arn}, got {listener_arn:?}"
            )
        });
    assert_eq!(
        id.len(),
        8,
        "case {case}: expected an 8-digit hex listener id, got {id:?}"
    );
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "case {case}: expected lowercase hex in the listener id, got {id:?}"
    );
}

/// RULE: a create mints its resource from the service's own state — a fresh ARN under the
/// parent it names, carrying the members the request supplied — persists it, and the
/// matching describe reports exactly what was persisted, with only the members the
/// resource's own shape declares.
#[tokio::test]
async fn a_custom_routing_listener_is_minted_from_state_and_reads_back() {
    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "mint").await;

    // The rows differ along what the rule generalises over: how many ranges the request
    // carries (one, two, three, the documented maximum of ten), how wide each one is (a
    // single port, a span) and where in the port space they sit (the published bounds 1
    // and 65535 included). `PortRanges` is @length 1..10 of `PortRange { FromPort,
    // ToPort }` over `PortNumber` @range 1..65535 (`smithy_query(query="shape",
    // shape="PortRanges,PortRange")`).
    let ten_ranges: Vec<Value> = (0..10)
        .map(|index| json!({ "FromPort": 5000 + index, "ToPort": 5000 + index }))
        .collect();
    let cases: Vec<(&str, Value)> = vec![
        ("single-port", json!([{ "FromPort": 80, "ToPort": 80 }])),
        ("one-span", json!([{ "FromPort": 1024, "ToPort": 2048 }])),
        (
            "two-ranges",
            json!([
                { "FromPort": 80, "ToPort": 81 },
                { "FromPort": 443, "ToPort": 443 },
            ]),
        ),
        (
            "three-ranges-at-the-bounds",
            json!([
                { "FromPort": 1, "ToPort": 1 },
                { "FromPort": 65535, "ToPort": 65535 },
                { "FromPort": 1, "ToPort": 65535 },
            ]),
        ),
        ("ten-ranges", Value::Array(ten_ranges)),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (case, port_ranges) in cases {
        let created =
            created_listener(&router, &accelerator_arn, case, port_ranges.clone()).await;
        let listener_arn = listener_arn_of(&created);

        assert_listener_arn(case, &accelerator_arn, &listener_arn);
        assert!(
            !seen.contains(&listener_arn),
            "case {case}: every create mints a distinct ARN, got {listener_arn:?}"
        );
        seen.push(listener_arn.clone());

        assert_eq!(
            created["PortRanges"], port_ranges,
            "case {case}: expected the requested port ranges, in order, got {created}"
        );
        // `CustomRoutingListener` models exactly two members: a custom routing listener
        // has no `Protocol` and no `ClientAffinity`.
        for absent in ["Protocol", "ClientAffinity"] {
            assert!(
                created.get(absent).is_none(),
                "case {case}: `CustomRoutingListener` models no {absent}, got {created}"
            );
        }

        // The round trip: the describe reports the persisted record, not the request.
        let (status, body) = describe_listener(&router, &listener_arn).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the created listener to read back, got {status} {body}"
        );
        assert_eq!(
            body["Listener"], created,
            "case {case}: the describe must report the persisted record, got {body}"
        );
        // A read changes nothing a later caller sees.
        let (again_status, again) = describe_listener(&router, &listener_arn).await;
        assert_eq!(
            (again_status, &again["Listener"]),
            (200, &created),
            "case {case}: a second read must report the same record, got {again}"
        );
    }
}

/// RULE: a port range is stored only when it names at least one port — both ends present
/// and running forwards, inside the model's own `PortNumber` bounds — and a request that
/// breaks the rule is refused and persists nothing.
#[tokio::test]
async fn a_port_range_that_names_no_ports_is_refused() {
    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "ports").await;

    // (case, PortRanges, expected status, expected error code)
    //
    // The bounds are the model's: `PortNumber` @range 1..65535 and `PortRanges`
    // @length 1..10. A value outside a bound the model declares is refused by the
    // boundary validator as this service's `InvalidArgumentException`; a range that
    // satisfies every declared bound yet names no ports reaches the handler, which
    // answers the modelled `InvalidPortRangeException`.
    let eleven_ranges: Vec<Value> = (0..11)
        .map(|index| json!({ "FromPort": 6000 + index, "ToPort": 6000 + index }))
        .collect();
    let cases: Vec<(&str, Value, u16, &str)> = vec![
        (
            "upside-down",
            json!([{ "FromPort": 8080, "ToPort": 80 }]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "upside-down-beyond-the-first-range",
            json!([
                { "FromPort": 80, "ToPort": 80 },
                { "FromPort": 500, "ToPort": 400 },
            ]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "missing-to-port",
            json!([{ "FromPort": 80 }]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "missing-from-port",
            json!([{ "ToPort": 80 }]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "empty-range",
            json!([{}]),
            400,
            "InvalidPortRangeException",
        ),
        // One past each published bound of `PortNumber`.
        (
            "port-zero",
            json!([{ "FromPort": 0, "ToPort": 80 }]),
            400,
            "InvalidArgumentException",
        ),
        (
            "port-past-the-maximum",
            json!([{ "FromPort": 80, "ToPort": 65536 }]),
            400,
            "InvalidArgumentException",
        ),
        // One past each published bound of `PortRanges`.
        ("no-ranges", json!([]), 400, "InvalidArgumentException"),
        (
            "eleven-ranges",
            Value::Array(eleven_ranges),
            400,
            "InvalidArgumentException",
        ),
    ];

    for (case, port_ranges, expected_status, expected_code) in cases {
        let (status, body) =
            create_listener(&router, &accelerator_arn, case, port_ranges).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (expected_status, expected_code),
            "case {case}: expected HTTP {expected_status} {expected_code}, got {status} {body}"
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {case}: expected a populated Message, got {body}"
        );
    }

    // The upside-down range's own wording, the sentence this service states that
    // violation with.
    let (_, refused) = create_listener(
        &router,
        &accelerator_arn,
        "wording",
        json!([{ "FromPort": 443, "ToPort": 80 }]),
    )
    .await;
    assert_eq!(
        error_message(&refused),
        "FromPort should be less than or equal to ToPort",
        "case wording: expected the port-range rule to be stated, got {refused}"
    );

    // Nothing above was persisted: a range the service accepts still mints the FIRST
    // listener of this accelerator.
    let created = created_listener(
        &router,
        &accelerator_arn,
        "after-refusals",
        json!([{ "FromPort": 80, "ToPort": 80 }]),
    )
    .await;
    assert_listener_arn(
        "after-refusals",
        &accelerator_arn,
        &listener_arn_of(&created),
    );
}

/// RULE: an `IdempotencyToken` identifies one create request within its own operation and
/// its own parent — repeating that request returns the resource the token already minted,
/// a different token mints another, and the same token sent to another create or against
/// another parent does not replay this one.
#[tokio::test]
async fn an_idempotency_token_replays_only_its_own_create() {
    let router = emulator();
    let first_parent = custom_routing_accelerator(&router, "replay-one").await;
    let second_parent = custom_routing_accelerator(&router, "replay-two").await;

    // The rows differ in the port ranges the repeated request carries, so the replay is
    // tested as a property of the token rather than of one body.
    let cases: Vec<(&str, Value)> = vec![
        ("single", json!([{ "FromPort": 80, "ToPort": 80 }])),
        ("span", json!([{ "FromPort": 1024, "ToPort": 4096 }])),
        (
            "two-ranges",
            json!([
                { "FromPort": 53, "ToPort": 53 },
                { "FromPort": 443, "ToPort": 444 },
            ]),
        ),
    ];

    for (case, port_ranges) in cases {
        let token = format!("replay-{case}");
        let first =
            created_listener(&router, &first_parent, &token, port_ranges.clone()).await;
        let again =
            created_listener(&router, &first_parent, &token, port_ranges.clone()).await;
        assert_eq!(
            again, first,
            "case {case}: the same token must replay the listener it minted, got {again}"
        );

        let fresh = created_listener(
            &router,
            &first_parent,
            &format!("{token}-other"),
            port_ranges.clone(),
        )
        .await;
        assert_ne!(
            listener_arn_of(&fresh),
            listener_arn_of(&first),
            "case {case}: a different token mints a different listener, got {fresh}"
        );

        // The same token against a DIFFERENT parent is a different create request.
        let other_parent =
            created_listener(&router, &second_parent, &token, port_ranges).await;
        assert_ne!(
            listener_arn_of(&other_parent),
            listener_arn_of(&first),
            "case {case}: a token is scoped to its parent accelerator, got {other_parent}"
        );
    }

    // One token, two different listener creates: each kind of listener is its own
    // resource, so neither replays the other's.
    let standard_parent = arn_of(&create_accelerator(&router, "std-replay", "std-replay").await);
    let shared = "shared-listener-token";
    let standard = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": standard_parent,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": shared,
        }),
    )
    .await["Listener"]
        .clone();
    let custom = created_listener(
        &router,
        &first_parent,
        shared,
        json!([{ "FromPort": 80, "ToPort": 80 }]),
    )
    .await;
    assert_ne!(
        listener_arn_of(&custom),
        listener_arn_of(&standard),
        "case cross-kind token: a custom routing create must not replay the standard \
         listener, got {custom}"
    );
}

/// RULE: the two kinds of listener are distinct resources — each hangs off an accelerator
/// of its own kind and is reported only by the operations of its own kind, and a
/// reference of the other kind is refused as a resource this API family does not serve.
#[tokio::test]
async fn each_listener_kind_is_reached_only_through_its_own_operations() {
    let router = emulator();
    let custom_parent = custom_routing_accelerator(&router, "kinds").await;
    let standard_parent = arn_of(&create_accelerator(&router, "std-kinds", "std-kinds").await);

    let custom_listener = listener_arn_of(
        &created_listener(
            &router,
            &custom_parent,
            "kind-custom",
            json!([{ "FromPort": 80, "ToPort": 80 }]),
        )
        .await,
    );
    let standard_listener = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": standard_parent,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": "kind-standard",
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("the create answers a ListenerArn")
        .to_string();
    let unminted = format!("{custom_parent}/listener/deadbeef");

    // Every cross-kind read is a row, plus an ARN that names nothing at all. The status
    // this service answers a missing resource with is pinned, for every operation that
    // can answer one, by [`super::not_found_error_status`]; the wording of a CROSSING —
    // an ARN that names a real listener of the other family — is the API-separation
    // sentence, pinned over the whole class by [`super::cross_family_resolution`].
    let cases: Vec<(&str, &str, &str, String)> = vec![
        (
            "custom via DescribeListener",
            "DescribeListener",
            &custom_listener,
            format!(
                "Custom Routing resources must use the Custom Routing APIs; got custom \
                 routing resource: {custom_listener}"
            ),
        ),
        (
            "standard via DescribeCustomRoutingListener",
            "DescribeCustomRoutingListener",
            &standard_listener,
            format!(
                "Only Custom Routing resources are supported by this custom routing API; \
                 got standard accelerator: {standard_listener}"
            ),
        ),
        (
            "unminted via DescribeCustomRoutingListener",
            "DescribeCustomRoutingListener",
            &unminted,
            format!("Listener {unminted} not found."),
        ),
    ];
    for (case, operation, listener_arn, expected) in cases {
        let (status, body) = call(
            &router,
            operation,
            json!({ "ListenerArn": listener_arn }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ListenerNotFoundException"),
            "case {case}: expected this API family not to serve the ARN, got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case}: expected the refusal to name the ARN the request carried, got {body}"
        );
    }

    // And each kind reads back through its own describe.
    let (status, body) = describe_listener(&router, &custom_listener).await;
    assert_eq!(
        (status, body["Listener"]["ListenerArn"].as_str().unwrap_or_default()),
        (200, custom_listener.as_str()),
        "case custom describe: expected {custom_listener} to read back, got {body}"
    );
    let read_standard = ok(
        &router,
        "DescribeListener",
        json!({ "ListenerArn": standard_listener }),
    )
    .await;
    assert_eq!(
        read_standard["Listener"]["Protocol"], "TCP",
        "case standard describe: expected the standard listener's own members, \
         got {read_standard}"
    );

    // A create's parent must be an accelerator of its own kind, whichever way round. A
    // parent ARN that names an accelerator of the OTHER family is refused as the family
    // crossing it is, naming the ARN the request carried.
    let cross_parent: Vec<(&str, &str, Value, String)> = vec![
        (
            "custom listener on a standard accelerator",
            "CreateCustomRoutingListener",
            json!({
                "AcceleratorArn": standard_parent,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "IdempotencyToken": "cross-custom",
            }),
            format!(
                "Only Custom Routing resources are supported by this custom routing API; \
                 got standard accelerator: {standard_parent}"
            ),
        ),
        (
            "standard listener on a custom routing accelerator",
            "CreateListener",
            json!({
                "AcceleratorArn": custom_parent,
                "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                "Protocol": "TCP",
                "IdempotencyToken": "cross-standard",
            }),
            format!(
                "Custom Routing resources must use the Custom Routing APIs; got custom \
                 routing resource: {custom_parent}"
            ),
        ),
    ];
    for (case, operation, body, expected) in cross_parent {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "AcceleratorNotFoundException"),
            "case {case}: expected the parent not to be served by this API, got {status} {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            expected,
            "case {case}: expected the API-separation wording, got {parsed}"
        );
    }
}

/// RULE: a listener create whose parent accelerator already holds the documented
/// per-accelerator quota is refused with the modelled `LimitExceededException`, and the
/// quota is counted per accelerator — a sibling accelerator's own allowance is untouched.
#[tokio::test]
async fn a_create_past_the_per_accelerator_listener_quota_is_refused() {
    // Listeners per accelerator: 10 (AWS Global Accelerator service quotas).
    const LISTENER_QUOTA: usize = 10;

    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "quota").await;
    for index in 0..LISTENER_QUOTA {
        created_listener(
            &router,
            &accelerator_arn,
            &format!("quota-{index}"),
            json!([{ "FromPort": 7000 + index, "ToPort": 7000 + index }]),
        )
        .await;
    }

    let (status, body) = create_listener(
        &router,
        &accelerator_arn,
        "one-too-many",
        json!([{ "FromPort": 9000, "ToPort": 9000 }]),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, "LimitExceededException"),
        "case listener past the quota: got {status} {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("the accelerator already holds the maximum of {LISTENER_QUOTA} listeners"),
        "case listener past the quota: expected the quota to be stated, got {body}"
    );

    // A sibling accelerator's allowance is its own.
    let sibling = custom_routing_accelerator(&router, "quota-sibling").await;
    let created = created_listener(
        &router,
        &sibling,
        "sibling",
        json!([{ "FromPort": 9000, "ToPort": 9000 }]),
    )
    .await;
    assert_listener_arn("sibling", &sibling, &listener_arn_of(&created));
}

// ---------------------------------------------------------------------------
// Custom routing endpoint groups: the level below a custom routing listener.
// ---------------------------------------------------------------------------

/// Endpoint groups per accelerator: 10 (AWS Global Accelerator service quotas), counted
/// across every one of that accelerator's listeners.
const ENDPOINT_GROUP_QUOTA: usize = 10;

/// Amazon Web Services Region codes, used one per endpoint group because a listener holds
/// at most one group per Region. Real Region codes rather than made-up strings, so a row
/// reads like a call a client makes; `EndpointGroupRegion` is a `GenericString` (len
/// 0..255), so the service constrains none of them
/// (`smithy_query(query="operation", operation="CreateCustomRoutingEndpointGroup")`).
const REGIONS: [&str; ENDPOINT_GROUP_QUOTA] = [
    "us-east-1",
    "us-west-2",
    "eu-west-1",
    "eu-central-1",
    "ap-southeast-2",
    "ap-northeast-1",
    "sa-east-1",
    "ca-central-1",
    "af-south-1",
    "me-south-1",
];

/// `CreateCustomRoutingEndpointGroup` with the Region and destination configurations a row
/// names.
async fn create_endpoint_group(
    router: &MultiServiceRouter,
    listener_arn: &str,
    token: &str,
    region: &str,
    destinations: Value,
) -> (u16, Value) {
    call(
        router,
        "CreateCustomRoutingEndpointGroup",
        json!({
            "ListenerArn": listener_arn,
            "EndpointGroupRegion": region,
            "DestinationConfigurations": destinations,
            "IdempotencyToken": token,
        }),
    )
    .await
}

/// The created endpoint group, requiring a 200.
async fn created_endpoint_group(
    router: &MultiServiceRouter,
    listener_arn: &str,
    token: &str,
    region: &str,
    destinations: Value,
) -> Value {
    let (status, body) =
        create_endpoint_group(router, listener_arn, token, region, destinations).await;
    assert_eq!(
        status, 200,
        "case {token}: expected HTTP 200 from CreateCustomRoutingEndpointGroup, got {status} \
         with {body}"
    );
    body["EndpointGroup"].clone()
}

/// `DescribeCustomRoutingEndpointGroup` for one ARN.
async fn describe_endpoint_group(
    router: &MultiServiceRouter,
    endpoint_group_arn: &str,
) -> (u16, Value) {
    call(
        router,
        "DescribeCustomRoutingEndpointGroup",
        json!({ "EndpointGroupArn": endpoint_group_arn }),
    )
    .await
}

/// The ARN of an endpoint group a create or a describe answered.
fn endpoint_group_arn_of(group: &Value) -> String {
    group["EndpointGroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("EndpointGroupArn missing from {group}"))
        .to_string()
}

/// One destination configuration per port in `ports`, all TCP: the shape a row uses when
/// what it varies is the LENGTH of the list rather than any one entry.
fn destinations_over(ports: std::ops::Range<i32>) -> Value {
    Value::Array(
        ports
            .map(|port| json!({ "FromPort": port, "ToPort": port, "Protocols": ["TCP"] }))
            .collect(),
    )
}

/// An endpoint group ARN hangs off its parent listener's ARN, with a 12-hex-digit id: the
/// shape the service's own ARNs carry
/// (`.../listener/<8 hex>/endpoint-group/<12 hex>`).
fn assert_endpoint_group_arn(case: &str, listener_arn: &str, endpoint_group_arn: &str) {
    let id = endpoint_group_arn
        .strip_prefix(&format!("{listener_arn}/endpoint-group/"))
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected an endpoint group ARN under {listener_arn}, got \
                 {endpoint_group_arn:?}"
            )
        });
    assert_eq!(
        id.len(),
        12,
        "case {case}: expected a 12-digit hex endpoint group id, got {id:?}"
    );
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "case {case}: expected lowercase hex in the endpoint group id, got {id:?}"
    );
}

/// One custom routing accelerator with one listener on it, returning both ARNs.
async fn custom_routing_listener(router: &MultiServiceRouter, label: &str) -> (String, String) {
    let accelerator_arn = custom_routing_accelerator(router, label).await;
    let listener_arn = listener_arn_of(
        &created_listener(
            router,
            &accelerator_arn,
            &format!("{label}-lis"),
            json!([{ "FromPort": 80, "ToPort": 81 }]),
        )
        .await,
    );
    (accelerator_arn, listener_arn)
}

/// RULE: a create mints its resource from the service's own state — a fresh ARN under the
/// parent it names, carrying the members the request supplied — persists it, and the
/// matching describe reports exactly what was persisted, with only the members the
/// resource's own shape declares.
#[tokio::test]
async fn a_custom_routing_endpoint_group_is_minted_from_state_and_reads_back() {
    let router = emulator();
    let (_, listener_arn) = custom_routing_listener(&router, "egmint").await;

    // The rows differ along what the rule generalises over: how many destination
    // configurations the request carries (one, two, three, the documented maximum of 100),
    // how wide each range is (a single port, a span), where in the port space it sits (the
    // published bounds 1 and 65535 included) and which protocols it names (either one,
    // both). `DestinationConfigurations` is @length 1..100 of
    // `CustomRoutingDestinationConfiguration { FromPort, ToPort, Protocols }`, over
    // `PortNumber` @range 1..65535 and `CustomRoutingProtocols` @length 1..2 of
    // `CustomRoutingProtocol` (TCP|UDP) — `smithy_query(query="shape",
    // shape="CustomRoutingDestinationConfigurations,CustomRoutingDestinationConfiguration,
    // CustomRoutingProtocol")`.
    let cases: Vec<(&str, Value)> = vec![
        (
            "single-port-tcp",
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        ),
        (
            "span-udp",
            json!([{ "FromPort": 1024, "ToPort": 2048, "Protocols": ["UDP"] }]),
        ),
        (
            "both-protocols",
            json!([{ "FromPort": 443, "ToPort": 443, "Protocols": ["TCP", "UDP"] }]),
        ),
        (
            "two-destinations",
            json!([
                { "FromPort": 80, "ToPort": 81, "Protocols": ["TCP"] },
                { "FromPort": 5000, "ToPort": 5001, "Protocols": ["UDP"] },
            ]),
        ),
        (
            "three-destinations-at-the-bounds",
            json!([
                { "FromPort": 1, "ToPort": 1, "Protocols": ["TCP"] },
                { "FromPort": 65535, "ToPort": 65535, "Protocols": ["UDP"] },
                { "FromPort": 1, "ToPort": 65535, "Protocols": ["UDP", "TCP"] },
            ]),
        ),
        ("hundred-destinations", destinations_over(7000..7100)),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (index, (case, destinations)) in cases.into_iter().enumerate() {
        // One Region per row: a listener holds at most one endpoint group per Region.
        let region = REGIONS[index];
        let created = created_endpoint_group(
            &router,
            &listener_arn,
            case,
            region,
            destinations.clone(),
        )
        .await;
        let endpoint_group_arn = endpoint_group_arn_of(&created);

        assert_endpoint_group_arn(case, &listener_arn, &endpoint_group_arn);
        assert!(
            !seen.contains(&endpoint_group_arn),
            "case {case}: every create mints a distinct ARN, got {endpoint_group_arn:?}"
        );
        seen.push(endpoint_group_arn.clone());

        assert_eq!(
            created["EndpointGroupRegion"],
            json!(region),
            "case {case}: expected the Region the request named, got {created}"
        );
        assert_eq!(
            created["DestinationDescriptions"], destinations,
            "case {case}: expected the requested destinations, in order, got {created}"
        );
        // Endpoints (virtual private cloud subnets) join an endpoint group through
        // `AddCustomRoutingEndpoints`, so a freshly created group holds none — an empty
        // list, not a missing member.
        assert_eq!(
            created["EndpointDescriptions"],
            json!([]),
            "case {case}: expected a group with no endpoints yet, got {created}"
        );
        // `CustomRoutingEndpointGroup` models exactly four members: the custom routing
        // family has no health checks, no traffic dial and no port overrides.
        for absent in [
            "HealthCheckPort",
            "HealthCheckProtocol",
            "HealthCheckPath",
            "HealthCheckIntervalSeconds",
            "ThresholdCount",
            "TrafficDialPercentage",
            "PortOverrides",
        ] {
            assert!(
                created.get(absent).is_none(),
                "case {case}: `CustomRoutingEndpointGroup` models no {absent}, got {created}"
            );
        }

        // The round trip: the describe reports the persisted record, not the request.
        let (status, body) = describe_endpoint_group(&router, &endpoint_group_arn).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the created endpoint group to read back, got {status} {body}"
        );
        assert_eq!(
            body["EndpointGroup"], created,
            "case {case}: the describe must report the persisted record, got {body}"
        );
        // A read changes nothing a later caller sees.
        let (again_status, again) = describe_endpoint_group(&router, &endpoint_group_arn).await;
        assert_eq!(
            (again_status, &again["EndpointGroup"]),
            (200, &created),
            "case {case}: a second read must report the same record, got {again}"
        );
    }
}

/// RULE: a destination configuration is stored only when its port range names at least one
/// port — both ends present and running forwards, inside the model's own `PortNumber`
/// bounds — and a request that breaks the rule is refused and persists nothing.
#[tokio::test]
async fn a_destination_range_that_names_no_ports_is_refused() {
    let router = emulator();
    let (_, listener_arn) = custom_routing_listener(&router, "egports").await;

    // (case, DestinationConfigurations, expected status, expected error code)
    //
    // The bounds are the model's: `PortNumber` @range 1..65535,
    // `CustomRoutingDestinationConfigurations` @length 1..100 and `CustomRoutingProtocols`
    // @length 1..2 over the enum TCP|UDP. A value outside a bound the model declares, a
    // required member the request omits, or a value the enum does not name, is refused by
    // the boundary validator as this service's `InvalidArgumentException`; a range that
    // satisfies every declared bound yet names no ports reaches the handler, which answers
    // the modelled `InvalidPortRangeException`.
    let cases: Vec<(&str, Value, u16, &str)> = vec![
        (
            "upside-down",
            json!([{ "FromPort": 8080, "ToPort": 80, "Protocols": ["TCP"] }]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "upside-down-beyond-the-first-destination",
            json!([
                { "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] },
                { "FromPort": 500, "ToPort": 400, "Protocols": ["UDP"] },
            ]),
            400,
            "InvalidPortRangeException",
        ),
        (
            "missing-to-port",
            json!([{ "FromPort": 80, "Protocols": ["TCP"] }]),
            400,
            "InvalidArgumentException",
        ),
        (
            "missing-from-port",
            json!([{ "ToPort": 80, "Protocols": ["TCP"] }]),
            400,
            "InvalidArgumentException",
        ),
        (
            "missing-protocols",
            json!([{ "FromPort": 80, "ToPort": 80 }]),
            400,
            "InvalidArgumentException",
        ),
        // One past each published bound of `PortNumber`.
        (
            "port-zero",
            json!([{ "FromPort": 0, "ToPort": 80, "Protocols": ["TCP"] }]),
            400,
            "InvalidArgumentException",
        ),
        (
            "port-past-the-maximum",
            json!([{ "FromPort": 80, "ToPort": 65536, "Protocols": ["TCP"] }]),
            400,
            "InvalidArgumentException",
        ),
        // One past each published bound of the two lists.
        (
            "no-destinations",
            json!([]),
            400,
            "InvalidArgumentException",
        ),
        (
            "hundred-and-one-destinations",
            destinations_over(8000..8101),
            400,
            "InvalidArgumentException",
        ),
        (
            "no-protocols",
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": [] }]),
            400,
            "InvalidArgumentException",
        ),
        (
            "three-protocols",
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP", "UDP", "TCP"] }]),
            400,
            "InvalidArgumentException",
        ),
        // A protocol the enum does not name.
        (
            "unmodelled-protocol",
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["SCTP"] }]),
            400,
            "InvalidArgumentException",
        ),
    ];

    for (case, destinations, expected_status, expected_code) in cases {
        let (status, body) = create_endpoint_group(
            &router,
            &listener_arn,
            case,
            REGIONS[0],
            destinations,
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (expected_status, expected_code),
            "case {case}: expected HTTP {expected_status} {expected_code}, got {status} {body}"
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {case}: expected a populated Message, got {body}"
        );
    }

    // The upside-down range's own wording, the sentence this service states that violation
    // with wherever a port range reaches it.
    let (_, refused) = create_endpoint_group(
        &router,
        &listener_arn,
        "wording",
        REGIONS[0],
        json!([{ "FromPort": 443, "ToPort": 80, "Protocols": ["TCP"] }]),
    )
    .await;
    assert_eq!(
        error_message(&refused),
        "FromPort should be less than or equal to ToPort",
        "case wording: expected the port-range rule to be stated, got {refused}"
    );

    // Nothing above was persisted: the Region every refused request named is still free,
    // so a destination the service accepts mints the FIRST group of this listener.
    let created = created_endpoint_group(
        &router,
        &listener_arn,
        "after-refusals",
        REGIONS[0],
        json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
    )
    .await;
    assert_endpoint_group_arn(
        "after-refusals",
        &listener_arn,
        &endpoint_group_arn_of(&created),
    );
}

/// RULE: a listener holds at most one endpoint group per Amazon Web Services Region, so a
/// create naming a Region one of its groups already covers is refused — while the same
/// Region under a different listener is a different collection.
#[tokio::test]
async fn an_endpoint_group_region_is_unique_within_its_listener() {
    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "egregion").await;
    let first_listener = listener_arn_of(
        &created_listener(
            &router,
            &accelerator_arn,
            "region-one",
            json!([{ "FromPort": 80, "ToPort": 80 }]),
        )
        .await,
    );
    let second_listener = listener_arn_of(
        &created_listener(
            &router,
            &accelerator_arn,
            "region-two",
            json!([{ "FromPort": 443, "ToPort": 443 }]),
        )
        .await,
    );

    // The rows differ in the Region and in the destinations the second request carries, so
    // the conflict is tested as a property of the Region rather than of one body.
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "same-destinations",
            REGIONS[0],
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        ),
        (
            "other-destinations",
            REGIONS[1],
            json!([{ "FromPort": 9000, "ToPort": 9100, "Protocols": ["UDP"] }]),
        ),
    ];

    for (case, region, destinations) in cases {
        let first = created_endpoint_group(
            &router,
            &first_listener,
            &format!("{case}-first"),
            region,
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        )
        .await;

        let (status, body) = create_endpoint_group(
            &router,
            &first_listener,
            &format!("{case}-again"),
            region,
            destinations.clone(),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "EndpointGroupAlreadyExistsException"),
            "case {case}: expected the Region to be taken on this listener, got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            "Endpoint group already exists.",
            "case {case}: expected the conflict to be stated, got {body}"
        );

        // The refusal changed nothing: the group that holds the Region is still the first.
        let (read_status, read) =
            describe_endpoint_group(&router, &endpoint_group_arn_of(&first)).await;
        assert_eq!(
            (read_status, &read["EndpointGroup"]),
            (200, &first),
            "case {case}: expected the first group to survive the refusal, got {read}"
        );

        // The same Region under a DIFFERENT listener is a different collection.
        let sibling = created_endpoint_group(
            &router,
            &second_listener,
            &format!("{case}-sibling"),
            region,
            destinations,
        )
        .await;
        assert_endpoint_group_arn(case, &second_listener, &endpoint_group_arn_of(&sibling));
    }
}

/// RULE: an `IdempotencyToken` identifies one create request within its own operation and
/// its own parent — repeating that request returns the resource the token already minted,
/// a different token mints another, and the same token sent to another create or against
/// another parent does not replay this one.
#[tokio::test]
async fn an_idempotency_token_replays_only_its_own_endpoint_group_create() {
    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "egreplay").await;
    let first_parent = listener_arn_of(
        &created_listener(
            &router,
            &accelerator_arn,
            "replay-one",
            json!([{ "FromPort": 80, "ToPort": 80 }]),
        )
        .await,
    );
    let second_parent = listener_arn_of(
        &created_listener(
            &router,
            &accelerator_arn,
            "replay-two",
            json!([{ "FromPort": 443, "ToPort": 443 }]),
        )
        .await,
    );

    // The rows differ in the destinations the repeated request carries, so the replay is
    // tested as a property of the token rather than of one body.
    let cases: Vec<(&str, Value)> = vec![
        (
            "single",
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        ),
        (
            "span",
            json!([{ "FromPort": 1024, "ToPort": 4096, "Protocols": ["UDP"] }]),
        ),
        (
            "two-destinations",
            json!([
                { "FromPort": 53, "ToPort": 53, "Protocols": ["UDP"] },
                { "FromPort": 443, "ToPort": 444, "Protocols": ["TCP", "UDP"] },
            ]),
        ),
    ];

    for (index, (case, destinations)) in cases.into_iter().enumerate() {
        let token = format!("replay-{case}");
        let region = REGIONS[index];
        let first = created_endpoint_group(
            &router,
            &first_parent,
            &token,
            region,
            destinations.clone(),
        )
        .await;
        let again = created_endpoint_group(
            &router,
            &first_parent,
            &token,
            region,
            destinations.clone(),
        )
        .await;
        assert_eq!(
            again, first,
            "case {case}: the same token must replay the group it minted, got {again}"
        );

        // A different token is a different create request: another Region, because a
        // listener holds one group per Region.
        let fresh = created_endpoint_group(
            &router,
            &first_parent,
            &format!("{token}-other"),
            REGIONS[index + 3],
            destinations.clone(),
        )
        .await;
        assert_ne!(
            endpoint_group_arn_of(&fresh),
            endpoint_group_arn_of(&first),
            "case {case}: a different token mints a different endpoint group, got {fresh}"
        );

        // The same token against a DIFFERENT parent is a different create request.
        let other_parent =
            created_endpoint_group(&router, &second_parent, &token, region, destinations).await;
        assert_ne!(
            endpoint_group_arn_of(&other_parent),
            endpoint_group_arn_of(&first),
            "case {case}: a token is scoped to its parent listener, got {other_parent}"
        );
    }

    // One token, two different endpoint group creates: each kind of endpoint group is its
    // own resource, so neither replays the other's.
    let standard_accelerator = arn_of(&create_accelerator(&router, "std-egreplay", "std-eg").await);
    let standard_listener = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": standard_accelerator,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": "std-eg-lis",
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("the create answers a ListenerArn")
        .to_string();
    let shared = "shared-endpoint-group-token";
    let standard = ok(
        &router,
        "CreateEndpointGroup",
        json!({
            "ListenerArn": standard_listener,
            "EndpointGroupRegion": REGIONS[0],
            "IdempotencyToken": shared,
        }),
    )
    .await["EndpointGroup"]
        .clone();
    let (_, third_parent) = custom_routing_listener(&router, "egreplay-cross").await;
    let custom = created_endpoint_group(
        &router,
        &third_parent,
        shared,
        REGIONS[0],
        json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
    )
    .await;
    assert_ne!(
        endpoint_group_arn_of(&custom),
        endpoint_group_arn_of(&standard),
        "case cross-kind token: a custom routing create must not replay the standard \
         endpoint group, got {custom}"
    );
}

/// RULE: an endpoint group create whose parent accelerator already holds the documented
/// per-accelerator quota is refused with the modelled `LimitExceededException`, and the
/// quota is counted per accelerator across all of its listeners — a sibling accelerator's
/// own allowance is untouched.
#[tokio::test]
async fn a_create_past_the_per_accelerator_endpoint_group_quota_is_refused() {
    let router = emulator();
    let accelerator_arn = custom_routing_accelerator(&router, "egquota").await;
    // Two listeners, so the quota is shown to be the ACCELERATOR's and not a listener's.
    let mut listeners: Vec<String> = Vec::new();
    for index in 0..2 {
        listeners.push(listener_arn_of(
            &created_listener(
                &router,
                &accelerator_arn,
                &format!("quota-lis-{index}"),
                json!([{ "FromPort": 6000 + index, "ToPort": 6000 + index }]),
            )
            .await,
        ));
    }
    for (index, region) in REGIONS.iter().enumerate() {
        created_endpoint_group(
            &router,
            &listeners[index % listeners.len()],
            &format!("quota-{index}"),
            region,
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        )
        .await;
    }

    let (status, body) = create_endpoint_group(
        &router,
        &listeners[0],
        "one-too-many",
        "us-east-2",
        json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (403, "LimitExceededException"),
        "case endpoint group past the quota: got {status} {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Accelerator {accelerator_arn} already holds the maximum of \
             {ENDPOINT_GROUP_QUOTA} endpoint groups."
        ),
        "case endpoint group past the quota: expected the quota to be stated, got {body}"
    );

    // A sibling accelerator's allowance is its own.
    let (_, sibling_listener) = custom_routing_listener(&router, "egquota-sibling").await;
    let created = created_endpoint_group(
        &router,
        &sibling_listener,
        "sibling",
        REGIONS[0],
        json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
    )
    .await;
    assert_endpoint_group_arn(
        "sibling",
        &sibling_listener,
        &endpoint_group_arn_of(&created),
    );
}

/// RULE: the two kinds of endpoint group are distinct resources — each hangs off a listener
/// of its own kind and is reported only by the operations of its own kind, and a reference
/// of the other kind is refused as a resource this API family does not serve, while an ARN
/// of neither kind keeps the absent-resource wording.
#[tokio::test]
async fn each_endpoint_group_kind_is_reached_only_through_its_own_operations() {
    let router = emulator();
    let (_, custom_listener) = custom_routing_listener(&router, "egkinds").await;
    let standard_accelerator = arn_of(&create_accelerator(&router, "std-egkinds", "std-k").await);
    let standard_listener = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": standard_accelerator,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": "std-k-lis",
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("the create answers a ListenerArn")
        .to_string();

    let custom_group = endpoint_group_arn_of(
        &created_endpoint_group(
            &router,
            &custom_listener,
            "kind-custom",
            REGIONS[0],
            json!([{ "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] }]),
        )
        .await,
    );
    let standard_group = ok(
        &router,
        "CreateEndpointGroup",
        json!({
            "ListenerArn": standard_listener,
            "EndpointGroupRegion": REGIONS[0],
            "IdempotencyToken": "kind-standard",
        }),
    )
    .await["EndpointGroup"]["EndpointGroupArn"]
        .as_str()
        .expect("the create answers an EndpointGroupArn")
        .to_string();
    let unminted = format!("{custom_listener}/endpoint-group/feedfacefeed");

    // Every cross-kind read is a row, plus an ARN that names nothing at all. The wording of
    // a CROSSING — an ARN that names a real endpoint group of the other family — is the
    // API-separation sentence [`super::cross_family_resolution`] pins over the whole class.
    let cases: Vec<(&str, &str, &str, String)> = vec![
        (
            "custom via DescribeEndpointGroup",
            "DescribeEndpointGroup",
            &custom_group,
            format!(
                "Custom Routing resources must use the Custom Routing APIs; got custom \
                 routing resource: {custom_group}"
            ),
        ),
        (
            "standard via DescribeCustomRoutingEndpointGroup",
            "DescribeCustomRoutingEndpointGroup",
            &standard_group,
            format!(
                "Only Custom Routing resources are supported by this custom routing API; \
                 got standard accelerator: {standard_group}"
            ),
        ),
        (
            "unminted via DescribeCustomRoutingEndpointGroup",
            "DescribeCustomRoutingEndpointGroup",
            &unminted,
            format!("EndpointGroup {unminted} does not exist."),
        ),
        (
            "unminted via DescribeEndpointGroup",
            "DescribeEndpointGroup",
            &unminted,
            format!("EndpointGroup {unminted} does not exist."),
        ),
    ];
    for (case, operation, endpoint_group_arn, expected) in cases {
        let (status, body) = call(
            &router,
            operation,
            json!({ "EndpointGroupArn": endpoint_group_arn }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "EndpointGroupNotFoundException"),
            "case {case}: expected this API family not to serve the ARN, got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case}: expected the refusal to name the ARN the request carried, got {body}"
        );
    }

    // And each kind reads back through its own describe.
    let (status, body) = describe_endpoint_group(&router, &custom_group).await;
    assert_eq!(
        (
            status,
            body["EndpointGroup"]["EndpointGroupArn"]
                .as_str()
                .unwrap_or_default()
        ),
        (200, custom_group.as_str()),
        "case custom describe: expected {custom_group} to read back, got {body}"
    );
    let read_standard = ok(
        &router,
        "DescribeEndpointGroup",
        json!({ "EndpointGroupArn": standard_group }),
    )
    .await;
    assert_eq!(
        read_standard["EndpointGroup"]["HealthCheckProtocol"],
        json!("TCP"),
        "case standard describe: expected the standard group's own members, \
         got {read_standard}"
    );

    // A create's parent must be a listener of its own kind, whichever way round. A parent
    // ARN that names a listener of the OTHER family is refused as the family crossing it
    // is, naming the ARN the request carried; one that names no listener at all keeps the
    // absent-resource wording.
    let unminted_listener = format!("{standard_accelerator}/listener/deadbeef");
    let parents: Vec<(&str, &str, Value, String)> = vec![
        (
            "custom group on a standard listener",
            "CreateCustomRoutingEndpointGroup",
            json!({
                "ListenerArn": standard_listener,
                "EndpointGroupRegion": REGIONS[1],
                "DestinationConfigurations": [
                    { "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] },
                ],
                "IdempotencyToken": "cross-custom",
            }),
            format!(
                "Only Custom Routing resources are supported by this custom routing API; \
                 got standard accelerator: {standard_listener}"
            ),
        ),
        (
            "standard group on a custom routing listener",
            "CreateEndpointGroup",
            json!({
                "ListenerArn": custom_listener,
                "EndpointGroupRegion": REGIONS[1],
                "IdempotencyToken": "cross-standard",
            }),
            format!(
                "Custom Routing resources must use the Custom Routing APIs; got custom \
                 routing resource: {custom_listener}"
            ),
        ),
        (
            "custom group on a listener of neither family",
            "CreateCustomRoutingEndpointGroup",
            json!({
                "ListenerArn": unminted_listener,
                "EndpointGroupRegion": REGIONS[1],
                "DestinationConfigurations": [
                    { "FromPort": 80, "ToPort": 80, "Protocols": ["TCP"] },
                ],
                "IdempotencyToken": "cross-absent",
            }),
            format!("Listener {unminted_listener} not found."),
        ),
    ];
    for (case, operation, body, expected) in parents {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ListenerNotFoundException"),
            "case {case}: expected the parent not to be served by this API, got {status} {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            expected,
            "case {case}: expected the parent-lookup wording, got {parsed}"
        );
    }
}
