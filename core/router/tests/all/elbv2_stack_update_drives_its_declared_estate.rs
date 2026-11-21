// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateStack` does to a deployed Elastic Load Balancing v2 estate whose stack owns
//! its own VPC and internet gateway — and what the load balancer then SERVES.
//!
//! RULE: a stack operation asks the owning service only about the resources whose declaration
//! this deployment CHANGED — every other resource of the stack is carried forward untouched,
//! whatever its type — and each changed declaration is applied through that service's own call:
//! a property it can change is applied in place and the resource keeps its identity, a property
//! it cannot change replaces the resource and re-points what referenced it, and a change that
//! service REFUSES fails that resource, rolls the stack back and leaves the configuration in
//! force. The operation PROGRESSES on this emulator's own clock, not on its reader's: a
//! deployment nobody polls creates the resources behind a readiness wait all the same. So the
//! change under test is the thing that decides the operation's outcome, no resource of the stack
//! can refuse a deployment it was not asked about, and what the load balancer answers at the
//! address the stack published follows the configuration that is in force at each of those
//! points.
//!
//! The estate here is the one that breaks the rule in practice: an internet-facing Application
//! Load Balancer placed in subnets of the stack's OWN VPC, which the stack also gives an
//! internet gateway and an `AWS::EC2::VPCGatewayAttachment` — the attachment every one of these
//! updates restates exactly as it declared it, and which therefore has nothing asked of Amazon
//! EC2 about it.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation drives the deployment, Amazon EC2 owns the network it is placed in, and
//! Elastic Load Balancing owns the load balancer, the groups, the listener and its rules, serves
//! the published address and answers every call the deployment makes. Everything runs over ONE
//! engine, assembled exactly as the binary assembles it, so what is asserted is what each
//! service persisted.
//!
//! The tests span what the rule generalises over — WHICH KIND of change one deployment states: a
//! property the owning service applies in place (the body a rule's own action renders and the
//! group a rule forwards to, beside an attribute that is a second call of that service), a
//! property it cannot change at all (the forwarded group's name, which replaces the group), and
//! a change it refuses (a rule given a priority its sibling already holds) — beside the create
//! that no client reads at all.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE dial of an address a load balancer PUBLISHED this test crate keeps — a real connection
// to the authority, whose port it first holds to being one of the emulator's own — so this suite
// reaches the deployed estate exactly as a client handed that name does, rather than by handing
// the name back to the router.
use crate::elbv2_published_address_enforces_its_web_acl::exchange;
// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the two query wires this suite drives carry.
const CFN_VERSION: &str = "2010-05-15";
const ELBV2_VERSION: &str = "2015-12-01";

/// Elastic Load Balancing's two models share one signing name and one endpoint, so the v2 model
/// is addressed by the routing key that names IT.
const ELBV2_KEY: &str = "elasticloadbalancingv2";

/// How long a case waits for a stack operation to reach a terminal status, or for the address
/// the stack published to answer, and how often it looks again.
///
/// Every wait below waits for its CONDITION — a status that is no longer `*_IN_PROGRESS`, an
/// answer from the published address — which the loop polls for; the deadline is only the
/// backstop that turns something nothing will ever satisfy into a readable failure instead of a
/// hang, so it is generous.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The stack statuses a deployment of this estate settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The resource status a resource whose update the owning service refused carries.
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// The logical ids of the gateway attachment every test declares and none changes, and of the
/// two rules on the listener.
const ATTACHMENT: &str = "IgwAttach";
const FIRST_RULE: &str = "RuleFirst";
const SECOND_RULE: &str = "RuleSecond";

/// The priority the first rule holds in every template below — so the one a second rule cannot
/// be given — and the one the second rule holds while the two differ.
const FIRST_PRIORITY: &str = "10";
const SECOND_PRIORITY: &str = "50";

/// The name the load balancer is declared with, which is how a client that has read nothing of
/// the stack addresses it in the service that owns it.
const ALB_NAME: &str = "update-estate-alb";

/// The path each rule's condition matches, and the one the listener's own default action answers.
const FIRST_PATH: &str = "/a";
const SECOND_PATH: &str = "/b";

/// What the resource's failure has to carry, whatever the owning service worded its refusal as:
/// the clause an SDK renders around the call CloudFormation's handler made — the service that
/// answered and the HTTP status it answered with — and the handler's own classification of a
/// priority that is already taken.
///
/// The refusal's MESSAGE TEXT is deliberately not asserted here: which sentence Elastic Load
/// Balancing words a priority collision with is disputed between two open elbv2 issues
/// (`20261112T140934Z-elbv2-7-net-new-causes-loadbalancer-dnsname-is.md` cause 7 and
/// `20261113T153150Z-elbv2-3-net-new-causes-the-setrulepriorities-priorityinuse.md` cause 1), so
/// pinning either spelling here would freeze a wording whose own fix belongs to those issues.
/// What this suite is about is whose refusal reaches the resource, which these three do name.
const REFUSING_SERVICE: &str = "Service: ElasticLoadBalancingV2";
const REFUSED_STATUS: &str = "Status Code: 400";
const TAKEN_PRIORITY_CLASS: &str = "HandlerErrorCode: AlreadyExists";

/// The values a template DECLARES, so a test can deploy one template and then update the stack
/// with the same template carrying one changed value.
#[derive(Clone, Copy, Debug)]
struct Declared {
    /// The `MessageBody` of the first rule's fixed-response action, which is what a client that
    /// dials the published address on [`FIRST_PATH`] is answered with.
    first_body: &'static str,
    /// The logical id of the target group the second rule's forward action names.
    second_forward: &'static str,
    /// The second rule's `Priority`.
    second_priority: &'static str,
    /// The `Name` of the first target group — a property Elastic Load Balancing cannot change on
    /// the group that exists, so a new one asks for a new group.
    first_group_name: &'static str,
    /// The load balancer's `deletion_protection.enabled` attribute.
    deletion_protection: &'static str,
}

/// What every test deploys first.
const BASE: Declared = Declared {
    first_body: "a1",
    second_forward: "TgA",
    second_priority: SECOND_PRIORITY,
    first_group_name: "update-estate-a",
    deletion_protection: "false",
};

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// Call one query operation of the service `key` addresses, answered with its status and XML
/// body. The key is both the host label and the credential scope, which are the two signals the
/// router resolves a service by.
async fn query(
    router: &MultiServiceRouter,
    key: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{key}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{key}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Call one CloudFormation operation, which must be accepted.
async fn cfn(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "cloudformation", CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudFormation {action}, got {status} with body {body}"
    );
    body
}

/// Call one Elastic Load Balancing v2 operation, answered with its status and body.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, ELBV2_KEY, ELBV2_VERSION, action, members).await
}

/// The template one test deploys: the network the load balancer is placed in, the internet
/// gateway and the attachment that puts it on the VPC, the load balancer, its two target groups,
/// the listener and the two rules on it.
fn template(declared: &Declared) -> String {
    let Declared {
        first_body,
        second_forward,
        second_priority,
        first_group_name,
        deletion_protection,
    } = declared;
    format!(
        "Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.4.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.4.1.0/24
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.4.2.0/24
      AvailabilityZone: {REGION}b
  Igw:
    Type: AWS::EC2::InternetGateway
  {ATTACHMENT}:
    Type: AWS::EC2::VPCGatewayAttachment
    Properties:
      VpcId: !Ref Vpc
      InternetGatewayId: !Ref Igw
  Routes:
    Type: AWS::EC2::RouteTable
    Properties:
      VpcId: !Ref Vpc
  DefaultRoute:
    Type: AWS::EC2::Route
    DependsOn: {ATTACHMENT}
    Properties:
      RouteTableId: !Ref Routes
      DestinationCidrBlock: 0.0.0.0/0
      GatewayId: !Ref Igw
  SubnetARoutes:
    Type: AWS::EC2::SubnetRouteTableAssociation
    Properties:
      SubnetId: !Ref SubnetA
      RouteTableId: !Ref Routes
  SubnetBRoutes:
    Type: AWS::EC2::SubnetRouteTableAssociation
    Properties:
      SubnetId: !Ref SubnetB
      RouteTableId: !Ref Routes
  Sg:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group the load balancer is placed in
      VpcId: !Ref Vpc
      SecurityGroupIngress:
        - IpProtocol: tcp
          FromPort: 80
          ToPort: 80
          CidrIp: 0.0.0.0/0
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {ALB_NAME}
      Type: application
      Scheme: internet-facing
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
      SecurityGroups:
        - !Ref Sg
      LoadBalancerAttributes:
        - Key: deletion_protection.enabled
          Value: \"{deletion_protection}\"
  TgA:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {first_group_name}
      Port: 80
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
  TgB:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: update-estate-b
      Port: 80
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: 80
      Protocol: HTTP
      DefaultActions:
        - Type: fixed-response
          FixedResponseConfig:
            StatusCode: \"404\"
            ContentType: text/plain
            MessageBody: no rule matched
  {FIRST_RULE}:
    Type: AWS::ElasticLoadBalancingV2::ListenerRule
    Properties:
      ListenerArn: !Ref Listener
      Priority: {FIRST_PRIORITY}
      Conditions:
        - Field: path-pattern
          Values:
            - {FIRST_PATH}
      Actions:
        - Type: fixed-response
          FixedResponseConfig:
            StatusCode: \"200\"
            ContentType: text/plain
            MessageBody: {first_body}
  {SECOND_RULE}:
    Type: AWS::ElasticLoadBalancingV2::ListenerRule
    Properties:
      ListenerArn: !Ref Listener
      Priority: {second_priority}
      Conditions:
        - Field: path-pattern
          Values:
            - {SECOND_PATH}
      Actions:
        - Type: forward
          TargetGroupArn: !Ref {second_forward}
Outputs:
  AlbArn:
    Value: !Ref Alb
  AlbDns:
    Value: !GetAtt Alb.DNSName
  ListenerArn:
    Value: !Ref Listener
  TgAArn:
    Value: !Ref TgA
  TgBArn:
    Value: !Ref TgB
  RuleFirstArn:
    Value: !Ref {FIRST_RULE}
  RuleSecondArn:
    Value: !Ref {SECOND_RULE}
"
    )
}

/// The outputs every template above declares, which is the map a client reads a deployment's
/// identifiers out of.
const OUTPUT_KEYS: &[&str] = &[
    "AlbArn",
    "AlbDns",
    "ListenerArn",
    "TgAArn",
    "TgBArn",
    "RuleFirstArn",
    "RuleSecondArn",
];

/// Read a stack until it reports a status that is no longer in progress, and answer that read's
/// body.
async fn settle(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let started = Instant::now();
    loop {
        let body = cfn(router, case, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack operation never settled; last status {reported:?}, and its \
             events were {:?}",
            events(router, case, stack).await
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every (`LogicalResourceId`, `ResourceStatus`, `ResourceStatusReason`) a stack's event stream
/// carries, read INSIDE the event that carries them so no triple pairs one event's id with
/// another's status.
async fn events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String)> {
    let body = cfn(router, case, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .iter()
        .map(|event| {
            (
                opt_element(event, "LogicalResourceId").unwrap_or_default(),
                opt_element(event, "ResourceStatus").unwrap_or_default(),
                opt_element(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// The identifiers a described stack publishes.
fn identifiers(described: &str) -> Vec<(String, String)> {
    let outputs = outputs_of(described);
    OUTPUT_KEYS
        .iter()
        .map(|key| {
            (
                (*key).to_string(),
                outputs.get(*key).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

/// The value one output carries.
fn identifier(case: &str, described: &str, key: &str) -> String {
    outputs_of(described)
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key:?} output, got {described}"))
}

/// One rule as Elastic Load Balancing reports it, read by its OWN ARN so nothing has to be
/// paired across a list: its priority, the bodies its fixed-response actions render and the
/// groups its forward actions name.
struct Rule {
    priority: String,
    bodies: Vec<String>,
    /// The groups its forward actions name, as the SET of them: that service reports one
    /// action's group both on the action itself and inside the forward configuration it carries,
    /// so the two readings of one group are one group.
    groups: BTreeSet<String>,
}

/// The rule `rule_arn` names, as that service reports it.
async fn rule_of(router: &MultiServiceRouter, case: &str, rule_arn: &str) -> Rule {
    let (status, body) = elbv2(router, "DescribeRules", &[("RuleArns.member.1", rule_arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeRules on {rule_arn:?}, got {status} with body \
         {body}"
    );
    Rule {
        priority: opt_element(&body, "Priority").unwrap_or_default(),
        bodies: elements(&body, "MessageBody"),
        groups: elements(&body, "TargetGroupArn").into_iter().collect(),
    }
}

/// The attribute pairs Elastic Load Balancing reports for one load balancer.
async fn load_balancer_attributes(
    router: &MultiServiceRouter,
    case: &str,
    load_balancer_arn: &str,
) -> Vec<(String, String)> {
    let (status, body) = elbv2(
        router,
        "DescribeLoadBalancerAttributes",
        &[("LoadBalancerArn", load_balancer_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeLoadBalancerAttributes, got {status} with body \
         {body}"
    );
    members_of(&body)
        .iter()
        .map(|attribute| {
            (
                opt_element(attribute, "Key").unwrap_or_default(),
                opt_element(attribute, "Value").unwrap_or_default(),
            )
        })
        .collect()
}

/// What the address the stack published as the load balancer's `DNSName` answers on `path`, as
/// `(status, the whole answer)`, once that answer carries `wanted` — dialled again until it does,
/// or the deadline passes.
///
/// A load balancer comes up over a span the clock owes, so the first dial can land before its
/// listener is serving: the wait is for the ANSWER, never for a span of time.
async fn served(case: &str, dns_name: &str, path: &str, wanted: &str) -> (u16, String) {
    let started = Instant::now();
    loop {
        let answered = exchange(case, dns_name, "GET", path, &[]).await;
        if answered.raw.contains(wanted) || started.elapsed() >= SETTLE_DEADLINE {
            return (answered.status, answered.raw);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Assert that the published address answers `path` with `body`, as a client handed that name
/// is answered.
async fn assert_serves(case: &str, dns_name: &str, path: &str, body: &str) {
    let (status, raw) = served(case, dns_name, path, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the published address {dns_name:?} to answer {path} with 200, \
         got {status} and {raw:?}"
    );
    assert!(
        raw.contains(body),
        "case {case:?}: expected the published address {dns_name:?} to answer {path} with \
         {body:?}, got {raw:?}"
    );
}

/// Deploy `BASE` as a new stack and answer the stack id and the described stack, having asserted
/// it reached `CREATE_COMPLETE` with every output resolved.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let body = cfn(
        router,
        case,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template(&BASE))],
    )
    .await;
    let stack_id = element(&body, "StackId");
    let described = settle(router, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the create must complete; its events were {:?}",
        events(router, case, &stack_id).await
    );
    for (key, value) in identifiers(&described) {
        assert!(
            !value.is_empty(),
            "case {case:?}: output {key} of a CREATE_COMPLETE stack must carry a value, got \
             {described}"
        );
    }
    (stack_id, described)
}

/// Update `stack` with the template `declared` states and answer the settled stack.
async fn update(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    declared: &Declared,
) -> String {
    cfn(
        router,
        case,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &template(declared))],
    )
    .await;
    settle(router, case, stack).await
}

/// Assert that the gateway attachment the deployment restated unchanged had nothing asked of it:
/// it reported no transition of its own beyond the create that made it.
///
/// The rule's "every other resource is carried forward untouched" half, which every update suite
/// below holds the deployment to.
fn attachment_untouched(case: &str, reported: &[(String, String, String)]) {
    let touched: Vec<&(String, String, String)> = reported
        .iter()
        .filter(|(logical_id, status, _)| logical_id == ATTACHMENT && !status.starts_with("CREATE_"))
        .collect();
    assert!(
        touched.is_empty(),
        "case {case:?}: an unchanged gateway attachment must have nothing asked of it by the \
         update, got {touched:?}"
    );
}

/// Assert that the only resources this stack's events report as having FAILED to update are the
/// ones `expected` names.
fn only_these_failed(case: &str, reported: &[(String, String, String)], expected: &[&str]) {
    let failed: Vec<&(String, String, String)> = reported
        .iter()
        .filter(|(logical_id, status, _)| {
            status == UPDATE_FAILED && !expected.contains(&logical_id.as_str())
        })
        .collect();
    assert!(
        failed.is_empty(),
        "case {case:?}: only {expected:?} may fail this update — a resource the deployment did \
         not change is asked nothing — got {failed:?}"
    );
}

/// What the service that OWNS the estate holds of it, read with no CloudFormation call at all:
/// the priorities of every rule on the listener of the load balancer the template named, or
/// `None` while that service does not hold the load balancer or its listener yet.
///
/// The load balancer is addressed by the NAME the declaration gives it, which is the one handle a
/// client that has read nothing of the stack has.
async fn estate_priorities(router: &MultiServiceRouter) -> Option<Vec<String>> {
    let (status, balancers) = elbv2(
        router,
        "DescribeLoadBalancers",
        &[("Names.member.1", ALB_NAME)],
    )
    .await;
    if status != 200 {
        return None;
    }
    let load_balancer = opt_element(&balancers, "LoadBalancerArn")?;
    let (status, listeners) = elbv2(
        router,
        "DescribeListeners",
        &[("LoadBalancerArn", &load_balancer)],
    )
    .await;
    if status != 200 {
        return None;
    }
    let listener = opt_element(&listeners, "ListenerArn")?;
    let (status, rules) = elbv2(router, "DescribeRules", &[("ListenerArn", &listener)]).await;
    (status == 200).then(|| elements(&rules, "Priority"))
}

/// RULE, the half that says WHOSE the progress of a started operation is: a deployment proceeds
/// on this emulator's own clock, so the resources of a stack nobody reads are created in the
/// services that own them all the same — the ones behind a resource whose service reported it not
/// yet ready included.
///
/// The estate's listener and its rules are exactly those resources: they declare the load
/// balancer, which Elastic Load Balancing answers the create of in `provisioning`, so nothing of
/// them can be created until that service reports it ready. Here the whole wait is driven with no
/// CloudFormation call made between the `CreateStack` and the one read at the end, which is the
/// position of a client driven by the stack's own events rather than by polling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployment_nobody_reads_still_creates_the_resources_behind_its_waits() {
    let case = "a deployment the client starts and does not read";
    let router = router();
    let body = cfn(
        &router,
        case,
        "CreateStack",
        &[
            ("StackName", "elbv2-update-unwatched"),
            ("TemplateBody", &template(&BASE)),
        ],
    )
    .await;
    let stack_id = element(&body, "StackId");

    // Nothing of CloudFormation is asked anything until the OWNING service holds the whole
    // estate: the wait is for the condition, looked for in that service alone.
    let wanted = [
        FIRST_PRIORITY.to_string(),
        SECOND_PRIORITY.to_string(),
    ];
    let started = Instant::now();
    let held = loop {
        let held = estate_priorities(&router).await.unwrap_or_default();
        let complete = wanted.iter().all(|priority| held.contains(priority));
        if complete || started.elapsed() >= SETTLE_DEADLINE {
            break held;
        }
        tokio::time::sleep(POLL).await;
    };
    for priority in &wanted {
        assert!(
            held.contains(priority),
            "case {case:?}: the rule at priority {priority:?} must be created in the service that \
             owns it with nobody reading the stack, got {held:?}"
        );
    }

    // ...and the FIRST read of the stack reports the deployment that progressed without it.
    let described = settle(&router, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the create must complete; its events were {:?}",
        events(&router, case, &stack_id).await
    );
}

/// RULE, the half an applied change takes: a changed declaration of a property the owning
/// service CAN change is applied to the resource that exists, through that service's own call,
/// the address the stack published serves what is then in force, and nothing else in the stack
/// is touched — the gateway attachment the deployment restates unchanged among it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_place_update_applies_each_changed_declaration_and_asks_nothing_about_the_rest() {
    let case = "a rule's own fixed response and the group its sibling forwards to, beside the \
                load balancer's deletion protection";
    let router = router();
    let (stack_id, described) = deploy(&router, case, "elbv2-update-in-place").await;
    let before = identifiers(&described);
    let dns_name = identifier(case, &described, "AlbDns");
    let first_rule = identifier(case, &described, "RuleFirstArn");
    let second_rule = identifier(case, &described, "RuleSecondArn");
    let second_group = identifier(case, &described, "TgBArn");

    // The deployed estate SERVES: the address the stack published answers the rule it declared.
    assert_serves(case, &dns_name, FIRST_PATH, BASE.first_body).await;

    let described = update(
        &router,
        case,
        &stack_id,
        &Declared {
            first_body: "a2",
            second_forward: "TgB",
            deletion_protection: "true",
            ..BASE
        },
    )
    .await;
    let reported = events(&router, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_COMPLETE),
        "case {case:?}: the update must complete; its events were {reported:?}"
    );
    only_these_failed(case, &reported, &[]);

    // Nothing was replaced: every identifier the stack published is the one a client already
    // holds.
    assert_eq!(
        identifiers(&described),
        before,
        "case {case:?}: an in-place update must keep every identifier the stack published"
    );

    // Elastic Load Balancing applied every one of the three changes.
    let first = rule_of(&router, case, &first_rule).await;
    assert_eq!(
        first.bodies,
        vec!["a2".to_string()],
        "case {case:?}: expected the rule to render the body the update declared, got {:?}",
        first.bodies
    );
    let second = rule_of(&router, case, &second_rule).await;
    assert_eq!(
        second.groups,
        BTreeSet::from([second_group]),
        "case {case:?}: expected the rule to forward to the group the update declared, got {:?}",
        second.groups
    );
    let attributes =
        load_balancer_attributes(&router, case, &identifier(case, &described, "AlbArn")).await;
    assert!(
        attributes
            .iter()
            .any(|(key, value)| key == "deletion_protection.enabled" && value == "true"),
        "case {case:?}: expected the declared attribute to be in force, got {attributes:?}"
    );

    // ...and the published address serves what the update put in force.
    assert_serves(case, &dns_name, FIRST_PATH, "a2").await;

    attachment_untouched(case, &reported);
}

/// RULE, the half a change the owning service cannot apply takes: it REPLACES the resource, and
/// what referenced the old one is re-pointed at the new one while keeping its own identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_the_owning_service_cannot_apply_replaces_the_resource_it_names() {
    let case = "the forwarded group's name, which Elastic Load Balancing cannot change";
    let router = router();
    let (stack_id, described) = deploy(&router, case, "elbv2-update-replace").await;
    let dns_name = identifier(case, &described, "AlbDns");
    let listener_arn = identifier(case, &described, "ListenerArn");
    let second_rule = identifier(case, &described, "RuleSecondArn");
    let replaced_group = identifier(case, &described, "TgAArn");
    assert_serves(case, &dns_name, FIRST_PATH, BASE.first_body).await;

    let described = update(
        &router,
        case,
        &stack_id,
        &Declared {
            first_group_name: "update-estate-a2",
            ..BASE
        },
    )
    .await;
    let reported = events(&router, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_COMPLETE),
        "case {case:?}: the update must complete; its events were {reported:?}"
    );
    only_these_failed(case, &reported, &[]);
    attachment_untouched(case, &reported);

    let group = identifier(case, &described, "TgAArn");
    assert_ne!(
        group, replaced_group,
        "case {case:?}: expected the replaced group to be published under a new ARN"
    );
    assert_eq!(
        identifier(case, &described, "ListenerArn"),
        listener_arn,
        "case {case:?}: expected the listener to come through the replacement with its own ARN"
    );
    assert_eq!(
        identifier(case, &described, "RuleSecondArn"),
        second_rule,
        "case {case:?}: expected the rule to come through the replacement with its own ARN"
    );

    // The new group carries the declared name, and the replaced one is gone from that service.
    let (status, body) = elbv2(
        &router,
        "DescribeTargetGroups",
        &[("TargetGroupArns.member.1", &group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for the new group, got {status} with body {body}"
    );
    assert_eq!(
        opt_element(&body, "TargetGroupName").as_deref(),
        Some("update-estate-a2"),
        "case {case:?}: expected the new group to carry the declared name, got {body}"
    );
    let (status, body) = elbv2(
        &router,
        "DescribeTargetGroups",
        &[("TargetGroupArns.member.1", &replaced_group)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the replaced group to be gone, got {status} with body {body}"
    );
    assert_eq!(
        opt_element(&body, "Code").as_deref(),
        Some("TargetGroupNotFound"),
        "case {case:?}: expected TargetGroupNotFound for the replaced group, got {body}"
    );

    // What forwarded to the replaced group now forwards to the one that exists...
    let second = rule_of(&router, case, &second_rule).await;
    assert_eq!(
        second.groups,
        BTreeSet::from([group]),
        "case {case:?}: expected the rule to forward to the group that now exists, got {:?}",
        second.groups
    );
    // ...and the estate goes on serving across the replacement.
    assert_serves(case, &dns_name, FIRST_PATH, BASE.first_body).await;
}

/// RULE, the half a REFUSED change takes: the resource the owning service refused fails carrying
/// that service's own refusal, the stack rolls back, and the configuration in force is the one
/// the refused deployment never replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_the_owning_service_refuses_fails_that_resource_and_rolls_the_stack_back() {
    let case = "a rule given the priority its sibling already holds";
    let router = router();
    let (stack_id, described) = deploy(&router, case, "elbv2-update-refused").await;
    let dns_name = identifier(case, &described, "AlbDns");
    let first_rule = identifier(case, &described, "RuleFirstArn");
    let second_rule = identifier(case, &described, "RuleSecondArn");
    let before = identifiers(&described);
    assert_serves(case, &dns_name, FIRST_PATH, BASE.first_body).await;

    let described = update(
        &router,
        case,
        &stack_id,
        &Declared {
            first_body: "a2",
            second_priority: FIRST_PRIORITY,
            ..BASE
        },
    )
    .await;
    let reported = events(&router, case, &stack_id).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_ROLLBACK_COMPLETE),
        "case {case:?}: a refused change must roll the stack back; its events were {reported:?}"
    );

    // The resource that failed is the rule whose change was refused, carrying the refusal
    // Elastic Load Balancing answered.
    assert!(
        reported.iter().any(|(logical_id, status, reason)| {
            logical_id == SECOND_RULE
                && status == UPDATE_FAILED
                && [REFUSING_SERVICE, REFUSED_STATUS, TAKEN_PRIORITY_CLASS]
                    .iter()
                    .all(|part| reason.contains(part))
        }),
        "case {case:?}: expected the rule's failure to carry the refusal Elastic Load Balancing \
         answered — {REFUSING_SERVICE}, {REFUSED_STATUS}, {TAKEN_PRIORITY_CLASS} — got \
         {reported:?}"
    );
    // ...and no OTHER resource of the stack failed: one the deployment did not change is asked
    // nothing, so it cannot refuse the operation.
    only_these_failed(case, &reported, &[SECOND_RULE]);
    attachment_untouched(case, &reported);

    // The rollback left every identifier and the configuration in force as they were.
    assert_eq!(
        identifiers(&described),
        before,
        "case {case:?}: a rolled-back update must keep every identifier the stack published"
    );
    for (rule_arn, priority) in [
        (&first_rule, FIRST_PRIORITY),
        (&second_rule, SECOND_PRIORITY),
    ] {
        let held = rule_of(&router, case, rule_arn).await;
        assert_eq!(
            held.priority, priority,
            "case {case:?}: expected {rule_arn:?} back at priority {priority:?}, got {:?}",
            held.priority
        );
    }
    let first = rule_of(&router, case, &first_rule).await;
    assert_eq!(
        first.bodies,
        vec![BASE.first_body.to_string()],
        "case {case:?}: expected the rolled-back rule to render the body in force, got {:?}",
        first.bodies
    );
    assert_serves(case, &dns_name, FIRST_PATH, BASE.first_body).await;
}
