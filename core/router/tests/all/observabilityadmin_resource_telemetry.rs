// SPDX-License-Identifier: CC0-1.0

//! The telemetry configurations CloudWatch Observability Admin's telemetry-config audit
//! reports — `ListResourceTelemetry`.
//!
//! RULE: an audit reports one telemetry configuration per resource of a supported type the
//! account holds in the Region the request was signed for, and every member of that
//! configuration is what the service that OWNS the resource holds right now: the resource's
//! own identifier and tags, and the state of each kind of telemetry read from the owning
//! service's configuration for it (`Enabled` / `Disabled`, and `NotApplicable` for a kind of
//! telemetry the resource has no configuration for at all). The request only NARROWS that
//! audit — by resource type, which also decides the order the types are reported in, by
//! identifier prefix, by tag and by telemetry state, each a conjunct — and pages it; the
//! audit writes nothing, in this service or in the ones it asks.
//!
//! This suite lives beside the router because the resources are Amazon EC2's: every instance
//! and VPC below is created by EC2's own operations on the SAME engine that serves
//! Observability Admin, so each identifier in an assertion is one EC2 handed out and each
//! telemetry state is one EC2 really holds — detailed monitoring for an instance
//! (`Instance$Monitoring$State`) and a flow log for a VPC (`DescribeFlowLogs`) — rather than
//! something this suite told Observability Admin to report.
//!
//! The rows differ along the resource type, along whether its telemetry is configured, along
//! which filter narrows the audit and along the page size the walk uses, because the rule is
//! about reporting what the owners hold, not about one recorded request.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for: an audit covers the Region the request was
/// signed for, and the resources are created in that same Region.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every configuration's
/// `AccountIdentifier` carries.
const ACCOUNT: &str = "000000000000";

/// The AMI every launch below runs from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// `AWS::EC2::Instance`, a `ResourceType` value
/// (`smithy_query(query="shape", shape="ResourceType")` on the observabilityadmin model).
const INSTANCE_TYPE: &str = "AWS::EC2::Instance";

/// `AWS::EC2::VPC`, another value of that enum.
const VPC_TYPE: &str = "AWS::EC2::VPC";

/// A third value of that enum, for the rows that ask about a supported resource type this
/// account holds nothing of.
const FUNCTION_TYPE: &str = "AWS::Lambda::Function";

/// The `TelemetrySourceType` value `AWS::EC2::VPC` correlates with: a VPC's logs are its flow
/// logs ("such as VPC_FLOW_LOGS or EKS_AUDIT_LOGS", `TelemetryRule$TelemetrySourceTypes`).
const VPC_FLOW_LOGS: &str = "VPC_FLOW_LOGS";

/// The CloudWatch Logs log group the flow logs below publish to: "a new or existing
/// CloudWatch Logs log group" (`CreateFlowLogsRequest$LogGroupName`), so EC2 resolves nothing
/// for it.
const LOG_GROUP: &str = "/aws/vpc/flow-logs";

/// The delivery role a CloudWatch Logs flow log requires: "this parameter is required if the
/// destination type is cloud-watch-logs" (`CreateFlowLogsRequest$DeliverLogsPermissionArn`).
const DELIVER_LOGS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/flow-logs";

/// The tag key the fixture puts on one instance and one VPC, for the rows that narrow an
/// audit by tag. Distinctive, so no resource the account came with can carry it.
const AUDIT_TAG_KEY: &str = "TelemetryAudited";

/// That tag's value.
const AUDIT_TAG_VALUE: &str = "yes";

/// How long a poll waits for a transition the clock owes — an instance reaching `terminated`,
/// detailed monitoring reaching `enabled`. Generous by design: it is a deadline, not a
/// measurement, and nothing below asserts on how long a transition took.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator carrying Amazon EC2 and Observability Admin over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_observabilityadmin_observabilityadmin::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and which Region it is served in; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// POST one ec2Query action, as its HTTP status and XML body.
async fn ec2_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One `ListResourceTelemetry` call over the restJson1 wire, as its status and parsed body.
async fn audit_call(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/ListResourceTelemetry")
        .header(
            "host",
            format!("observabilityadmin.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization("observabilityadmin"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// One resource as a case names it: its type and the identifier its owner holds it under,
/// which is the pair an audit reports it as.
type Resource = (String, String);

/// One row of the configuration table: the case, the resource, the telemetry type audited for
/// it, that telemetry's state, the tags its owner holds and the source type it correlates
/// with.
type ConfigurationRow<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a [(&'a str, &'a str)],
    Option<&'a str>,
);

/// One row of the resource-type table: the case, the types the request names and the
/// resources the audit must report, in order.
type TypeRow<'a> = (&'a str, &'a [&'a str], Vec<Resource>);

/// One row of the filter table: the case, the whole request and the resources it narrows the
/// audit to.
type FilterRow<'a> = (&'a str, Value, Vec<Resource>);

/// One telemetry configuration as a case reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Audited {
    account: Option<String>,
    resource_type: Option<String>,
    resource_identifier: Option<String>,
    telemetry_state: BTreeMap<String, String>,
    tags: BTreeMap<String, String>,
    source_type: Option<String>,
    last_update: Option<i64>,
}

/// The string map a document member carries, empty when it carries none.
fn string_map(document: &Value, member: &str) -> BTreeMap<String, String> {
    document
        .get(member)
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One string member of a document.
fn member(document: &Value, name: &str) -> Option<String> {
    document
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The configurations an answer reports, in the order it reported them.
fn configurations(body: &Value) -> Vec<Audited> {
    body.get("TelemetryConfigurations")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| Audited {
                    account: member(entry, "AccountIdentifier"),
                    resource_type: member(entry, "ResourceType"),
                    resource_identifier: member(entry, "ResourceIdentifier"),
                    telemetry_state: string_map(entry, "TelemetryConfigurationState"),
                    tags: string_map(entry, "ResourceTags"),
                    source_type: member(entry, "TelemetrySourceType"),
                    last_update: entry.get("LastUpdateTimeStamp").and_then(Value::as_i64),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One audit that must succeed, as the configurations it reported and its `NextToken`.
async fn audit(
    router: &MultiServiceRouter,
    case: &str,
    request: &Value,
) -> (Vec<Audited>, Option<String>) {
    let (status, body) = audit_call(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    (configurations(&body), member(&body, "NextToken"))
}

/// The `(resource type, identifier)` pairs an audit reported, in report order.
fn reported(configurations: &[Audited]) -> Vec<Resource> {
    configurations
        .iter()
        .map(|entry| {
            (
                entry.resource_type.clone().unwrap_or_default(),
                entry.resource_identifier.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// A tag specification for one resource type, as an ec2Query form carries it.
fn tag_params(resource_type: &str, tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut params = vec![param("TagSpecification.1.ResourceType", resource_type)];
    for (position, (key, value)) in tags.iter().enumerate() {
        let index = position + 1;
        params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
        params.push(param(&format!("TagSpecification.1.Tag.{index}.Value"), value));
    }
    params
}

/// Launch one instance, with detailed monitoring on or off, and answer its id.
async fn launch(router: &MultiServiceRouter, monitored: bool, tags: &[(&str, &str)]) -> String {
    let mut params = vec![
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        // "Specifies whether detailed monitoring is enabled for the instance"
        // (`RunInstancesRequest$Monitoring`).
        param("Monitoring.Enabled", &monitored.to_string()),
    ];
    params.extend(tag_params("instance", tags));
    let body = ec2(router, "RunInstances", &params).await;
    element(&body, "instanceId")
}

/// Create one VPC and answer its id.
async fn create_vpc(router: &MultiServiceRouter, cidr: &str, tags: &[(&str, &str)]) -> String {
    let mut params = vec![param("CidrBlock", cidr)];
    params.extend(tag_params("vpc", tags));
    let body = ec2(router, "CreateVpc", &params).await;
    element(&body, "vpcId")
}

/// Turn flow logs on for `vpc_id`, publishing to CloudWatch Logs, and answer the flow log id.
async fn create_flow_log(router: &MultiServiceRouter, vpc_id: &str) -> String {
    let params = [
        param("ResourceType", "VPC"),
        param("ResourceId.1", vpc_id),
        param("TrafficType", "ALL"),
        param("LogDestinationType", "cloud-watch-logs"),
        param("LogGroupName", LOG_GROUP),
        param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN),
    ];
    let body = ec2(router, "CreateFlowLogs", &params).await;
    let ids = elements(&body, "item");
    assert_eq!(
        ids.len(),
        1,
        "setup: CreateFlowLogs on {vpc_id} must mint exactly one flow log, got {ids:?}"
    );
    ids[0].clone()
}

/// The resources every case audits: an instance with detailed monitoring and one without, a
/// VPC with flow logs and one without — each created through its owner's own API — beside
/// the VPCs the account already held, which the audit reports for the same reason.
struct Fixture {
    router: MultiServiceRouter,
    monitored_instance: String,
    plain_instance: String,
    logged_vpc: String,
    plain_vpc: String,
    /// The VPCs EC2 already held before this fixture created any — an account comes with a
    /// default VPC, and it is a resource of the account like any other, so the audit reports
    /// it and every expectation below counts it.
    held_vpcs: Vec<String>,
}

impl Fixture {
    async fn new() -> Self {
        let router = emulator();
        let held_vpcs = elements(&ec2(&router, "DescribeVpcs", &[]).await, "vpcId");
        let monitored_instance = launch(&router, true, &[(AUDIT_TAG_KEY, AUDIT_TAG_VALUE)]).await;
        let plain_instance = launch(&router, false, &[]).await;
        let logged_vpc =
            create_vpc(&router, "10.90.0.0/16", &[(AUDIT_TAG_KEY, AUDIT_TAG_VALUE)]).await;
        let plain_vpc = create_vpc(&router, "10.91.0.0/16", &[]).await;
        create_flow_log(&router, &logged_vpc).await;
        Self {
            router,
            monitored_instance,
            plain_instance,
            logged_vpc,
            plain_vpc,
            held_vpcs,
        }
    }

    /// Every `(type, identifier)` pair the fixture's resources are audited under, in the
    /// order an unnarrowed audit reports them: instances before VPCs, each by identifier.
    fn every_resource(&self) -> Vec<Resource> {
        let mut instances = self.instances();
        instances.extend(self.vpcs());
        instances
    }

    /// The instances of the account, in the order the audit reports them.
    fn instances(&self) -> Vec<Resource> {
        let mut instances = vec![
            (INSTANCE_TYPE.to_string(), self.monitored_instance.clone()),
            (INSTANCE_TYPE.to_string(), self.plain_instance.clone()),
        ];
        instances.sort();
        instances
    }

    /// The VPCs of the account — the two this fixture created and the ones it already held —
    /// in the order the audit reports them.
    fn vpcs(&self) -> Vec<Resource> {
        let mut vpcs: Vec<Resource> = self
            .held_vpcs
            .iter()
            .chain([&self.logged_vpc, &self.plain_vpc])
            .map(|vpc_id| (VPC_TYPE.to_string(), vpc_id.clone()))
            .collect();
        vpcs.sort();
        vpcs
    }

    /// The VPCs whose logs are not configured: every one but the one with a flow log.
    fn unlogged_vpcs(&self) -> Vec<Resource> {
        self.vpcs()
            .into_iter()
            .filter(|(_, vpc_id)| *vpc_id != self.logged_vpc)
            .collect()
    }
}

/// The `TelemetryConfigurationState` of a resource whose one audited kind of telemetry is
/// `audited`, in `state` — every other modelled `TelemetryType` being `NotApplicable`
/// (`smithy_query(query="shape", shape="TelemetryType,TelemetryState")`).
fn expected_state(audited: &str, state: &str) -> BTreeMap<String, String> {
    ["Insights", "Logs", "Metrics", "Traces"]
        .into_iter()
        .map(|telemetry_type| {
            let value = if telemetry_type == audited {
                state
            } else {
                "NotApplicable"
            };
            (telemetry_type.to_string(), value.to_string())
        })
        .collect()
}

/// The configuration of the one resource `identifier` names, or `None` when the audit
/// reported none for it.
fn one(configurations: &[Audited], identifier: &str) -> Option<Audited> {
    configurations
        .iter()
        .find(|entry| entry.resource_identifier.as_deref() == Some(identifier))
        .cloned()
}

/// Poll `read` until it holds, or fail after [`CONVERGENCE_BUDGET`].
async fn poll_until<F, Fut>(case: &str, what: &str, mut read: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let last = match read().await {
            Ok(()) => return,
            Err(reason) => reason,
        };
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {what} never held; last read was {last}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An audit reports one configuration per resource its owner holds, with the telemetry state
/// that owner's own configuration puts the resource in — and reading it twice reports exactly
/// the same, because an audit writes nothing.
#[tokio::test]
async fn the_audit_reports_each_discovered_resource_as_its_owner_holds_it() {
    let fixture = Fixture::new().await;

    // (case, the resource, the audited telemetry type, its state, the tags, the source type)
    let rows: [ConfigurationRow; 4] = [
        (
            "an instance with detailed monitoring",
            &fixture.monitored_instance,
            "Metrics",
            "Enabled",
            &[(AUDIT_TAG_KEY, AUDIT_TAG_VALUE)],
            None,
        ),
        (
            "an instance without it",
            &fixture.plain_instance,
            "Metrics",
            "Disabled",
            &[],
            None,
        ),
        (
            "a VPC with flow logs",
            &fixture.logged_vpc,
            "Logs",
            "Enabled",
            &[(AUDIT_TAG_KEY, AUDIT_TAG_VALUE)],
            Some(VPC_FLOW_LOGS),
        ),
        (
            "a VPC without them",
            &fixture.plain_vpc,
            "Logs",
            "Disabled",
            &[],
            Some(VPC_FLOW_LOGS),
        ),
    ];

    for read in 0..2 {
        let case = format!("read {read}");
        let (configurations, next_token) = audit(&fixture.router, &case, &json!({})).await;
        assert_eq!(
            reported(&configurations),
            fixture.every_resource(),
            "case {case:?}: expected one configuration per resource the owners hold, got \
             {configurations:?}"
        );
        assert_eq!(
            next_token, None,
            "case {case:?}: the whole audit fits one page, so it carries no NextToken, got \
             {next_token:?}"
        );

        for (row, identifier, telemetry_type, state, tags, source_type) in rows {
            let case = format!("{case}, {row}");
            let audited = one(&configurations, identifier)
                .unwrap_or_else(|| panic!("case {case:?}: no configuration for {identifier}"));
            assert_eq!(
                audited.telemetry_state,
                expected_state(telemetry_type, state),
                "case {case:?}: expected {telemetry_type} {state}, got {:?}",
                audited.telemetry_state
            );
            let expected_tags: BTreeMap<String, String> = tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                audited.tags, expected_tags,
                "case {case:?}: expected the tags its owner holds ({expected_tags:?}), got {:?}",
                audited.tags
            );
            assert_eq!(
                audited.source_type.as_deref(),
                source_type,
                "case {case:?}: expected TelemetrySourceType {source_type:?}, got {:?}",
                audited.source_type
            );
            assert_eq!(
                audited.account.as_deref(),
                Some(ACCOUNT),
                "case {case:?}: expected the caller's account, got {:?}",
                audited.account
            );
            // The instant reported is the one the owning service holds for the configuration
            // the state was read from — a flow log's creation time — so it is there exactly
            // for a resource whose telemetry is configured through such a record.
            let configured_through_a_record = state == "Enabled" && telemetry_type == "Logs";
            assert_eq!(
                audited.last_update.is_some(),
                configured_through_a_record,
                "case {case:?}: expected LastUpdateTimeStamp present={configured_through_a_record}, \
                 got {:?}",
                audited.last_update
            );
            assert!(
                audited.last_update.is_none_or(|at| at > 0),
                "case {case:?}: expected an epoch-millisecond instant, got {:?}",
                audited.last_update
            );
        }
    }
}

/// `ResourceTypes` selects the types reported and decides the ORDER they are reported in:
/// "if this parameter is provided, the service returns the resources in the same order as
/// specified in the request".
#[tokio::test]
async fn the_resource_types_filter_selects_and_orders_the_report() {
    let fixture = Fixture::new().await;
    let instances = fixture.instances();
    let vpcs = fixture.vpcs();

    // (case, the requested types, the resources reported in order)
    let rows: [TypeRow; 5] = [
        ("one type", &[VPC_TYPE], vpcs.clone()),
        ("the other type", &[INSTANCE_TYPE], instances.clone()),
        (
            "both, VPCs first",
            &[VPC_TYPE, INSTANCE_TYPE],
            vpcs.iter().chain(instances.iter()).cloned().collect(),
        ),
        (
            "both, instances first",
            &[INSTANCE_TYPE, VPC_TYPE],
            instances.iter().chain(vpcs.iter()).cloned().collect(),
        ),
        (
            "a supported type the account holds nothing of",
            &[FUNCTION_TYPE],
            Vec::new(),
        ),
    ];

    for (case, requested, expected) in rows {
        let (configurations, _) = audit(
            &fixture.router,
            case,
            &json!({ "ResourceTypes": requested }),
        )
        .await;
        assert_eq!(
            reported(&configurations),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            reported(&configurations)
        );
    }

    // A value that is no `ResourceType` at all is a 400 ValidationException, not a type the
    // audit silently reports nothing for.
    let (status, body) = audit_call(
        &fixture.router,
        &json!({ "ResourceTypes": ["AWS::Nothing::Here"] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a type no resource could be of\": expected HTTP 400, got {status} with {body}"
    );
}

/// The identifier prefix, the tags and the telemetry state each narrow the audit, and they
/// narrow it TOGETHER: a resource is reported only when it satisfies every filter the request
/// carried.
#[tokio::test]
async fn the_prefix_tag_and_telemetry_state_filters_narrow_the_report() {
    let fixture = Fixture::new().await;
    let monitored = (INSTANCE_TYPE.to_string(), fixture.monitored_instance.clone());
    let plain_instance = (INSTANCE_TYPE.to_string(), fixture.plain_instance.clone());
    let logged = (VPC_TYPE.to_string(), fixture.logged_vpc.clone());
    let instances = fixture.instances();
    let vpcs = fixture.vpcs();
    let unlogged_vpcs = fixture.unlogged_vpcs();

    // (case, the request, the resources reported)
    let rows: [FilterRow; 10] = [
        (
            "the prefix every VPC identifier starts with",
            json!({ "ResourceIdentifierPrefix": "vpc-" }),
            vpcs.clone(),
        ),
        (
            "the prefix every instance identifier starts with",
            json!({ "ResourceIdentifierPrefix": "i-0" }),
            instances.clone(),
        ),
        (
            "one resource's whole identifier",
            json!({ "ResourceIdentifierPrefix": fixture.logged_vpc }),
            vec![logged.clone()],
        ),
        (
            "a prefix no resource of the account carries",
            json!({ "ResourceIdentifierPrefix": "nothing-here" }),
            Vec::new(),
        ),
        (
            "a tag pair its owner holds on two resources",
            json!({ "ResourceTags": { AUDIT_TAG_KEY: AUDIT_TAG_VALUE } }),
            vec![monitored.clone(), logged.clone()],
        ),
        (
            "that tag key with another value",
            json!({ "ResourceTags": { AUDIT_TAG_KEY: "no" } }),
            Vec::new(),
        ),
        (
            "the telemetry state of a configured resource",
            json!({ "TelemetryConfigurationState": { "Logs": "Enabled" } }),
            vec![logged.clone()],
        ),
        (
            "the telemetry state every other VPC's logs are in",
            json!({ "TelemetryConfigurationState": { "Logs": "Disabled" } }),
            unlogged_vpcs.clone(),
        ),
        (
            "the telemetry state of an unmonitored instance",
            json!({ "TelemetryConfigurationState": { "Metrics": "Disabled" } }),
            vec![plain_instance.clone()],
        ),
        (
            "a tag and a telemetry state together",
            json!({
                "ResourceTags": { AUDIT_TAG_KEY: AUDIT_TAG_VALUE },
                "TelemetryConfigurationState": { "Metrics": "Enabled" },
            }),
            vec![monitored.clone()],
        ),
    ];

    for (case, request, expected) in rows {
        let (configurations, _) = audit(&fixture.router, case, &request).await;
        assert_eq!(
            reported(&configurations),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            reported(&configurations)
        );
    }
}

/// A page carries at most `MaxResults` configurations and its `NextToken` resumes the walk
/// after it, so a walk at any page size reports every resource exactly once and the last page
/// carries no token.
#[tokio::test]
async fn a_page_is_bounded_by_max_results_and_its_token_resumes_the_walk() {
    let fixture = Fixture::new().await;
    let every_resource = fixture.every_resource();

    for page_size in 1..=5usize {
        let case = format!("pages of {page_size}");
        let mut walked: Vec<Resource> = Vec::new();
        let mut token: Option<String> = None;
        for page in 0..every_resource.len() + 1 {
            let mut request = json!({ "MaxResults": page_size });
            if let Some(token) = token.clone() {
                request["NextToken"] = Value::String(token);
            }
            let (configurations, next) = audit(&fixture.router, &case, &request).await;
            assert!(
                configurations.len() <= page_size,
                "case {case:?}, page {page}: expected at most {page_size} configurations, got \
                 {}",
                configurations.len()
            );
            walked.extend(reported(&configurations));
            token = next;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case:?}: the walk must end with no token, got {token:?}"
        );
        assert_eq!(
            walked, every_resource,
            "case {case:?}: expected the whole audit once, in order, got {walked:?}"
        );
    }

    // A token this operation never issued is a 400 `ValidationException`, not an empty page.
    for (case, token) in [
        ("a token that is not hex", "not-a-token"),
        ("hex that decodes to something else", "6162"),
    ] {
        let (status, body) =
            audit_call(&fixture.router, &json!({ "NextToken": token })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            member(&body, "__type")
                .unwrap_or_default()
                .rsplit('#')
                .next()
                .unwrap_or_default(),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body}"
        );
    }
}

/// An audit reports what the owning service holds NOW: telemetry turned off in EC2 is
/// reported as `Disabled` at once, telemetry turned on is reported as `Enabled` once EC2
/// settles it, and a resource EC2 no longer holds is audited by nothing.
#[tokio::test]
async fn the_audit_follows_what_the_owning_service_holds_now() {
    let fixture = Fixture::new().await;

    // Flow logs off: the VPC is still a resource of the account, with its logs now
    // unconfigured.
    let flow_log = create_flow_log(&fixture.router, &fixture.plain_vpc).await;
    let (configurations, _) = audit(&fixture.router, "flow logs on", &json!({})).await;
    assert_eq!(
        one(&configurations, &fixture.plain_vpc)
            .expect("the VPC is audited")
            .telemetry_state,
        expected_state("Logs", "Enabled"),
        "case \"flow logs on\": expected Logs Enabled once EC2 holds a flow log for the VPC"
    );
    ec2(
        &fixture.router,
        "DeleteFlowLogs",
        &[param("FlowLogId.1", &flow_log)],
    )
    .await;
    let (configurations, _) = audit(&fixture.router, "flow logs off", &json!({})).await;
    assert_eq!(
        one(&configurations, &fixture.plain_vpc)
            .expect("the VPC is audited")
            .telemetry_state,
        expected_state("Logs", "Disabled"),
        "case \"flow logs off\": expected Logs Disabled once EC2 holds no flow log for the VPC"
    );

    // Detailed monitoring turned on through EC2's own operation: EC2 takes the instance
    // through `pending` before `enabled`, so the audit reports `Enabled` from the moment EC2
    // reports monitoring in force.
    ec2(
        &fixture.router,
        "MonitorInstances",
        &[param("InstanceId.1", &fixture.plain_instance)],
    )
    .await;
    poll_until(
        "monitoring turned on",
        "the instance's Metrics reached Enabled",
        || async {
            let (configurations, _) =
                audit(&fixture.router, "monitoring turned on", &json!({})).await;
            let audited = one(&configurations, &fixture.plain_instance)
                .ok_or_else(|| "the instance is not audited".to_string())?;
            if audited.telemetry_state == expected_state("Metrics", "Enabled") {
                Ok(())
            } else {
                Err(format!("{:?}", audited.telemetry_state))
            }
        },
    )
    .await;

    // A terminated instance no longer exists, so nothing is audited for it.
    ec2(
        &fixture.router,
        "TerminateInstances",
        &[param("InstanceId.1", &fixture.monitored_instance)],
    )
    .await;
    poll_until(
        "the instance terminated",
        "the terminated instance left the audit",
        || async {
            let (configurations, _) =
                audit(&fixture.router, "the instance terminated", &json!({})).await;
            match one(&configurations, &fixture.monitored_instance) {
                None => Ok(()),
                Some(audited) => Err(format!("{audited:?}")),
            }
        },
    )
    .await;
}
