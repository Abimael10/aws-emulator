// SPDX-License-Identifier: CC0-1.0

//! What a client reads out of a deployed, monitored load-balanced search estate, end to end
//! over the awsQuery wire.
//!
//! RULE under test: **once a stack's deployment has completed, `DescribeStacks` reports every
//! output the template declares as a COMPLETE pair — an `OutputKey` AND the `OutputValue` its
//! `Value` expression resolved to — and that value is the identity or the attribute the service
//! that OWNS the resource publishes for it.** `Ref` answers the resource's physical id, and
//! `Fn::GetAtt` the named attribute, whichever expression the template wraps them in
//! (`Fn::Select` over a list-valued attribute, `Fn::Sub`, `Fn::Join`) and whichever section the
//! name comes from (a resource, a template parameter, a pseudo parameter). An output is
//! reported without a value only where the expression names something the estate does not hold.
//!
//! Why it matters, and the reported symptom: a client that deploys infrastructure reads the
//! stack's outputs into a map (`{o["OutputKey"]: o["OutputValue"]}`) to learn what it has just
//! created, and an entry carrying a key and no value raises `KeyError: 'OutputValue'` there —
//! which abandoned a five-phase diagnosis lifecycle at phase 1 of 5, with nothing downstream
//! able to address the estate at all.
//!
//! The estate is the one such a lifecycle drives: a VPC and two of its subnets, a security
//! group, an Application Load Balancer with a target group and a listener, an Amazon OpenSearch
//! Service domain, and a CloudWatch alarm over the load balancer's own metric — whose dimension
//! is itself read with `Fn::GetAtt` off the load balancer, so the deployment resolves references
//! BETWEEN resources of five services as well as into the outputs.
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//!
//! * where each resource's IDENTITY comes from — a row that NAMES every resource and one that
//!   names none, so CloudFormation generates them;
//! * where the search domain is REACHED — a public domain, which publishes one host, and one
//!   inside the stack's OWN VPC, which has no public host and publishes its host in a map an
//!   output reads through a dotted `Fn::GetAtt` path;
//! * the SPELLING of the body — JSON with the long-form intrinsic functions, and the same
//!   estate as YAML with the short-form tags;
//! * where the value of a template parameter comes from — one supplied by the request, and one
//!   left to the declared default.
//!
//! No expected value is a literal from a recording: each is computed from the identity the SAME
//! response reports, or read back out of the owning service — Amazon EC2, Elastic Load
//! Balancing, CloudWatch and Amazon OpenSearch Service — on its own wire, so a row cannot drift
//! from the service the stack created the resource in.
//!
//! Five services on one engine, which is why the suite lives here: CloudFormation reaches every
//! one of them through the in-process callback to create what the template declares.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, elements, form_encode, opt_element, outputs_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the estate belongs to. It is
/// the Region [`authorization`] signs for.
///
/// `pub(crate)` here, and for the helpers below, because a sibling suite deploys the SAME estate
/// over the same five services and holds it to a different rule — what each of its resources
/// SETTLES in ([`crate::cloudformation_estate_listener_settles_on_its_create`]) rather than what
/// its outputs resolve to. One router, one wire, one Region, called twice, rather than a second
/// copy that can drift from this one: a suite signing for one Region and writing its subnets'
/// Availability Zones for another asserts nothing about either.
pub(crate) const REGION: &str = "us-east-1";

/// The API versions the query protocols of the three query-wire services carry.
const CFN_VERSION: &str = "2010-05-15";
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// Amazon OpenSearch Service's signing name (`sigv4Name: es`) and the URI its domain
/// operations are bound to (`DescribeDomain` is `GET {DomainName}` under it).
const OPENSEARCH_SIGNING_NAME: &str = "es";
const OPENSEARCH_DOMAIN_URI: &str = "/2021-01-01/opensearch/domain";

/// The key `DomainStatus$Endpoints` publishes a VPC-reached domain's IPv4 host under
/// (`smithy_query(query="doc",shape="DomainStatus",member="Endpoints")` of that service's
/// model — "IPv4 IP addresses — `'vpc', …`").
const ENDPOINTS_KEY_VPC: &str = "vpc";

/// How long a row waits for a deployment to settle, and how often it looks. Generous on
/// purpose: a stack completes when the services that own its resources report them ready, which
/// no fixed span names, and the deadline is here only so a failure is a failure rather than a
/// hang.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The emulator with the five services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: the stack really creates its network in Amazon EC2, its
/// load balancer in Elastic Load Balancing, its domain in Amazon OpenSearch Service and its
/// alarm in CloudWatch.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    // The CLASSIC Elastic Load Balancing front door too: the two Elastic Load Balancing models
    // share one endpoint and one signing name, so a request CloudFormation makes for an ELBv2
    // resource reaches that door and is claimed from there by the model that declares the
    // action and the version.
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(engine),
    );
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service the host label addresses, and answer its status
/// and body. `signed_as` is the credential scope the request carries, where it carries one —
/// Elastic Load Balancing's two models share a signing name, so the ELBv2 calls below are
/// addressed by host and by the `Version` only that model declares.
async fn send(
    router: &MultiServiceRouter,
    host_label: &str,
    signed_as: Option<&str>,
    body: String,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{host_label}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(signing_name) = signed_as {
        request = request.header("authorization", authorization(signing_name));
    }
    let request = request
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
pub(crate) async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    send(
        router,
        "cloudformation",
        Some("cloudformation"),
        form(action, CFN_VERSION, params),
    )
    .await
}

/// Drive one Amazon EC2 action that must succeed, and answer its body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "ec2", Some("ec2"), form(action, EC2_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one Elastic Load Balancing v2 action that must succeed, and answer its body.
pub(crate) async fn elbv2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        None,
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Call one CloudWatch operation over its awsJson wire, requiring it to succeed.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization(CLOUDWATCH_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// `DescribeDomain` on Amazon OpenSearch Service's own `restJson1` wire: the `DomainStatus` that
/// service publishes for the domain the stack created.
pub(crate) async fn describe_domain(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("{OPENSEARCH_DOMAIN_URI}/{name}"))
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization(OPENSEARCH_SIGNING_NAME))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeDomain({name}), got {status} with {parsed}"
    );
    parsed
        .get("DomainStatus")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no DomainStatus in {parsed}"))
}

/// One member of a reported record as the single text an attribute reads as, or `None` where
/// the record carries no such member.
fn reported_text(record: &Value, member: &str) -> Option<String> {
    record
        .get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether the answer reports a stack no operation is still in flight on.
fn is_settled(described: &str) -> bool {
    opt_element(described, "StackStatus")
        .is_some_and(|status| status.ends_with("_COMPLETE") || status.ends_with("_FAILED"))
}

/// Poll `DescribeStacks` until the stack reports no operation in flight, and answer the body of
/// that observation. Nothing sleeps inside a handler: each poll collects what the clock already
/// owed. The wait is for the CONDITION, bounded only so a hang is a reported failure rather than
/// a test that never ends.
pub(crate) async fn settle_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeStacks, got {status} with {described}"
        );
        if is_settled(&described) {
            return described;
        }
        last = described;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the stack never reached a terminal status, last seen {last}");
}

/// Every `ResourceStatusReason` the stack's events carry — where an owning service's own refusal
/// of a resource is reported, which is what a deployment that did not complete must be quoted
/// with.
pub(crate) async fn resource_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

// ---------------------------------------------------------------------------
// The estate, and the rows it is deployed as
// ---------------------------------------------------------------------------

/// Where the row's search domain is reached: what decides which address its owning service
/// publishes for it, and so which expression the output over that address is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// A public domain: the service publishes `Endpoint`, which `DomainEndpoint` reads.
    Public,
    /// A domain inside the stack's own VPC: no public host at all, and one entry in the
    /// endpoint map, which a dotted `Fn::GetAtt` path reads.
    Vpc,
}

/// How the body of a row is spelled: the spelling is no part of what an output resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// JSON with the long-form intrinsic functions.
    Json,
    /// YAML with the short-form intrinsic tags.
    Yaml,
}

/// One row: the stack, where its resources' names come from, where its domain is reached, how
/// its body is spelled, and the value the request supplies for the template's parameter.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The names the template states for its resources, or `None` for a row that states none —
    /// so CloudFormation generates every one of them.
    names: Option<Names>,
    reach: Reach,
    spelling: Spelling,
    /// The `Tier` parameter value the request supplies, or `None` to leave it to the declared
    /// default.
    tier: Option<&'static str>,
}

/// The names a row states for the resources whose type takes one. A domain name is inside
/// `DomainName`'s own `^[a-z][a-z0-9\-]+$` and 3..28 characters.
struct Names {
    load_balancer: &'static str,
    target_group: &'static str,
    domain: &'static str,
    alarm: &'static str,
}

/// The parameter the template declares, and the value it declares as the default.
const TIER_PARAMETER: &str = "Tier";
const TIER_DEFAULT: &str = "diagnosis";

const ROWS: &[Row] = &[
    Row {
        case: "every resource named by the template, a public search domain, JSON long-form body",
        stack: "monitored-search-named",
        names: Some(Names {
            load_balancer: "named-estate-alb",
            target_group: "named-estate-tg",
            domain: "named-estate-search",
            alarm: "named-estate-5xx",
        }),
        reach: Reach::Public,
        spelling: Spelling::Json,
        tier: Some("edge"),
    },
    Row {
        case: "no resource named by the template and the domain inside the stack's own VPC, \
               JSON long-form body",
        stack: "monitored-search-generated",
        names: None,
        reach: Reach::Vpc,
        spelling: Spelling::Json,
        tier: None,
    },
    Row {
        case: "the same estate as YAML with the short-form tags, the domain inside the stack's \
               own VPC",
        stack: "monitored-search-yaml",
        names: Some(Names {
            load_balancer: "yaml-estate-alb",
            target_group: "yaml-estate-tg",
            domain: "yaml-estate-search",
            alarm: "yaml-estate-5xx",
        }),
        reach: Reach::Vpc,
        spelling: Spelling::Yaml,
        tier: Some("internal"),
    },
];

/// The network every row's estate is placed in: a VPC and two subnets of it in two Availability
/// Zones, since Elastic Load Balancing places an Application Load Balancer in at least two.
const VPC_CIDR: &str = "10.61.0.0/16";
const SUBNET_A: (&str, &str) = ("10.61.1.0/24", "us-east-1a");
const SUBNET_B: (&str, &str) = ("10.61.2.0/24", "us-east-1b");

/// The metric the alarm watches: an Application Load Balancer's own 5XX count, in the namespace
/// of its family, under the `LoadBalancer` dimension whose value is the load balancer's full
/// name — which the template reads with `Fn::GetAtt`, so the alarm the deployment creates is
/// built from a reference to another resource of the same stack.
const ALARM_NAMESPACE: &str = "AWS/ApplicationELB";
const ALARM_METRIC: &str = "HTTPCode_ELB_5XX_Count";
const ALARM_DIMENSION: &str = "LoadBalancer";

/// Every output key a row declares. The value of each is held to the rule: a complete pair, and
/// the identity or attribute the owning service publishes.
const DECLARED_OUTPUTS: &[&str] = &[
    "AlarmArn",
    "AlarmName",
    "AlbArn",
    "AlbDnsName",
    "AlbFirstSecurityGroup",
    "AlbFullName",
    "AlbZoneId",
    "DomainArn",
    "DomainHost",
    "DomainName",
    "EstateAccount",
    "EstateRegion",
    "EstateSummary",
    "HealthUrl",
    "ListenerArn",
    "SecurityGroupId",
    "SubnetId",
    "TargetGroupArn",
    "TargetGroupFullName",
    "Tier",
    "VpcId",
];

/// The expression the row's output over the domain's address is written as: the public host for
/// a domain that has one, and the entry of the VPC endpoint map for one that has no public host
/// at all — read through the dotted path `Fn::GetAtt` addresses a map-valued attribute with.
fn domain_host_attribute(reach: Reach) -> String {
    match reach {
        Reach::Public => "DomainEndpoint".to_string(),
        Reach::Vpc => format!("DomainEndpoints.{ENDPOINTS_KEY_VPC}"),
    }
}

/// The row's body as JSON with the long-form intrinsic functions.
fn json_template(row: &Row) -> String {
    let mut load_balancer = json!({
        "Type": "application",
        // INTERNAL: the estate attaches no internet gateway to the VPC it declares, and an
        // internet-facing load balancer can only be placed in a VPC that has one.
        "Scheme": "internal",
        "SecurityGroups": [{"Ref": "AlbSg"}],
        "Subnets": [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}],
    });
    let mut target_group = json!({
        "Port": 80,
        "Protocol": "HTTP",
        "TargetType": "ip",
        "VpcId": {"Ref": "Vpc"},
        "HealthCheckPath": "/health",
    });
    let mut domain = json!({
        "EngineVersion": "OpenSearch_2.11",
        "ClusterConfig": {"InstanceType": "t3.small.search", "InstanceCount": 1},
        "EBSOptions": {"EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10},
    });
    let mut alarm = json!({
        "Namespace": ALARM_NAMESPACE,
        "MetricName": ALARM_METRIC,
        "Statistic": "Sum",
        "Period": 60,
        "EvaluationPeriods": 1,
        "Threshold": 1,
        "ComparisonOperator": "GreaterThanOrEqualToThreshold",
        "TreatMissingData": "notBreaching",
        "Dimensions": [{
            "Name": ALARM_DIMENSION,
            "Value": {"Fn::GetAtt": ["Alb", "LoadBalancerFullName"]},
        }],
    });
    if let Some(names) = &row.names {
        insert(&mut load_balancer, "Name", json!(names.load_balancer));
        insert(&mut target_group, "Name", json!(names.target_group));
        insert(&mut domain, "DomainName", json!(names.domain));
        insert(&mut alarm, "AlarmName", json!(names.alarm));
    }
    if row.reach == Reach::Vpc {
        insert(
            &mut domain,
            "VPCOptions",
            json!({
                "SubnetIds": [{"Ref": "SubnetA"}],
                "SecurityGroupIds": [{"Ref": "AlbSg"}],
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a monitored load-balanced search estate",
        "Parameters": {TIER_PARAMETER: {"Type": "String", "Default": TIER_DEFAULT}},
        "Resources": {
            "Vpc": {"Type": "AWS::EC2::VPC", "Properties": {"CidrBlock": VPC_CIDR}},
            "SubnetA": {"Type": "AWS::EC2::Subnet", "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": SUBNET_A.0,
                "AvailabilityZone": SUBNET_A.1,
            }},
            "SubnetB": {"Type": "AWS::EC2::Subnet", "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": SUBNET_B.0,
                "AvailabilityZone": SUBNET_B.1,
            }},
            "AlbSg": {"Type": "AWS::EC2::SecurityGroup", "Properties": {
                "GroupDescription": "the load balancer group",
                "VpcId": {"Ref": "Vpc"},
            }},
            "Alb": {
                "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
                "Properties": load_balancer,
            },
            "Targets": {
                "Type": "AWS::ElasticLoadBalancingV2::TargetGroup",
                "Properties": target_group,
            },
            "Listener": {
                "Type": "AWS::ElasticLoadBalancingV2::Listener",
                "Properties": {
                    "LoadBalancerArn": {"Ref": "Alb"},
                    "Port": 80,
                    "Protocol": "HTTP",
                    "DefaultActions": [{"Type": "forward", "TargetGroupArn": {"Ref": "Targets"}}],
                },
            },
            "Domain": {"Type": "AWS::OpenSearchService::Domain", "Properties": domain},
            "Alarm": {"Type": "AWS::CloudWatch::Alarm", "Properties": alarm},
        },
        "Outputs": outputs_section(row),
    })
    .to_string()
}

/// Insert one property into a properties object.
fn insert(properties: &mut Value, name: &str, value: Value) {
    properties
        .as_object_mut()
        .expect("the properties are an object")
        .insert(name.to_string(), value);
}

/// The `Outputs` section every row declares, in the long-form spelling: a `Ref` per resource, a
/// `Fn::GetAtt` per published attribute, one entry of a list-valued attribute read with
/// `Fn::Select`, two composed values (`Fn::Sub`, `Fn::Join`), the two pseudo parameters and the
/// template's own parameter.
fn outputs_section(row: &Row) -> Value {
    let host = domain_host_attribute(row.reach);
    json!({
        "VpcId": {"Value": {"Ref": "Vpc"}},
        "SubnetId": {"Value": {"Ref": "SubnetA"}},
        "SecurityGroupId": {"Value": {"Ref": "AlbSg"}},
        "AlbArn": {"Value": {"Ref": "Alb"}},
        "AlbDnsName": {"Value": {"Fn::GetAtt": ["Alb", "DNSName"]}},
        "AlbFullName": {"Value": {"Fn::GetAtt": ["Alb", "LoadBalancerFullName"]}},
        "AlbZoneId": {"Value": {"Fn::GetAtt": ["Alb", "CanonicalHostedZoneID"]}},
        "AlbFirstSecurityGroup": {
            "Value": {"Fn::Select": ["0", {"Fn::GetAtt": ["Alb", "SecurityGroups"]}]},
        },
        "TargetGroupArn": {"Value": {"Ref": "Targets"}},
        "TargetGroupFullName": {"Value": {"Fn::GetAtt": ["Targets", "TargetGroupFullName"]}},
        "ListenerArn": {"Value": {"Ref": "Listener"}},
        "AlarmName": {"Value": {"Ref": "Alarm"}},
        "AlarmArn": {"Value": {"Fn::GetAtt": ["Alarm", "Arn"]}},
        "DomainName": {"Value": {"Ref": "Domain"}},
        "DomainArn": {"Value": {"Fn::GetAtt": ["Domain", "Arn"]}},
        "DomainHost": {"Value": {"Fn::GetAtt": ["Domain", host]}},
        "HealthUrl": {"Value": {"Fn::Sub": "https://${Alb.DNSName}/health"}},
        "EstateSummary": {
            "Value": {"Fn::Join": ["|", [
                {"Ref": "Vpc"},
                {"Fn::GetAtt": ["Alb", "DNSName"]},
                {"Ref": "Alarm"},
            ]]},
        },
        "EstateRegion": {"Value": {"Ref": "AWS::Region"}},
        "EstateAccount": {"Value": {"Ref": "AWS::AccountId"}},
        "Tier": {
            "Value": {"Ref": TIER_PARAMETER},
            "Description": "the tier the estate was deployed for",
        },
    })
}

/// The SAME estate as YAML with the short-form intrinsic tags, so a row can assert that the
/// spelling of the body is no part of what an output resolves to.
///
/// The properties that carry no intrinsic are spliced in as the JSON they also are — YAML 1.2 is
/// a superset of JSON, so a nested flow mapping keeps the two spellings one estate rather than
/// two — while every reference is written with the short-form `!Ref` / `!GetAtt` / `!Sub` /
/// `!Join` / `!Select` tags, which is what the two spellings differ in.
fn yaml_template(row: &Row) -> String {
    let names = row
        .names
        .as_ref()
        .expect("a YAML row states its resources' names");
    let host = domain_host_attribute(row.reach);
    let vpc_options = match row.reach {
        Reach::Public => String::new(),
        Reach::Vpc => concat!(
            "      VPCOptions:\n",
            "        SubnetIds:\n",
            "          - !Ref SubnetA\n",
            "        SecurityGroupIds:\n",
            "          - !Ref AlbSg\n",
        )
        .to_string(),
    };
    format!(
        "AWSTemplateFormatVersion: '2010-09-09'\n\
         Description: a monitored load-balanced search estate\n\
         Parameters:\n\
         \x20 {TIER_PARAMETER}:\n\
         \x20   Type: String\n\
         \x20   Default: {TIER_DEFAULT}\n\
         Resources:\n\
         \x20 Vpc:\n\
         \x20   Type: AWS::EC2::VPC\n\
         \x20   Properties:\n\
         \x20     CidrBlock: {VPC_CIDR}\n\
         \x20 SubnetA:\n\
         \x20   Type: AWS::EC2::Subnet\n\
         \x20   Properties:\n\
         \x20     VpcId: !Ref Vpc\n\
         \x20     CidrBlock: {subnet_a_cidr}\n\
         \x20     AvailabilityZone: {subnet_a_zone}\n\
         \x20 SubnetB:\n\
         \x20   Type: AWS::EC2::Subnet\n\
         \x20   Properties:\n\
         \x20     VpcId: !Ref Vpc\n\
         \x20     CidrBlock: {subnet_b_cidr}\n\
         \x20     AvailabilityZone: {subnet_b_zone}\n\
         \x20 AlbSg:\n\
         \x20   Type: AWS::EC2::SecurityGroup\n\
         \x20   Properties:\n\
         \x20     GroupDescription: the load balancer group\n\
         \x20     VpcId: !Ref Vpc\n\
         \x20 Alb:\n\
         \x20   Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n\
         \x20   Properties:\n\
         \x20     Name: {load_balancer}\n\
         \x20     Type: application\n\
         \x20     Scheme: internal\n\
         \x20     SecurityGroups:\n\
         \x20       - !Ref AlbSg\n\
         \x20     Subnets:\n\
         \x20       - !Ref SubnetA\n\
         \x20       - !Ref SubnetB\n\
         \x20 Targets:\n\
         \x20   Type: AWS::ElasticLoadBalancingV2::TargetGroup\n\
         \x20   Properties:\n\
         \x20     Name: {target_group}\n\
         \x20     Port: 80\n\
         \x20     Protocol: HTTP\n\
         \x20     TargetType: ip\n\
         \x20     VpcId: !Ref Vpc\n\
         \x20     HealthCheckPath: /health\n\
         \x20 Listener:\n\
         \x20   Type: AWS::ElasticLoadBalancingV2::Listener\n\
         \x20   Properties:\n\
         \x20     LoadBalancerArn: !Ref Alb\n\
         \x20     Port: 80\n\
         \x20     Protocol: HTTP\n\
         \x20     DefaultActions:\n\
         \x20       - Type: forward\n\
         \x20         TargetGroupArn: !Ref Targets\n\
         \x20 Domain:\n\
         \x20   Type: AWS::OpenSearchService::Domain\n\
         \x20   Properties:\n\
         \x20     DomainName: {domain}\n\
         \x20     EngineVersion: OpenSearch_2.11\n\
         \x20     ClusterConfig: {{\"InstanceType\": \"t3.small.search\", \"InstanceCount\": 1}}\n\
         \x20     EBSOptions: {{\"EBSEnabled\": true, \"VolumeType\": \"gp3\", \"VolumeSize\": 10}}\n\
         {vpc_options}\
         \x20 Alarm:\n\
         \x20   Type: AWS::CloudWatch::Alarm\n\
         \x20   Properties:\n\
         \x20     AlarmName: {alarm}\n\
         \x20     Namespace: {ALARM_NAMESPACE}\n\
         \x20     MetricName: {ALARM_METRIC}\n\
         \x20     Statistic: Sum\n\
         \x20     Period: 60\n\
         \x20     EvaluationPeriods: 1\n\
         \x20     Threshold: 1\n\
         \x20     ComparisonOperator: GreaterThanOrEqualToThreshold\n\
         \x20     TreatMissingData: notBreaching\n\
         \x20     Dimensions:\n\
         \x20       - Name: {ALARM_DIMENSION}\n\
         \x20         Value: !GetAtt Alb.LoadBalancerFullName\n\
         Outputs:\n\
         \x20 VpcId:\n\
         \x20   Value: !Ref Vpc\n\
         \x20 SubnetId:\n\
         \x20   Value: !Ref SubnetA\n\
         \x20 SecurityGroupId:\n\
         \x20   Value: !Ref AlbSg\n\
         \x20 AlbArn:\n\
         \x20   Value: !Ref Alb\n\
         \x20 AlbDnsName:\n\
         \x20   Value: !GetAtt Alb.DNSName\n\
         \x20 AlbFullName:\n\
         \x20   Value: !GetAtt Alb.LoadBalancerFullName\n\
         \x20 AlbZoneId:\n\
         \x20   Value: !GetAtt Alb.CanonicalHostedZoneID\n\
         \x20 AlbFirstSecurityGroup:\n\
         \x20   Value: !Select [0, !GetAtt Alb.SecurityGroups]\n\
         \x20 TargetGroupArn:\n\
         \x20   Value: !Ref Targets\n\
         \x20 TargetGroupFullName:\n\
         \x20   Value: !GetAtt Targets.TargetGroupFullName\n\
         \x20 ListenerArn:\n\
         \x20   Value: !Ref Listener\n\
         \x20 AlarmName:\n\
         \x20   Value: !Ref Alarm\n\
         \x20 AlarmArn:\n\
         \x20   Value: !GetAtt Alarm.Arn\n\
         \x20 DomainName:\n\
         \x20   Value: !Ref Domain\n\
         \x20 DomainArn:\n\
         \x20   Value: !GetAtt Domain.Arn\n\
         \x20 DomainHost:\n\
         \x20   Value: !GetAtt Domain.{host}\n\
         \x20 HealthUrl:\n\
         \x20   Value: !Sub \"https://${{Alb.DNSName}}/health\"\n\
         \x20 EstateSummary:\n\
         \x20   Value: !Join [\"|\", [!Ref Vpc, !GetAtt Alb.DNSName, !Ref Alarm]]\n\
         \x20 EstateRegion:\n\
         \x20   Value: !Ref AWS::Region\n\
         \x20 EstateAccount:\n\
         \x20   Value: !Ref AWS::AccountId\n\
         \x20 Tier:\n\
         \x20   Description: the tier the estate was deployed for\n\
         \x20   Value: !Ref {TIER_PARAMETER}\n",
        subnet_a_cidr = SUBNET_A.0,
        subnet_a_zone = SUBNET_A.1,
        subnet_b_cidr = SUBNET_B.0,
        subnet_b_zone = SUBNET_B.1,
        load_balancer = names.load_balancer,
        target_group = names.target_group,
        domain = names.domain,
        alarm = names.alarm,
    )
}

/// The body a row deploys, in its own spelling.
fn template(row: &Row) -> String {
    match row.spelling {
        Spelling::Json => json_template(row),
        Spelling::Yaml => yaml_template(row),
    }
}

/// The value in force for the template's parameter: the one the request supplied, or the
/// declared default for a row that supplies none.
fn tier_in_force(row: &Row) -> &str {
    row.tier.unwrap_or(TIER_DEFAULT)
}

/// One output's value, which must be there: an `OutputKey` with no `OutputValue` beside it is
/// exactly what the standard `{o["OutputKey"]: o["OutputValue"]}` read raises on.
pub(crate) fn output<'a>(outputs: &'a BTreeMap<String, String>, case: &str, key: &str) -> &'a str {
    outputs
        .get(key)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no resolved output {key:?} in {outputs:?}"))
}

/// The account segment of an ARN (`arn:aws:<service>:<region>:<account>:…`).
fn account_of(arn: &str) -> &str {
    arn.split(':').nth(4).unwrap_or_default()
}

#[tokio::test]
async fn every_output_of_a_deployed_monitored_estate_carries_its_resolved_value() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = template(row);
        let mut parameters: Vec<(&str, &str)> = vec![
            ("StackName", row.stack),
            ("TemplateBody", body.as_str()),
        ];
        if let Some(tier) = row.tier {
            parameters.push(("Parameters.member.1.ParameterKey", TIER_PARAMETER));
            parameters.push(("Parameters.member.1.ParameterValue", tier));
        }
        let (status, created) = cloudformation(&router, "CreateStack", &parameters).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateStack, got {status} with {created}"
        );

        let described = settle_stack(&router, case, row.stack).await;
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the estate to deploy, got {stack_status:?} with reasons {:?}",
            resource_reasons(&router, row.stack).await
        );

        // The symptom itself: every declared output is a COMPLETE pair. `outputs_of` pairs a
        // key with the value of its OWN member, so an output reported with a key and no value
        // is absent from this map rather than misaligned onto the next member's value.
        let outputs = outputs_of(&described);
        let reported: BTreeSet<&str> = outputs.keys().map(String::as_str).collect();
        let declared: BTreeSet<&str> = DECLARED_OUTPUTS.iter().copied().collect();
        assert_eq!(
            reported, declared,
            "case {case:?}: every Output of a CREATE_COMPLETE stack must be reported with its \
             OutputValue; got {outputs:?} in {described}"
        );
        for key in DECLARED_OUTPUTS {
            let value = output(&outputs, case, key);
            assert!(
                !value.is_empty(),
                "case {case:?}: output {key:?} carries an empty value ({outputs:?})"
            );
        }

        // Amazon EC2 owns the network: the ids the `Ref` outputs carry resolve THERE, and the
        // subnet reports membership of the very VPC the stack published.
        let vpc_id = output(&outputs, case, "VpcId");
        let subnet_id = output(&outputs, case, "SubnetId");
        let security_group_id = output(&outputs, case, "SecurityGroupId");
        let described_vpc = ec2(&router, case, "DescribeVpcs", &[("VpcId.1", vpc_id)]).await;
        assert_eq!(
            opt_element(&described_vpc, "vpcId").as_deref(),
            Some(vpc_id),
            "case {case:?}: expected Amazon EC2 to hold the VPC the stack published \
             ({vpc_id:?}), got {described_vpc}"
        );
        let described_subnet =
            ec2(&router, case, "DescribeSubnets", &[("SubnetId.1", subnet_id)]).await;
        assert_eq!(
            opt_element(&described_subnet, "subnetId").as_deref(),
            Some(subnet_id),
            "case {case:?}: expected Amazon EC2 to hold the subnet the stack published \
             ({subnet_id:?}), got {described_subnet}"
        );
        assert_eq!(
            opt_element(&described_subnet, "vpcId").as_deref(),
            Some(vpc_id),
            "case {case:?}: expected the published subnet to be in the published VPC, got \
             {described_subnet}"
        );

        // Elastic Load Balancing owns the load balancer, its target group and its listener:
        // every attribute the stack published for them is that service's own report.
        let alb_arn = output(&outputs, case, "AlbArn");
        let described_alb = elbv2(
            &router,
            case,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", alb_arn)],
        )
        .await;
        for (key, member) in [
            ("AlbDnsName", "DNSName"),
            ("AlbZoneId", "CanonicalHostedZoneId"),
        ] {
            let published = opt_element(&described_alb, member);
            assert_eq!(
                published.as_deref(),
                Some(output(&outputs, case, key)),
                "case {case:?}: expected the output {key:?} to be Elastic Load Balancing's own \
                 LoadBalancer${member} ({published:?}), got {outputs:?}"
            );
        }
        assert_eq!(
            opt_element(&described_alb, "VpcId").as_deref(),
            Some(vpc_id),
            "case {case:?}: expected the load balancer to be placed in the published VPC, got \
             {described_alb}"
        );
        // "For attributes that return a list, use Fn::Select": the entry the output selected is
        // the security group the stack also published as its own resource, and the one Elastic
        // Load Balancing reports for the load balancer.
        assert_eq!(
            output(&outputs, case, "AlbFirstSecurityGroup"),
            security_group_id,
            "case {case:?}: expected the selected entry of the load balancer's SecurityGroups \
             to be the stack's own security group, got {outputs:?}"
        );
        let reported_groups = block_of(&described_alb, "SecurityGroups")
            .map(|list| elements(list, "member"))
            .unwrap_or_default();
        assert!(
            reported_groups.iter().any(|group| group == security_group_id),
            "case {case:?}: expected Elastic Load Balancing to report the security group \
             {security_group_id:?} for the load balancer, got {reported_groups:?} in \
             {described_alb}"
        );
        // The full name is the load balancer's own identity inside its ARN — which is what the
        // alarm's dimension is written with, and what CloudWatch stores for it.
        let alb_full_name = output(&outputs, case, "AlbFullName");
        assert!(
            alb_arn.ends_with(alb_full_name),
            "case {case:?}: expected the published LoadBalancerFullName {alb_full_name:?} to be \
             the load balancer's own identity inside {alb_arn:?}"
        );

        let target_group_arn = output(&outputs, case, "TargetGroupArn");
        let described_group = elbv2(
            &router,
            case,
            "DescribeTargetGroups",
            &[("TargetGroupArns.member.1", target_group_arn)],
        )
        .await;
        assert_eq!(
            opt_element(&described_group, "TargetGroupArn").as_deref(),
            Some(target_group_arn),
            "case {case:?}: expected Elastic Load Balancing to hold the target group the stack \
             published, got {described_group}"
        );
        let target_group_full_name = output(&outputs, case, "TargetGroupFullName");
        assert!(
            target_group_arn.ends_with(target_group_full_name),
            "case {case:?}: expected the published TargetGroupFullName \
             {target_group_full_name:?} to be the group's own identity inside \
             {target_group_arn:?}"
        );

        let listener_arn = output(&outputs, case, "ListenerArn");
        let described_listener = elbv2(
            &router,
            case,
            "DescribeListeners",
            &[("ListenerArns.member.1", listener_arn)],
        )
        .await;
        assert_eq!(
            opt_element(&described_listener, "ListenerArn").as_deref(),
            Some(listener_arn),
            "case {case:?}: expected Elastic Load Balancing to hold the listener the stack \
             published, got {described_listener}"
        );
        assert_eq!(
            opt_element(&described_listener, "LoadBalancerArn").as_deref(),
            Some(alb_arn),
            "case {case:?}: expected the published listener to be on the published load \
             balancer, got {described_listener}"
        );

        // CloudWatch owns the alarm: the name the `Ref` output carries addresses it there, its
        // ARN is that service's own, and the dimension the template read off the load balancer
        // with Fn::GetAtt is the one the alarm really watches.
        let alarm_name = output(&outputs, case, "AlarmName");
        let alarm_arn = output(&outputs, case, "AlarmArn");
        let described_alarms = cloudwatch(
            &router,
            case,
            "DescribeAlarms",
            &json!({"AlarmNames": [alarm_name]}),
        )
        .await;
        let alarm = described_alarms
            .get("MetricAlarms")
            .and_then(Value::as_array)
            .and_then(|alarms| alarms.first())
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected CloudWatch to hold the alarm {alarm_name:?} the \
                     stack published, got {described_alarms}"
                )
            });
        assert_eq!(
            reported_text(&alarm, "AlarmName").as_deref(),
            Some(alarm_name),
            "case {case:?}: expected CloudWatch's own alarm name, got {alarm}"
        );
        assert_eq!(
            reported_text(&alarm, "AlarmArn").as_deref(),
            Some(alarm_arn),
            "case {case:?}: expected the output AlarmArn to be CloudWatch's own MetricAlarm$\
             AlarmArn, got {alarm}"
        );
        let watched: BTreeMap<String, String> = alarm
            .get("Dimensions")
            .and_then(Value::as_array)
            .map(|dimensions| {
                dimensions
                    .iter()
                    .filter_map(|dimension| {
                        Some((
                            reported_text(dimension, "Name")?,
                            reported_text(dimension, "Value")?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            watched.get(ALARM_DIMENSION).map(String::as_str),
            Some(alb_full_name),
            "case {case:?}: expected the alarm to watch the load balancer the template read \
             with Fn::GetAtt ({alb_full_name:?}), got {watched:?}"
        );

        // Amazon OpenSearch Service owns the domain: its ARN and the address it is reached on
        // are that service's own report, whether it is reached publicly or inside the VPC the
        // stack created.
        let domain_name = output(&outputs, case, "DomainName");
        let domain_status = describe_domain(&router, case, domain_name).await;
        assert_eq!(
            reported_text(&domain_status, "ARN").as_deref(),
            Some(output(&outputs, case, "DomainArn")),
            "case {case:?}: expected the output DomainArn to be the owning service's own \
             DomainStatus$ARN, got {domain_status}"
        );
        let published_host = match row.reach {
            Reach::Public => reported_text(&domain_status, "Endpoint"),
            Reach::Vpc => domain_status
                .get("Endpoints")
                .and_then(Value::as_object)
                .and_then(|endpoints| endpoints.get(ENDPOINTS_KEY_VPC))
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        assert_eq!(
            published_host.as_deref(),
            Some(output(&outputs, case, "DomainHost")),
            "case {case:?}: expected the output DomainHost to be the address the owning service \
             publishes for the domain ({published_host:?}), got {outputs:?}"
        );

        // The composed outputs are built from the SAME resolved values: a substitution, a join,
        // the two pseudo parameters, and the parameter value in force.
        let alb_dns_name = output(&outputs, case, "AlbDnsName");
        assert_eq!(
            output(&outputs, case, "HealthUrl"),
            format!("https://{alb_dns_name}/health"),
            "case {case:?}: expected Fn::Sub to substitute the resolved attribute, got \
             {outputs:?}"
        );
        assert_eq!(
            output(&outputs, case, "EstateSummary"),
            format!("{vpc_id}|{alb_dns_name}|{alarm_name}"),
            "case {case:?}: expected Fn::Join to join the resolved references, got {outputs:?}"
        );
        assert_eq!(
            output(&outputs, case, "EstateRegion"),
            REGION,
            "case {case:?}: expected the pseudo parameter AWS::Region to be the Region the \
             request was signed for, got {outputs:?}"
        );
        assert_eq!(
            output(&outputs, case, "EstateAccount"),
            account_of(alarm_arn),
            "case {case:?}: expected the pseudo parameter AWS::AccountId to be the account the \
             estate's ARNs carry, got {outputs:?}"
        );
        assert_eq!(
            output(&outputs, case, "Tier"),
            tier_in_force(row),
            "case {case:?}: expected the parameter's value in force, got {outputs:?}"
        );

        // The names a row STATES are the identities the estate holds, which is the other half of
        // what a client reads these outputs for; a row that states none is held only to the
        // owning services' agreement above.
        if let Some(names) = &row.names {
            assert_eq!(
                domain_name, names.domain,
                "case {case:?}: Ref returns the domain name the template stated, got {outputs:?}"
            );
            assert_eq!(
                alarm_name, names.alarm,
                "case {case:?}: Ref returns the alarm name the template stated, got {outputs:?}"
            );
            assert!(
                alb_arn.contains(names.load_balancer),
                "case {case:?}: expected the load balancer's ARN to carry the name the template \
                 stated ({:?}), got {alb_arn:?}",
                names.load_balancer
            );
            assert!(
                target_group_arn.contains(names.target_group),
                "case {case:?}: expected the target group's ARN to carry the name the template \
                 stated ({:?}), got {target_group_arn:?}",
                names.target_group
            );
        }
    }
}
