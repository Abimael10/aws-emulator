// SPDX-License-Identifier: CC0-1.0

//! REFUSED and served Elastic Load Balancing calls in CloudTrail event history, across two
//! services on one engine.
//!
//! RULE: a service that serves a control-plane API call records it as a management event of
//! that service whether it served the call or REFUSED it, so `cloudtrail:LookupEvents`
//! reports the call under its own `eventName` with the serving service's `eventSource`, with
//! `readOnly` saying whether the call only reported state — and a refused call carries the
//! FAULT SHAPE's name as its `errorCode`, which on a query protocol is not the code the
//! caller saw: the caller is answered the shape's `awsQueryError` code, and the record names
//! the shape, so the two differ by the `Exception` suffix the shape carries. A refused call
//! records no `responseElements` at all, and each record names the request by the id the
//! caller's own response carried.
//!
//! Only a cross-service suite can pin this: the record is written by the service that SERVES
//! the call (Elastic Load Balancing, awsQuery, reached through the `elasticloadbalancing`
//! signing name every AWS SDK ELBv2 client signs) and read back through CloudTrail
//! (`awsJson1_1`) — two services over ONE engine, as `core/router/src/lib.rs` mounts them.
//! The per-record field semantics are pinned over a wider table in the serving crate's own
//! suite (`services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/\
//! tests/all/management_event_recording.rs`), which reads the records where they are written
//! rather than through CloudTrail.
//!
//! The rows span what the rule generalises over: a write that is served, a write REFUSED
//! because the resource is still referenced, the same write served once the reference is
//! gone, a READ refused for a resource nothing is held under, and a write refused for a
//! malformed tag list — three fault shapes, both kinds of call, and both outcomes.
//!
//! Every call goes over the wire, on a router with request authentication off as every suite
//! in this crate runs it. Assembling the whole router costs stack (see `boot_serve.rs`), so
//! it is built — and the requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wire helpers this test crate owns, ONCE: the form encoder every awsQuery body is built
// with and the readers that pull one element's text — or every occurrence of it, with the
// escaped entities decoded — out of an XML answer. A suite that writes its own drifts from
// the others.
use crate::query_wire::{element, elements, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for, and so the Region the records live in.
const REGION: &str = "us-west-2";

/// The access key id every call carries.
const ACCESS_KEY_ID: &str = "AKIDTEST";

/// The signing name every AWS SDK Elastic Load Balancing client signs for, which both
/// Elastic Load Balancing models share; the envelope's `Version` is what names the model.
const ELB_SIGNING_NAME: &str = "elasticloadbalancing";

/// The API version of the ELBv2 model, which routes an envelope to it.
const ELB_VERSION: &str = "2015-12-01";

/// The `eventSource` CloudTrail stamps on an Elastic Load Balancing management event: the
/// `<endpointPrefix>.amazonaws.com` form, which both Elastic Load Balancing APIs share.
const ELB_EVENT_SOURCE: &str = "elasticloadbalancing.amazonaws.com";

/// The EC2 API version an ec2Query body carries: the network the load balancer is placed in
/// is EC2's, so this suite creates it through EC2 itself rather than naming ids it made up.
const EC2_API_VERSION: &str = "2016-11-15";

/// What one call answered.
struct Answer {
    status: u16,
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
}

/// Dispatch one request signed for `signing_name`.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
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
                 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One awsQuery call to Elastic Load Balancing at the ELBv2 model's API version.
fn elbv2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
    for (name, value) in params {
        body.push_str(&format!("&{}={}", form_encode(name), form_encode(value)));
    }
    dispatch(
        router,
        runtime,
        ELB_SIGNING_NAME,
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    )
}

/// One Elastic Load Balancing call that must be served, with the body it answered.
fn elbv2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let answered = elbv2(router, runtime, action, params);
    assert_eq!(
        answered.status,
        200,
        "{action} must be served, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One ec2Query call to Amazon EC2 that must be served, with the body it answered.
fn ec2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push_str(&format!("&{}={}", form_encode(name), form_encode(value)));
    }
    let answered = dispatch(
        router,
        runtime,
        "ec2",
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    );
    assert_eq!(
        answered.status,
        200,
        "{action} must be served by Amazon EC2, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// The network the estate lives in: one VPC with two subnets in different zones, created
/// through EC2 itself, so every id below is one EC2 minted.
fn seed_network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
) -> (String, Vec<String>) {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", "10.70.0.0/16")]),
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in [("10.70.1.0/24", "us-west-2a"), ("10.70.2.0/24", "us-west-2b")] {
        subnets.push(element(
            &ec2_ok(
                router,
                runtime,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            ),
            "subnetId",
        ));
    }
    assert!(
        vpc.starts_with("vpc-") && subnets.iter().all(|subnet| subnet.starts_with("subnet-")),
        "the network must be created by EC2, got VPC {vpc:?} and subnets {subnets:?}"
    );
    (vpc, subnets)
}

/// `cloudtrail:LookupEvents` filtered on one attribute, answering each event's
/// `CloudTrailEvent` document.
fn lookup(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<Value> {
    let answered = dispatch(
        router,
        runtime,
        "cloudtrail",
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
            let document = event["CloudTrailEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("an event carries its CloudTrailEvent: {event}"));
            serde_json::from_str(document)
                .unwrap_or_else(|error| panic!("CloudTrailEvent is JSON ({error}): {document}"))
        })
        .collect()
}

/// The one record among `events` that names the request answered under `request_id` — how a
/// client holding its own response finds the record of that very call.
fn by_request_id<'a>(case: &str, events: &'a [Value], request_id: &str) -> &'a Value {
    assert!(
        !request_id.is_empty(),
        "case {case:?}: the caller's response must name the request it answers"
    );
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("requestID") == Some(&json!(request_id)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record naming the request {request_id:?}, got \
         {events:?}"
    );
    found[0]
}

/// One refused call: the case, the operation, the code the CALLER is answered, the fault
/// shape the RECORD names, and whether the call only reports state.
struct Refused {
    case: &'static str,
    operation: &'static str,
    /// The awsQuery code of the fault shape, which is what the caller reads.
    caller_code: &'static str,
    /// The fault shape's own name, which is what the record carries.
    recorded_code: &'static str,
    read_only: bool,
}

#[test]
fn refused_elastic_load_balancing_calls_are_in_cloudtrail_event_history() {
    std::thread::Builder::new()
        .name("elbv2-management-events".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // The estate: the network EC2 holds, a load balancer placed in it, and the
            // target group a listener forwards to.
            let (vpc, subnets) = seed_network(&router, &runtime);
            let created_group = elbv2_ok(
                &router,
                &runtime,
                "CreateTargetGroup",
                &[
                    ("Name", "audit-elbv2-tg"),
                    ("Protocol", "HTTP"),
                    ("Port", "80"),
                    ("VpcId", vpc.as_str()),
                ],
            );
            let group = element(&created_group, "TargetGroupArn");
            let create_request_id = element(&created_group, "RequestId");
            let balancer = element(
                &elbv2_ok(
                    &router,
                    &runtime,
                    "CreateLoadBalancer",
                    &[
                        ("Name", "audit-elbv2-alb"),
                        // Internal, so the placement needs no internet gateway: what this
                        // suite is about is the record, not the placement.
                        ("Scheme", "internal"),
                        ("Subnets.member.1", subnets[0].as_str()),
                        ("Subnets.member.2", subnets[1].as_str()),
                    ],
                ),
                "LoadBalancerArn",
            );
            let listener = element(
                &elbv2_ok(
                    &router,
                    &runtime,
                    "CreateListener",
                    &[
                        ("LoadBalancerArn", balancer.as_str()),
                        ("Protocol", "HTTP"),
                        ("Port", "80"),
                        ("DefaultActions.member.1.Type", "forward"),
                        ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
                    ],
                ),
                "ListenerArn",
            );
            assert!(
                !group.is_empty() && !balancer.is_empty() && !listener.is_empty(),
                "the estate must be created: group {group:?}, load balancer {balancer:?}, \
                 listener {listener:?}"
            );

            // The SERVED write is in event history with no error at all, and with what it
            // answered.
            let creates = lookup(&router, &runtime, "EventName", "CreateTargetGroup");
            let served = by_request_id("the served create", &creates, &create_request_id);
            assert_eq!(
                served.get("eventSource"),
                Some(&json!(ELB_EVENT_SOURCE)),
                "the record names the serving service, got {served}"
            );
            assert_eq!(
                served.get("errorCode"),
                None,
                "a served call carries no error code at all, got {served}"
            );
            assert_eq!(
                served.get("readOnly"),
                Some(&json!(false)),
                "a create changes the account's resources, got {served}"
            );
            assert_ne!(
                served.get("responseElements"),
                Some(&Value::Null),
                "a served write records what it answered, got {served}"
            );

            // An ARN of a target group this account holds none under, for the refused READ.
            let absent_group = format!(
                "arn:aws:elasticloadbalancing:{REGION}:{}:targetgroup/audit-absent/\
                 0123456789abcdef",
                emulator_server::emulator_account_id()
            );

            // (case, operation, the fields, the code the caller sees, the shape the record
            // names, whether the call only reports state)
            let rows: Vec<(Refused, Vec<(&str, &str)>)> = vec![
                (
                    Refused {
                        case: "a delete of the target group the listener forwards to",
                        operation: "DeleteTargetGroup",
                        caller_code: "ResourceInUse",
                        recorded_code: "ResourceInUseException",
                        read_only: false,
                    },
                    vec![("TargetGroupArn", group.as_str())],
                ),
                (
                    Refused {
                        case: "a read of a target group nothing is held under",
                        operation: "DescribeTargetGroups",
                        caller_code: "TargetGroupNotFound",
                        recorded_code: "TargetGroupNotFoundException",
                        read_only: true,
                    },
                    vec![("TargetGroupArns.member.1", absent_group.as_str())],
                ),
                (
                    Refused {
                        case: "a tag write naming one key twice",
                        operation: "AddTags",
                        caller_code: "DuplicateTagKeys",
                        recorded_code: "DuplicateTagKeysException",
                        read_only: false,
                    },
                    vec![
                        ("ResourceArns.member.1", group.as_str()),
                        ("Tags.member.1.Key", "stage"),
                        ("Tags.member.1.Value", "one"),
                        ("Tags.member.2.Key", "stage"),
                        ("Tags.member.2.Value", "two"),
                    ],
                ),
            ];

            for (row, fields) in &rows {
                let case = row.case;
                let answered = elbv2(&router, &runtime, row.operation, fields);
                let body = answered.text();
                assert_eq!(
                    (
                        answered.status,
                        opt_element(&body, "Code").unwrap_or_default().as_str(),
                    ),
                    (400, row.caller_code),
                    "case {case:?}: expected 400 {} from the caller's side, got {} with {body}",
                    row.caller_code,
                    answered.status
                );
                // Read through the decoding reader, so the message compares as the service
                // wrote it rather than as its XML escaping.
                let message = elements(&body, "Message").into_iter().next().unwrap_or_default();
                assert!(
                    !message.is_empty(),
                    "case {case:?}: a refusal states what the caller has to fix, got {body}"
                );
                let request_id = element(&body, "RequestId");

                let events = lookup(&router, &runtime, "EventName", row.operation);
                let record = by_request_id(case, &events, &request_id);
                assert_eq!(
                    record.get("eventSource"),
                    Some(&json!(ELB_EVENT_SOURCE)),
                    "case {case:?}: the record names the serving service, got {record}"
                );
                assert_eq!(
                    record.get("errorCode"),
                    Some(&json!(row.recorded_code)),
                    "case {case:?}: the record names the fault shape, got {record}"
                );
                assert_ne!(
                    record.get("errorCode"),
                    Some(&json!(row.caller_code)),
                    "case {case:?}: the record names the SHAPE, not the awsQuery code the \
                     caller saw, got {record}"
                );
                assert_eq!(
                    record.get("errorMessage"),
                    Some(&json!(message)),
                    "case {case:?}: the record carries the message the caller saw, got {record}"
                );
                assert_eq!(
                    record.get("responseElements"),
                    Some(&Value::Null),
                    "case {case:?}: a refused call answered nothing to record, got {record}"
                );
                assert_eq!(
                    record.get("readOnly"),
                    Some(&json!(row.read_only)),
                    "case {case:?}: expected readOnly {}, got {record}",
                    row.read_only
                );
            }

            // The same write, once the reference is gone: served, and recorded with no error
            // — so the refusal above was about the state, not about the operation.
            elbv2_ok(
                &router,
                &runtime,
                "DeleteListener",
                &[("ListenerArn", listener.as_str())],
            );
            let deleted = elbv2_ok(
                &router,
                &runtime,
                "DeleteTargetGroup",
                &[("TargetGroupArn", group.as_str())],
            );
            let delete_request_id = element(&deleted, "RequestId");
            let deletes = lookup(&router, &runtime, "EventName", "DeleteTargetGroup");
            let served_delete =
                by_request_id("the served delete", &deletes, &delete_request_id);
            assert_eq!(
                served_delete.get("errorCode"),
                None,
                "a served delete carries no error code at all, got {served_delete}"
            );
            assert_eq!(
                served_delete.get("readOnly"),
                Some(&json!(false)),
                "a delete changes the account's resources, got {served_delete}"
            );
        })
        .expect("spawn the Elastic Load Balancing management-event thread")
        .join()
        .expect(
            "refused Elastic Load Balancing calls must be in CloudTrail event history",
        );
}
