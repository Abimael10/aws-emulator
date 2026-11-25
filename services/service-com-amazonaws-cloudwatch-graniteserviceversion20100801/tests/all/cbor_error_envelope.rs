// SPDX-License-Identifier: CC0-1.0

//! The ERROR ENVELOPE this service answers a failure in on its `rpcv2Cbor` route, end to end
//! over the wire.
//!
//! This service's model declares `rpcv2Cbor` beside `awsJson1_0` and `awsQuery`
//! (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`),
//! so a client may call every operation over
//! `POST /service/GraniteServiceVersion20100801/operation/<Operation>` with
//! `smithy-protocol: rpc-v2-cbor` — and what a failure of such a call looks like is as much a
//! requirement as what a success does.
//!
//! ONE rule, asserted over a table: **a failure answered on that route carries HTTP 400 when
//! the caller caused it, whatever `httpError` its own shape binds, and a CBOR map holding the
//! FULLY QUALIFIED shape id of its class under `__type` — this service's own versioned package
//! `com.amazonaws.cloudwatch.v2010_08_01` for a fault its API declares, the package of the
//! framework in front of the service for the request-handling classes that framework answers —
//! plus the failure's own message where it has one, and no other member of the exception
//! shape.**
//!
//! An SDK resolves a CBOR error to an exception class by matching that value, so a bare local
//! name resolves to nothing: a client that deleted a mute rule and re-reads it must see
//! `ResourceNotFoundException` rather than an unmodelled error, and a 404 for a resource a
//! request named would be read by proxies and generic HTTP layers as a routing problem rather
//! than as the service-level client error it is.
//!
//! The rows differ along what the rule generalises over: the error CLASS (three faults this
//! service's API declares, and two the framework in front of it answers), the OPERATION that
//! answered it (two operations answer the same class), whether the failure carries a message at
//! all, whether its own shape carries a member that is not text, which LAYER rendered it (a
//! handler, the constraint walk's own refusal, and a refusal this service words ahead of the
//! handler), and the status the class arrived with (404, 400).
//!
//! The last two suites are the other half of the rule: the same failure on the `awsJson1_0`
//! route keeps the status and the members its model binds — the envelope is what differs, not
//! the error — and a SUCCESS on the CBOR route is answered exactly as it was.

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    DeleteAlarmMuteRuleInput, DescribeInsightRulesInput, GetAlarmMuteRuleInput,
    GetAlarmMuteRuleOutput, GetDashboardInput, GetMetricStreamInput, MuteTargets,
    PutAlarmMuteRuleInput, PutDashboardInput, Rule, Schedule, DELETE_ALARM_MUTE_RULE_INPUT_SCHEMA,
    DESCRIBE_INSIGHT_RULES_INPUT_SCHEMA, GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
    GET_DASHBOARD_INPUT_SCHEMA, GET_METRIC_STREAM_INPUT_SCHEMA, PUT_ALARM_MUTE_RULE_INPUT_SCHEMA,
    PUT_DASHBOARD_INPUT_SCHEMA,
};
// The entries a client decodes out of an answer on this route: the service's own reader, so
// this suite holds no second definition of it.
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::service::cbor_error_envelope::error_entries;

use crate::harness::{build_router, call};

/// The package a fault this service's own API declares is named in on this route.
const MODELLED_FAULT_PACKAGE: &str = "com.amazonaws.cloudwatch.v2010_08_01";

/// The package the request-handling classes of the framework in front of the service are named
/// in — the same package the `awsJson` route names them in.
const FRAMEWORK_SERVICE_PACKAGE: &str = "com.amazon.coral.service";

/// The package the REQUEST-VALIDATION class belongs to, which is the validating layer's own
/// rather than the service framework's.
const FRAMEWORK_VALIDATE_PACKAGE: &str = "com.amazon.coral.validate";

/// The status a failure the CALLER caused is answered with on this route.
const CLIENT_FAULT_STATUS: u16 = 400;

/// The member an error envelope names the class in.
const TYPE_MEMBER: &str = "__type";

/// `input` as the `rpcv2Cbor` request body a client sends: written through the protocol's own
/// codec against the operation's input schema, so nothing here encodes CBOR by hand.
fn request_body<I: SerializableStruct>(input_schema: &Schema, input: &I) -> bytes::Bytes {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(input_schema, input)
        .expect("the input shape serializes");
    bytes::Bytes::from(serializer.finish())
}

/// One `rpcv2Cbor` call: its HTTP status and the body bytes as answered.
async fn cbor_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: bytes::Bytes,
) -> (u16, bytes::Bytes) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "/service/GraniteServiceVersion20100801/operation/{operation}"
        ))
        .header("content-type", "application/cbor")
        .header("accept", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        // The SigV4 credential scope is what routes the request to the `monitoring` signing
        // name, exactly as it does for this service's JSON and query calls.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/monitoring/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=deadbeef",
        )
        .body(body)
        .expect("a well-formed rpcv2Cbor request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// The entries of a CBOR error map, decoded as a client decodes one: `(key, value)` pairs in
/// the order the map carries them.
fn error_map(case: &str, body: &[u8]) -> Vec<(String, String)> {
    error_entries(body).unwrap_or_else(|| {
        panic!("case {case:?}: the error body must decode as a CBOR map, got {body:?}")
    })
}

/// The message an answer in the JSON envelope carries, under the key its own shape declares it
/// — `message` on the parameter faults, `Message` on the resource and fault shapes.
fn json_message(body: &Value) -> Option<(String, String)> {
    let object = body.as_object()?;
    ["message", "Message"].into_iter().find_map(|key| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(|value| (key.to_string(), value.to_string()))
    })
}

/// The name of a mute rule, dashboard or metric stream this suite names, kept distinct per row
/// so no row reads another's state.
fn named(what: &str) -> String {
    format!("cbor-envelope-{what}")
}

/// A mute rule schedule hours away: a rule written with it is SCHEDULED, which is enough for a
/// read of it to succeed and for a delete to remove it.
fn still_to_come() -> Rule {
    let opens = ::chrono::Utc::now() + ::chrono::TimeDelta::hours(3);
    Rule {
        schedule: Schedule {
            expression: format!("at({})", opens.format("%Y-%m-%dT%H:%M")),
            duration: "PT15M".to_string(),
            timezone: None,
        },
    }
}

/// One row of the table: the failing call, and what a client reads on this route.
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The operation the row calls.
    operation: &'static str,
    /// The body of the CBOR call.
    cbor: bytes::Bytes,
    /// The same request in the JSON envelope, for the status and the message that failure
    /// carries whichever envelope it travels in.
    json: Value,
    /// The fully qualified shape id a client reads the class under on this route.
    shape_id: String,
    /// The status that same failure carries on the JSON route, which is the one its own shape
    /// binds.
    modelled_status: u16,
}

/// Every row's failing call, over both routes, after the state each row reads has been set up.
async fn rows(router: &MultiServiceRouter) -> Vec<Row> {
    // The recorded sequence: a mute rule is written, then deleted, then read.
    let deleted = named("deleted-rule");
    let (status, _) = cbor_call(
        router,
        "PutAlarmMuteRule",
        request_body(
            PUT_ALARM_MUTE_RULE_INPUT_SCHEMA,
            &PutAlarmMuteRuleInput {
                name: deleted.clone(),
                rule: still_to_come(),
                mute_targets: Some(MuteTargets {
                    alarm_names: vec![named("alarm")],
                }),
                ..PutAlarmMuteRuleInput::default()
            },
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the recorded setup: PutAlarmMuteRule must answer 200 over rpc-v2-cbor"
    );
    let (status, _) = cbor_call(
        router,
        "DeleteAlarmMuteRule",
        request_body(
            DELETE_ALARM_MUTE_RULE_INPUT_SCHEMA,
            &DeleteAlarmMuteRuleInput {
                alarm_mute_rule_name: deleted.clone(),
            },
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the recorded setup: DeleteAlarmMuteRule must answer 200 over rpc-v2-cbor"
    );

    let never = named("a-rule-no-one-wrote");
    let stream = named("stream");
    let dashboard = named("dashboard");
    let over_the_length_bound = "n".repeat(256);
    // A widget document that is well-formed JSON and does not satisfy the dashboard body syntax:
    // a text widget renders `markdown` and this one carries none, so the refusal enumerates one
    // problem — the fault whose shape carries a LIST of them beside its sentence.
    let invalid_body =
        json!({"widgets": [{"type": "text", "properties": {"background": "transparent"}}]})
            .to_string();
    vec![
        Row {
            case: "a mute rule the caller just deleted",
            operation: "GetAlarmMuteRule",
            cbor: request_body(
                GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
                &GetAlarmMuteRuleInput {
                    alarm_mute_rule_name: deleted.clone(),
                },
            ),
            json: json!({"AlarmMuteRuleName": deleted}),
            shape_id: format!("{MODELLED_FAULT_PACKAGE}#ResourceNotFoundException"),
            modelled_status: 404,
        },
        Row {
            case: "a mute rule no one ever wrote",
            operation: "GetAlarmMuteRule",
            cbor: request_body(
                GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
                &GetAlarmMuteRuleInput {
                    alarm_mute_rule_name: never.clone(),
                },
            ),
            json: json!({"AlarmMuteRuleName": never}),
            shape_id: format!("{MODELLED_FAULT_PACKAGE}#ResourceNotFoundException"),
            modelled_status: 404,
        },
        Row {
            case: "the same class from another operation",
            operation: "GetMetricStream",
            cbor: request_body(
                GET_METRIC_STREAM_INPUT_SCHEMA,
                &GetMetricStreamInput {
                    name: stream.clone(),
                },
            ),
            json: json!({"Name": stream}),
            shape_id: format!("{MODELLED_FAULT_PACKAGE}#ResourceNotFoundException"),
            modelled_status: 404,
        },
        Row {
            case: "another fault of this service's API, carrying its sentence",
            operation: "GetDashboard",
            cbor: request_body(
                GET_DASHBOARD_INPUT_SCHEMA,
                &GetDashboardInput {
                    dashboard_name: dashboard.clone(),
                },
            ),
            json: json!({"DashboardName": dashboard}),
            shape_id: format!("{MODELLED_FAULT_PACKAGE}#DashboardNotFoundError"),
            modelled_status: 404,
        },
        Row {
            case: "a fault whose shape carries a member that is not text",
            operation: "PutDashboard",
            cbor: request_body(
                PUT_DASHBOARD_INPUT_SCHEMA,
                &PutDashboardInput {
                    dashboard_name: dashboard.clone(),
                    dashboard_body: invalid_body.clone(),
                    ..PutDashboardInput::default()
                },
            ),
            json: json!({"DashboardName": dashboard, "DashboardBody": invalid_body}),
            shape_id: format!("{MODELLED_FAULT_PACKAGE}#DashboardInvalidInputError"),
            modelled_status: 400,
        },
        Row {
            case: "a page size under the published minimum, worded ahead of the handler",
            operation: "DescribeInsightRules",
            cbor: request_body(
                DESCRIBE_INSIGHT_RULES_INPUT_SCHEMA,
                &DescribeInsightRulesInput {
                    max_results: Some(0),
                    ..DescribeInsightRulesInput::default()
                },
            ),
            json: json!({"MaxResults": 0}),
            shape_id: format!("{FRAMEWORK_SERVICE_PACKAGE}#InvalidParameterValueException"),
            modelled_status: 400,
        },
        Row {
            case: "a required value supplied empty",
            operation: "GetAlarmMuteRule",
            cbor: request_body(
                GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
                &GetAlarmMuteRuleInput {
                    alarm_mute_rule_name: String::new(),
                },
            ),
            json: json!({"AlarmMuteRuleName": ""}),
            shape_id: format!("{FRAMEWORK_SERVICE_PACKAGE}#MissingRequiredParameterException"),
            modelled_status: 400,
        },
        Row {
            case: "a value past the length bound its own model binds",
            operation: "GetAlarmMuteRule",
            cbor: request_body(
                GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
                &GetAlarmMuteRuleInput {
                    alarm_mute_rule_name: over_the_length_bound.clone(),
                },
            ),
            json: json!({"AlarmMuteRuleName": over_the_length_bound}),
            shape_id: format!("{FRAMEWORK_VALIDATE_PACKAGE}#ValidationException"),
            modelled_status: 400,
        },
    ]
}

/// Every failure on this route names its class in full, carries that name and its own message
/// alone, and travels under the status of the fault side rather than of the shape.
#[tokio::test]
async fn a_cbor_failure_names_its_class_in_full_and_carries_nothing_else() {
    let router = build_router();
    for row in rows(&router).await {
        let case = row.case;
        let (json_status, json_body) = call(&router, row.operation, &row.json).await;
        assert_eq!(
            json_status, row.modelled_status,
            "case {case:?}: the same failure on the JSON route carries the status its shape \
             binds, got {json_status} with {json_body}"
        );

        let (status, answered) = cbor_call(&router, row.operation, row.cbor.clone()).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: expected status {CLIENT_FAULT_STATUS} on the CBOR route, got \
             {status} with {:?}",
            error_map(case, &answered)
        );
        let actual = error_map(case, &answered);
        let mut expected = vec![(TYPE_MEMBER.to_string(), row.shape_id.clone())];
        // The sentence is the failure's own, so it is read off the SAME failure in the other
        // envelope rather than restated here; a failure that states none carries the class
        // alone.
        expected.extend(json_message(&json_body));
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the CBOR map {expected:?}, got {actual:?}"
        );
    }
}

/// The envelope is what differs, not the error: the same not-found on the JSON route keeps the
/// status and the members its model binds, where the CBOR answer states the class alone.
#[tokio::test]
async fn the_json_route_keeps_the_modelled_status_and_members() {
    let router = build_router();
    let missing = named("json-side-rule");
    let (status, body) = call(
        &router,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": missing}),
    )
    .await;
    assert_eq!(
        status, 404,
        "case the JSON route: a not-found keeps the 404 its shape binds, got {status} with \
         {body}"
    );
    for member in ["ResourceType", "ResourceId"] {
        assert!(
            body.get(member).and_then(Value::as_str).is_some(),
            "case the JSON route: the answer carries {member}, got {body}"
        );
    }

    let (status, answered) = cbor_call(
        &router,
        "GetAlarmMuteRule",
        request_body(
            GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
            &GetAlarmMuteRuleInput {
                alarm_mute_rule_name: missing,
            },
        ),
    )
    .await;
    let entries = error_map("the CBOR route", &answered);
    assert_eq!(
        status, CLIENT_FAULT_STATUS,
        "case the CBOR route: expected status {CLIENT_FAULT_STATUS}, got {status} with \
         {entries:?}"
    );
    let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        vec![TYPE_MEMBER],
        "case the CBOR route: the map carries the class alone, got {entries:?}"
    );
}

/// A SUCCESS on this route is answered exactly as it was: the envelope above is a failure's,
/// and nothing of a 200 passes through it.
#[tokio::test]
async fn a_cbor_success_is_answered_as_it_was() {
    let router = build_router();
    let live = named("live-rule");
    let alarm = named("live-alarm");
    let (status, _) = cbor_call(
        &router,
        "PutAlarmMuteRule",
        request_body(
            PUT_ALARM_MUTE_RULE_INPUT_SCHEMA,
            &PutAlarmMuteRuleInput {
                name: live.clone(),
                rule: still_to_come(),
                mute_targets: Some(MuteTargets {
                    alarm_names: vec![alarm],
                }),
                ..PutAlarmMuteRuleInput::default()
            },
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a live rule: PutAlarmMuteRule must answer 200 over rpc-v2-cbor"
    );

    let (status, answered) = cbor_call(
        &router,
        "GetAlarmMuteRule",
        request_body(
            GET_ALARM_MUTE_RULE_INPUT_SCHEMA,
            &GetAlarmMuteRuleInput {
                alarm_mute_rule_name: live.clone(),
            },
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a live rule: GetAlarmMuteRule must answer 200 over rpc-v2-cbor, got {status}"
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&answered);
    let read = GetAlarmMuteRuleOutput::deserialize(&mut deserializer).unwrap_or_else(|error| {
        panic!("case a live rule: the answer must decode against its output schema, got {error}")
    });
    assert_eq!(
        read.name.as_deref(),
        Some(live.as_str()),
        "case a live rule: the answer reports the rule that was written, got {:?}",
        read.name
    );
    assert_eq!(
        read.status.as_deref(),
        Some("SCHEDULED"),
        "case a live rule: a rule whose window is still to come reports SCHEDULED, got {:?}",
        read.status
    );
}
