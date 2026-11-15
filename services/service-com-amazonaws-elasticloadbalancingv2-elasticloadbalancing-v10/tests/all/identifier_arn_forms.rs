// SPDX-License-Identifier: CC0-1.0

//! What the ELBv2 operations answer an identifier that is no ARN of the resource their
//! member takes — over the wire, through the router.
//!
//! The rule these tables pin: a member documented as the ARN of a load balancer or of a
//! target group is judged on the value ALONE, before anything is looked up. A value that
//! is not an ARN this API could have minted for the Region the request addresses — not an
//! ARN at all, an ARN of another vendor, one of another Region, one naming another
//! resource type, or one whose resource field is not the form this API mints — names no
//! resource that could ever exist, so it is refused as a bad REQUEST: HTTP 400
//! `ValidationError` (the code this service answers a value it cannot accept with; its
//! model declares no error shape for the rule) quoting the offending value. Only an ARN
//! that passes reaches the store, where an identifier nothing holds is the operation's own
//! modelled `LoadBalancerNotFound` / `TargetGroupNotFound` — which is the second table
//! here, and the reason the first one matters: a client cannot tell a wrong argument from
//! a deleted resource if both answer the same code.
//!
//! The refusal's WORDING belongs to the operation: most of them name the kind of ARN the
//! member takes (`'<value>' is not a valid load balancer ARN`), while the two attribute
//! reads state the form the value failed to take (`'<value>' must be in ARN format`). Both
//! are recorded behaviour.
//!
//! Every row of the first table differs along what the rule generalises over: which part
//! of the ARN is wrong (none of it, the vendor, the Region, the resource type, the type
//! code, the name, the id, the field count) — and every row is applied to every operation
//! that takes such a member, because the rule is about the member and not about the
//! operation. The wire harness (`emulator`, `call`, `action`, `p`) is
//! `all/provisioning_loop.rs`'s.

use crate::provisioning_loop::{
    account, action, call, create_http_target_group, emulator, p, Reply, REGION,
};
use crate::target_group_attributes_and_modification::unescaped;
use smithy_server_runtime::MultiServiceRouter;

/// The awsQuery error code this service answers a value it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The `awsQueryError` code of the modelled `LoadBalancerNotFoundException`.
const LOAD_BALANCER_NOT_FOUND: &str = "LoadBalancerNotFound";

/// The `awsQueryError` code of the modelled `TargetGroupNotFoundException`.
const TARGET_GROUP_NOT_FOUND: &str = "TargetGroupNotFound";

/// How one operation words its refusal of an identifier that is no ARN of the resource its
/// member takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wording {
    /// Names the kind of ARN the member takes — the reading every member takes unless its
    /// own refusal is recorded otherwise.
    Kind(&'static str),
    /// States the form the value failed to take, which is what the two attribute reads
    /// answer.
    Format,
}

impl Wording {
    /// The whole message `value` earns under this wording.
    fn message(self, value: &str) -> String {
        match self {
            Self::Kind(noun) => format!("'{value}' is not a valid {noun} ARN"),
            Self::Format => format!("'{value}' must be in ARN format"),
        }
    }
}

/// One operation under test: the action, the form field its identifier travels in, the
/// other fields it needs to reach its own lookup, how it words an identifier refusal, and
/// the awsQuery code it answers for a well-formed ARN this store holds nothing for
/// (`None` where the operation is idempotent and serves such a call).
#[derive(Clone)]
struct Op {
    action: &'static str,
    member: &'static str,
    fields: Vec<(String, String)>,
    wording: Wording,
    absent: Option<&'static str>,
}

/// Every operation that takes a LOAD BALANCER's ARN, with the members each needs beside
/// it.
fn load_balancer_operations(target_group: &str) -> Vec<Op> {
    let kind = Wording::Kind("load balancer");
    vec![
        Op {
            action: "DescribeLoadBalancers",
            member: "LoadBalancerArns.member.1",
            fields: vec![],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "DescribeLoadBalancerAttributes",
            member: "LoadBalancerArn",
            fields: vec![],
            // Recorded: this read states the FORM rather than the kind.
            wording: Wording::Format,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "ModifyLoadBalancerAttributes",
            member: "LoadBalancerArn",
            fields: vec![
                p("Attributes.member.1.Key", "deletion_protection.enabled"),
                p("Attributes.member.1.Value", "true"),
            ],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "DescribeTargetGroups",
            member: "LoadBalancerArn",
            fields: vec![],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "DescribeListeners",
            member: "LoadBalancerArn",
            fields: vec![],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "SetIpAddressType",
            member: "LoadBalancerArn",
            fields: vec![p("IpAddressType", "dualstack")],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "SetSecurityGroups",
            member: "LoadBalancerArn",
            fields: vec![p("SecurityGroups.member.1", "sg-11111111")],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "SetSubnets",
            member: "LoadBalancerArn",
            fields: vec![
                p("Subnets.member.1", "subnet-11111111"),
                p("Subnets.member.2", "subnet-22222222"),
            ],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "ModifyIpPools",
            member: "LoadBalancerArn",
            fields: vec![p("IpamPools.Ipv4IpamPoolId", "ipam-pool-0123456789abcdef")],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "DescribeCapacityReservation",
            member: "LoadBalancerArn",
            fields: vec![],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "ModifyCapacityReservation",
            member: "LoadBalancerArn",
            fields: vec![p("ResetCapacityReservation", "true")],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "CreateListener",
            member: "LoadBalancerArn",
            fields: vec![
                p("Protocol", "HTTP"),
                p("Port", "80"),
                p("DefaultActions.member.1.Type", "forward"),
                p("DefaultActions.member.1.TargetGroupArn", target_group),
            ],
            wording: kind,
            absent: Some(LOAD_BALANCER_NOT_FOUND),
        },
        Op {
            action: "DeleteLoadBalancer",
            member: "LoadBalancerArn",
            fields: vec![],
            // A delete of a load balancer this store does not hold is served: deleting
            // what is already gone is the state the caller asked for.
            wording: kind,
            absent: None,
        },
    ]
}

/// Every operation that takes a TARGET GROUP's ARN, with the members each needs beside it.
fn target_group_operations() -> Vec<Op> {
    let kind = Wording::Kind("target group");
    vec![
        Op {
            action: "DescribeTargetGroups",
            member: "TargetGroupArns.member.1",
            fields: vec![],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "DescribeTargetGroupAttributes",
            member: "TargetGroupArn",
            fields: vec![],
            // Recorded: this read states the FORM rather than the kind.
            wording: Wording::Format,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "ModifyTargetGroupAttributes",
            member: "TargetGroupArn",
            fields: vec![
                p("Attributes.member.1.Key", "deregistration_delay.timeout_seconds"),
                p("Attributes.member.1.Value", "120"),
            ],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "ModifyTargetGroup",
            member: "TargetGroupArn",
            fields: vec![p("HealthCheckEnabled", "true")],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "DescribeTargetHealth",
            member: "TargetGroupArn",
            fields: vec![],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "RegisterTargets",
            member: "TargetGroupArn",
            fields: vec![p("Targets.member.1.Id", "i-0123456789abcdef0")],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "DeregisterTargets",
            member: "TargetGroupArn",
            fields: vec![p("Targets.member.1.Id", "i-0123456789abcdef0")],
            wording: kind,
            absent: Some(TARGET_GROUP_NOT_FOUND),
        },
        Op {
            action: "DeleteTargetGroup",
            member: "TargetGroupArn",
            fields: vec![],
            // Idempotent, for the reason `DeleteLoadBalancer` is.
            wording: kind,
            absent: None,
        },
    ]
}

/// A load balancer ARN of this service, assembled from its parts so a row can vary any
/// one of them.
fn load_balancer_arn(region: &str, type_code: &str, name: &str, id: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{region}:{}:loadbalancer/{type_code}/{name}/{id}",
        account()
    )
}

/// A target group ARN of this service, on the same footing.
fn target_group_arn(region: &str, name: &str, id: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{region}:{}:targetgroup/{name}/{id}",
        account()
    )
}

/// The identifier forms NO load balancer of this API can be named by, one row per part of
/// the ARN that can be wrong.
fn malformed_load_balancer_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("a bare load balancer name", "probe-lb".to_string()),
        (
            "the published DNS name rather than the ARN",
            format!("probe-lb-0123456789abcdef.{REGION}.elb.amazonaws.com"),
        ),
        (
            "a value with too few ARN fields",
            "arn:aws:elasticloadbalancing".to_string(),
        ),
        (
            "an ARN whose resource field is empty",
            format!("arn:aws:elasticloadbalancing:{REGION}:{}:", account()),
        ),
        (
            "an ARN of another vendor",
            format!("arn:aws:ec2:{REGION}:{}:instance/i-0123456789abcdef0", account()),
        ),
        (
            "a well-formed load balancer ARN of another Region",
            load_balancer_arn("eu-west-1", "app", "elsewhere", "0123456789abcdef"),
        ),
        (
            "a target group ARN where a load balancer belongs",
            target_group_arn(REGION, "cross-type", "0123456789abcdef"),
        ),
        (
            "an ARN with a load balancer type code this service never mints",
            load_balancer_arn(REGION, "web", "probe-lb", "0123456789abcdef"),
        ),
        (
            "an ARN whose id is not a resource id of this service",
            load_balancer_arn(REGION, "app", "probe-lb", "1"),
        ),
        (
            "an ARN whose name no load balancer could carry",
            load_balancer_arn(REGION, "app", "internal-probe", "0123456789abcdef"),
        ),
    ]
}

/// The same, for a target group.
fn malformed_target_group_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("a bare target group name", "probe-tg".to_string()),
        (
            "a value with too few ARN fields",
            "arn:aws:elasticloadbalancing".to_string(),
        ),
        (
            "an ARN whose resource field is empty",
            format!("arn:aws:elasticloadbalancing:{REGION}:{}:", account()),
        ),
        (
            "an ARN of another vendor",
            format!("arn:aws:ec2:{REGION}:{}:instance/i-0123456789abcdef0", account()),
        ),
        (
            "a well-formed target group ARN of another Region",
            target_group_arn("eu-west-1", "elsewhere", "0123456789abcdef"),
        ),
        (
            "a load balancer ARN where a target group belongs",
            load_balancer_arn(REGION, "app", "cross-type", "0123456789abcdef"),
        ),
        (
            "an ARN whose id is not a resource id of this service",
            target_group_arn(REGION, "probe-tg", "1"),
        ),
        (
            "an ARN carrying a type code a target group ARN has none of",
            format!(
                "arn:aws:elasticloadbalancing:{REGION}:{}:targetgroup/app/probe-tg/0123456789abcdef",
                account()
            ),
        ),
    ]
}

/// Send `op` with `identifier` in its own member.
async fn call_with(router: &MultiServiceRouter, op: &Op, identifier: &str) -> Reply {
    let mut form = action(op.action);
    form.push(p(op.member, identifier));
    for field in &op.fields {
        form.push(field.clone());
    }
    call(router, &form).await
}

#[tokio::test]
async fn an_identifier_that_is_no_resource_arn_is_refused_on_the_request() {
    let (router, _engine) = emulator();
    // A real target group, so `CreateListener` reaches its own load balancer lookup
    // instead of being refused for the action it forwards to.
    let target_group = create_http_target_group(&router, "arn-forms-tg").await;

    let mut cases: Vec<(String, Op, String)> = Vec::new();
    for op in load_balancer_operations(&target_group) {
        for (form_case, identifier) in malformed_load_balancer_identifiers() {
            cases.push((format!("{} given {form_case}", op.action), op.clone(), identifier));
        }
    }
    for op in target_group_operations() {
        for (form_case, identifier) in malformed_target_group_identifiers() {
            cases.push((format!("{} given {form_case}", op.action), op.clone(), identifier));
        }
    }

    for (case, op, identifier) in &cases {
        let reply = call_with(&router, op, identifier).await;
        let expected = op.wording.message(identifier);
        let stated = reply.text("Message").map(|text| unescaped(&text));
        assert_eq!(
            (reply.status, reply.error_code(), stated.clone()),
            (400, VALIDATION_ERROR.to_string(), Some(expected.clone())),
            "case {case:?}: expected 400 {VALIDATION_ERROR} {expected:?}, got {} body {}",
            reply.status,
            reply.body
        );
    }
}

#[tokio::test]
async fn a_well_formed_arn_this_store_holds_nothing_for_is_the_modelled_not_found() {
    let (router, _engine) = emulator();
    let target_group = create_http_target_group(&router, "arn-forms-absent-tg").await;

    // The same operations, against an identifier that is well formed in every way the
    // table above varies — this service's vendor, the Region the request addresses, the
    // resource form this API mints — and that nothing holds. That is what separates the
    // two classes: the refusal here is about the STORE, not about the request.
    let absent_load_balancer = load_balancer_arn(REGION, "app", "absent-lb", "0123456789abcdef");
    let absent_target_group = target_group_arn(REGION, "absent-tg", "0123456789abcdef");

    let mut cases: Vec<(String, Op, String)> = Vec::new();
    for op in load_balancer_operations(&target_group) {
        cases.push((
            format!("{} given an absent load balancer", op.action),
            op.clone(),
            absent_load_balancer.clone(),
        ));
    }
    for op in target_group_operations() {
        cases.push((
            format!("{} given an absent target group", op.action),
            op.clone(),
            absent_target_group.clone(),
        ));
    }

    for (case, op, identifier) in &cases {
        let reply = call_with(&router, op, identifier).await;
        match op.absent {
            Some(code) => assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, code),
                "case {case:?}: expected 400 {code}, got {} body {}",
                reply.status,
                reply.body
            ),
            None => assert_eq!(
                reply.status, 200,
                "case {case:?}: an idempotent delete of what is already gone is served, got {} \
                 body {}",
                reply.status, reply.body
            ),
        }
    }
}

#[tokio::test]
async fn the_identifier_check_serves_the_resources_this_store_holds() {
    let (router, _engine) = emulator();

    // The control the two tables above need: an ARN this API minted is served by the
    // reads that take one, so the check refuses only what no ARN of this service is.
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", "arn-forms-lb"));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let created = call(&router, &form).await;
    assert_eq!(
        created.status, 200,
        "setup: CreateLoadBalancer answered {} body {}",
        created.status, created.body
    );
    let load_balancer = created
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("setup: no LoadBalancerArn in {}", created.body));
    let target_group = create_http_target_group(&router, "arn-forms-served-tg").await;

    // (case, action, member, identifier)
    let served: Vec<(&str, &str, &str, &String)> = vec![
        (
            "DescribeLoadBalancers by the minted ARN",
            "DescribeLoadBalancers",
            "LoadBalancerArns.member.1",
            &load_balancer,
        ),
        (
            "DescribeLoadBalancerAttributes by the minted ARN",
            "DescribeLoadBalancerAttributes",
            "LoadBalancerArn",
            &load_balancer,
        ),
        (
            "DescribeTargetGroups filtered by the minted load balancer ARN",
            "DescribeTargetGroups",
            "LoadBalancerArn",
            &load_balancer,
        ),
        (
            "DescribeListeners filtered by the minted load balancer ARN",
            "DescribeListeners",
            "LoadBalancerArn",
            &load_balancer,
        ),
        (
            "DescribeTargetGroups by the minted target group ARN",
            "DescribeTargetGroups",
            "TargetGroupArns.member.1",
            &target_group,
        ),
        (
            "DescribeTargetGroupAttributes by the minted ARN",
            "DescribeTargetGroupAttributes",
            "TargetGroupArn",
            &target_group,
        ),
        (
            "DescribeTargetHealth by the minted ARN",
            "DescribeTargetHealth",
            "TargetGroupArn",
            &target_group,
        ),
    ];

    for (case, operation, member, identifier) in &served {
        let mut form = action(operation);
        form.push(p(member, identifier));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200, got {} body {}",
            reply.status, reply.body
        );
    }
}
