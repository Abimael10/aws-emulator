// SPDX-License-Identifier: CC0-1.0

//! The WHOLE life of an Elastic Load Balancing v2 estate a CloudFormation stack declares,
//! driven on the emulator a client STARTS and declared the way a non-Rust SDK client declares
//! it: a JSON template whose references are written in their long form (`{"Ref": …}`,
//! `{"Fn::GetAtt": [...]}`), over a public-subnet network of the stack's own.
//!
//! RULE: every operation over a declared Elastic Load Balancing v2 estate reaches a TERMINAL
//! stack status, and what the estate is once that status is reported — in the service that owns
//! it and at the address the load balancer published — is what the template that settled
//! declares: the create serves the rules it declared, a change the owning service REFUSES rolls
//! back and leaves the configuration it never replaced serving, a change that service accepts is
//! served, and the TEARDOWN releases the resources, so the owning service no longer holds them
//! and the address the stack published stops accepting connections altogether.
//!
//! A client gates its deployment on a terminal status and then uses the address the stack
//! published, so an operation that settles in neither direction is worse than one that fails: the
//! gate never returns, and nothing about the estate can be read back.
//!
//! The rows are the four operations themselves — create, refused update, accepted update,
//! delete — each with the status it settles in and the answer the published address then gives.
//! What this suite adds to the two that already drive this estate is the last row and the
//! declaration form: `zz_elbv2_stack_probe` drives the first three operations from a YAML
//! template with the short-form intrinsics, and `elbv2_stack_update_drives_its_declared_estate`
//! spans WHICH KIND of change a deployment states on a router assembled in process — neither
//! tears the estate down, and a teardown is observable only against a REAL socket, which is the
//! one thing it has to CLOSE. The sequence is driven in full rather than from a freshly created
//! estate because it is one state machine: the release has to follow a rollback and an applied
//! update, as the deployment a client runs does.
//!
//! The readings it shares rather than copies: the port of a published address
//! ([`dialable_port`], which holds that address to being one of this emulator's own) and the
//! framing of an answer that arrived on such a socket ([`parsed`]).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
// The ONE framing this test crate keeps of an HTTP/1.1 answer that arrived on a published
// address's own socket, and the ONE reading of whether such an address is one of this emulator's
// own — a `host:port` on the interface the core binds.
use crate::cloudformation_cloudfront_published_edge_address::{parsed, Served};
use crate::elbv2_published_address_enforces_its_web_acl::dialable_port;
use crate::query_wire::{elements, members_of, opt_element, outputs_of};

/// How long a step waits for the condition it needs — a terminal stack status, the answer the
/// published address gives, the moment that address stops accepting connections — and how often
/// it looks again. Every wait below waits for its CONDITION; the deadline is only the backstop
/// that turns something nothing will ever satisfy into a readable failure instead of a hang, so
/// it is generous.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(100);

/// How long one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The stack this suite deploys, and the statuses its four operations settle in.
const STACK: &str = "json-declared-elbv2-estate";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// The logical id of the rule each update restates. The template declares it under this very
/// name ([`template`]), so the events a refused update carries name it.
const FIRST_RULE: &str = "RuleA";

/// The priority the second rule holds throughout — so the one the first cannot be moved onto —
/// and the free priority the last update moves it to.
const HELD_PRIORITY: i64 = 20;
const FREE_PRIORITY: i64 = 15;

/// The priority the first rule is created at.
const FIRST_PRIORITY: i64 = 10;

/// The path both rules match, so the two really compete and the priority decides which answers,
/// and a path no rule is written for, which the listener's own default action answers.
const SHARED_PATH: &str = "/a";
const UNMATCHED_PATH: &str = "/elsewhere";

/// The bodies the rules render: the first rule's as created, as the updates ask for it, and the
/// second rule's, which answers whenever it is the rule the listener evaluates first.
const CREATED_BODY: &str = "a1";
const UPDATED_BODY: &str = "a2";
const SECOND_BODY: &str = "b1";

/// The body the listener's default action renders, and the status it renders it with.
const DEFAULT_BODY: &str = "no rule matched";
const DEFAULT_STATUS: u16 = 404;

/// What the refused resource's failure has to carry, whatever sentence the owning service worded
/// its refusal with: the clause an SDK renders around the call the handler made — the service
/// that answered and the HTTP status it answered with — and the handler's own classification of a
/// priority that is already taken. The MESSAGE TEXT is deliberately not asserted here: which
/// sentence Elastic Load Balancing words a priority collision with is pinned by that service's
/// own suites, over its own wire.
const REFUSING_SERVICE: &str = "Service: ElasticLoadBalancingV2";
const REFUSED_STATUS: &str = "Status Code: 400";
const TAKEN_PRIORITY_CLASS: &str = "HandlerErrorCode: AlreadyExists";

/// The template, as an SDK client sends it: JSON, every reference in its long form, a VPC with an
/// internet gateway, two public subnets with a route table of their own, the security group the
/// load balancer is placed in, an internet-facing Application Load Balancer with no name of its
/// own, its HTTP listener and the two rules that compete for one path.
fn template(first_priority: i64, first_body: &str) -> String {
    let rule = |priority: i64, body: &str| {
        json!({
            "Type": "AWS::ElasticLoadBalancingV2::ListenerRule",
            "Properties": {
                "ListenerArn": {"Ref": "Listener"},
                "Priority": priority,
                "Conditions": [{"Field": "path-pattern", "Values": [SHARED_PATH]}],
                "Actions": [{
                    "Type": "fixed-response",
                    "FixedResponseConfig": {
                        "StatusCode": "200",
                        "ContentType": "text/plain",
                        "MessageBody": body,
                    },
                }],
            },
        })
    };
    let subnet = |cidr: &str, zone: usize| {
        json!({
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": cidr,
                "MapPublicIpOnLaunch": true,
                "AvailabilityZone": {"Fn::Select": [zone, {"Fn::GetAZs": ""}]},
            },
        })
    };
    let association = |subnet: &str| {
        json!({
            "Type": "AWS::EC2::SubnetRouteTableAssociation",
            "Properties": {
                "SubnetId": {"Ref": subnet},
                "RouteTableId": {"Ref": "RouteTable"},
            },
        })
    };
    let mut declared: Value = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an elbv2 estate declared in JSON",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {
                    "CidrBlock": "10.11.0.0/16",
                    "EnableDnsSupport": true,
                    "EnableDnsHostnames": true,
                },
            },
            "Igw": {"Type": "AWS::EC2::InternetGateway"},
            "IgwAttach": {
                "Type": "AWS::EC2::VPCGatewayAttachment",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "InternetGatewayId": {"Ref": "Igw"},
                },
            },
            "SubnetA": subnet("10.11.1.0/24", 0),
            "SubnetB": subnet("10.11.2.0/24", 1),
            "RouteTable": {
                "Type": "AWS::EC2::RouteTable",
                "Properties": {"VpcId": {"Ref": "Vpc"}},
            },
            "PublicRoute": {
                "Type": "AWS::EC2::Route",
                "DependsOn": "IgwAttach",
                "Properties": {
                    "RouteTableId": {"Ref": "RouteTable"},
                    "DestinationCidrBlock": "0.0.0.0/0",
                    "GatewayId": {"Ref": "Igw"},
                },
            },
            "AssocA": association("SubnetA"),
            "AssocB": association("SubnetB"),
            "Sg": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the group the load balancer is placed in",
                    "VpcId": {"Ref": "Vpc"},
                    "SecurityGroupIngress": [{
                        "IpProtocol": "tcp",
                        "FromPort": 80,
                        "ToPort": 80,
                        "CidrIp": "0.0.0.0/0",
                    }],
                },
            },
            "Alb": {
                "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
                "Properties": {
                    "Type": "application",
                    "Scheme": "internet-facing",
                    "Subnets": [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}],
                    "SecurityGroups": [{"Ref": "Sg"}],
                },
            },
            "Listener": {
                "Type": "AWS::ElasticLoadBalancingV2::Listener",
                "Properties": {
                    "LoadBalancerArn": {"Ref": "Alb"},
                    "Port": 80,
                    "Protocol": "HTTP",
                    "DefaultActions": [{
                        "Type": "fixed-response",
                        "FixedResponseConfig": {
                            "StatusCode": DEFAULT_STATUS.to_string(),
                            "ContentType": "text/plain",
                            "MessageBody": DEFAULT_BODY,
                        },
                    }],
                },
            },
            "RuleB": rule(HELD_PRIORITY, SECOND_BODY),
        },
        "Outputs": {
            "AlbDns": {"Value": {"Fn::GetAtt": ["Alb", "DNSName"]}},
            "AlbArn": {"Value": {"Ref": "Alb"}},
            "ListenerArn": {"Value": {"Ref": "Listener"}},
        },
    });
    // The rule each update restates, declared under the very name the assertions below read its
    // events by, so the two cannot drift apart.
    declared["Resources"][FIRST_RULE] = rule(first_priority, first_body);
    declared["Outputs"]["RuleAArn"] = json!({"Value": {"Ref": FIRST_RULE}});
    declared.to_string()
}

/// Read `stack` until it reports a status that is no longer in progress, and answer that read's
/// body. `stack` is the stack's ID for a read that has to survive the stack's deletion: a deleted
/// stack is no longer addressable by name.
fn settled(emulator: &Started, case: &str, stack: &str) -> String {
    let started = Instant::now();
    loop {
        let (code, body) = emulator.cloudformation("DescribeStacks", &[("StackName", stack)]);
        assert_eq!(
            code, 200,
            "case {case:?}: expected 200 from DescribeStacks({stack}), got {code} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack operation never settled; last status {reported:?}, the \
             reasons its events carried were {:?}, and the emulator's log:\n{}",
            reasons(emulator, stack),
            emulator.log()
        );
        std::thread::sleep(LOOK);
    }
}

/// Every reason the stack's events carry, for the message of an assertion that failed.
fn reasons(emulator: &Started, stack: &str) -> Vec<String> {
    let (_, body) = emulator.cloudformation("DescribeStackEvents", &[("StackName", stack)]);
    elements(&body, "ResourceStatusReason")
}

/// Every (`LogicalResourceId`, `ResourceStatus`, `ResourceStatusReason`) the stack's event stream
/// carries, read INSIDE the event that carries them, so values belonging to two different events
/// cannot be paired.
fn events(emulator: &Started, stack: &str) -> Vec<(String, String, String)> {
    let (_, body) = emulator.cloudformation("DescribeStackEvents", &[("StackName", stack)]);
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

/// Why an exchange on the published address produced no answer.
#[derive(Debug)]
enum Unreachable {
    /// The DIAL was refused: nothing accepts a connection at that address any more, which is the
    /// condition the teardown leg waits for.
    Dial(String),
    /// The connection was made, but the exchange on it did not complete.
    Exchange(String),
}

/// One exchange on the address the load balancer published: a connection in the clear, one
/// HTTP/1.1 `GET` closed after its answer, which is what a client handed that address sends.
///
/// The port is read off the published address by [`dialable_port`], which is also the assertion
/// that the address is one of this emulator's own, and the connection is made to that port on the
/// loopback interface the core binds. The answer is framed by [`parsed`].
fn exchange(case: &str, published: &str, path: &str) -> Result<Served, Unreachable> {
    let port = dialable_port(case, published);
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .map_err(|error| Unreachable::Dial(error.to_string()))?;
    stream
        .set_read_timeout(Some(EXCHANGE_TIMEOUT))
        .map_err(|error| Unreachable::Exchange(error.to_string()))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| Unreachable::Exchange(error.to_string()))?;
    let mut answered = Vec::new();
    stream
        .read_to_end(&mut answered)
        .map_err(|error| Unreachable::Exchange(error.to_string()))?;
    if answered.is_empty() {
        return Err(Unreachable::Exchange("the answer was empty".to_string()));
    }
    Ok(parsed(case, &String::from_utf8_lossy(&answered)))
}

/// The `(status, body)` of one exchange, or the reason there was none — what an assertion
/// compares and reports.
fn answered(case: &str, published: &str, path: &str) -> Result<(u16, String), String> {
    exchange(case, published, path)
        .map(|served| (served.status, served.body))
        .map_err(|unreachable| match unreachable {
            Unreachable::Dial(reason) => format!("the dial was refused: {reason}"),
            Unreachable::Exchange(reason) => format!("the exchange did not complete: {reason}"),
        })
}

/// Assert the published address answers `path` with `expected` STRAIGHT AWAY — on the first
/// exchange, with no second look.
///
/// This is what a client that gates on the stack's status is owed: the status it read is terminal,
/// so the estate the template declares is the estate in force, and the address the stack
/// published answers out of it. A load balancer comes up over a span the clock owes it, but the
/// deployment is held open across that span, so by the time the status is reported there is
/// nothing left to wait for — a client that had to poll the data plane after a terminal status
/// could not tell a deployment that finished from one that silently did not.
fn assert_answers_at_once(case: &str, published: &str, path: &str, expected: (u16, &str)) {
    let got = answered(case, published, path);
    assert_eq!(
        got.as_ref().map(|(status, body)| (*status, body.as_str())),
        Ok(expected),
        "case {case:?}: expected {published}{path} to answer {expected:?} on the first exchange \
         after the stack settled, got {got:?}"
    );
}

/// Assert the published address answers `path` with `expected` — looked for again until it does,
/// since a change takes effect at the next request the listener serves.
fn assert_answers(case: &str, published: &str, path: &str, expected: (u16, &str)) {
    let started = Instant::now();
    let mut got = answered(case, published, path);
    loop {
        if got
            .as_ref()
            .is_ok_and(|(status, body)| (*status, body.as_str()) == expected)
        {
            return;
        }
        if started.elapsed() >= SETTLE_DEADLINE {
            break;
        }
        std::thread::sleep(LOOK);
        got = answered(case, published, path);
    }
    panic!("case {case:?}: expected {published}{path} to answer {expected:?}, got {got:?}");
}

/// Submit an update of the stack with the template `first_priority`/`first_body` states, and
/// answer the settled stack.
fn updated(emulator: &Started, case: &str, first_priority: i64, first_body: &str) -> String {
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
    settled(emulator, case, STACK)
}

/// One Elastic Load Balancing read over that service's own wire.
fn elbv2(emulator: &Started, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    emulator.form_call("elasticloadbalancing", "2015-12-01", action, members)
}

/// The value of one output the stack published.
fn output(case: &str, described: &str, key: &str) -> String {
    outputs_of(described)
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no {key}, got {described}"))
}

#[test]
fn a_json_declared_elbv2_estate_is_created_changed_and_torn_down_to_a_terminal_status() {
    let emulator = Started::start();

    // The create: the stack settles, and the address it published answers each rule it declared —
    // the lower priority of the two rules that match the shared path, and the listener's own
    // default action for a path no rule is written for.
    let case = "the create of the estate";
    let (code, body) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template(FIRST_PRIORITY, CREATED_BODY)),
        ],
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from CreateStack, got {code} with {body}"
    );
    let stack_id = opt_element(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateStack published no StackId: {body}"));
    let described = settled(&emulator, case, STACK);
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the create must complete; its reasons were {:?}, and the emulator's \
         log:\n{}",
        reasons(&emulator, STACK),
        emulator.log()
    );
    let published = output(case, &described, "AlbDns");
    let load_balancer_arn = output(case, &described, "AlbArn");
    let listener_arn = output(case, &described, "ListenerArn");
    assert_answers_at_once(case, &published, SHARED_PATH, (200, CREATED_BODY));
    assert_answers_at_once(
        case,
        &published,
        UNMATCHED_PATH,
        (DEFAULT_STATUS, DEFAULT_BODY),
    );

    // An update the owning service refuses: the rule moved onto the priority its sibling holds
    // fails with that service's own refusal, the stack rolls back, and the configuration in force
    // is the one the refused deployment never replaced.
    let case = "the update that moves a rule onto a priority in use";
    let described = updated(&emulator, case, HELD_PRIORITY, UPDATED_BODY);
    let reported = events(&emulator, STACK);
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
         answered — {REFUSING_SERVICE}, {REFUSED_STATUS}, {TAKEN_PRIORITY_CLASS} — got {reported:?}"
    );
    assert_answers(case, &published, SHARED_PATH, (200, CREATED_BODY));

    // ...and the same change on a FREE priority is applied, which the published address serves and
    // the owning service holds.
    let case = "the update that moves the same rule onto a free priority";
    let described = updated(&emulator, case, FREE_PRIORITY, UPDATED_BODY);
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(UPDATE_COMPLETE),
        "case {case:?}: the update must complete; its events were {:?}",
        events(&emulator, STACK)
    );
    assert_answers(case, &published, SHARED_PATH, (200, UPDATED_BODY));
    let (code, rules) = elbv2(&emulator, "DescribeRules", &[("ListenerArn", &listener_arn)]);
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from DescribeRules on {listener_arn}, got {code} with {rules}"
    );
    assert!(
        rules.contains(&format!("<Priority>{FREE_PRIORITY}</Priority>")),
        "case {case:?}: expected the rule at the priority the update declared, got {rules}"
    );

    // The teardown: the stack settles, the owning service no longer holds the load balancer, and
    // the address it published accepts no connection — the socket a client was handed is released
    // with the resource that published it.
    let case = "the teardown of the estate";
    let (code, body) = emulator.cloudformation("DeleteStack", &[("StackName", STACK)]);
    assert_eq!(
        code, 200,
        "case {case:?}: expected 200 from DeleteStack, got {code} with {body}"
    );
    let described = settled(&emulator, case, &stack_id);
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(DELETE_COMPLETE),
        "case {case:?}: the teardown must complete; its events were {:?}",
        events(&emulator, &stack_id)
    );
    let (code, balancers) = elbv2(
        &emulator,
        "DescribeLoadBalancers",
        &[("LoadBalancerArns.member.1", &load_balancer_arn)],
    );
    assert_eq!(
        code, 400,
        "case {case:?}: expected the torn-down load balancer to be gone from Elastic Load \
         Balancing, got {code} with {balancers}"
    );
    assert!(
        balancers.contains("LoadBalancerNotFound"),
        "case {case:?}: expected a LoadBalancerNotFound for {load_balancer_arn}, got {balancers}"
    );
    let started = Instant::now();
    let mut last = exchange(case, &published, SHARED_PATH);
    while !matches!(last, Err(Unreachable::Dial(_))) && started.elapsed() < SETTLE_DEADLINE {
        std::thread::sleep(LOOK);
        last = exchange(case, &published, SHARED_PATH);
    }
    assert!(
        matches!(last, Err(Unreachable::Dial(_))),
        "case {case:?}: expected the dial of {published} to be REFUSED once the stack that \
         published it was torn down, got {:?}",
        last.map(|served| (served.status, served.body))
    );
}
