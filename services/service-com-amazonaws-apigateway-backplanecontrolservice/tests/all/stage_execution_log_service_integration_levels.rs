// SPDX-License-Identifier: CC0-1.0

//! What the `loggingLevel` and the `dataTraceEnabled` in force for a method let a request
//! served through an `AWS` SERVICE integration leave in the group API Gateway publishes for the
//! stage's execution log.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `MethodSetting$loggingLevel` "specifies the logging level for this method, which affects
//! > the log entries pushed to Amazon CloudWatch Logs. Valid values are `OFF`, `ERROR`, and
//! > `INFO`. Choose `ERROR` to write only error-level entries to CloudWatch Logs, or choose
//! > `INFO` to include all `ERROR` events as well as extra informational events", and
//! > `MethodSetting$dataTraceEnabled` "specifies whether data trace logging is enabled for this
//! > method, which affects the log entries pushed to Amazon CloudWatch Logs"
//! > (`smithy_query(query="shape", shape="MethodSetting")`). So, once the account carries the
//! > CloudWatch role API Gateway writes an account's logs as, the entry in force for the method
//! > a request reached decides what that request leaves in
//! > `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}`, every record naming the request it
//! > is about by the id the answer reported: nothing at `OFF`; at `ERROR` only a request the
//! > stage answered WITH an error, and of that request only the entries reporting the failure —
//! > the configuration error of an integration that could not be performed — and none of the
//! > informational steps of serving it; at `INFO` those entries and the informational steps
//! > beside them. `dataTraceEnabled` is what puts the PAYLOADS of the call in that trace, one
//! > line per transformation point the request crossed, so a request that reached no backend at
//! > all leaves only the outer two and a request that reached one leaves all four — and the
//! > payload lines are informational, so a level that writes no informational step writes no
//! > payload line whatever the data trace says.
//!
//! The rows span the product of what that generalises over: every documented level with the data
//! trace both ways, each driven over a request the stage SERVED through its integration and a
//! request it could not perform at all — the method whose `Integration$credentials` names a role
//! this account's IAM holds nothing under, which is a configuration error of the integration
//! rather than an answer from a backend. The recorded case is a subset of those rows (`OFF`,
//! `ERROR`, `INFO`, then `INFO` with the data trace on).
//!
//! What a sibling suite already pins is the same gate over an `AWS_PROXY` integration
//! ([`crate::stage_execution_log_data_trace_gate`]) and the configuration error a proxy answer
//! this api cannot read earns ([`crate::stage_execution_log_failure_lines`]). What this suite
//! adds is the SERVICE-integration half of the rule: the two inner payload lines are the
//! document the integration's request template rendered and the answer the owning service gave,
//! and the configuration error is the one an execution role that cannot be assumed is.
//!
//! Only a cross-service suite can pin this: the api, its stage's method settings and the
//! emission of the trace are API Gateway's, the table the integration calls is Amazon
//! DynamoDB's, both roles are AWS IAM's, and the group the trace lands in is CloudWatch Logs' —
//! all four on ONE engine, as `core/router/src/lib.rs` mounts them. Every assertion reads the
//! outcome back through CloudWatch Logs' own `FilterLogEvents`, so a record that did not reach
//! that store cannot pass.

use serde_json::json;

use crate::account_logging_role::{install_cloudwatch_role, role_arn};
use crate::dynamodb_service_integration::{dynamodb, given_role, given_table};
use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, method_response, put_integration, put_method,
};
use crate::rest_api_lifecycle::{
    create_api, gateway_with_dynamodb_iam_and_logs, resource_under, Gateway,
};
use crate::stage_access_logging::{
    gate_execution_logging, records_opening_with, trace_of, traced_line, traced_payload,
    PAYLOAD_PREFIXES,
};

/// The region every request of this suite is addressed in.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to.
const STAGE: &str = "logs";

/// The table the integration writes into, and the item every call updates.
const TABLE: &str = "counted";
const ITEM_KEY: &str = "counter";

/// The role the served method's integration is made under, granted the one action it makes.
const ROLE: &str = "apigateway-dynamodb-logs";

/// The role the FAILING method's integration names: a well-formed role ARN of this account that
/// IAM holds nothing under, so the call can be made under nothing and the integration is a
/// configuration error.
const ABSENT_ROLE: &str = "apigateway-dynamodb-never-created";

/// The account's CloudWatch role, created through IAM's OWN `CreateRole` under the path AWS's
/// managed push-to-logs role is created under: API Gateway writes a stage's execution log as
/// this role, so nothing is delivered before it is installed.
const CLOUDWATCH_ROLE: &str = "apigw-service-integration-logs";

/// The two resources the api carries: the one whose integration is performed, and the one whose
/// execution role does not exist.
const SERVED_PART: &str = "count";
const BROKEN_PART: &str = "broken";

/// The expression the request template reads this call's `m` query parameter through —
/// `$input.params(x)` "returns the method request parameter value", so the document the
/// integration sends carries a value of the REQUEST.
const MARKER_PARAMETER: &str = "$input.params('m')";

/// The sentence API Gateway opens a request's trace with, whichever level wrote it.
const OPENS: &str = "Execution log for request";

/// The informational line that reports the stage STARTED serving the request, which is the entry
/// `INFO` adds and `ERROR` does not.
const STARTING: &str = "Starting execution for request:";

/// The words API Gateway states a configuration error of the method's integration in. The reason
/// it names follows the colon, and what that reason says is the integration's own business, so
/// the class is what is asserted.
const CONFIGURATION_ERROR: &str = "Execution failed due to configuration error:";

/// The status a request the stage could not perform its integration for is answered with: the
/// api's `API_CONFIGURATION_ERROR` gateway response.
const CONFIGURATION_ERROR_STATUS: u16 = 500;

/// The request template the served method renders the table call's input document from, with
/// `marker` the text the `m` parameter carries — [`MARKER_PARAMETER`] for the template as it is
/// stored, the request's own value for the document the backend was sent.
fn request_template(marker: &str) -> String {
    format!(
        concat!(
            r#"{{"TableName":"{table}","Key":{{"id":{{"S":"{key}"}}}},"#,
            r#""UpdateExpression":"SET marker = :m","#,
            r#""ExpressionAttributeValues":{{":m":{{"S":"{marker}"}}}},"#,
            r#""ReturnValues":"UPDATED_NEW"}}"#,
        ),
        table = TABLE,
        key = ITEM_KEY,
        marker = marker,
    )
}

/// The document the api publishes out of what the table answered.
const RESPONSE_TEMPLATE: &str = r#"{"marker":"$input.path('$.Attributes.marker.S')"}"#;

/// One row of the gating table: the two settings in force for every method of the stage.
struct Row {
    case: &'static str,
    /// The stage-wide `loggingLevel`.
    level: &'static str,
    /// The `dataTraceEnabled` patched beside it.
    data_trace: bool,
}

/// The rows: every documented level of `MethodSetting$loggingLevel` with the data trace both
/// ways. The levels are the three the member documents, which is the whole catalog.
const ROWS: &[Row] = &[
    Row {
        case: "OFF, data trace off",
        level: "OFF",
        data_trace: false,
    },
    Row {
        case: "OFF, data trace on",
        level: "OFF",
        data_trace: true,
    },
    Row {
        case: "ERROR, data trace off",
        level: "ERROR",
        data_trace: false,
    },
    Row {
        case: "ERROR, data trace on",
        level: "ERROR",
        data_trace: true,
    },
    Row {
        case: "INFO, data trace off",
        level: "INFO",
        data_trace: false,
    },
    Row {
        case: "INFO, data trace on",
        level: "INFO",
        data_trace: true,
    },
];

/// One `GET` method under the api's root whose `AWS` integration names the table's `UpdateItem`
/// action under `role`, with the two integration responses that map what the table answered.
async fn given_method(gateway: &Gateway, api: &str, root: &str, path_part: &str, role: &str) {
    let resource = resource_under(gateway, api, root, path_part).await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &resource,
        "GET",
        json!({
            "type": "AWS",
            "httpMethod": "POST",
            "uri": format!("arn:aws:apigateway:{REGION}:dynamodb:action/UpdateItem"),
            "credentials": role,
            "requestTemplates": { "application/json": request_template(MARKER_PARAMETER) },
        }),
    )
    .await;
    method_response(gateway, api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": { "application/json": RESPONSE_TEMPLATE } }),
    )
    .await;
}

/// Gate every method of the stage at `level` with `data_trace` beside it, and assert the stage
/// REPORTS both settings back: the entry the data plane resolves is the entry the patch stored,
/// so a stage reporting another pair is gating by another configuration than the caller named.
async fn gate(gateway: &Gateway, api: &str, case: &str, level: &str, data_trace: bool) {
    gate_execution_logging(&gateway.router, api, STAGE, level, data_trace).await;
    let read = gateway
        .call("GET", &format!("/restapis/{api}/stages/{STAGE}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetStage to answer 200, got {} with {:?}",
        read.status, read.body
    );
    let settings = &read.body["methodSettings"]["*/*"];
    assert_eq!(
        (&settings["loggingLevel"], &settings["dataTraceEnabled"]),
        (&json!(level), &json!(data_trace)),
        "case {case:?}: expected the stage to report loggingLevel {level:?} with \
         dataTraceEnabled {data_trace:?}, got {:?}",
        read.body
    );
}

/// What one invoke of the stage left behind: the records of that request's trace, the status it
/// was answered with and the body the client holds.
struct Served {
    status: u16,
    body: String,
    trace: Vec<String>,
    request_id: String,
}

/// Invoke `path_part` of the deployed stage with `marker` as its `m` parameter, and read back
/// the records the stage's execution log group holds about THAT request.
async fn served(gateway: &Gateway, api: &str, case: &str, path_part: &str, marker: &str) -> Served {
    let answered = invoke(
        gateway,
        api,
        "GET",
        &format!("/{STAGE}/{path_part}?m={marker}"),
        &[("content-type", "application/json")],
    )
    .await;
    let request_id = answered
        .header("x-amzn-requestid")
        .unwrap_or_else(|| panic!("case {case:?}: the answer named no request id"))
        .to_string();
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
    let trace = trace_of(&gateway.router, &group, &request_id).await;
    Served {
        status: answered.status,
        body: answered.body,
        trace,
        request_id,
    }
}

/// Assert the lines of ONE request's trace against the rule: whether anything was written at
/// all, whether the informational steps are among it, whether the failure is, and which payload
/// lines exist.
fn assert_trace(case: &str, request_id: &str, trace: &[String], expected: &Expected) {
    if !expected.traced {
        assert!(
            trace.is_empty(),
            "case {case:?}: expected nothing about this request to be written, got {trace:?}"
        );
        return;
    }
    let opens = traced_line(request_id, &format!("{OPENS} {request_id}"));
    assert!(
        trace.contains(&opens),
        "case {case:?}: expected the record {opens:?} in this request's trace, got {trace:?}"
    );
    let starting = traced_line(request_id, &format!("{STARTING} {request_id}"));
    assert_eq!(
        trace.contains(&starting),
        expected.informational,
        "case {case:?}: expected the record {starting:?} in this request's trace={}, got \
         {trace:?}",
        expected.informational
    );
    let failed = traced_line(request_id, CONFIGURATION_ERROR);
    assert_eq!(
        trace
            .iter()
            .any(|record| record.starts_with(&failed) && record.len() > failed.len()),
        expected.configuration_error,
        "case {case:?}: expected a record opening {failed:?} in this request's trace={}, got \
         {trace:?}",
        expected.configuration_error
    );
    for (point, prefix) in PAYLOAD_PREFIXES.iter().enumerate() {
        let prefix = traced_line(request_id, prefix);
        let wanted = usize::from(expected.payload_points.contains(&point));
        assert_eq!(
            records_opening_with(trace, &prefix),
            wanted,
            "case {case:?}: expected the payload line {prefix:?} exactly {wanted} time(s), got \
             {trace:?}"
        );
    }
}

/// What the rule says one request's trace holds: computed from the settings in force and from
/// whether the request reached a backend, never written out per row.
struct Expected {
    traced: bool,
    informational: bool,
    configuration_error: bool,
    /// The transformation points of [`PAYLOAD_PREFIXES`] whose payload line was written.
    payload_points: Vec<usize>,
}

impl Expected {
    /// The trace the rule owes a request answered with `status` under `level`/`data_trace`,
    /// where `reached_backend` says whether the integration was performed at all.
    fn of(level: &str, data_trace: bool, status: u16, reached_backend: bool) -> Self {
        let failing = status >= 400;
        let traced = match level {
            "INFO" => true,
            "ERROR" => failing,
            _ => false,
        };
        let informational = traced && level == "INFO";
        // A data trace puts one line per transformation point the request CROSSED in the
        // trace, and the payload lines are informational: the outer two are crossed by every
        // request, the integration's own two only by a request that reached a backend.
        let payload_points = if informational && data_trace {
            if reached_backend {
                vec![0, 1, 2, 3]
            } else {
                vec![0, 3]
            }
        } else {
            Vec::new()
        };
        Self {
            traced,
            informational,
            configuration_error: traced && !reached_backend,
            payload_points,
        }
    }
}

/// RULE: once the account carries its CloudWatch role, the `loggingLevel` and
/// `dataTraceEnabled` in force for the method a request reached decide what that request leaves
/// in the stage's execution log group — nothing at `OFF`, only the failure of a request answered
/// with an error at `ERROR`, the informational steps beside it at `INFO`, and the payloads of
/// every transformation point the request crossed where the data trace is on.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits a call this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_level_and_the_data_trace_gate_what_a_service_integration_leaves() {
    let gateway = gateway_with_dynamodb_iam_and_logs();
    install_cloudwatch_role(&gateway, CLOUDWATCH_ROLE).await;
    given_table(&gateway, TABLE).await;
    let granted = given_role(
        &gateway,
        ROLE,
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "dynamodb:UpdateItem",
                "Resource": format!("arn:aws:dynamodb:{REGION}:000000000000:table/{TABLE}"),
            }],
        }),
    )
    .await;
    let absent = role_arn("/", ABSENT_ROLE);

    let (api, root) = create_api(&gateway, "dynamodb-execution-logs").await;
    given_method(&gateway, &api, &root, SERVED_PART, &granted).await;
    given_method(&gateway, &api, &root, BROKEN_PART, &absent).await;
    deploy(&gateway, &api, STAGE).await;

    for (rank, row) in ROWS.iter().enumerate() {
        let case = row.case;
        gate(&gateway, &api, case, row.level, row.data_trace).await;

        // The request the stage SERVES through its integration: the marker is this row's, so
        // the document the backend was sent and the item the table holds are this call's.
        let marker = format!("marker-{rank}");
        let answered = served(&gateway, &api, case, SERVED_PART, &marker).await;
        assert_eq!(
            (answered.status, answered.body.as_str()),
            (200, format!(r#"{{"marker":"{marker}"}}"#).as_str()),
            "case {case:?}: expected the table's answer mapped to the api's document, got {} \
             with {:?}",
            answered.status,
            answered.body
        );
        let expected = Expected::of(row.level, row.data_trace, answered.status, true);
        assert_trace(case, &answered.request_id, &answered.trace, &expected);
        if !expected.payload_points.is_empty() {
            // The integration's own two points: the document the request template RENDERED,
            // which carries the value this request's `m` parameter did, and the answer the
            // owning service gave before this api's integration response mapped it.
            let sent = traced_payload(
                &answered.trace,
                case,
                &traced_line(&answered.request_id, PAYLOAD_PREFIXES[1]),
            );
            assert_eq!(
                sent,
                request_template(&marker),
                "case {case:?}: expected the integration request as it was sent, got {sent:?}"
            );
            let received = traced_payload(
                &answered.trace,
                case,
                &traced_line(&answered.request_id, PAYLOAD_PREFIXES[2]),
            );
            assert!(
                received.contains(&marker) && received.contains("Attributes"),
                "case {case:?}: expected the backend's own answer carrying {marker:?}, got \
                 {received:?}"
            );
            // And the outer one: the method response line is exactly the body the client holds.
            let answered_line = traced_payload(
                &answered.trace,
                case,
                &traced_line(&answered.request_id, PAYLOAD_PREFIXES[3]),
            );
            assert_eq!(
                answered_line, answered.body,
                "case {case:?}: expected the method response line to carry the body the client \
                 received ({:?}), got {answered_line:?}",
                answered.body
            );
        }

        // The request whose integration could not be performed at all: its execution role is a
        // role IAM holds nothing under, so no call is made and no backend was reached.
        let refused = served(&gateway, &api, case, BROKEN_PART, &marker).await;
        assert_eq!(
            refused.status, CONFIGURATION_ERROR_STATUS,
            "case {case:?}: expected HTTP {CONFIGURATION_ERROR_STATUS} for an integration whose \
             execution role does not exist, got {} with {:?}",
            refused.status, refused.body
        );
        let expected = Expected::of(row.level, row.data_trace, refused.status, false);
        assert_trace(case, &refused.request_id, &refused.trace, &expected);
    }

    // The served calls were REAL calls: the table holds what the last row's integration wrote,
    // reported by the service that owns it rather than by the api that called it.
    let (status, read) = dynamodb(
        &gateway,
        "GetItem",
        json!({ "TableName": TABLE, "Key": { "id": { "S": ITEM_KEY } } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected GetItem to answer 200, got {status} with {read:?}"
    );
    assert_eq!(
        read["Item"]["marker"]["S"],
        json!(format!("marker-{}", ROWS.len() - 1)),
        "expected the table to hold the marker the last served call wrote, got {read:?}"
    );
}
