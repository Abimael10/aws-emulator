// SPDX-License-Identifier: CC0-1.0

//! What a flow log whose destination is CloudWatch Logs actually DELIVERS, read back out of
//! the group through CloudWatch Logs' own `FilterLogEvents`.
//!
//! The rules under test, each over a SET of inputs rather than on one request:
//!
//! 1. A FLOW LOG PUBLISHES RECORDS FOR THE INTERFACES ITS SCOPE CONTAINS, AND FOR NO OTHERS:
//!    "Creates one or more flow logs to capture information about IP traffic for a specific
//!    network interface, subnet, or VPC … each network interface has a unique log stream in
//!    the log group" (`CreateFlowLogs`). So the `interface-id` of the records in the group —
//!    and the streams they travel — are exactly the interfaces the monitored resource
//!    contains: one interface for a `NetworkInterface` flow log, that subnet's interfaces for
//!    a `Subnet` one, that VPC's for a `VPC` one, and never an interface of another subnet or
//!    another VPC.
//! 2. THE RECORD IS WRITTEN IN THE FLOW LOG'S OWN `LogFormat`: the fields the format names, in
//!    the order it names them, separated by the single space the format separates them with
//!    ("specify the fields using the `${field-id}` format, separated by spaces",
//!    `CreateFlowLogsRequest$LogFormat`, which is why a format that joins its markers with a
//!    separator of its own is refused rather than delivered), with the no-value symbol
//!    for a field the record has no value for ("If a field is not applicable or could not be
//!    computed for a specific record, the record displays a '-' symbol for that entry", Amazon
//!    VPC User Guide, "Flow log records") and the `version` the fields the format names imply.
//!    A window in which the interface carried no traffic is published all the same, with
//!    `log-status` `NODATA` — "There was no network traffic to or from the network interface
//!    during the aggregation interval" (same table) — which is the state of every interface of
//!    this emulator, nothing here sending a packet between two instances.
//! 3. THE DELIVERY ROLE DECIDES WHETHER ANYTHING IS DELIVERED AT ALL: the records are written
//!    under "the ARN of the IAM role that allows Amazon EC2 to publish flow logs to the log
//!    destination" (`CreateFlowLogsRequest$DeliverLogsPermissionArn`), so a role whose
//!    permissions do not allow `logs:PutLogEvents` on the group delivers NOTHING and the flow
//!    log reports it where a client reads it: `DeliverLogsStatus` `FAILED` with
//!    `DeliverLogsErrorMessage` `Access error` — "`Access error` indicates that the IAM role
//!    associated with the flow log does not have sufficient permissions to publish to
//!    CloudWatch Logs" (`FlowLog$DeliverLogsErrorMessage`). A role that allows it delivers and
//!    the flow log reports `SUCCESS`.
//!
//! Every service is mounted on ONE shared engine, exactly as the assembled emulator mounts
//! them: the group is created and read through CloudWatch Logs' own wire, and the delivery
//! role and its policies through IAM's, so what this suite asserts is what a client of those
//! services sees.
//!
//! Every wait here is a POLL of the condition with a generous deadline — a record is owed by
//! the clock that closes an aggregation window — so nothing depends on how fast the host runs.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;
use tokio::time::Instant;

use crate::ec2_query::{call, element, elements, query_call_as};
use crate::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The access key id every call here presents: the account's own published credential, which
/// is the identity that provisions the roles, the groups and the flow logs.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The `X-Amz-Target` prefix CloudWatch Logs' awsJson operations carry.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The deadline every poll below gives up at, and how often it looks again.
///
/// Generous: what is under test is the record a window holds, not how soon the window closes,
/// so all the deadline bounds is a hang — and the interval is short enough that the deadline
/// costs many looks rather than one.
const POLL_DEADLINE: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The no-value symbol a record writes for a field it has no value for, and the `log-status`
/// of a window the interface carried no traffic in. Written out here rather than read from the
/// crate, so this suite cannot drift from the implementation's own spelling.
const NO_VALUE: &str = "-";
const NODATA: &str = "NODATA";

/// The delivery outcomes `FlowLog$DeliverLogsStatus` reports, and the message the model gives
/// the role that may not publish.
const SUCCESS: &str = "SUCCESS";
const FAILED: &str = "FAILED";
const ACCESS_ERROR: &str = "Access error";

/// The write a flow log's records are put in the group with, which the delivery role is asked
/// for.
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// The trust policy a flow-log delivery role carries, as AWS's own published example writes
/// it: the service that publishes the records may assume it.
const FLOW_LOG_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"vpc-flow-logs.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// EC2 beside the two services a CloudWatch Logs flow log needs — the one that owns the group
/// its records land in, and the one that holds its delivery role's permissions — on ONE shared
/// engine, exactly as the assembled router mounts them.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere;
        // these cases are about these services' behaviour.
        router.set_request_authentication(false);
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router, engine }
    }

    /// One AMI of the calling account, seeded the way a register operation would store it —
    /// what a launch needs an image id for.
    fn given_image(&self) -> String {
        let store = self.engine.state::<ec2::state::ComAmazonawsEc2Amazonec2State>();
        let mut guard = store.lock();
        guard.insert_image(ec2::state::ImageSpec {
            name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
            ..ec2::state::ImageSpec::default()
        })
    }

    /// One instance of the account in `subnet_id`, whose launch creates the primary network
    /// interface a flow log of that subnet's VPC captures.
    async fn given_instance(&self, case: &str, image_id: &str, subnet_id: &str) -> String {
        let body = self
            .served(
                case,
                "RunInstances",
                &[
                    param("ImageId", image_id),
                    param("InstanceType", "t3.micro"),
                    param("SubnetId", subnet_id),
                    param("MinCount", "1"),
                    param("MaxCount", "1"),
                ],
            )
            .await;
        elements(&body, "instanceId")
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
    }

    /// One of EC2's own ec2Query actions.
    async fn ec2_call(&self, action: &str, params: &[Param]) -> (u16, String) {
        call(&self.router, REGION, action, params).await
    }

    /// One of IAM's own awsQuery actions, made by the account's published credential.
    async fn iam_call(&self, action: &str, params: &[(&str, &str)]) -> (u16, String) {
        query_call_as(
            &self.router,
            REGION,
            "iam",
            IAM_API_VERSION,
            ACCOUNT_KEY_ID,
            action,
            params,
        )
        .await
    }

    /// One of CloudWatch Logs' own awsJson operations.
    async fn logs_call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("logs.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/logs/\
                     aws4_request, SignedHeaders=host;x-amz-date, Signature=00"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One EC2 call that must be served, answering its body.
    async fn served(&self, case: &str, action: &str, params: &[Param]) -> String {
        let (status, body) = self.ec2_call(action, params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {action} must answer 200, got {status} with body {body}"
        );
        body
    }

    /// One VPC of the account.
    async fn given_vpc(&self, case: &str, cidr_block: &str) -> String {
        let body = self
            .served(case, "CreateVpc", &[param("CidrBlock", cidr_block)])
            .await;
        element(&body, "vpcId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no vpcId in {body}"))
    }

    /// One subnet of `vpc_id`.
    async fn given_subnet(&self, case: &str, vpc_id: &str, cidr_block: &str) -> String {
        let body = self
            .served(
                case,
                "CreateSubnet",
                &[param("VpcId", vpc_id), param("CidrBlock", cidr_block)],
            )
            .await;
        element(&body, "subnetId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnetId in {body}"))
    }

    /// One elastic network interface in `subnet_id` — the thing a flow log's records are
    /// about.
    async fn given_interface(&self, case: &str, subnet_id: &str) -> String {
        let body = self
            .served(
                case,
                "CreateNetworkInterface",
                &[param("SubnetId", subnet_id)],
            )
            .await;
        element(&body, "networkInterfaceId").unwrap_or_else(|| {
            panic!("case {case:?}: CreateNetworkInterface reported no id in {body}")
        })
    }

    /// One log group of the account, created through CloudWatch Logs' own wire — the
    /// "existing CloudWatch Logs log group" a flow log is pointed at.
    async fn given_log_group(&self, case: &str, name: &str) {
        let (status, payload) = self
            .logs_call("CreateLogGroup", &json!({ "logGroupName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateLogGroup({name}) must answer 200, got {status} with {payload}"
        );
    }

    /// One IAM role carrying `document` as its single inline policy, answering its ARN.
    async fn given_role(&self, case: &str, role_name: &str, document: Option<&str>) -> String {
        let (status, created) = self
            .iam_call(
                "CreateRole",
                &[
                    ("RoleName", role_name),
                    ("AssumeRolePolicyDocument", FLOW_LOG_TRUST_POLICY),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the role {role_name} must be created, got {status} {created}"
        );
        if let Some(document) = document {
            let (status, put) = self
                .iam_call(
                    "PutRolePolicy",
                    &[
                        ("RoleName", role_name),
                        ("PolicyName", "flow-log-delivery"),
                        ("PolicyDocument", document),
                    ],
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the policy of {role_name} must be stored, got {status} {put}"
            );
        }
        format!(
            "arn:aws:iam::{}:role/{role_name}",
            ::engine::emulator_account_id()
        )
    }

    /// The events `log_group` holds right now, as `(stream, message)` pairs — read through
    /// CloudWatch Logs' own `FilterLogEvents`, which is how a client reads its flow logs.
    async fn events(&self, case: &str, log_group: &str) -> Vec<(String, String)> {
        let (status, payload) = self
            .logs_call("FilterLogEvents", &json!({ "logGroupName": log_group }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: FilterLogEvents({log_group}) must answer 200, got {status} with \
             {payload}"
        );
        payload
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        (
                            event
                                .get("logStreamName")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            event
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Look again until `log_group` holds at least `wanted` events, or [`POLL_DEADLINE`]
    /// passes: the records are owed by the clock that closes an aggregation window, so the
    /// wait is for the CONDITION and not for a span of time.
    async fn polled_events(
        &self,
        case: &str,
        log_group: &str,
        wanted: usize,
    ) -> Vec<(String, String)> {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let held = self.events(case, log_group).await;
            if held.len() >= wanted {
                return held;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {log_group} must hold at least {wanted} flow log records, got \
                 {held:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The `(deliverLogsStatus, deliverLogsErrorMessage)` `DescribeFlowLogs` reports for
    /// `flow_log_id` right now.
    async fn delivery_outcome(&self, case: &str, flow_log_id: &str) -> (String, Option<String>) {
        let body = self
            .served(
                case,
                "DescribeFlowLogs",
                &[param("FlowLogId.1", flow_log_id)],
            )
            .await;
        let parsed = document(case, &body);
        let item = parsed
            .descendants()
            .find(|node| {
                node.is_element()
                    && child_text(*node, "flowLogId").as_deref() == Some(flow_log_id)
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: DescribeFlowLogs reported no {flow_log_id} in {body}")
            });
        (
            child_text(item, "deliverLogsStatus").unwrap_or_default(),
            child_text(item, "deliverLogsErrorMessage"),
        )
    }

    /// Look again until `flow_log_id` reports the delivery status `wanted`, or
    /// [`POLL_DEADLINE`] passes.
    async fn polled_delivery_outcome(
        &self,
        case: &str,
        flow_log_id: &str,
        wanted: &str,
    ) -> (String, Option<String>) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let outcome = self.delivery_outcome(case, flow_log_id).await;
            if outcome.0 == wanted {
                return outcome;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {flow_log_id} must report deliverLogsStatus {wanted:?}, got \
                 {outcome:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// An identity-based document with one statement: `effect` on `actions` over `resource`.
fn policy(effect: &str, actions: &[&str], resource: &str) -> String {
    let actions = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<String>>()
        .join(",");
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":[{actions}],"Resource":"{resource}"}}]}}"#
    )
}

/// The document a flow-log delivery role carries in AWS's own published example: the group
/// writes the delivery makes, over every log group of the account.
fn delivery_policy() -> String {
    policy(
        "Allow",
        &[
            "logs:CreateLogGroup",
            "logs:CreateLogStream",
            PUT_LOG_EVENTS,
            "logs:DescribeLogGroups",
            "logs:DescribeLogStreams",
        ],
        "*",
    )
}

/// What one `CreateFlowLogs` of this suite asks for: the resource it monitors, where its
/// records go, the role it publishes under, the layout it writes them in and the traffic it
/// captures.
struct FlowLogRequest<'a> {
    resource_type: &'a str,
    resource_id: &'a str,
    log_group: &'a str,
    role_arn: &'a str,
    /// `None` for a create that names no `LogFormat` at all, which gets the documented
    /// default.
    log_format: Option<&'a str>,
    traffic_type: &'a str,
}

/// One `CreateFlowLogs` that must be served, answering the one flow log id it minted.
async fn created_flow_log(
    emulator: &Emulator,
    case: &str,
    request: &FlowLogRequest<'_>,
) -> String {
    let mut params = vec![
        param("ResourceType", request.resource_type),
        param("ResourceId.1", request.resource_id),
        param("TrafficType", request.traffic_type),
        param("LogDestinationType", "cloud-watch-logs"),
        param("LogGroupName", request.log_group),
        param("DeliverLogsPermissionArn", request.role_arn),
    ];
    if let Some(log_format) = request.log_format {
        params.push(param("LogFormat", log_format));
    }
    let body = emulator.served(case, "CreateFlowLogs", &params).await;
    let ids = elements(&body, "item");
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: CreateFlowLogs must report exactly one flow log id, got {ids:?} in {body}"
    );
    ids[0].clone()
}

/// The fields of a record, split on the single space the formats below separate them with.
fn fields(record: &str) -> Vec<&str> {
    record.split(' ').collect()
}

// ---------------------------------------------------------------------------
// 1. A flow log publishes records for the interfaces its scope contains
// ---------------------------------------------------------------------------

/// The format the scope rows read their records with: the interface the record is about,
/// beside the VPC and subnet that interface sits in.
const SCOPE_FORMAT: &str = "${interface-id} ${vpc-id} ${subnet-id} ${log-status}";

/// Where the `interface-id`, `vpc-id` and `subnet-id` sit in a record of [`SCOPE_FORMAT`].
const INTERFACE_FIELD: usize = 0;
const VPC_FIELD: usize = 1;
const SUBNET_FIELD: usize = 2;

/// RULE: the records in a flow log's group are about exactly the network interfaces its
/// monitored resource CONTAINS — one interface for a `NetworkInterface` flow log, the subnet's
/// for a `Subnet` one, the VPC's for a `VPC` one — and each interface's records travel a stream
/// of their own, named after it.
#[tokio::test]
async fn a_flow_log_publishes_records_for_the_interfaces_its_scope_contains() {
    let emulator = Emulator::new();
    let case = "the estate";
    let vpc = emulator.given_vpc(case, "10.80.0.0/16").await;
    let first_subnet = emulator.given_subnet(case, &vpc, "10.80.1.0/24").await;
    let second_subnet = emulator.given_subnet(case, &vpc, "10.80.2.0/24").await;
    let first = emulator.given_interface(case, &first_subnet).await;
    let second = emulator.given_interface(case, &first_subnet).await;
    let third = emulator.given_interface(case, &second_subnet).await;
    // Another VPC of the same account, whose interface no flow log below captures.
    let other_vpc = emulator.given_vpc(case, "10.90.0.0/16").await;
    let other_subnet = emulator.given_subnet(case, &other_vpc, "10.90.1.0/24").await;
    let outsider = emulator.given_interface(case, &other_subnet).await;
    let role = emulator
        .given_role(case, "scope-delivery", Some(&delivery_policy()))
        .await;

    // (case, the FlowLogsResourceType, the resource monitored, the interfaces its records
    // must be about)
    let rows: [(&str, &str, &str, Vec<&str>); 4] = [
        (
            "one network interface",
            "NetworkInterface",
            first.as_str(),
            vec![first.as_str()],
        ),
        (
            "a subnet holding two interfaces",
            "Subnet",
            first_subnet.as_str(),
            vec![first.as_str(), second.as_str()],
        ),
        (
            "a subnet holding one interface",
            "Subnet",
            second_subnet.as_str(),
            vec![third.as_str()],
        ),
        (
            "a VPC holding every subnet's interfaces",
            "VPC",
            vpc.as_str(),
            vec![first.as_str(), second.as_str(), third.as_str()],
        ),
    ];

    // Every flow log is created first, so the windows of all of them run together rather than
    // one row's poll waiting out the row before it.
    let mut created: Vec<(&str, String, String, Vec<&str>)> = Vec::new();
    for (case, resource_type, resource_id, expected) in rows {
        let log_group = format!("/aws/vpc/flow-logs/scope/{resource_id}");
        emulator.given_log_group(case, &log_group).await;
        let flow_log_id = created_flow_log(
            &emulator,
            case,
            &FlowLogRequest {
                resource_type,
                resource_id,
                log_group: &log_group,
                role_arn: &role,
                log_format: Some(SCOPE_FORMAT),
                traffic_type: "ALL",
            },
        )
        .await;
        created.push((case, log_group, flow_log_id, expected));
    }

    for (case, log_group, flow_log_id, expected) in created {
        let events = emulator
            .polled_events(case, &log_group, expected.len())
            .await;
        let reported: BTreeSet<String> = events
            .iter()
            .map(|(_, record)| fields(record)[INTERFACE_FIELD].to_string())
            .collect();
        let wanted: BTreeSet<String> = expected.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            reported, wanted,
            "case {case:?}: the group {log_group} must carry records for exactly {wanted:?}, got \
             {reported:?}"
        );
        assert!(
            !reported.contains(&outsider),
            "case {case:?}: the group {log_group} must carry no record of the interface outside \
             its scope ({outsider}), got {reported:?}"
        );
        // "Each network interface has a unique log stream in the log group"
        // (`CreateFlowLogs`): the stream a record travels is named after the interface it is
        // about and the traffic the flow log captures.
        for (stream, record) in &events {
            let interface = fields(record)[INTERFACE_FIELD];
            let expected = format!("{interface}-all");
            assert_eq!(
                *stream, expected,
                "case {case:?}: the record {record:?} must travel the stream {expected:?}, got \
                 {stream}"
            );
        }
        // The VPC and subnet of each record are the ones that interface really sits in.
        for (_, record) in &events {
            let written = fields(record);
            assert_eq!(
                written[VPC_FIELD], vpc,
                "case {case:?}: the record {record:?} must name the VPC its interface sits in"
            );
            assert!(
                written[SUBNET_FIELD] == first_subnet || written[SUBNET_FIELD] == second_subnet,
                "case {case:?}: the record {record:?} must name a subnet of the monitored VPC, \
                 got {:?}",
                written[SUBNET_FIELD]
            );
        }
        // A flow log that is delivering reports it ([`SUCCESS`]).
        let (status, message) = emulator
            .polled_delivery_outcome(case, &flow_log_id, SUCCESS)
            .await;
        assert_eq!(
            (status.as_str(), message.as_deref()),
            (SUCCESS, None),
            "case {case:?}: a delivering flow log must report {SUCCESS} and no error message"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The record is written in the flow log's own LogFormat
// ---------------------------------------------------------------------------

/// One row of the format table: the format the flow log is created with (`None` for a create
/// that names none at all, which gets the documented default), the fields a record of it must
/// carry in order, and the `version` such a record reports.
struct FormatRow {
    case: &'static str,
    log_format: Option<&'static str>,
    fields: &'static [&'static str],
    version: &'static str,
}

/// The default format's fields, in the order AWS documents them — written out here rather than
/// read from the crate, so this suite pins the layout a client parses.
const DEFAULT_FIELDS: &[&str] = &[
    "version",
    "account-id",
    "interface-id",
    "srcaddr",
    "dstaddr",
    "srcport",
    "dstport",
    "protocol",
    "packets",
    "bytes",
    "start",
    "end",
    "action",
    "log-status",
];

/// RULE: a record is the flow log's `LogFormat` with each `${field-id}` token replaced by what
/// that record reports for it — so the fields are the ones the format names, in its order —
/// and a field the record has no value for is written as the no-value symbol. A window the
/// interface carried no traffic in is published with `log-status` `NODATA`.
#[tokio::test]
async fn a_record_follows_the_log_format_the_flow_log_was_created_with() {
    let emulator = Emulator::new();
    let case = "the estate";
    let vpc = emulator.given_vpc(case, "10.81.0.0/16").await;
    let subnet = emulator.given_subnet(case, &vpc, "10.81.1.0/24").await;
    let interface = emulator.given_interface(case, &subnet).await;
    let role = emulator
        .given_role(case, "format-delivery", Some(&delivery_policy()))
        .await;

    let rows: [FormatRow; 5] = [
        FormatRow {
            case: "no LogFormat at all, which gets the documented default",
            log_format: None,
            fields: DEFAULT_FIELDS,
            version: "2",
        },
        FormatRow {
            case: "the default fields written out in another order",
            log_format: Some("${log-status} ${interface-id} ${account-id} ${version}"),
            fields: &["log-status", "interface-id", "account-id", "version"],
            version: "2",
        },
        FormatRow {
            case: "a format naming only the fields of the flow",
            log_format: Some("${srcaddr} ${dstaddr} ${srcport} ${dstport} ${protocol} ${action}"),
            fields: &["srcaddr", "dstaddr", "srcport", "dstport", "protocol", "action"],
            // No `version` field is written at all, so the row asserts none.
            version: "",
        },
        FormatRow {
            case: "a format naming fields a later version added",
            log_format: Some("${version} ${interface-id} ${vpc-id} ${subnet-id} ${type}"),
            fields: &["version", "interface-id", "vpc-id", "subnet-id", "type"],
            version: "3",
        },
        FormatRow {
            case: "a format naming a field of the newest published block",
            log_format: Some("${version} ${interface-id} ${flow-direction} ${traffic-path}"),
            fields: &["version", "interface-id", "flow-direction", "traffic-path"],
            version: "5",
        },
    ];

    let mut created: Vec<(&FormatRow, String)> = Vec::new();
    for row in &rows {
        let log_group = format!("/aws/vpc/flow-logs/format/{}", created.len());
        emulator.given_log_group(row.case, &log_group).await;
        created_flow_log(
            &emulator,
            row.case,
            &FlowLogRequest {
                resource_type: "NetworkInterface",
                resource_id: &interface,
                log_group: &log_group,
                role_arn: &role,
                log_format: row.log_format,
                traffic_type: "ALL",
            },
        )
        .await;
        created.push((row, log_group));
    }

    for (row, log_group) in created {
        let case = row.case;
        let events = emulator.polled_events(case, &log_group, 1).await;
        for (_, record) in &events {
            let written = fields(record);
            assert_eq!(
                written.len(),
                row.fields.len(),
                "case {case:?}: the record {record:?} must carry one value per field of the \
                 format ({:?})",
                row.fields
            );
            for (field, value) in row.fields.iter().zip(written.iter()) {
                let expected = match *field {
                    "version" => row.version.to_string(),
                    "account-id" => ::engine::emulator_account_id().to_string(),
                    "interface-id" => interface.clone(),
                    "vpc-id" => vpc.clone(),
                    "subnet-id" => subnet.clone(),
                    "log-status" => NODATA.to_string(),
                    // The bounds of the window are asserted below, as the span they are.
                    "start" | "end" => continue,
                    // Every field of the FLOW: a window with no traffic in it has no value
                    // for any of them.
                    _ => NO_VALUE.to_string(),
                };
                assert_eq!(
                    *value, expected,
                    "case {case:?}: the {field} of {record:?} must be {expected:?}, got {value:?}"
                );
            }
            // The window a record reports is a real span of time: both bounds are epoch
            // seconds, and the end is after the start.
            if let (Some(start), Some(end)) = (
                row.fields.iter().position(|field| *field == "start"),
                row.fields.iter().position(|field| *field == "end"),
            ) {
                let start: i64 = written[start].parse().unwrap_or_else(|error| {
                    panic!("case {case:?}: the start of {record:?} must be epoch seconds: {error}")
                });
                let end: i64 = written[end].parse().unwrap_or_else(|error| {
                    panic!("case {case:?}: the end of {record:?} must be epoch seconds: {error}")
                });
                assert!(
                    end > start,
                    "case {case:?}: the window of {record:?} must end after it starts, got \
                     {start}..{end}"
                );
            }
        }
    }
}

/// RULE: a record format lays out ONE field per space-separated token — "specify the fields
/// using the `${field-id}` format, separated by spaces"
/// (`CreateFlowLogsRequest$LogFormat`) — so a format that joins its markers with a separator
/// of its own gives one token that names no published field: the create is REFUSED and the
/// log group it would have published to is left with nothing at all.
#[tokio::test]
async fn a_format_that_does_not_separate_its_fields_with_spaces_is_refused() {
    let emulator = Emulator::new();
    let case = "a comma-separated format";
    let vpc = emulator.given_vpc(case, "10.82.0.0/16").await;
    let subnet = emulator.given_subnet(case, &vpc, "10.82.1.0/24").await;
    let interface = emulator.given_interface(case, &subnet).await;
    let role = emulator
        .given_role(case, "separator-delivery", Some(&delivery_policy()))
        .await;
    let log_group = "/aws/vpc/flow-logs/separators";
    emulator.given_log_group(case, log_group).await;
    let (status, body) = emulator
        .ec2_call(
            "CreateFlowLogs",
            &[
                param("ResourceType", "NetworkInterface"),
                param("ResourceId.1", &interface),
                param("TrafficType", "ALL"),
                param("LogDestinationType", "cloud-watch-logs"),
                param("LogGroupName", log_group),
                param("DeliverLogsPermissionArn", &role),
                param("LogFormat", "${interface-id},${action},${log-status}"),
            ],
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a format that names no field, got {status} with \
         body {body}"
    );
    // Nothing was turned on, so nothing publishes: the group a served create would have
    // delivered to holds no record at all.
    let held = emulator.events(case, log_group).await;
    assert!(
        held.is_empty(),
        "case {case:?}: a refused create must publish nothing, got {held:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. The delivery role decides whether anything is delivered at all
// ---------------------------------------------------------------------------

/// One row of the delivery-role table: the document the role carries, the delivery status the
/// flow log must report, the error message it must carry, and whether its group holds records.
struct RoleRow {
    case: &'static str,
    role_name: &'static str,
    document: Option<String>,
    status: &'static str,
    message: Option<&'static str>,
    delivers: bool,
}

/// RULE: the records are written under the flow log's delivery role, so a role whose
/// permissions do not allow the write that puts them in the group delivers NOTHING and the
/// flow log reports `FAILED` with the message the model gives that cause, while a role that
/// allows it delivers and the flow log reports `SUCCESS`.
#[tokio::test]
async fn the_delivery_role_decides_whether_the_records_are_delivered() {
    let emulator = Emulator::new();
    let case = "the estate";
    let vpc = emulator.given_vpc(case, "10.83.0.0/16").await;
    let subnet = emulator.given_subnet(case, &vpc, "10.83.1.0/24").await;
    let interface = emulator.given_interface(case, &subnet).await;

    let group_of = |row: &str| format!("/aws/vpc/flow-logs/role/{row}");
    let rows: [RoleRow; 5] = [
        RoleRow {
            case: "a role carrying the published delivery policy",
            role_name: "role-delivery-allowed",
            document: Some(delivery_policy()),
            status: SUCCESS,
            message: None,
            delivers: true,
        },
        RoleRow {
            case: "a role granted the write on this group alone",
            role_name: "role-delivery-scoped",
            document: Some(policy(
                "Allow",
                &["logs:CreateLogStream", PUT_LOG_EVENTS],
                &format!(
                    "arn:aws:logs:{REGION}:{}:log-group:{}:*",
                    ::engine::emulator_account_id(),
                    group_of("role-delivery-scoped"),
                ),
            )),
            status: SUCCESS,
            message: None,
            delivers: true,
        },
        RoleRow {
            case: "a role that may create the stream but not put the events",
            role_name: "role-delivery-without-put",
            document: Some(policy(
                "Allow",
                &["logs:CreateLogGroup", "logs:CreateLogStream"],
                "*",
            )),
            status: FAILED,
            message: Some(ACCESS_ERROR),
            delivers: false,
        },
        RoleRow {
            case: "a role explicitly denied the write",
            role_name: "role-delivery-denied",
            document: Some(policy("Deny", &[PUT_LOG_EVENTS], "*")),
            status: FAILED,
            message: Some(ACCESS_ERROR),
            delivers: false,
        },
        RoleRow {
            case: "a role granted the write on another group",
            role_name: "role-delivery-elsewhere",
            document: Some(policy(
                "Allow",
                &[PUT_LOG_EVENTS],
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vpc/somewhere-else:*",
            )),
            status: FAILED,
            message: Some(ACCESS_ERROR),
            delivers: false,
        },
    ];

    let mut created: Vec<(&RoleRow, String, String)> = Vec::new();
    for row in &rows {
        let log_group = group_of(row.role_name);
        emulator.given_log_group(row.case, &log_group).await;
        let role = emulator
            .given_role(row.case, row.role_name, row.document.as_deref())
            .await;
        let flow_log_id = created_flow_log(
            &emulator,
            row.case,
            &FlowLogRequest {
                resource_type: "NetworkInterface",
                resource_id: &interface,
                log_group: &log_group,
                role_arn: &role,
                log_format: Some(SCOPE_FORMAT),
                traffic_type: "ALL",
            },
        )
        .await;
        created.push((row, log_group, flow_log_id));
    }

    for (row, log_group, flow_log_id) in created {
        let case = row.case;
        let (status, message) = emulator
            .polled_delivery_outcome(case, &flow_log_id, row.status)
            .await;
        assert_eq!(
            (status.as_str(), message.as_deref()),
            (row.status, row.message),
            "case {case:?}: the flow log must report the delivery outcome of its role"
        );
        if row.delivers {
            let events = emulator.polled_events(case, &log_group, 1).await;
            for (_, record) in &events {
                assert_eq!(
                    fields(record)[INTERFACE_FIELD],
                    interface,
                    "case {case:?}: the delivered record {record:?} must be about {interface}"
                );
            }
        } else {
            // The group was created by this suite, so it is there to be read — and a flow log
            // that may not write stays empty rather than quietly delivering anyway.
            let held = emulator.events(case, &log_group).await;
            assert!(
                held.is_empty(),
                "case {case:?}: the group of a flow log whose role may not publish must stay \
                 empty, got {held:?}"
            );
        }
    }
}

/// RULE: the interfaces a flow log captures are the ones the STORE holds for its monitored
/// resource, whoever created them — so the primary network interface a LAUNCH creates for its
/// instance is captured by that VPC's flow log, and its records name that instance.
#[tokio::test]
async fn the_primary_interface_a_launch_creates_is_captured_by_its_vpcs_flow_log() {
    let emulator = Emulator::new();
    let case = "a launched instance";
    let image_id = emulator.given_image();
    let vpc = emulator.given_vpc(case, "10.85.0.0/16").await;
    let subnet = emulator.given_subnet(case, &vpc, "10.85.1.0/24").await;
    let instance_id = emulator.given_instance(case, &image_id, &subnet).await;
    let interface =
        crate::ec2_query::primary_network_interface(&emulator.router, REGION, case, &instance_id)
            .await;
    let role = emulator
        .given_role(case, "launch-delivery", Some(&delivery_policy()))
        .await;
    let log_group = "/aws/vpc/flow-logs/launched";
    emulator.given_log_group(case, log_group).await;
    created_flow_log(
        &emulator,
        case,
        &FlowLogRequest {
            resource_type: "VPC",
            resource_id: &vpc,
            log_group,
            role_arn: &role,
            // The fields that say WHOSE traffic the record is about.
            log_format: Some("${interface-id} ${instance-id} ${vpc-id} ${subnet-id} ${log-status}"),
            traffic_type: "ACCEPT",
        },
    )
    .await;

    let events = emulator.polled_events(case, log_group, 1).await;
    for (stream, record) in &events {
        let written = fields(record);
        assert_eq!(
            written,
            vec![
                interface.as_str(),
                instance_id.as_str(),
                vpc.as_str(),
                subnet.as_str(),
                NODATA,
            ],
            "case {case:?}: the record {record:?} must be about the launched instance's own \
             interface"
        );
        // The stream carries the traffic this flow log captures, which is not every flow.
        assert_eq!(
            *stream,
            format!("{interface}-accept"),
            "case {case:?}: the record must travel the stream of the traffic the flow log \
             captures, got {stream}"
        );
    }
}

/// RULE: which interfaces a flow log captures follows the STORE, so an interface created after
/// the flow log is captured by it too — the scope is the resource, not a snapshot of it.
#[tokio::test]
async fn an_interface_added_to_the_scope_afterwards_is_captured_too() {
    let emulator = Emulator::new();
    let case = "an interface created after the flow log";
    let vpc = emulator.given_vpc(case, "10.84.0.0/16").await;
    let subnet = emulator.given_subnet(case, &vpc, "10.84.1.0/24").await;
    let first = emulator.given_interface(case, &subnet).await;
    let role = emulator
        .given_role(case, "late-delivery", Some(&delivery_policy()))
        .await;
    let log_group = "/aws/vpc/flow-logs/late";
    emulator.given_log_group(case, log_group).await;
    created_flow_log(
        &emulator,
        case,
        &FlowLogRequest {
            resource_type: "VPC",
            resource_id: &vpc,
            log_group,
            role_arn: &role,
            log_format: Some(SCOPE_FORMAT),
            traffic_type: "ALL",
        },
    )
    .await;

    let events = emulator.polled_events(case, log_group, 1).await;
    let reported: BTreeSet<String> = events
        .iter()
        .map(|(_, record)| fields(record)[INTERFACE_FIELD].to_string())
        .collect();
    assert_eq!(
        reported,
        BTreeSet::from([first.clone()]),
        "case {case:?}: the VPC held one interface when the flow log was created, got {reported:?}"
    );

    let second = emulator.given_interface(case, &subnet).await;
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reported: BTreeSet<String> = emulator
            .events(case, log_group)
            .await
            .iter()
            .map(|(_, record)| fields(record)[INTERFACE_FIELD].to_string())
            .collect();
        if reported.contains(&second) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the interface {second} created inside the monitored VPC must be \
             captured too, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
