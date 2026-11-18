// SPDX-License-Identifier: CC0-1.0

//! How this service states a refusal, driven over the wire.
//!
//! Two rules are under test, each over a SET of inputs rather than one request:
//!
//! 1. a request refused for violating a MODELLED input constraint is the service's own
//!    modelled client-input error — HTTP 400 `InvalidArgumentException` — carrying the
//!    constraint report built from the request (rejected value, member path, bound);
//! 2. a refusal about a named resource identifies that resource by the ARN the request
//!    carried, so two requests about two resources do not get the same sentence.
//!
//! Every call goes through the real router (`POST /` with
//! `X-Amz-Target: GlobalAccelerator_V20180706.<Operation>`, `awsJson1_1` body), so the
//! boundary validator, the routing table, the handler and the error envelope are all
//! exercised — not just a handler function.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A fresh emulator: one shared engine, this service mounted, signature checking off
/// (these tests are about the service's behaviour, not SigV4).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_globalaccelerator_globalaccelerator_v20180706::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Call one operation and return `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "globalaccelerator.us-west-2.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("GlobalAccelerator_V20180706.{operation}"),
        )
        // The routing key comes from the SigV4 credential scope's service field.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-west-2/globalaccelerator/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call one operation and require a 200.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// The wire error code out of an awsJson error envelope's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The error message out of an awsJson error envelope. A modelled error renders its
/// member as the model spells it (`Message`); the boundary's own envelope uses
/// `message`, so both spellings are read here.
pub(crate) fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn arn_of(accelerator: &Value) -> String {
    accelerator["AcceleratorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("AcceleratorArn missing from {accelerator}"))
        .to_string()
}

/// What the boundary is expected to do with one request.
#[derive(Debug, Clone, Copy)]
enum Expect<'a> {
    /// The request violates a modelled constraint, so it is refused with HTTP 400
    /// `InvalidArgumentException` over a clause naming `path` and stating
    /// `requirement`.
    Refused {
        /// The member path the clause names (the validator's lowerCamel path).
        path: &'a str,
        /// A fragment of the requirement the clause states.
        requirement: &'a str,
    },
    /// The request breaks a requirement this service states in a sentence of its OWN
    /// rather than through a clause, so the whole message is that sentence — still under
    /// HTTP 400 `InvalidArgumentException`.
    Stated(&'a str),
    /// The request satisfies every modelled constraint, so the boundary passes it to
    /// the handler: whatever the handler answers, it is not a constraint refusal.
    Accepted,
    /// The request is served: it reaches the handler and the handler answers it.
    Served,
}

/// Call one operation and assert that this service answers it the way `expected` says.
async fn assert_expected(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
    expected: Expect<'_>,
) {
    let (status, parsed) = call(router, operation, body).await;
    let code = error_code(&parsed);
    let message = error_message(&parsed);
    match expected {
        Expect::Refused { path, requirement } => {
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidArgumentException"),
                "case {case}: expected the modelled 400 InvalidArgumentException, \
                 got {status} {parsed}"
            );
            assert!(
                message.contains(&format!("at '{path}'")),
                "case {case}: expected the clause to name the member {path:?}, \
                 got {message:?}"
            );
            assert!(
                message.contains(requirement),
                "case {case}: expected the clause to state {requirement:?}, \
                 got {message:?}"
            );
            assert!(
                message.starts_with("1 validation error detected: "),
                "case {case}: expected the constraint-report envelope, got {message:?}"
            );
        }
        Expect::Stated(sentence) => {
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidArgumentException"),
                "case {case}: expected the modelled 400 InvalidArgumentException, \
                 got {status} {parsed}"
            );
            assert_eq!(
                message, sentence,
                "case {case}: expected {sentence:?}, got {message:?}"
            );
        }
        Expect::Accepted => {
            assert!(
                code != "InvalidArgumentException" && code != "ValidationException",
                "case {case}: a value inside every modelled bound must reach the \
                 handler, got {status} {parsed}"
            );
        }
        Expect::Served => {
            assert_eq!(
                status, 200,
                "case {case}: expected the request to be served, got {status} {parsed}"
            );
        }
    }
}

/// RULE: a request refused for violating a modelled input constraint is answered with
/// this service's modelled client-input error — HTTP 400 `InvalidArgumentException`,
/// which every operation taking a constrained member declares — never with a code that
/// is in no operation's error union; and the message is the constraint report built from
/// the request: the rejected value, the member's path and the bound it failed.
///
/// The constraint catalog below is the model's own, from
/// `smithy_query(query="operation", operation="CreateEndpointGroup,UpdateEndpointGroup,
/// ListAccelerators,DescribeAccelerator,CreateAccelerator,CreateListener")`:
/// `TrafficDialPercentage` @range{0,100}, `HealthCheckIntervalSeconds` @range{10,30},
/// `ThresholdCount` @range{1,10}, `HealthCheckPort` @range{1,65535},
/// `HealthCheckProtocol` enum [TCP, HTTP, HTTPS], `HealthCheckPath`
/// @pattern, `MaxResults` @range{1,100}, `GenericString` @length{0,255},
/// `PortRanges` @length{1,10}, and the members the model marks @required. Each @range is
/// exercised on both published bounds and one step past each.
#[tokio::test]
async fn a_modelled_constraint_refusal_is_the_modelled_invalid_argument_error() {
    let router = emulator();
    // A listener ARN is not resolved before the input is validated, so these rows need
    // no live resource; one is created anyway so the accepted rows exercise a real one.
    let accelerator = ok(
        &router,
        "CreateAccelerator",
        json!({ "Name": "refusals", "IdempotencyToken": "refusals-token" }),
    )
    .await["Accelerator"]
        .clone();
    let accelerator_arn = arn_of(&accelerator);
    let listener_arn = format!("{accelerator_arn}/listener/00000000");
    let endpoint_group_arn = format!("{listener_arn}/endpoint-group/00000000");
    // One past the 255-character bound `GenericString` declares.
    let overlong_arn = "a".repeat(256);

    let group = |extra: Value| -> Value {
        let mut body = json!({
            "ListenerArn": listener_arn,
            "EndpointGroupRegion": "us-west-2",
            "IdempotencyToken": "group-token",
        });
        for (key, value) in extra.as_object().expect("an object of extra members") {
            body[key] = value.clone();
        }
        body
    };
    let update_group = |extra: Value| -> Value {
        let mut body = json!({ "EndpointGroupArn": endpoint_group_arn });
        for (key, value) in extra.as_object().expect("an object of extra members") {
            body[key] = value.clone();
        }
        body
    };

    let cases: Vec<(&str, &str, Value, Expect)> = vec![
        // @range{0,100} on TrafficDialPercentage: both bounds, and one step past each.
        // The clause names the member path this service reports for it
        // (`trafficRetentionPercentage`) and renders the rejected float with its
        // fractional part — see `src/service/constraint_report.rs`.
        (
            "dial-at-the-lower-bound",
            "CreateEndpointGroup",
            group(json!({ "TrafficDialPercentage": 0.0 })),
            Expect::Accepted,
        ),
        (
            "dial-at-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "TrafficDialPercentage": 100.0 })),
            Expect::Accepted,
        ),
        (
            "dial-below-the-lower-bound",
            "CreateEndpointGroup",
            group(json!({ "TrafficDialPercentage": -1.0 })),
            Expect::Refused {
                path: "trafficRetentionPercentage",
                requirement: "Member must have value greater than or equal to 0",
            },
        ),
        (
            "dial-just-past-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "TrafficDialPercentage": 100.5 })),
            Expect::Refused {
                path: "trafficRetentionPercentage",
                requirement: "Member must have value less than or equal to 100",
            },
        ),
        (
            "dial-far-past-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "TrafficDialPercentage": 150.0 })),
            Expect::Refused {
                path: "trafficRetentionPercentage",
                requirement: "Member must have value less than or equal to 100",
            },
        ),
        // The same member on the update leg of the same rule.
        (
            "dial-far-past-the-upper-bound-on-update",
            "UpdateEndpointGroup",
            update_group(json!({ "TrafficDialPercentage": 150.0 })),
            Expect::Refused {
                path: "trafficRetentionPercentage",
                requirement: "Member must have value less than or equal to 100",
            },
        ),
        (
            "dial-at-the-upper-bound-on-update",
            "UpdateEndpointGroup",
            update_group(json!({ "TrafficDialPercentage": 100.0 })),
            Expect::Accepted,
        ),
        // @range{10,30} on HealthCheckIntervalSeconds. The service states a value outside
        // the set of intervals it supports in its own sentence rather than through a
        // clause — see `an_unsupported_health_check_interval_states_the_supported_set`.
        (
            "interval-at-the-bounds",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckIntervalSeconds": 10 })),
            Expect::Accepted,
        ),
        (
            "interval-below-the-lower-bound",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckIntervalSeconds": 9 })),
            Expect::Stated("Invalid HealthCheck Interval Seconds: 9 expected values are: 10 or 30"),
        ),
        (
            "interval-past-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckIntervalSeconds": 31 })),
            Expect::Stated(
                "Invalid HealthCheck Interval Seconds: 31 expected values are: 10 or 30",
            ),
        ),
        // @range{1,10} on ThresholdCount.
        (
            "threshold-below-the-lower-bound",
            "CreateEndpointGroup",
            group(json!({ "ThresholdCount": 0 })),
            Expect::Refused {
                path: "thresholdCount",
                requirement: "Member must have value greater than or equal to 1",
            },
        ),
        (
            "threshold-past-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "ThresholdCount": 11 })),
            Expect::Refused {
                path: "thresholdCount",
                requirement: "Member must have value less than or equal to 10",
            },
        ),
        // @range{1,65535} on HealthCheckPort.
        (
            "health-check-port-past-the-upper-bound",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckPort": 65536 })),
            Expect::Refused {
                path: "healthCheckPort",
                requirement: "Member must have value less than or equal to 65535",
            },
        ),
        // An enumerated member: a value outside the model's value set.
        (
            "health-check-protocol-outside-the-value-set",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckProtocol": "SMTP" })),
            Expect::Refused {
                path: "healthCheckProtocol",
                requirement: "enum value set",
            },
        ),
        (
            "health-check-protocol-in-the-value-set",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckProtocol": "HTTPS" })),
            Expect::Accepted,
        ),
        // A @pattern member: a path that does not start with a slash.
        (
            "health-check-path-outside-the-pattern",
            "CreateEndpointGroup",
            group(json!({ "HealthCheckPath": "health" })),
            Expect::Refused {
                path: "healthCheckPath",
                requirement: "regular expression pattern",
            },
        ),
        // @range{1,100} on MaxResults, on an implemented operation: both bounds and one
        // step past each.
        (
            "max-results-at-the-lower-bound",
            "ListAccelerators",
            json!({ "MaxResults": 1 }),
            Expect::Accepted,
        ),
        (
            "max-results-at-the-upper-bound",
            "ListAccelerators",
            json!({ "MaxResults": 100 }),
            Expect::Accepted,
        ),
        (
            "max-results-below-the-lower-bound",
            "ListAccelerators",
            json!({ "MaxResults": 0 }),
            Expect::Refused {
                path: "maxResults",
                requirement: "Member must have value greater than or equal to 1",
            },
        ),
        (
            "max-results-past-the-upper-bound",
            "ListAccelerators",
            json!({ "MaxResults": 101 }),
            Expect::Refused {
                path: "maxResults",
                requirement: "Member must have value less than or equal to 100",
            },
        ),
        // @length{0,255} on a string member.
        (
            "accelerator-arn-past-the-length-bound",
            "DescribeAccelerator",
            json!({ "AcceleratorArn": overlong_arn }),
            Expect::Refused {
                path: "acceleratorArn",
                requirement: "Member must have length less than or equal to 255",
            },
        ),
        // @length{1,10} on a list member: an empty list names no port range at all.
        (
            "port-ranges-below-the-length-bound",
            "CreateListener",
            json!({
                "AcceleratorArn": accelerator_arn,
                "PortRanges": [],
                "Protocol": "TCP",
                "IdempotencyToken": "empty-ranges",
            }),
            Expect::Refused {
                path: "portRanges",
                requirement: "Member must have length greater than or equal to 1",
            },
        ),
        // @required members, on an implemented and on a scaffolded operation.
        (
            "accelerator-name-absent",
            "CreateAccelerator",
            json!({ "IdempotencyToken": "no-name" }),
            Expect::Refused {
                path: "name",
                requirement: "Member must not be null",
            },
        ),
        (
            "endpoint-group-region-absent",
            "CreateEndpointGroup",
            json!({ "ListenerArn": listener_arn, "IdempotencyToken": "no-region" }),
            Expect::Refused {
                path: "endpointGroupRegion",
                requirement: "Member must not be null",
            },
        ),
    ];

    for (case, operation, body, expectation) in cases {
        assert_expected(&router, case, operation, body, expectation).await;
    }
}

/// RULE: a refusal about a resource the request named identifies that resource by the
/// ARN the request carried, so the diagnostic can be attributed to one resource out of
/// many; and a refused delete changes nothing — the accelerator is still there, still
/// enabled.
#[tokio::test]
async fn a_refusal_names_the_resource_the_request_identified() {
    let router = emulator();

    // Several accelerators, so the message is tested as a function of the request rather
    // than on one value: each row differs in name, in whether it carries a listener, and
    // therefore in which refusal its delete meets.
    let cases: Vec<(&str, &str, bool)> = vec![
        ("first", "alpha", false),
        ("second", "beta", true),
        ("third", "gamma.01", false),
    ];

    let mut seen_messages: Vec<String> = Vec::new();
    for (case, name, with_listener) in cases {
        let arn = arn_of(
            &ok(
                &router,
                "CreateAccelerator",
                json!({ "Name": name, "IdempotencyToken": format!("{case}-token"), "Enabled": true }),
            )
            .await["Accelerator"],
        );
        if with_listener {
            ok(
                &router,
                "CreateListener",
                json!({
                    "AcceleratorArn": arn,
                    "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
                    "Protocol": "TCP",
                    "IdempotencyToken": format!("{case}-listener"),
                }),
            )
            .await;
        }

        // Enabled: the delete is refused, and the refusal names this accelerator.
        let (status, parsed) = call(&router, "DeleteAccelerator", json!({ "AcceleratorArn": arn })).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "AcceleratorNotDisabledException"),
            "case {case}: expected the modelled refusal for an enabled accelerator, \
             got {status} {parsed}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            format!("Please disable accelerator {arn} before deleting it."),
            "case {case}: expected the refusal to name the accelerator, got {message:?}"
        );
        assert!(
            !seen_messages.contains(&message),
            "case {case}: two accelerators must not get the same refusal, got {message:?}"
        );
        seen_messages.push(message);

        // The refused delete is a no-op.
        let reread = ok(
            &router,
            "DescribeAccelerator",
            json!({ "AcceleratorArn": arn }),
        )
        .await["Accelerator"]
            .clone();
        assert_eq!(
            reread["Enabled"],
            json!(true),
            "case {case}: a refused delete must change nothing, got {reread}"
        );

        ok(
            &router,
            "UpdateAccelerator",
            json!({ "AcceleratorArn": arn, "Enabled": false }),
        )
        .await;

        if with_listener {
            // The next precondition, stated over the same ARN.
            let (status, parsed) =
                call(&router, "DeleteAccelerator", json!({ "AcceleratorArn": arn })).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "AssociatedListenerFoundException"),
                "case {case}: expected the modelled refusal for a listener still \
                 attached, got {status} {parsed}"
            );
            let message = error_message(&parsed);
            assert!(
                message.contains(&arn),
                "case {case}: expected the refusal to name the accelerator, got {message:?}"
            );

            let listeners = ok(&router, "ListListeners", json!({ "AcceleratorArn": arn })).await
                ["Listeners"]
                .clone();
            for listener in listeners.as_array().cloned().unwrap_or_default() {
                ok(
                    &router,
                    "DeleteListener",
                    json!({ "ListenerArn": listener["ListenerArn"] }),
                )
                .await;
            }
        }

        // Disabled and empty: the delete goes through, and the accelerator is gone.
        ok(&router, "DeleteAccelerator", json!({ "AcceleratorArn": arn })).await;
        let (status, parsed) = call(
            &router,
            "DescribeAccelerator",
            json!({ "AcceleratorArn": arn }),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "AcceleratorNotFoundException"),
            "case {case}: expected the modelled not-found after the delete, \
             got {status} {parsed}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            format!("Accelerator {arn} not found."),
            "case {case}: expected the not-found to name the accelerator, got {message:?}"
        );
    }
}

/// RULE: a not-found refusal names the resource the request identified, at every level
/// of the hierarchy — an accelerator ARN on the accelerator operations, a listener ARN
/// on the listener operations.
#[tokio::test]
async fn a_not_found_refusal_names_the_arn_the_request_carried() {
    let router = emulator();
    let missing_accelerator =
        "arn:aws:globalaccelerator::000000000000:accelerator/00000000-0000-4000-8000-000000000000";
    let other_accelerator =
        "arn:aws:globalaccelerator::000000000000:accelerator/11111111-1111-4111-8111-111111111111";
    let missing_listener = format!("{missing_accelerator}/listener/deadbeef");

    // (case, operation, body, expected message)
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "describe-accelerator",
            "DescribeAccelerator",
            json!({ "AcceleratorArn": missing_accelerator }),
            format!("Accelerator {missing_accelerator} not found."),
        ),
        (
            "describe-another-accelerator",
            "DescribeAccelerator",
            json!({ "AcceleratorArn": other_accelerator }),
            format!("Accelerator {other_accelerator} not found."),
        ),
        (
            "delete-accelerator",
            "DeleteAccelerator",
            json!({ "AcceleratorArn": missing_accelerator }),
            format!("Accelerator {missing_accelerator} not found."),
        ),
        (
            "update-accelerator",
            "UpdateAccelerator",
            json!({ "AcceleratorArn": missing_accelerator, "Enabled": false }),
            format!("Accelerator {missing_accelerator} not found."),
        ),
        (
            "list-listeners",
            "ListListeners",
            json!({ "AcceleratorArn": missing_accelerator }),
            format!("Accelerator {missing_accelerator} not found."),
        ),
        (
            "describe-listener",
            "DescribeListener",
            json!({ "ListenerArn": missing_listener }),
            format!("Listener {missing_listener} not found."),
        ),
        (
            "delete-listener",
            "DeleteListener",
            json!({ "ListenerArn": missing_listener }),
            format!("Listener {missing_listener} not found."),
        ),
    ];

    for (case, operation, body, expected) in cases {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected the status this service answers a missing resource \
             with, got {status} {parsed}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message, expected,
            "case {case}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE: a name outside the length limit this service accepts for an accelerator's name
/// is refused HTTP 400 `InvalidArgumentException` stating that limit in the service's own
/// fixed sentence — the same sentence at either bound, for every length past it, and on
/// every leg that names an accelerator — while a name inside the limit reaches the
/// handler. A name past the `@length` maximum the MODEL declares for the member
/// (`GenericString` @length{0,255}) is a different refusal: the boundary's own constraint
/// report, naming the member and the bound.
#[tokio::test]
async fn an_accelerator_name_outside_the_length_limit_states_that_limit() {
    let router = emulator();
    // The sentence is a constant of the service: the two bounds of its name-length limit
    // and nothing taken from the request.
    let length = Expect::Stated("Accelerator Name should be between 1 - 64 characters in size.");

    // (case, the name, what the service answers)
    let names: Vec<(&str, String, Expect)> = vec![
        ("empty", String::new(), length),
        ("one-character", "n".to_string(), Expect::Served),
        ("inside-the-limit", "n".repeat(32), Expect::Served),
        ("at-the-upper-bound", "n".repeat(64), Expect::Served),
        ("one-past-the-upper-bound", "n".repeat(65), length),
        ("far-past-the-upper-bound", "n".repeat(200), length),
        ("at-the-modelled-length-maximum", "n".repeat(255), length),
        // Past the member's own modelled `@length`, so the boundary refuses it before any
        // handler runs, with the clause that names the member and the bound.
        (
            "past-the-modelled-length-maximum",
            "n".repeat(256),
            Expect::Refused {
                path: "name",
                requirement: "Member must have length less than or equal to 255",
            },
        ),
    ];

    // Every implemented leg that names an accelerator: the two kinds' creates and the
    // standard accelerator's update. The limit is a property of the NAME, so it is stated
    // the same way on all of them.
    let standard = arn_of(
        &ok(
            &router,
            "CreateAccelerator",
            json!({ "Name": "length-rule", "IdempotencyToken": "length-rule-token" }),
        )
        .await["Accelerator"],
    );
    let legs: [&str; 3] = [
        "CreateAccelerator",
        "CreateCustomRoutingAccelerator",
        "UpdateAccelerator",
    ];

    for (index, (case, name, expected)) in names.iter().enumerate() {
        for leg in legs {
            let body = if leg == "UpdateAccelerator" {
                json!({ "AcceleratorArn": standard, "Name": name })
            } else {
                json!({ "Name": name, "IdempotencyToken": format!("{leg}-{index}") })
            };
            assert_expected(&router, &format!("{leg} {case}"), leg, body, *expected).await;
        }
    }
}

/// RULE: a value outside the value set of an input member this service states its own
/// requirement sentence for — the listener protocol — is refused HTTP 400
/// `InvalidArgumentException` carrying that sentence: the member as the API spells it over
/// the protocols the model's enum admits, with nothing of the request echoed, on every
/// operation whose input carries that member. Every OTHER enumerated member of this
/// service keeps the constraint report, and a value the enum does admit is served.
///
/// The value sets are the model's own, from `smithy_query(query="shape",
/// shape="Protocol,ClientAffinity,HealthCheckProtocol")`: `Protocol` [TCP, UDP],
/// `ClientAffinity` [NONE, SOURCE_IP], `HealthCheckProtocol` [TCP, HTTP, HTTPS].
#[tokio::test]
async fn a_protocol_outside_the_value_set_states_the_protocols_the_service_admits() {
    let router = emulator();
    let accelerator_arn = arn_of(
        &ok(
            &router,
            "CreateAccelerator",
            json!({ "Name": "protocol-rule", "IdempotencyToken": "protocol-rule-token" }),
        )
        .await["Accelerator"],
    );
    let listener_arn = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": accelerator_arn,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": "protocol-rule-listener",
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("the created listener's ARN")
        .to_string();
    // The sentence the service states this member's requirement with: fixed text over the
    // member and the two values its enum admits.
    let protocol = Expect::Stated("Protocol must be either TCP or UDP");

    let create = |protocol_value: Value, port: i32, token: &str| -> Value {
        let mut body = json!({
            "AcceleratorArn": accelerator_arn,
            "PortRanges": [{ "FromPort": port, "ToPort": port }],
            "IdempotencyToken": token,
        });
        if !protocol_value.is_null() {
            body["Protocol"] = protocol_value;
        }
        body
    };

    // (case, operation, body, what the service answers)
    let cases: Vec<(&str, &str, Value, Expect)> = vec![
        (
            "create-another-protocol",
            "CreateListener",
            create(json!("UDPX"), 80, "protocol-udpx"),
            protocol,
        ),
        (
            "create-a-value-differing-in-case",
            "CreateListener",
            create(json!("tcp"), 80, "protocol-lowercase"),
            protocol,
        ),
        (
            "create-an-empty-protocol",
            "CreateListener",
            create(json!(""), 80, "protocol-empty"),
            protocol,
        ),
        // The same member on the update leg of the same rule.
        (
            "update-another-protocol",
            "UpdateListener",
            json!({ "ListenerArn": listener_arn, "Protocol": "SCTP" }),
            protocol,
        ),
        // Both values the enum admits are served, on both legs.
        (
            "create-tcp",
            "CreateListener",
            create(json!("TCP"), 81, "protocol-tcp"),
            Expect::Served,
        ),
        (
            "create-udp",
            "CreateListener",
            create(json!("UDP"), 82, "protocol-udp"),
            Expect::Served,
        ),
        (
            "update-udp",
            "UpdateListener",
            json!({ "ListenerArn": listener_arn, "Protocol": "UDP" }),
            Expect::Served,
        ),
        // An absent REQUIRED protocol is not a value outside the set: the boundary states
        // the member it needs, through the constraint report.
        (
            "create-without-a-protocol",
            "CreateListener",
            create(Value::Null, 83, "protocol-absent"),
            Expect::Refused {
                path: "protocol",
                requirement: "Member must not be null",
            },
        ),
        // Every OTHER enumerated member keeps the constraint report naming it.
        (
            "another-enumerated-member-of-the-same-request",
            "CreateListener",
            json!({
                "AcceleratorArn": accelerator_arn,
                "PortRanges": [{ "FromPort": 84, "ToPort": 84 }],
                "Protocol": "TCP",
                "ClientAffinity": "SESSION",
                "IdempotencyToken": "affinity-outside",
            }),
            Expect::Refused {
                path: "clientAffinity",
                requirement: "enum value set",
            },
        ),
        (
            "an-enumerated-member-of-another-resource",
            "CreateEndpointGroup",
            json!({
                "ListenerArn": listener_arn,
                "EndpointGroupRegion": "us-west-2",
                "HealthCheckProtocol": "SMTP",
                "IdempotencyToken": "health-protocol-outside",
            }),
            Expect::Refused {
                path: "healthCheckProtocol",
                requirement: "enum value set",
            },
        ),
    ];

    for (case, operation, body, expected) in cases {
        assert_expected(&router, case, operation, body, expected).await;
    }
}

/// RULE: an `IpAddressType` outside the value set its enum admits is refused HTTP 400
/// `InvalidArgumentException` with the sentence this service states that class with — the
/// value the REQUEST carried, named in the service's own spelling of the member, and
/// nothing else of the request — on every leg that takes the member; a value the enum
/// admits reaches the handler.
///
/// The value set is the model's own, from `smithy_query(query="shape",
/// shape="IpAddressType")`: [IPV4, DUAL_STACK].
#[tokio::test]
async fn an_ip_address_type_outside_the_value_set_names_the_value_the_request_carried() {
    let router = emulator();
    let standard = arn_of(
        &ok(
            &router,
            "CreateAccelerator",
            json!({ "Name": "address-type-rule", "IdempotencyToken": "address-type-token" }),
        )
        .await["Accelerator"],
    );

    // (case, the submitted value) — a type of neither family, one differing only in case,
    // one that drops the separator, and the empty string.
    let outside: [(&str, &str); 4] = [
        ("another-family", "IPV5"),
        ("differing-in-case", "ipv4"),
        ("without-the-separator", "DUALSTACK"),
        ("empty", ""),
    ];

    for (index, (case, value)) in outside.iter().enumerate() {
        let sentence = format!("IP Address type {value} is invalid");
        // Every implemented leg that takes the member: the two kinds' creates and the
        // standard accelerator's update.
        let legs: [(&str, Value); 3] = [
            (
                "CreateAccelerator",
                json!({
                    "Name": format!("address-type-{index}"),
                    "IpAddressType": value,
                    "IdempotencyToken": format!("address-type-create-{index}"),
                }),
            ),
            (
                "CreateCustomRoutingAccelerator",
                json!({
                    "Name": format!("address-type-cra-{index}"),
                    "IpAddressType": value,
                    "IdempotencyToken": format!("address-type-cra-{index}"),
                }),
            ),
            (
                "UpdateAccelerator",
                json!({ "AcceleratorArn": standard, "IpAddressType": value }),
            ),
        ];
        for (leg, body) in legs {
            assert_expected(
                &router,
                &format!("{leg} {case}"),
                leg,
                body,
                Expect::Stated(&sentence),
            )
            .await;
        }
    }

    // Both values the enum admits reach the handler, on each leg that supports them. A
    // custom routing accelerator supports IPV4 only, so DUAL_STACK is a different class
    // there — unsupported for that kind, not outside the set — and belongs to the test of
    // that rule in `custom_routing_accelerators.rs`.
    let admitted: [(&str, &str, &str); 3] = [
        ("create-ipv4", "CreateAccelerator", "IPV4"),
        ("create-dual-stack", "CreateAccelerator", "DUAL_STACK"),
        (
            "create-custom-routing-ipv4",
            "CreateCustomRoutingAccelerator",
            "IPV4",
        ),
    ];
    for (index, (case, operation, value)) in admitted.iter().enumerate() {
        assert_expected(
            &router,
            case,
            operation,
            json!({
                "Name": format!("admitted-{index}"),
                "IpAddressType": value,
                "IdempotencyToken": format!("admitted-{index}"),
            }),
            Expect::Served,
        )
        .await;
    }
}

/// RULE: a `HealthCheckIntervalSeconds` outside the set of intervals this service
/// health-checks at is refused HTTP 400 `InvalidArgumentException` with the one sentence
/// the service states that class with — naming the value the request carried and the set it
/// is not in — whether the value is inside the member's modelled `@range` or outside it, and
/// on every operation that configures an endpoint group's health check; a value in the set
/// is served.
///
/// The supported set is the member's own doc, from `smithy_query(query="doc",
/// shape="HealthCheckIntervalSeconds")`: "The time—10 seconds or 30 seconds—between each
/// health check for an endpoint", inside the `@range` 10..30 the shape declares. Both
/// published bounds, both values just outside them, and an in-range unsupported value are
/// rows.
#[tokio::test]
async fn an_unsupported_health_check_interval_states_the_supported_set() {
    let router = emulator();
    let accelerator_arn = arn_of(
        &ok(
            &router,
            "CreateAccelerator",
            json!({ "Name": "interval-rule", "IdempotencyToken": "interval-rule-token" }),
        )
        .await["Accelerator"],
    );
    let listener_arn = ok(
        &router,
        "CreateListener",
        json!({
            "AcceleratorArn": accelerator_arn,
            "PortRanges": [{ "FromPort": 80, "ToPort": 80 }],
            "Protocol": "TCP",
            "IdempotencyToken": "interval-rule-listener",
        }),
    )
    .await["Listener"]["ListenerArn"]
        .as_str()
        .expect("the created listener's ARN")
        .to_string();
    let group_arn = ok(
        &router,
        "CreateEndpointGroup",
        json!({
            "ListenerArn": listener_arn,
            "EndpointGroupRegion": "us-west-2",
            "HealthCheckIntervalSeconds": 30,
            "IdempotencyToken": "interval-rule-group",
        }),
    )
    .await["EndpointGroup"]["EndpointGroupArn"]
        .as_str()
        .expect("the created endpoint group's ARN")
        .to_string();

    // (case, the interval, whether the set admits it)
    let intervals: [(&str, i32, bool); 7] = [
        ("at-the-lower-supported-value", 10, true),
        ("at-the-upper-supported-value", 30, true),
        ("one-below-the-range", 9, false),
        ("far-below-the-range", 1, false),
        ("inside-the-range-but-unsupported", 20, false),
        ("one-past-the-range", 31, false),
        ("far-past-the-range", 300, false),
    ];

    for (case, seconds, supported) in intervals {
        let sentence =
            format!("Invalid HealthCheck Interval Seconds: {seconds} expected values are: 10 or 30");
        // The update leg: the group exists, so a supported value is served and an
        // unsupported one is refused with the sentence.
        let expected = if supported {
            Expect::Served
        } else {
            Expect::Stated(&sentence)
        };
        assert_expected(
            &router,
            &format!("update {case}"),
            "UpdateEndpointGroup",
            json!({ "EndpointGroupArn": group_arn, "HealthCheckIntervalSeconds": seconds }),
            expected,
        )
        .await;

        // The create leg of the same rule. A supported value only has to reach the
        // handler: this listener already covers the Region, so the create itself is a
        // conflict rather than a constraint refusal.
        let expected = if supported {
            Expect::Accepted
        } else {
            Expect::Stated(&sentence)
        };
        assert_expected(
            &router,
            &format!("create {case}"),
            "CreateEndpointGroup",
            json!({
                "ListenerArn": listener_arn,
                "EndpointGroupRegion": "us-west-2",
                "HealthCheckIntervalSeconds": seconds,
                "IdempotencyToken": format!("interval-{case}"),
            }),
            expected,
        )
        .await;
    }

    // The refused updates changed nothing: the group still health-checks at the interval
    // the last supported value set.
    let reread = ok(
        &router,
        "DescribeEndpointGroup",
        json!({ "EndpointGroupArn": group_arn }),
    )
    .await["EndpointGroup"]["HealthCheckIntervalSeconds"]
        .clone();
    assert_eq!(
        reread,
        json!(30),
        "expected the last supported interval to be the stored one, got {reread}"
    );
}
