// SPDX-License-Identifier: CC0-1.0

//! A CloudWatch Logs call in CloudTrail event history, across the services that serve one
//! audit question on one engine.
//!
//! RULE: a service that serves a control-plane API call records it as a management event of
//! THAT service, whatever the call answered, so `cloudtrail:LookupEvents` reports the call
//! under its own `eventName` with the serving service's `eventSource` — the call that was
//! served and the one the authorization gate in front of the operation refused alike, each
//! joined to the response its caller got by the request id that response named. The record of
//! a call the permission check stopped carries the AUTHORIZATION layer's own error code with
//! the message the caller was answered with, and neither the parameters it would have been
//! dispatched with nor any response elements. That is how a client answers "find the trace of
//! this failed call", alarms on denied attempts, and tells "nobody tried" from "the attempt
//! was refused".
//!
//! Only a cross-service suite can pin this: the record is written by the service that SERVES
//! the call (CloudWatch Logs, awsJson1_1), the refused caller's session is minted by the
//! token service against a role IAM holds, and the records are read back through CloudTrail
//! (awsJson1_1) — four services over ONE engine, as `core/router/src/lib.rs` mounts them.
//!
//! Every call goes over the wire, on a router with request authentication off as every suite
//! in this crate runs it. Assembling the whole router costs stack (see `boot_serve.rs`), so
//! it is built — and the requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region the calls below are signed for, and the Region their records are looked up in.
const REGION: &str = "us-west-2";

/// The access key id the control calls carry: a key IAM holds no record of, so it resolves to
/// the account's own identity, which carries no IAM policies and is allowed every action.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The `eventSource` CloudTrail stamps on a management event of CloudWatch Logs.
const EVENT_SOURCE: &str = "logs.amazonaws.com";

/// The refused call of the audit question, and the allowed control beside it.
const REFUSED: &str = "DescribeLogGroups";
const CONTROL: &str = "CreateLogGroup";

/// The `errorCode` a record of an IAM denial carries — the authorization layer's own, not the
/// `AccessDeniedException` the caller is answered with.
const RECORDED_ACCESS_DENIED: &str = "AccessDenied";

/// What one call answered.
struct Answer {
    status: u16,
    request_id: String,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The message the caller was answered with, whichever spelling the envelope used.
    fn message(&self) -> Option<String> {
        let body = self.json();
        body.get("message")
            .or_else(|| body.get("Message"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Dispatch one request signed for `signing_name` in [`REGION`] with `access_key_id` in the
/// credential scope.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    access_key_id: &str,
    headers: &[(&str, &str)],
    body: bytes::Bytes,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host.clone())
        .header(
            // The signature is never checked: this router runs with authentication off.
            // What the scope carries is the key id and the Region a handler reads.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let request_id = response
        .headers()
        .get("x-amzn-RequestId")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        request_id,
        body,
    }
}

/// One awsJson1_1 call to CloudWatch Logs, signed with `access_key_id`.
fn logs(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    dispatch(
        router,
        runtime,
        "logs",
        access_key_id,
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        bytes::Bytes::from(body.to_string()),
    )
}

/// One awsQuery call, signed with the account's own credential, to the service `signing_name`
/// serves at `version`.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        runtime,
        signing_name,
        ACCOUNT_KEY,
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    );
    assert_eq!(
        answered.status,
        200,
        "{action} must be served, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// `cloudtrail:LookupEvents` in [`REGION`], filtered on one attribute, answering each reported
/// event's envelope paired with its parsed `CloudTrailEvent` document.
fn lookup(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<(Value, Value)> {
    let answered = dispatch(
        router,
        runtime,
        "cloudtrail",
        ACCOUNT_KEY,
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "CloudTrail_20131101.LookupEvents"),
        ],
        bytes::Bytes::from(
            json!({
                "LookupAttributes": [{
                    "AttributeKey": attribute_key,
                    "AttributeValue": attribute_value,
                }],
                "MaxResults": 50,
            })
            .to_string(),
        ),
    );
    assert_eq!(
        answered.status,
        200,
        "LookupEvents({attribute_key}={attribute_value}) must succeed, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()["Events"]
        .as_array()
        .unwrap_or_else(|| panic!("LookupEvents reports an Events list, got {}", answered.text()))
        .iter()
        .map(|event| {
            let raw = event["CloudTrailEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("an event carries its CloudTrailEvent: {event}"));
            let document: Value = serde_json::from_str(raw)
                .unwrap_or_else(|error| panic!("CloudTrailEvent is JSON ({error}): {raw}"));
            (event.clone(), document)
        })
        .collect()
}

/// The ONE reported event of `records` whose record names request `request_id` — the join a
/// client holding a response makes.
fn joined_by_request_id(case: &str, records: &[(Value, Value)], request_id: &str) -> (Value, Value) {
    let found: Vec<&(Value, Value)> = records
        .iter()
        .filter(|(_, document)| document.get("requestID") == Some(&json!(request_id)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record naming request {request_id:?}, got {found:?}"
    );
    found[0].clone()
}

#[test]
fn a_refused_logs_call_and_the_control_beside_it_are_in_cloudtrail_event_history() {
    std::thread::Builder::new()
        .name("cloudwatch-logs-management-events".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // A session of a role granted ONE action, and that of another service: the caller
            // the authorization gate refuses. The role and its policy are IAM's own records
            // and the session is the token service's own mint, both over this same engine.
            let role_name = "logs-audited-reader-role";
            let trust = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {
                        "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id()),
                    },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string();
            let created_role = query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust),
                ],
            );
            let role_arn = element(&created_role, "Arn");
            assert!(
                !role_arn.is_empty(),
                "CreateRole answers the role's Arn, got {created_role}"
            );
            query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "PutRolePolicy",
                &[
                    ("RoleName", role_name.to_string()),
                    ("PolicyName", "caller-identity-only".to_string()),
                    (
                        "PolicyDocument",
                        json!({
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sts:GetCallerIdentity",
                                "Resource": "*",
                            }],
                        })
                        .to_string(),
                    ),
                ],
            );
            let session_name = "audited-session";
            let assumed = query(
                &router,
                &runtime,
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    ("RoleArn", role_arn.clone()),
                    ("RoleSessionName", session_name.to_string()),
                ],
            );
            let session_key = element(&assumed, "AccessKeyId");
            let session_arn = element(&assumed, "Arn");
            assert!(
                !session_key.is_empty() && !session_arn.is_empty(),
                "AssumeRole answers a session credential and the session's Arn, got {assumed}"
            );

            // The two calls the audit question is about: the read that session is refused,
            // and the control the account's own credential is served.
            let prefix = "/audited/least-privilege";
            let denied = logs(
                &router,
                &runtime,
                &session_key,
                REFUSED,
                &json!({"logGroupNamePrefix": prefix}),
            );
            let group = format!("{prefix}/control");
            let control = logs(
                &router,
                &runtime,
                ACCOUNT_KEY,
                CONTROL,
                &json!({"logGroupName": &group}),
            );

            // The API side first, because a record can only carry what the caller was told.
            let denial = format!(
                "User: {session_arn} is not authorized to perform: logs:{REFUSED} on resource: \
                 arn:aws:logs:{REGION}:{}:log-group::log-stream: because no identity-based \
                 policy allows the logs:{REFUSED} action",
                engine::emulator_account_id()
            );
            assert_eq!(
                (
                    denied.json()["__type"].as_str().map(str::to_string),
                    denied.message(),
                ),
                (
                    Some("AccessDeniedException".to_string()),
                    Some(denial.clone()),
                ),
                "case \"the unauthorized session\": expected the authorization refusal, got {} \
                 with {:?}",
                denied.status,
                denied.text()
            );
            assert_eq!(
                control.status,
                200,
                "the control create must be served, got {} with {:?}",
                control.status,
                control.text()
            );
            assert!(
                !denied.request_id.is_empty()
                    && !control.request_id.is_empty()
                    && denied.request_id != control.request_id,
                "each response names its own request, got {:?} and {:?}",
                denied.request_id,
                control.request_id
            );

            // Found the way a client finds the records of its own calls: a lookup by event
            // name over the account's event history.
            let refused_records = lookup(&router, &runtime, "EventName", REFUSED);
            let (refused_event, refused) =
                joined_by_request_id("the refused read", &refused_records, &denied.request_id);
            assert_eq!(
                (
                    refused.get("eventName"),
                    refused.get("eventSource"),
                    refused.get("readOnly"),
                    refused.get("awsRegion"),
                    refused.get("managementEvent"),
                    refused.get("errorCode"),
                    refused.get("errorMessage"),
                    refused.get("requestParameters"),
                    refused.get("responseElements"),
                ),
                (
                    Some(&json!(REFUSED)),
                    Some(&json!(EVENT_SOURCE)),
                    // A listing is a read whether or not it was allowed.
                    Some(&json!(true)),
                    Some(&json!(REGION)),
                    Some(&json!(true)),
                    Some(&json!(RECORDED_ACCESS_DENIED)),
                    Some(&json!(denial)),
                    Some(&Value::Null),
                    Some(&Value::Null),
                ),
                "case \"the refused read\": expected the refused call's own record, got {refused}"
            );
            assert_eq!(
                (
                    refused.pointer("/userIdentity/type"),
                    refused.pointer("/userIdentity/arn"),
                    refused.pointer("/userIdentity/accessKeyId"),
                    refused.pointer("/userIdentity/sessionContext/sessionIssuer/arn"),
                    refused.pointer("/userIdentity/sessionContext/sessionIssuer/userName"),
                ),
                (
                    Some(&json!("AssumedRole")),
                    Some(&json!(session_arn)),
                    Some(&json!(session_key)),
                    Some(&json!(role_arn)),
                    Some(&json!(role_name)),
                ),
                "case \"the refused read\": expected the session that made the call, got {refused}"
            );
            // The envelope a lookup reports beside the record: the same event, and the same
            // id, so a client reading either finds the other.
            assert_eq!(
                (
                    refused_event.get("EventName"),
                    refused_event.get("EventSource"),
                    refused_event.get("ReadOnly"),
                    refused_event.get("Username"),
                    refused_event.get("EventId"),
                    refused_event.get("Resources"),
                ),
                (
                    Some(&json!(REFUSED)),
                    Some(&json!(EVENT_SOURCE)),
                    Some(&json!("true")),
                    Some(&json!(session_name)),
                    refused.get("eventID"),
                    Some(&json!([])),
                ),
                "case \"the refused read\": expected the envelope of its own record, got \
                 {refused_event}"
            );

            // The control, recorded as the write it is: with the parameters it was dispatched
            // with, no error, and the group it created named among its resources.
            let control_records = lookup(&router, &runtime, "EventName", CONTROL);
            let (control_event, served) =
                joined_by_request_id("the allowed control", &control_records, &control.request_id);
            assert_eq!(
                (
                    served.get("eventName"),
                    served.get("eventSource"),
                    served.get("readOnly"),
                    served.get("errorCode"),
                    served.get("errorMessage"),
                    served.pointer("/requestParameters/logGroupName"),
                ),
                (
                    Some(&json!(CONTROL)),
                    Some(&json!(EVENT_SOURCE)),
                    Some(&json!(false)),
                    None,
                    None,
                    Some(&json!(group)),
                ),
                "case \"the allowed control\": expected the served call's own record, got {served}"
            );
            // The envelope a lookup reports beside it names the group the call created, by
            // the name the account holds it under: `Resource$ResourceName` is "the name of
            // the resource referenced by the event … These are user-created names".
            assert_eq!(
                (
                    control_event.get("ReadOnly"),
                    control_event.get("Resources"),
                ),
                (
                    Some(&json!("false")),
                    Some(&json!([{
                        "ResourceType": "AWS::Logs::LogGroup",
                        "ResourceName": group,
                    }])),
                ),
                "case \"the allowed control\": expected the envelope to name the group it \
                 created, got {control_event}"
            );
        })
        .expect("spawn the cloudwatch-logs-management-events thread")
        .join()
        .expect("a refused logs call and its control must be in event history");
}
