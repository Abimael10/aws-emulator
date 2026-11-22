// SPDX-License-Identifier: CC0-1.0

//! What a stack's `AWS::ElasticLoadBalancingV2::Listener` SETTLES in, in the estate a client
//! really deploys it with: a load-balanced, monitored search estate whose resources belong to
//! five services.
//!
//! RULE under test: **a stack's resource whose owning service creates it SYNCHRONOUSLY — the
//! create answers the resource itself and that service models no state in which it is still
//! coming up — reaches `CREATE_COMPLETE` on that create's own answer; only a resource whose
//! owning service reports a non-terminal state of its own is held open, and a create that
//! service refuses fails the resource with that refusal.** Elastic Load Balancing's
//! `CreateListener` is such a create — it answers `Listeners`, the `Listener` shape carries no
//! state member at all, and every error it models is a client refusal (`IncompatibleProtocols`,
//! `InvalidLoadBalancerAction`, `TargetGroupNotFound`, `LoadBalancerNotFound`, …) — while the
//! same service's `CreateLoadBalancer` answers a load balancer `provisioning`, so that resource
//! IS held until it reports itself `active`. A listener therefore never waits for a readiness
//! condition nothing in that service will ever report, and the stack holding it completes and
//! publishes its `Outputs`.
//!
//! Why it matters, and the reported symptom: the normal way an Application Load Balancer, its
//! target groups, its listener, its search domain and its CloudWatch alarm are created is ONE
//! template. A listener left in `CREATE_IN_PROGRESS` until CloudFormation's own create wait runs
//! out fails the resource with a wait that ran out, rolls the whole stack back to
//! `ROLLBACK_COMPLETE`, and tears down every resource and every output the template had made —
//! so a client that reads the estate out of the stack's outputs is given a rollback with no
//! actionable error code at all, and nothing downstream can address the estate.
//!
//! The rows differ along what the rule generalises over rather than restating one template: the
//! load balancer's FAMILY (application, network) and SCHEME (internal, internet-facing), the
//! listener's own protocol and port, whether its protocol MATCHES the protocol of the target
//! group it forwards to (an `HTTP:80` listener in front of an `HTTPS:443` target group is a
//! pairing Elastic Load Balancing accepts, and the one the reported estate asked for), whether
//! its default action forwards at all or answers a fixed response, how MANY listeners one load
//! balancer carries, and whether the stack also holds the resources of two FURTHER services —
//! an Amazon OpenSearch Service domain inside the stack's own VPC and the alarm over the load
//! balancer's metric — which the listener must not be held behind.
//!
//! No expected value is a literal from a recording: every identifier is read back out of the
//! service that OWNS it — Elastic Load Balancing for the load balancer and its listeners, Amazon
//! EC2 for the network, Amazon OpenSearch Service for the domain — and each listener's ARN
//! namespace is derived from the load balancer's own published ARN.
//!
//! Five services on one engine, which is why this suite lives here: CloudFormation reaches every
//! one of them through the in-process callback. The router, the wire helpers, the stack poller
//! and the domain read are the sibling suite's own
//! ([`crate::cloudformation_monitored_alb_search_estate`], which deploys the same estate and
//! holds it to what its outputs RESOLVE to), not another copy. The complement of the rule — a
//! pairing Elastic Load Balancing refuses, which fails the resource with that service's own
//! modelled refusal rather than with a wait — is pinned where that single service's wire is
//! enough to state it, in
//! `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/\
//! elbv2_listener_create_settles.rs`.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::cloudformation_monitored_alb_search_estate::{
    build_router, cloudformation, describe_domain, elbv2, output, resource_reasons, settle_stack,
    REGION,
};
use crate::query_wire::{block_of, elements, members_of, opt_element, outputs_of, pairs_of};

/// The status a resource and a stack that completed are reported in, and the suffix of every
/// status a failure is reported with.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const FAILED_SUFFIX: &str = "_FAILED";

/// The fixed text of the reason CloudFormation fails a resource whose create wait expired with.
/// What no listener may ever be reported with, since nothing about a listener is waited for.
const RAN_OUT: &str = "when the wait for it to be created ran out";

/// The `LoadBalancerState$Code` Elastic Load Balancing reports a load balancer that finished
/// coming up in — "the load balancer is ready" — as against the `provisioning` its create
/// answers. The one state in this estate that is genuinely waited for.
const LOAD_BALANCER_ACTIVE: &str = "active";

/// The network every row's estate is placed in: a VPC and two subnets of it in two Availability
/// Zones, since Elastic Load Balancing places an Application Load Balancer in at least two.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_A_CIDR: &str = "10.71.1.0/24";
const SUBNET_B_CIDR: &str = "10.71.2.0/24";

/// One target group a row declares.
struct TargetGroup {
    logical_id: &'static str,
    output_key: &'static str,
    port: u32,
    protocol: &'static str,
}

/// One listener a row declares: the logical id the template gives it, the output its ARN is
/// published through, the port and protocol it is created with, and the target group its default
/// action forwards to — `None` for a default action that forwards nowhere and answers a fixed
/// response instead.
struct Listener {
    logical_id: &'static str,
    output_key: &'static str,
    port: u32,
    protocol: &'static str,
    forwards_to: Option<&'static str>,
}

/// One row: the estate a stack is deployed as, and the resources whose settling it is held to.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// `LoadBalancer$Type`, and the segment that family puts in every ARN of the load balancer
    /// and of its listeners (`loadbalancer/<family>/…`, `listener/<family>/…`).
    family: &'static str,
    family_segment: &'static str,
    /// `LoadBalancer$Scheme`. An internet-facing placement additionally needs an internet
    /// gateway attached to the VPC, which that row's template declares.
    scheme: &'static str,
    internet_gateway: bool,
    target_groups: &'static [TargetGroup],
    listeners: &'static [Listener],
    /// Whether the estate also declares the search domain inside its own VPC and the alarm over
    /// the load balancer's metric: resources of two further services, created in the same walk
    /// as the listener.
    search_estate: bool,
}

const ROWS: &[Row] = &[
    Row {
        // The estate the issue reported, with the pairing it asked for: an HTTP listener in
        // front of ip HTTPS target groups on an INTERNAL application load balancer, beside a
        // search domain in the stack's own VPC and an alarm over the load balancer.
        case: "an HTTP:80 listener on an internal application load balancer forwarding to an ip \
               HTTPS:443 target group, beside a VPC search domain and an alarm",
        stack: "listener-settles-search-estate",
        family: "application",
        family_segment: "app",
        scheme: "internal",
        internet_gateway: false,
        target_groups: &[
            TargetGroup {
                logical_id: "DashboardsTg",
                output_key: "DashboardsTgArn",
                port: 443,
                protocol: "HTTPS",
            },
            TargetGroup {
                logical_id: "ApiTg",
                output_key: "ApiTgArn",
                port: 443,
                protocol: "HTTPS",
            },
        ],
        listeners: &[Listener {
            logical_id: "HttpListener",
            output_key: "HttpListenerArn",
            port: 80,
            protocol: "HTTP",
            forwards_to: Some("DashboardsTg"),
        }],
        search_estate: true,
    },
    Row {
        // The COUNT and the ACTION form the rule generalises over, on the other scheme: two
        // listeners on one internet-facing load balancer, one forwarding and one answering a
        // fixed response, so a listener that names no target group at all settles the same way.
        case: "two listeners on an internet-facing application load balancer, one forwarding to \
               an HTTP:80 target group and one answering a fixed response",
        stack: "listener-settles-public-pair",
        family: "application",
        family_segment: "app",
        scheme: "internet-facing",
        internet_gateway: true,
        target_groups: &[TargetGroup {
            logical_id: "WebTg",
            output_key: "WebTgArn",
            port: 80,
            protocol: "HTTP",
        }],
        listeners: &[
            Listener {
                logical_id: "WebListener",
                output_key: "WebListenerArn",
                port: 80,
                protocol: "HTTP",
                forwards_to: Some("WebTg"),
            },
            Listener {
                logical_id: "GateListener",
                output_key: "GateListenerArn",
                port: 8080,
                protocol: "HTTP",
                forwards_to: None,
            },
        ],
        search_estate: false,
    },
    Row {
        // Another FAMILY and another protocol: a network load balancer's TCP listener, whose ARN
        // carries the `net` segment, beside the same search domain and alarm.
        case: "a TCP:5000 listener on an internal network load balancer forwarding to a TCP \
               target group, beside a VPC search domain and an alarm",
        stack: "listener-settles-stream-estate",
        family: "network",
        family_segment: "net",
        scheme: "internal",
        internet_gateway: false,
        target_groups: &[TargetGroup {
            logical_id: "StreamTg",
            output_key: "StreamTgArn",
            port: 5000,
            protocol: "TCP",
        }],
        listeners: &[Listener {
            logical_id: "StreamListener",
            output_key: "StreamListenerArn",
            port: 5000,
            protocol: "TCP",
            forwards_to: Some("StreamTg"),
        }],
        search_estate: true,
    },
];

/// The output the load balancer's own ARN is published through, and the outputs over the network
/// and the search estate: the identifiers a client reads the estate out of.
const LOAD_BALANCER_OUTPUT: &str = "AlbArn";
const LOAD_BALANCER_DNS_OUTPUT: &str = "AlbDnsName";
const VPC_OUTPUT: &str = "VpcId";
const DOMAIN_NAME_OUTPUT: &str = "DomainName";
const DOMAIN_ARN_OUTPUT: &str = "DomainArn";
const ALARM_NAME_OUTPUT: &str = "AlarmName";

/// The metric an alarm of this estate watches, in the namespace of the load balancer's family,
/// under the `LoadBalancer` dimension whose value the template reads off the load balancer with
/// `Fn::GetAtt` — so the alarm is built from a reference to another resource of the same stack.
const ALARM_METRIC: &str = "HTTPCode_ELB_5XX_Count";
const ALARM_DIMENSION: &str = "LoadBalancer";

/// The body a row is deployed as, as the JSON document it is: `Resources` and `Outputs` are read
/// back off this same document, so the logical ids and output keys a row is asserted over are
/// the ones its template really declares rather than a list kept beside it.
fn template(row: &Row) -> Value {
    let mut resources = Map::new();
    resources.insert(
        "Vpc".to_string(),
        json!({"Type": "AWS::EC2::VPC", "Properties": {"CidrBlock": VPC_CIDR}}),
    );
    for (logical_id, cidr, zone) in [
        ("SubnetA", SUBNET_A_CIDR, format!("{REGION}a")),
        ("SubnetB", SUBNET_B_CIDR, format!("{REGION}b")),
    ] {
        resources.insert(
            logical_id.to_string(),
            json!({"Type": "AWS::EC2::Subnet", "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": cidr,
                "AvailabilityZone": zone,
            }}),
        );
    }
    resources.insert(
        "LbSg".to_string(),
        json!({"Type": "AWS::EC2::SecurityGroup", "Properties": {
            "GroupDescription": "the load balancer group",
            "VpcId": {"Ref": "Vpc"},
        }}),
    );
    if row.internet_gateway {
        // An internet-facing load balancer's nodes have public addresses, which reach the
        // internet only through a gateway attached to the VPC.
        resources.insert(
            "Igw".to_string(),
            json!({"Type": "AWS::EC2::InternetGateway"}),
        );
        resources.insert(
            "IgwAttachment".to_string(),
            json!({"Type": "AWS::EC2::VPCGatewayAttachment", "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "InternetGatewayId": {"Ref": "Igw"},
            }}),
        );
    }
    let mut load_balancer = json!({
        "Type": row.family,
        "Scheme": row.scheme,
        "Subnets": [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}],
    });
    if row.family == "application" {
        // Security groups are an Application Load Balancer's own placement property.
        load_balancer
            .as_object_mut()
            .expect("the properties are an object")
            .insert("SecurityGroups".to_string(), json!([{"Ref": "LbSg"}]));
    }
    resources.insert(
        "Lb".to_string(),
        json!({
            "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
            "Properties": load_balancer,
        }),
    );
    for group in row.target_groups {
        resources.insert(
            group.logical_id.to_string(),
            json!({
                "Type": "AWS::ElasticLoadBalancingV2::TargetGroup",
                "Properties": {
                    "Port": group.port,
                    "Protocol": group.protocol,
                    "TargetType": "ip",
                    "VpcId": {"Ref": "Vpc"},
                },
            }),
        );
    }
    for listener in row.listeners {
        let default_action = match listener.forwards_to {
            Some(group) => json!({"Type": "forward", "TargetGroupArn": {"Ref": group}}),
            None => json!({
                "Type": "fixed-response",
                "FixedResponseConfig": {
                    "StatusCode": "404",
                    "ContentType": "text/plain",
                    "MessageBody": "no-match",
                },
            }),
        };
        resources.insert(
            listener.logical_id.to_string(),
            json!({
                "Type": "AWS::ElasticLoadBalancingV2::Listener",
                "Properties": {
                    "LoadBalancerArn": {"Ref": "Lb"},
                    "Port": listener.port,
                    "Protocol": listener.protocol,
                    "DefaultActions": [default_action],
                },
            }),
        );
    }
    let mut outputs = Map::new();
    outputs.insert(VPC_OUTPUT.to_string(), json!({"Value": {"Ref": "Vpc"}}));
    outputs.insert(
        LOAD_BALANCER_OUTPUT.to_string(),
        json!({"Value": {"Ref": "Lb"}}),
    );
    outputs.insert(
        LOAD_BALANCER_DNS_OUTPUT.to_string(),
        json!({"Value": {"Fn::GetAtt": ["Lb", "DNSName"]}}),
    );
    for group in row.target_groups {
        outputs.insert(
            group.output_key.to_string(),
            json!({"Value": {"Ref": group.logical_id}}),
        );
    }
    for listener in row.listeners {
        outputs.insert(
            listener.output_key.to_string(),
            json!({"Value": {"Ref": listener.logical_id}}),
        );
    }
    if row.search_estate {
        resources.insert(
            "Domain".to_string(),
            json!({"Type": "AWS::OpenSearchService::Domain", "Properties": {
                "EngineVersion": "OpenSearch_2.11",
                "ClusterConfig": {"InstanceType": "t3.small.search", "InstanceCount": 1},
                "EBSOptions": {"EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10},
                "VPCOptions": {
                    "SubnetIds": [{"Ref": "SubnetA"}],
                    "SecurityGroupIds": [{"Ref": "LbSg"}],
                },
            }}),
        );
        resources.insert(
            "Alarm".to_string(),
            json!({"Type": "AWS::CloudWatch::Alarm", "Properties": {
                "Namespace": alarm_namespace(row),
                "MetricName": ALARM_METRIC,
                "Statistic": "Sum",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                "TreatMissingData": "notBreaching",
                "Dimensions": [{
                    "Name": ALARM_DIMENSION,
                    "Value": {"Fn::GetAtt": ["Lb", "LoadBalancerFullName"]},
                }],
            }}),
        );
        outputs.insert(
            DOMAIN_NAME_OUTPUT.to_string(),
            json!({"Value": {"Ref": "Domain"}}),
        );
        outputs.insert(
            DOMAIN_ARN_OUTPUT.to_string(),
            json!({"Value": {"Fn::GetAtt": ["Domain", "Arn"]}}),
        );
        outputs.insert(
            ALARM_NAME_OUTPUT.to_string(),
            json!({"Value": {"Ref": "Alarm"}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a monitored load-balanced search estate",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
}

/// The CloudWatch namespace of the load balancer's own metrics, which is per FAMILY.
fn alarm_namespace(row: &Row) -> &'static str {
    match row.family {
        "network" => "AWS/NetworkELB",
        _ => "AWS/ApplicationELB",
    }
}

/// The keys of one section of a row's body — the logical ids it declares, or the outputs it
/// publishes.
fn section_keys(body: &Value, section: &str) -> BTreeSet<String> {
    body.get(section)
        .and_then(Value::as_object)
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default()
}

/// Every `ResourceStatus` the stack's events carry for one logical id, NEWEST FIRST —
/// `DescribeStackEvents` reports events "in reverse chronological order", so a resource's last
/// status is the first entry and its first status the last.
fn statuses_of(events: &str, logical_id: &str) -> Vec<String> {
    members_of(events)
        .into_iter()
        .filter(|member| {
            elements(member, "LogicalResourceId")
                .first()
                .map(String::as_str)
                == Some(logical_id)
        })
        .filter_map(|member| elements(member, "ResourceStatus").into_iter().next())
        .collect()
}

#[tokio::test]
async fn a_stacks_listener_completes_on_its_creates_own_answer_in_the_whole_estate() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = template(row);
        let rendered = body.to_string();
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack), ("TemplateBody", &rendered)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateStack, got {status} with {created}"
        );

        // The shared poller of this target: it asks `DescribeStacks` again until the stack
        // reports a status no operation is still in flight in, under its own generous deadline,
        // so what this case waits for is the CONDITION rather than a span of time.
        let described = settle_stack(&router, case, row.stack).await;
        let reasons = resource_reasons(&router, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: expected the estate to deploy, got {described} with reasons \
             {reasons:?}"
        );
        // …and no resource of it was failed with a wait of this emulator's own running out,
        // which is not an outcome the services owning these resources produce.
        assert!(
            !reasons.iter().any(|reason| reason.contains(RAN_OUT)),
            "case {case:?}: expected no resource to be failed with a wait that ran out, got \
             {reasons:?}"
        );

        // Every resource the template declares is reported complete, and every output it
        // declares is reported as a COMPLETE pair — which is what a client reads the estate out
        // of, and what a rolled-back stack carries none of.
        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let reported_statuses = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
        let declared_resources = section_keys(&body, "Resources");
        assert_eq!(
            reported_statuses.keys().cloned().collect::<BTreeSet<_>>(),
            declared_resources,
            "case {case:?}: expected the stack to report every declared resource, got \
             {reported_statuses:?}"
        );
        for (logical_id, reported) in &reported_statuses {
            assert_eq!(
                reported, CREATE_COMPLETE,
                "case {case:?}: expected {logical_id} to be reported {CREATE_COMPLETE}, got \
                 {reported_statuses:?}"
            );
        }
        let physical_ids = pairs_of(&resources, "LogicalResourceId", "PhysicalResourceId");
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.keys().cloned().collect::<BTreeSet<_>>(),
            section_keys(&body, "Outputs"),
            "case {case:?}: expected every declared output to be reported with its value, got \
             {outputs:?}"
        );
        for (key, value) in &outputs {
            assert!(
                !value.is_empty(),
                "case {case:?}: output {key:?} carries an empty value ({outputs:?})"
            );
        }

        // The ONE resource of this estate that is genuinely waited for: Elastic Load Balancing
        // answers `CreateLoadBalancer` with a load balancer still coming up, so the stack held
        // that resource open until that service reported it ready — which it now is.
        let load_balancer_arn = output(&outputs, case, LOAD_BALANCER_OUTPUT);
        let described_load_balancer = elbv2(
            &router,
            case,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", load_balancer_arn)],
        )
        .await;
        let reported_state =
            block_of(&described_load_balancer, "State").and_then(|state| opt_element(state, "Code"));
        assert_eq!(
            reported_state.as_deref(),
            Some(LOAD_BALANCER_ACTIVE),
            "case {case:?}: expected Elastic Load Balancing to report the load balancer ready, \
             got {described_load_balancer}"
        );
        assert!(
            load_balancer_arn.contains(&format!(":loadbalancer/{}/", row.family_segment)),
            "case {case:?}: expected a {} load balancer ARN, got {load_balancer_arn:?}",
            row.family
        );

        let events = {
            let (_, body) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
            body
        };
        let mut published_listeners = BTreeSet::new();
        for listener in row.listeners {
            let logical_id = listener.logical_id;
            let listener_arn = output(&outputs, case, listener.output_key).to_string();

            // What BECAME of the resource in its own event stream: it opened
            // `CREATE_IN_PROGRESS` and ended `CREATE_COMPLETE`, with nothing failed in between.
            let statuses = statuses_of(&events, logical_id);
            assert_eq!(
                statuses.first().map(String::as_str),
                Some(CREATE_COMPLETE),
                "case {case:?}: expected {logical_id} to have reached {CREATE_COMPLETE}, got \
                 {statuses:?}"
            );
            assert_eq!(
                statuses.last().map(String::as_str),
                Some(CREATE_IN_PROGRESS),
                "case {case:?}: expected {logical_id}'s first event to be \
                 {CREATE_IN_PROGRESS}, got {statuses:?}"
            );
            assert!(
                !statuses
                    .iter()
                    .any(|status| status.ends_with(FAILED_SUFFIX)),
                "case {case:?}: expected no failed event for {logical_id}, got {statuses:?}"
            );

            // The physical id the stack holds for it is the ARN it published, in the ARN
            // namespace of the load balancer it was created on — `listener/<family>/<name>/<load
            // balancer id>/<listener id>` — which is derived from that load balancer's OWN
            // published ARN rather than written out here.
            assert_eq!(
                physical_ids.get(logical_id).map(String::as_str),
                Some(listener_arn.as_str()),
                "case {case:?}: expected {logical_id}'s physical id to be the ARN the stack \
                 published, got {physical_ids:?}"
            );
            let expected_prefix =
                format!("{}/", load_balancer_arn.replace(":loadbalancer/", ":listener/"));
            assert!(
                listener_arn.starts_with(&expected_prefix),
                "case {case:?}: expected {logical_id}'s ARN to be a listener ARN of the \
                 published load balancer ({expected_prefix:?}), got {listener_arn:?}"
            );

            // …and the listener EXISTS in the service that owns it, with the port and the
            // protocol the template declared, on the load balancer the stack published.
            let described_listener = elbv2(
                &router,
                case,
                "DescribeListeners",
                &[("ListenerArns.member.1", &listener_arn)],
            )
            .await;
            assert_eq!(
                opt_element(&described_listener, "Port").as_deref(),
                Some(listener.port.to_string().as_str()),
                "case {case:?}: expected {logical_id} on port {}, got {described_listener}",
                listener.port
            );
            assert_eq!(
                opt_element(&described_listener, "Protocol").as_deref(),
                Some(listener.protocol),
                "case {case:?}: expected {logical_id} to speak {}, got {described_listener}",
                listener.protocol
            );
            assert_eq!(
                opt_element(&described_listener, "LoadBalancerArn").as_deref(),
                Some(load_balancer_arn),
                "case {case:?}: expected {logical_id} to be on the load balancer the stack \
                 published, got {described_listener}"
            );
            published_listeners.insert(listener_arn);
        }

        // Every listener the template declared, and no other, is one of that load balancer's own
        // listeners — the read a client makes when it holds the load balancer rather than the
        // listener.
        let listed = elbv2(
            &router,
            case,
            "DescribeListeners",
            &[("LoadBalancerArn", load_balancer_arn)],
        )
        .await;
        assert_eq!(
            elements(&listed, "ListenerArn")
                .into_iter()
                .collect::<BTreeSet<_>>(),
            published_listeners,
            "case {case:?}: expected the load balancer's listeners to be exactly the ones the \
             stack published, got {listed}"
        );

        // The resources of the two FURTHER services the same walk created are real as well: the
        // search domain the listener was not held behind is held by the service that owns it,
        // under the ARN the stack published for it.
        if row.search_estate {
            let domain_name = output(&outputs, case, DOMAIN_NAME_OUTPUT);
            let domain_status = describe_domain(&router, case, domain_name).await;
            assert_eq!(
                domain_status.get("DomainName").and_then(Value::as_str),
                Some(domain_name),
                "case {case:?}: expected Amazon OpenSearch Service to hold the domain the stack \
                 published, got {domain_status}"
            );
            assert_eq!(
                domain_status.get("ARN").and_then(Value::as_str),
                Some(output(&outputs, case, DOMAIN_ARN_OUTPUT)),
                "case {case:?}: expected the published domain ARN to be the owning service's \
                 own, got {domain_status}"
            );
        }
    }
}
