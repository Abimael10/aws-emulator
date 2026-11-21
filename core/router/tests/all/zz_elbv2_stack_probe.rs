// SPDX-License-Identifier: CC0-1.0

//! The Elastic Load Balancing v2 estate a CloudFormation stack deploys, driven on the emulator a
//! client STARTS: the built binary, on its own port, with request authentication on, reached over
//! TLS with SigV4-signed calls — and the load balancer's own address reached in the clear, as a
//! client handed that address reaches it.
//!
//! RULE: a stack operation on a deployed estate settles on a TERMINAL status and the address the
//! load balancer published answers the configuration that is then in force — the create, an
//! update the owning service REFUSES (which rolls back and leaves the configuration it never
//! replaced) and the update that follows it alike. A client drives that whole sequence through
//! CloudFormation's own wire and the published address, so it has to hold on the emulator as it
//! is STARTED and not only on a router a test assembles in its own process: the per-resource
//! listener a load balancer publishes is a socket only a running process accepts connections on,
//! and the deployment is carried by that process's own clock rather than by a test's polling.
//!
//! The sibling suite `elbv2_stack_update_drives_its_declared_estate` spans what the rule
//! generalises over — which KIND of change a deployment states — on a router assembled in
//! process; this one pins the sequence THERE, on the one configuration a client actually runs.
//! It starts an emulator of its own rather than taking a leg on the start
//! `cfn_deployed_pipeline_on_a_started_emulator` shares, because it drives one stack through
//! three operations and the legs of that start are each a single exchange on it.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::time::{Duration, Instant};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
// The ONE reading this test crate keeps of whether an address a load balancer PUBLISHED is one
// of this emulator's own — a `host:port` on the interface the core binds rather than a name in
// Elastic Load Balancing's zone, which resolves to nothing on this machine.
use crate::elbv2_published_address_enforces_its_web_acl::dialable_port;
use crate::query_wire::{elements, members_of, opt_element, outputs_of};

/// How long a step waits for the condition it needs — a stack operation's terminal status, the
/// answer the published address gives — and how often it looks again.
///
/// Every wait below waits for its CONDITION; the deadline is only the backstop that turns
/// something nothing will ever satisfy into a readable failure instead of a hang, so it is
/// generous.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(100);

/// How long one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The stack this suite deploys, and the statuses its three operations settle in.
const STACK: &str = "started-elbv2-estate";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// The logical id of the rule each update restates, and of the attachment no update changes.
const FIRST_RULE: &str = "RuleA";
const ATTACHMENT: &str = "IgwAttach";

/// The priority the second rule holds throughout, so the one the first cannot be moved onto, and
/// the free priority the last update moves it to.
const HELD_PRIORITY: &str = "20";
const FREE_PRIORITY: &str = "15";

/// What the refused resource's failure has to carry, whatever sentence the owning service worded
/// its refusal with: the clause an SDK renders around the call the handler made, and the handler's
/// own classification of a priority that is already taken. The MESSAGE TEXT is deliberately not
/// asserted, for the reason the sibling suite
/// `elbv2_stack_update_drives_its_declared_estate` records — which spelling it carries is
/// disputed between two open elbv2 issues, whose own fix it is.
const REFUSING_SERVICE: &str = "Service: ElasticLoadBalancingV2";
const REFUSED_STATUS: &str = "Status Code: 400";
const TAKEN_PRIORITY_CLASS: &str = "HandlerErrorCode: AlreadyExists";

/// The template the stack is deployed and updated with: the network, the internet gateway and the
/// attachment that puts it on the VPC, an internet-facing Application Load Balancer, its HTTP
/// listener and the two rules that answer `/a` and `/b`.
fn template(first_priority: &str, first_body: &str) -> String {
    format!(
        "Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.7.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.7.1.0/24
      AvailabilityZone: us-east-1a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.7.2.0/24
      AvailabilityZone: us-east-1b
  Igw:
    Type: AWS::EC2::InternetGateway
  {ATTACHMENT}:
    Type: AWS::EC2::VPCGatewayAttachment
    Properties:
      VpcId: !Ref Vpc
      InternetGatewayId: !Ref Igw
  Sg:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group the load balancer is placed in
      VpcId: !Ref Vpc
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Type: application
      Scheme: internet-facing
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
      SecurityGroups:
        - !Ref Sg
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
      Priority: {first_priority}
      Conditions:
        - Field: path-pattern
          Values:
            - /a
      Actions:
        - Type: fixed-response
          FixedResponseConfig:
            StatusCode: \"200\"
            ContentType: text/plain
            MessageBody: {first_body}
  RuleB:
    Type: AWS::ElasticLoadBalancingV2::ListenerRule
    Properties:
      ListenerArn: !Ref Listener
      Priority: {HELD_PRIORITY}
      Conditions:
        - Field: path-pattern
          Values:
            - /b
      Actions:
        - Type: fixed-response
          FixedResponseConfig:
            StatusCode: \"200\"
            ContentType: text/plain
            MessageBody: b1
Outputs:
  AlbDns:
    Value: !GetAtt Alb.DNSName
  ListenerArn:
    Value: !Ref Listener
  RuleAArn:
    Value: !Ref {FIRST_RULE}
"
    )
}

/// Read the stack until it reports a status that is no longer in progress, and answer that read's
/// body.
fn settled(emulator: &Started, case: &str) -> String {
    let started = Instant::now();
    loop {
        let (code, body) = emulator.cloudformation("DescribeStacks", &[("StackName", STACK)]);
        assert_eq!(
            code, 200,
            "case {case:?}: expected 200 from DescribeStacks, got {code} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack operation never settled; last status {reported:?}, and the \
             reasons its events carried were {:?}; the emulator's log:\n{}",
            reasons(emulator),
            emulator.log()
        );
        std::thread::sleep(LOOK);
    }
}

/// Every (`LogicalResourceId`, `ResourceStatus`, `ResourceStatusReason`) the stack's event stream
/// carries, read INSIDE the event that carries them.
fn events(emulator: &Started) -> Vec<(String, String, String)> {
    let (_, body) = emulator.cloudformation("DescribeStackEvents", &[("StackName", STACK)]);
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

/// Every reason the stack's events carry, for the message of a wait that gave up.
fn reasons(emulator: &Started) -> Vec<String> {
    let (_, body) = emulator.cloudformation("DescribeStackEvents", &[("StackName", STACK)]);
    elements(&body, "ResourceStatusReason")
}

/// One exchange on the address the load balancer published: a connection in the clear, HTTP/1.1,
/// closed after the one request — what a client handed that address sends.
///
/// The blocking counterpart of the async exchange the web-ACL suite drives; the reading of the
/// published address itself is that suite's ([`dialable_port`]) rather than a second copy.
fn exchange(case: &str, published: &str, path: &str) -> String {
    let port = dialable_port(case, published);
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .unwrap_or_else(|error| panic!("case {case:?}: {published} refused a connection: {error}"));
    stream
        .set_read_timeout(Some(EXCHANGE_TIMEOUT))
        .expect("set the read timeout");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}

/// The BODY of one answer: what follows the blank line that ends its header block, and the whole
/// of it where an answer carried no such line at all.
fn body_of(answered: &str) -> &str {
    answered
        .split_once("\r\n\r\n")
        .or_else(|| answered.split_once("\n\n"))
        .map_or(answered, |(_, body)| body)
}

/// Assert the published address answers `path` with the status line `status` and `body` — looked
/// for again until it does, since a load balancer comes up over a span the clock owes and a
/// change takes effect at the next request.
///
/// The body is compared as the ANSWER's body ([`body_of`]) rather than as a tail of the whole
/// exchange, so a header the data plane adds later cannot make the comparison pass or fail for a
/// reason that is not the body.
fn assert_answers(case: &str, published: &str, path: &str, status: &str, body: &str) {
    let started = Instant::now();
    let answered = loop {
        let answered = exchange(case, published, path);
        let served =
            answered.starts_with(&format!("HTTP/1.1 {status}")) && body_of(&answered) == body;
        if served || started.elapsed() >= SETTLE_DEADLINE {
            break answered;
        }
        std::thread::sleep(LOOK);
    };
    assert!(
        answered.starts_with(&format!("HTTP/1.1 {status}")),
        "case {case:?}: expected {published}{path} to answer {status}, got {answered:?}"
    );
    assert_eq!(
        body_of(&answered),
        body,
        "case {case:?}: expected {published}{path} to answer {body:?}, got {answered:?}"
    );
}

/// Submit an update of the stack with the template `first_priority`/`first_body` states, and
/// answer the settled stack.
fn updated(emulator: &Started, case: &str, first_priority: &str, first_body: &str) -> String {
    let (code, body) = emulator.cloudformation(
        "UpdateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template(first_priority, first_body)),
        ],
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from UpdateStack, got {code} with {body}"
    );
    settled(emulator, case)
}

#[test]
fn a_deployed_elbv2_estate_is_driven_to_a_terminal_status_on_the_emulator_a_client_starts() {
    let emulator = Started::start();

    // The create: the stack settles, and the address it published answers each rule it declared.
    let case = "the create of the estate";
    let (code, body) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template("10", "a1")),
        ],
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from CreateStack, got {code} with {body}"
    );
    let described = settled(&emulator, case);
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the create must complete; its reasons were {:?}; the emulator's log:\n{}",
        reasons(&emulator),
        emulator.log()
    );
    let published = outputs_of(&described)
        .get("AlbDns")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no AlbDns, got {described}"));
    assert_answers(case, &published, "/a", "200", "a1");
    assert_answers(case, &published, "/b", "200", "b1");
    assert_answers(case, &published, "/", "404", "no rule matched");

    // An update the owning service refuses: the rule moved onto the priority its sibling holds
    // fails with that service's own refusal, the stack rolls back, and the configuration in force
    // is the one the refused deployment never replaced.
    let case = "the update that moves a rule onto a priority in use";
    let described = updated(&emulator, case, HELD_PRIORITY, "a2");
    let reported = events(&emulator);
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_ROLLBACK_COMPLETE),
        "case {case:?}: a refused change must roll the stack back; its events were {reported:?}"
    );
    assert!(
        reported.iter().any(|(logical_id, status, reason)| {
            logical_id == FIRST_RULE
                && status == UPDATE_FAILED
                && [REFUSING_SERVICE, REFUSED_STATUS, TAKEN_PRIORITY_CLASS]
                    .iter()
                    .all(|part| reason.contains(part))
        }),
        "case {case:?}: expected the rule's failure to carry the refusal Elastic Load Balancing \
         answered — {REFUSING_SERVICE}, {REFUSED_STATUS}, {TAKEN_PRIORITY_CLASS} — got \
         {reported:?}"
    );
    let others: Vec<&(String, String, String)> = reported
        .iter()
        .filter(|(logical_id, status, _)| status == UPDATE_FAILED && logical_id != FIRST_RULE)
        .collect();
    assert!(
        others.is_empty(),
        "case {case:?}: only the refused change may fail the update — a resource the deployment \
         did not change, the gateway attachment among them, is asked nothing — got {others:?}"
    );
    assert_answers(case, &published, "/a", "200", "a1");

    // ...and the same change on a FREE priority is applied, which the published address serves.
    let case = "the update that moves the same rule onto a free priority";
    let described = updated(&emulator, case, FREE_PRIORITY, "a2");
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_COMPLETE),
        "case {case:?}: the update must complete; its events were {:?}",
        events(&emulator)
    );
    assert_answers(case, &published, "/a", "200", "a2");

    // The rule kept its identity through all three operations, and Elastic Load Balancing holds
    // it at the priority the last one declared.
    let rule_arn = outputs_of(&described)
        .get("RuleAArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no RuleAArn"));
    let (code, rules) = emulator.form_call(
        "elasticloadbalancing",
        "2015-12-01",
        "DescribeRules",
        &[("RuleArns.member.1", &rule_arn)],
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from DescribeRules on {rule_arn}, got {code} with {rules}"
    );
    assert_eq!(
        opt_element(&rules, "Priority").as_deref(),
        Some(FREE_PRIORITY),
        "case {case:?}: expected the rule at the priority the update declared, got {rules}"
    );
}
