// SPDX-License-Identifier: CC0-1.0

//! A stack's `AWS::ElasticLoadBalancingV2::Listener` SETTLES on the answer of the create that
//! made it.
//!
//! The rule under test: **a resource type whose owning service creates it synchronously —
//! one whose create answers the resource itself, with no member reporting it still coming up —
//! reaches `CREATE_COMPLETE` on that answer, so the resource never waits for a readiness
//! condition nothing in that service will ever report.** Elastic Load Balancing's
//! `CreateListener` is such a create: it answers `Listeners`, the `Listener` shape carries no
//! state member at all, and every error it models is a client refusal
//! (`IncompatibleProtocols`, `InvalidLoadBalancerAction`, `TargetGroupNotFound`,
//! `LoadBalancerNotFound`, …). So a listener a template declares is `CREATE_COMPLETE` as soon
//! as that call returns, the stack holding it reaches `CREATE_COMPLETE` and publishes its
//! `Outputs`, and a configuration that service REFUSES fails the resource with the refusal
//! that service worded — never with a wait that ran out.
//!
//! What a client does with such a stack is the whole point: it deploys an ALB-fronted estate
//! in one template — the load balancer, the target groups traffic is routed to, the listener
//! that accepts connections and the alarm over them — reads the stack's `Outputs` into a map
//! and drives the estate by the identifiers it found. A stack that rolls itself back because
//! its listener never left `CREATE_IN_PROGRESS` publishes no output at all, and tears down
//! every resource it had created.
//!
//! The rows differ along what the rule generalises over: the load balancer FAMILY and SCHEME
//! (an internal and an internet-facing Application Load Balancer, an internal Network Load
//! Balancer), the listener's own protocol and port, whether the listener's protocol MATCHES
//! the protocol of the target group it forwards to (an `HTTP:80` listener in front of an
//! `HTTPS:443` target group is a pairing AWS accepts, and the one the reported estate asked
//! for), whether its default action forwards at all or answers a fixed response, how MANY
//! listeners one load balancer carries, and whether the stack holds further resources that
//! are still coming up while the listener is created. The complement — a pairing Elastic Load
//! Balancing refuses — is its own case at the end.

use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_resource_provisioning::{output, outputs_of, NETWORK};
use crate::stack_lifecycle::{
    call, create_stack, elbv2, member, resource_event_reason, resource_event_statuses, router,
    settled_stack, xml_blocks, xml_text, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The text a resource whose wait ran out is failed with
/// (`state::stabilising::StabilisationBarrier::not_stabilised_reason`): what no listener may
/// ever be reported with, since nothing about a listener is waited for.
const RAN_OUT: &str = "when the wait for it to be created ran out";

/// One listener a row declares: the logical id the template gives it, the output the template
/// publishes its ARN through, and the port and protocol it is created with.
struct Listener {
    logical_id: &'static str,
    output_key: &'static str,
    port: &'static str,
    protocol: &'static str,
    /// The segment the load balancer's family puts in a listener's ARN
    /// (`listener/<family>/<name>/<lb id>/<listener id>`).
    family_segment: &'static str,
}

/// One row: a template, the output its load balancer's ARN arrives through, and the listeners
/// it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The resources declared beside [`NETWORK`], and the template's whole `Outputs` section.
    template: &'static str,
    load_balancer_output: &'static str,
    listeners: &'static [Listener],
}

const ROWS: &[Row] = &[
    Row {
        // The estate the issue reported, with the pairing it asked for: an HTTP listener in
        // front of HTTPS target groups, on an INTERNAL load balancer, beside an alarm over the
        // pair and a second load balancer nothing references — a resource that is still coming
        // up while the listener is created, which the listener must not be held behind.
        case: "an HTTP:80 listener on an internal application load balancer forwarding to an \
               ip HTTPS:443 target group, beside an alarm and an unreferenced load balancer",
        stack_name: "listener-internal-estate",
        template: concat!(
            "  Alb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: estate-alb\n",
            "      Type: application\n",
            "      Scheme: internal\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "  Spare:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: estate-spare-alb\n",
            "      Type: application\n",
            "      Scheme: internal\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "  DashboardsTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: estate-dashboards\n",
            "      Port: 443\n",
            "      Protocol: HTTPS\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  ApiTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: estate-api\n",
            "      Port: 443\n",
            "      Protocol: HTTPS\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  HttpListener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref DashboardsTg\n",
            "  UnhealthyAlarm:\n",
            "    Type: AWS::CloudWatch::Alarm\n",
            "    Properties:\n",
            "      AlarmName: estate-unhealthy-hosts\n",
            "      Namespace: AWS/ApplicationELB\n",
            "      MetricName: UnHealthyHostCount\n",
            "      ComparisonOperator: GreaterThanThreshold\n",
            "      EvaluationPeriods: 1\n",
            "      Period: 60\n",
            "      Statistic: Maximum\n",
            "      Threshold: 0\n",
            "      Dimensions:\n",
            "        - Name: LoadBalancer\n",
            "          Value: !GetAtt Alb.LoadBalancerFullName\n",
            "        - Name: TargetGroup\n",
            "          Value: !GetAtt DashboardsTg.TargetGroupFullName\n",
            "Outputs:\n",
            "  VpcId:\n",
            "    Value: !Ref Vpc\n",
            "  AlbArn:\n",
            "    Value: !Ref Alb\n",
            "  AlbDnsName:\n",
            "    Value: !GetAtt Alb.DNSName\n",
            "  DashboardsTgArn:\n",
            "    Value: !Ref DashboardsTg\n",
            "  ApiTgArn:\n",
            "    Value: !Ref ApiTg\n",
            "  HttpListenerArn:\n",
            "    Value: !Ref HttpListener\n",
            "  AlarmName:\n",
            "    Value: !Ref UnhealthyAlarm\n",
        ),
        load_balancer_output: "AlbArn",
        listeners: &[Listener {
            logical_id: "HttpListener",
            output_key: "HttpListenerArn",
            port: "80",
            protocol: "HTTP",
            family_segment: "app",
        }],
    },
    Row {
        // The same rule where the default action FORWARDS NOWHERE: a fixed-response listener
        // names no target group at all, so nothing but the load balancer is read for it.
        case: "an HTTP:8080 listener answering a fixed response on an internet-facing \
               application load balancer with no target group",
        stack_name: "listener-fixed-response",
        template: concat!(
            "  Alb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: fixed-alb\n",
            "      Type: application\n",
            "      Scheme: internet-facing\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "  Gate:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 8080\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: fixed-response\n",
            "          FixedResponseConfig:\n",
            "            StatusCode: \"404\"\n",
            "            ContentType: text/plain\n",
            "            MessageBody: no-match\n",
            "Outputs:\n",
            "  AlbArn:\n",
            "    Value: !Ref Alb\n",
            "  GateArn:\n",
            "    Value: !Ref Gate\n",
        ),
        load_balancer_output: "AlbArn",
        listeners: &[Listener {
            logical_id: "Gate",
            output_key: "GateArn",
            port: "8080",
            protocol: "HTTP",
            family_segment: "app",
        }],
    },
    Row {
        // Another FAMILY: a Network Load Balancer's listener, whose ARN carries the `net`
        // segment and whose protocol is one only that family serves.
        case: "a TCP:5000 listener on an internal network load balancer forwarding to a TCP \
               target group",
        stack_name: "listener-network",
        template: concat!(
            "  Nlb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: stream-nlb\n",
            "      Type: network\n",
            "      Scheme: internal\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "  StreamTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: stream-targets\n",
            "      Port: 5000\n",
            "      Protocol: TCP\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  StreamListener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Nlb\n",
            "      Port: 5000\n",
            "      Protocol: TCP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref StreamTg\n",
            "Outputs:\n",
            "  AlbArn:\n",
            "    Value: !Ref Nlb\n",
            "  StreamListenerArn:\n",
            "    Value: !Ref StreamListener\n",
        ),
        load_balancer_output: "AlbArn",
        listeners: &[Listener {
            logical_id: "StreamListener",
            output_key: "StreamListenerArn",
            port: "5000",
            protocol: "TCP",
            family_segment: "net",
        }],
    },
    Row {
        // The COUNT the rule generalises over: two listeners on one load balancer, each
        // settling on its own create's answer.
        case: "two listeners on one application load balancer, each forwarding to its own \
               target group",
        stack_name: "listener-pair",
        template: concat!(
            "  Alb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: pair-alb\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "  WebTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: pair-web\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  AdminTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: pair-admin\n",
            "      Port: 443\n",
            "      Protocol: HTTPS\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  WebListener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref WebTg\n",
            "  AdminListener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 8443\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref AdminTg\n",
            "Outputs:\n",
            "  AlbArn:\n",
            "    Value: !Ref Alb\n",
            "  WebListenerArn:\n",
            "    Value: !Ref WebListener\n",
            "  AdminListenerArn:\n",
            "    Value: !Ref AdminListener\n",
        ),
        load_balancer_output: "AlbArn",
        listeners: &[
            Listener {
                logical_id: "WebListener",
                output_key: "WebListenerArn",
                port: "80",
                protocol: "HTTP",
                family_segment: "app",
            },
            Listener {
                logical_id: "AdminListener",
                output_key: "AdminListenerArn",
                port: "8443",
                protocol: "HTTP",
                family_segment: "app",
            },
        ],
    },
];

/// The whole template of one row: the network every load balancer is placed in, the row's own
/// resources and its outputs.
fn template_of(row: &Row) -> String {
    ["Resources:\n", NETWORK, row.template].concat()
}

/// The `StackResource` block of one resource of a described stack, read off
/// `DescribeStackResources` — the per-resource read a client makes about a resource it is
/// waiting for.
async fn stack_resource(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources({stack}, {logical_id}) must answer 200, got \
         {code} {body}"
    );
    xml_blocks(&body, "member")
        .into_iter()
        .find(|entry| xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: no resource {logical_id:?} in the stack's resources: {body}")
        })
}

#[tokio::test]
async fn a_stacks_listener_reaches_create_complete_on_the_creates_own_answer() {
    let router = router();
    for row in ROWS {
        let case = row.case;
        let (code, body) = create_stack(&router, row.stack_name, &template_of(row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {body}"
        );
        // The shared waiter of this target: it polls `DescribeStacks` until the stack reports
        // a status no operation is still in flight in, under its own generous deadline, so
        // what this case waits for is the CONDITION rather than a span of time.
        let described = settled_stack(&router, case, row.stack_name).await;
        if xml_text(&described, "StackStatus").as_deref() != Some(status::CREATE_COMPLETE) {
            let (_, events) = call(
                &router,
                "DescribeStackEvents",
                &[member("StackName", row.stack_name)],
            )
            .await;
            panic!(
                "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {:?} ({:?}); \
                 its events: {events}",
                xml_text(&described, "StackStatus"),
                xml_text(&described, "StackStatusReason"),
            );
        }
        // The stack publishes its outputs, which is what a client reads the estate out of: a
        // rolled-back stack carries none of them.
        let values = outputs_of(case, &described);
        let load_balancer_arn = output(case, &values, row.load_balancer_output);

        for listener in row.listeners {
            let logical_id = listener.logical_id;
            // What the resource's own event stream says BECAME of it: it opened
            // `CREATE_IN_PROGRESS` and ended `CREATE_COMPLETE`, with nothing failed in
            // between — never a wait of this emulator's own running out. The statuses arrive
            // NEWEST FIRST (`DescribeStackEvents` reports events "in reverse chronological
            // order"), so the resource's last status is the first entry and its first status
            // the last.
            let statuses =
                resource_event_statuses(&router, case, row.stack_name, logical_id).await;
            assert_eq!(
                statuses.first().map(String::as_str),
                Some(resource_status::CREATE_COMPLETE),
                "case {case:?}: expected {logical_id} to have reached CREATE_COMPLETE, got \
                 {statuses:?}"
            );
            assert_eq!(
                statuses.last().map(String::as_str),
                Some(resource_status::CREATE_IN_PROGRESS),
                "case {case:?}: expected {logical_id}'s first event to be CREATE_IN_PROGRESS, \
                 got {statuses:?}"
            );
            assert!(
                !statuses.iter().any(|status| status.ends_with("_FAILED")),
                "case {case:?}: expected no failed event for {logical_id}, got {statuses:?}"
            );
            let failure = resource_event_reason(
                &router,
                row.stack_name,
                logical_id,
                resource_status::CREATE_FAILED,
            )
            .await;
            assert!(
                failure.is_empty(),
                "case {case:?}: expected no failure reason for {logical_id}, got {failure:?}"
            );

            // …and the per-resource read a client polls answers the same: the resource is
            // complete, under the physical id the stack published for it.
            let listener_arn = output(case, &values, listener.output_key);
            let reported = stack_resource(&router, case, row.stack_name, logical_id).await;
            assert_eq!(
                xml_text(&reported, "ResourceStatus").as_deref(),
                Some(resource_status::CREATE_COMPLETE),
                "case {case:?}: expected {logical_id} to be reported CREATE_COMPLETE, got \
                 {reported}"
            );
            assert_eq!(
                xml_text(&reported, "PhysicalResourceId").as_deref(),
                Some(listener_arn.as_str()),
                "case {case:?}: expected {logical_id}'s physical id to be the ARN the stack \
                 published, got {reported}"
            );

            // The ARN is one Elastic Load Balancing minted for a listener of this load
            // balancer: `listener/<family>/<name>/<lb id>/<listener id>`.
            let prefix = format!(
                "arn:aws:elasticloadbalancing:{REGION}:{ACCOUNT_ID}:listener/{}/",
                listener.family_segment
            );
            assert!(
                listener_arn.starts_with(&prefix),
                "case {case:?}: expected {logical_id}'s ARN to be a {} listener ARN, got \
                 {listener_arn:?}",
                listener.family_segment
            );

            // The listener EXISTS in the service that owns it, with the port and protocol the
            // template declared, on the load balancer the stack published.
            let (code, answered) = elbv2(
                &router,
                "DescribeListeners",
                &[member("ListenerArns.member.1", &listener_arn)],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected DescribeListeners({listener_arn}) to answer 200, got \
                 {code} {answered}"
            );
            assert_eq!(
                xml_text(&answered, "Port").as_deref(),
                Some(listener.port),
                "case {case:?}: expected {logical_id} on port {}, got {answered}",
                listener.port
            );
            assert_eq!(
                xml_text(&answered, "Protocol").as_deref(),
                Some(listener.protocol),
                "case {case:?}: expected {logical_id} to speak {}, got {answered}",
                listener.protocol
            );
            assert_eq!(
                xml_text(&answered, "LoadBalancerArn").as_deref(),
                Some(load_balancer_arn.as_str()),
                "case {case:?}: expected {logical_id} to be on the load balancer the stack \
                 published, got {answered}"
            );

            // …and it is one of that load balancer's own listeners, which is the read a client
            // makes when it has the load balancer rather than the listener.
            let (code, listed) = elbv2(
                &router,
                "DescribeListeners",
                &[member("LoadBalancerArn", &load_balancer_arn)],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected DescribeListeners by load balancer to answer 200, got \
                 {code} {listed}"
            );
            assert!(
                listed.contains(&listener_arn),
                "case {case:?}: expected the load balancer's listeners to carry \
                 {listener_arn:?}, got {listed}"
            );
        }
    }
}

/// The complement of the rule: a pairing Elastic Load Balancing REFUSES fails the listener
/// with that service's own refusal — the modelled `IncompatibleProtocols` — and rolls the
/// stack back, rather than holding the resource open until a wait runs out.
#[tokio::test]
async fn a_listener_the_owning_service_refuses_fails_with_its_modelled_refusal() {
    let router = router();
    let case = "an HTTP listener on an application load balancer forwarding to a TCP target \
                group";
    let template = [
        "Resources:\n",
        NETWORK,
        concat!(
            "  Alb:\n",
            "    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n",
            "    Properties:\n",
            "      Name: mismatched-alb\n",
            "      Type: application\n",
            "      Subnets:\n",
            "        - !Ref SubnetA\n",
            "        - !Ref SubnetB\n",
            "  StreamTg:\n",
            "    Type: AWS::ElasticLoadBalancingV2::TargetGroup\n",
            "    Properties:\n",
            "      Name: mismatched-targets\n",
            "      Port: 5000\n",
            "      Protocol: TCP\n",
            "      TargetType: ip\n",
            "      VpcId: !Ref Vpc\n",
            "  HttpListener:\n",
            "    Type: AWS::ElasticLoadBalancingV2::Listener\n",
            "    Properties:\n",
            "      LoadBalancerArn: !Ref Alb\n",
            "      Port: 80\n",
            "      Protocol: HTTP\n",
            "      DefaultActions:\n",
            "        - Type: forward\n",
            "          TargetGroupArn: !Ref StreamTg\n",
            "Outputs:\n",
            "  AlbArn:\n",
            "    Value: !Ref Alb\n",
        ),
    ]
    .concat();
    let (code, body) = create_stack(&router, "listener-mismatch", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, "listener-mismatch").await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refusal to roll the stack back, got {described}"
    );
    let reason = resource_event_reason(
        &router,
        "listener-mismatch",
        "HttpListener",
        resource_status::CREATE_FAILED,
    )
    .await;
    // Elastic Load Balancing's own wording for this refusal, whose variable part is the
    // target group the request named — the ARN that service minted for it, which the
    // rolled-back stack publishes nowhere, so the sentence is matched up to its prefix.
    assert!(
        reason.contains(
            "The listener and the following target groups have incompatible protocols: \
             arn:aws:elasticloadbalancing:"
        ),
        "case {case:?}: expected the listener to fail with the owning service's own refusal, \
         got {reason:?}"
    );
    assert!(
        !reason.contains(RAN_OUT),
        "case {case:?}: expected a refusal rather than a wait that ran out, got {reason:?}"
    );
}
