// SPDX-License-Identifier: CC0-1.0

//! Two rules about one target group, over the whole mounted service set: WHEN a registration is
//! refused, and when the registration that was made is reported as `unused`.
//!
//! 1. **The authorization decision is taken from the REQUEST alone, ahead of every modelled
//!    check.** A call addressed to one target group is judged by IAM against the action
//!    `elasticloadbalancing:<OperationName>` on the ARN the request carried, before the store is
//!    read and before any value in the request is validated — so a principal an identity-based
//!    policy explicitly denies gets exactly ONE answer, HTTP 403 with the `Sender` fault, the
//!    error code `AccessDenied` and IAM's own sentence naming the principal, the action and that
//!    ARN, whatever else about the request would otherwise have been refused, and the target
//!    group is left exactly as it was. Each row asserts both halves: the refusal the denied
//!    session earns, and the DIFFERENT answer the very same request gets from a caller the
//!    policies permit — which is what shows the 403 came first rather than by accident.
//!
//!    The rows span what the rule generalises over: what else about the request is wrong (nothing
//!    at all, a well-formed target group ARN the account holds nothing for, a value that is no
//!    target group ARN, an ARN of another Region, a target id of no form an `ip` group's targets
//!    take), crossed with which of the two registration calls is being decided, since the action
//!    IAM is asked about is each operation's own name. What the role's document SAYS is spanned by
//!    the sibling suite `elbv2_target_group_write_authorization.rs`, whose request plumbing this
//!    suite reuses rather than restating.
//!
//! 2. **`unused` names only a target that is not registered, or a group no load balancer
//!    forwards to.** `TargetHealth$Reason` gives `unused` exactly the conditions
//!    `Target.NotRegistered` and `Target.NotInUse` ("the target group is not used by any load
//!    balancer"), so a registration of a group that an action of a listener on an active load
//!    balancer forwards to is never reported `unused`: it is in the health-check lifecycle, which
//!    reports `initial` while the checks that decide it are still owed and then the verdict they
//!    reached (`healthy` or `unhealthy`). The association is DERIVED from the listeners that
//!    exist, not latched: a load balancer that forwards nowhere leaves the group unused, a second
//!    group the listener does not forward to stays unused throughout, and deleting the listener
//!    returns the forwarded-to group's registration to `unused`/`Target.NotInUse`.
//!
//! The whole network — the VPC, the two zoned subnets the load balancer is placed in, and the
//! subnet the registered address comes from — is created through Amazon EC2 itself, so the
//! registration resolves against what that service holds rather than against a value this suite
//! made up. Every clock-owed move (the load balancer to `active`, the registration past its
//! checks) is POLLED for until it holds; nothing here waits a fixed time or asserts on elapsed
//! time.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

// The ONE request plumbing this test crate keeps for the Elastic Load Balancing v2
// authorization suites, and the ONE set of element readers for awsQuery answers
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::elbv2_target_group_write_authorization::{
    as_account, elbv2, error_code, form, refusal, router, send, session_of, ACCOUNT_KEY,
    ACTION_PREFIX, REGION,
};
use crate::query_wire::{block_of, element, opt_element};

/// The EC2 API version a client sends in an ec2Query body.
const EC2_VERSION: &str = "2016-11-15";

/// The awsQuery error code of `TargetGroupNotFoundException`, which both registration calls
/// model for a well-formed ARN the account holds no target group for.
const TARGET_GROUP_NOT_FOUND: &str = "TargetGroupNotFound";

/// The error code this service answers a value its own documented rules refuse with.
///
/// It is the refusal for a value that names no resource and no target AT ALL — an ARN of a form
/// this API never mints, an id written the way no target of the group's type is written — which
/// the model declares no error shape for, so the code is this service's own. The modelled
/// `InvalidTargetException` ("the specified target does not exist, is not in the same VPC as the
/// target group, or has an unsupported instance type") is the OTHER class: a target that exists
/// and cannot be registered. Both readings live in one place in the service crate
/// (`state::identifier_syntax`, `state::network`) and are tabled by its own suites
/// (`tests/all/input_rejection_classes.rs`, `tests/all/ip_targets.rs`); here they are only what
/// the permitted caller's answer must be, so that the refusal asserted above is visibly the
/// authorization one and not that.
const VALIDATION_ERROR: &str = "ValidationError";

/// The error code and the fault an unauthorized caller is refused under.
const ACCESS_DENIED: &str = "AccessDenied";
const SENDER: &str = "Sender";

/// The clause IAM closes a refusal with when a document in force explicitly denies the action.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// A document granting every action of this service and explicitly denying the two calls that
/// register and deregister a target — so each row's refusal is the explicit deny rather than a
/// grant that was never made.
const DENIES_THE_REGISTRATION_CALLS: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Action":"elasticloadbalancing:*","Resource":"*"},"#,
    r#"{"Effect":"Deny","Action":"elasticloadbalancing:RegisterTargets","Resource":"*"},"#,
    r#"{"Effect":"Deny","Action":"elasticloadbalancing:DeregisterTargets","Resource":"*"}]}"#
);

/// The two `TargetHealthStateEnum` spellings and the two `TargetHealthReasonEnum` reasons this
/// suite reads: `unused` is reported with `Target.NotRegistered` for a target the group holds no
/// registration for and with `Target.NotInUse` for a group no load balancer forwards to.
const UNUSED: &str = "unused";
const NOT_REGISTERED: &str = "Target.NotRegistered";
const NOT_IN_USE: &str = "Target.NotInUse";

/// The states reachable for a registration of a group a load balancer forwards to: the
/// `initial` of its outstanding checks and the two verdicts they reach. `unused` is NOT one of
/// them, which is the rule the second test asserts.
const REACHABLE_WHEN_FORWARDED: [&str; 3] = ["initial", "healthy", "unhealthy"];

/// The two reasons the model documents for `initial`: "target registration is in progress" for
/// a registration no check has observed yet, and the minimum number of checks still being sent
/// once they have started.
const INITIAL_REASONS: [&str; 2] = ["Elb.RegistrationInProgress", "Elb.InitialHealthChecking"];

/// How long a poll keeps looking for the condition it waits on, and how often it looks.
///
/// Generous on purpose: the deadline is only here so that a failure is a failure rather than a
/// hang, and the loops below wait for the CONDITION, so machine load decides how long a leg
/// takes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// The network every resource of the second test sits in: one VPC, two subnets in different
/// Availability Zones, and one address out of the first subnet's own block.
const VPC_BLOCK: &str = "10.70.0.0/16";
const SUBNET_BLOCKS: [(&str, &str); 2] = [
    ("10.70.1.0/24", "us-east-1a"),
    ("10.70.2.0/24", "us-east-1b"),
];
const ADDRESS: &str = "10.70.1.10";

/// The health-check schedule every target group here is created with: the shortest spacing and
/// the fewest consecutive failures the two members' own documented ranges allow (5-300 seconds
/// and 2-10 checks), because a verdict about a registration is that many checks at that spacing
/// away. What this suite asserts is WHICH states are reachable, not how long a verdict takes.
const CHECK_INTERVAL_SECONDS: &str = "5";
const UNHEALTHY_THRESHOLD: &str = "2";

/// One ec2Query call made as the account, which must succeed; answers its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        ACCOUNT_KEY,
        "ec2",
        form(action, EC2_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// An `ip` target group of `vpc` with health checking on and the schedule above; answers the ARN
/// this API minted for it.
async fn given_checked_group(router: &MultiServiceRouter, name: &str, vpc: &str) -> String {
    let created = as_account(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("VpcId", vpc),
            ("TargetType", "ip"),
            ("HealthCheckEnabled", "true"),
            ("HealthCheckIntervalSeconds", CHECK_INTERVAL_SECONDS),
            ("UnhealthyThresholdCount", UNHEALTHY_THRESHOLD),
        ],
    )
    .await;
    element(&created, "TargetGroupArn")
}

/// `(State, Reason)` of the registration `id` of the group `arn` names, as a read NAMING that
/// target reports it — the read a client polling one target makes. A reason the service does not
/// report reads as the empty string.
async fn health_of(router: &MultiServiceRouter, arn: &str, id: &str) -> (String, String) {
    let body = as_account(
        router,
        "DescribeTargetHealth",
        &[("TargetGroupArn", arn), ("Targets.member.1.Id", id)],
    )
    .await;
    (
        element(&body, "State"),
        opt_element(&body, "Reason").unwrap_or_default(),
    )
}

/// One row of the defect table: what else is wrong with the request, and the answer a caller the
/// policies PERMIT gets for that very request.
struct Row {
    case: &'static str,
    /// The `TargetGroupArn` the call carries.
    arn: String,
    /// The target id the call carries.
    target: &'static str,
    /// `(status, error code)` a permitted caller gets; the code is empty for a served call.
    permitted: (u16, &'static str),
}

#[tokio::test]
async fn a_denied_registration_is_refused_before_every_modelled_check() {
    let router = router();
    // The group the rows address, and a second one of the same account that is then DELETED: its
    // ARN is one this API really minted, so it is well formed, and it names nothing.
    let group = given_checked_group(&router, "order-probe", "vpc-authorization-order").await;
    let absent = given_checked_group(&router, "order-absent", "vpc-authorization-order").await;
    as_account(&router, "DeleteTargetGroup", &[("TargetGroupArn", &absent)]).await;
    // The same ARN addressed at another Region: a regional endpoint addresses the resources of
    // its own Region, so this names no target group this endpoint could hold.
    let elsewhere = group.replacen(&format!(":{REGION}:"), ":eu-west-1:", 1);

    let denied = session_of(
        &router,
        "elb-registration-denied",
        Some(DENIES_THE_REGISTRATION_CALLS),
    )
    .await;

    // The registration leg runs FIRST, so when the deregistration leg's clean row is refused the
    // address it names is one the group really holds — and the unchanged read is then what shows
    // the refused deregistration removed nothing.
    for operation in ["RegisterTargets", "DeregisterTargets"] {
        let action = format!("{ACTION_PREFIX}:{operation}");
        let rows = [
            Row {
                case: "nothing else wrong with the request",
                arn: group.clone(),
                target: "10.0.1.10",
                permitted: (200, ""),
            },
            Row {
                case: "a target group ARN the account holds nothing for",
                arn: absent.clone(),
                target: "10.0.1.11",
                permitted: (400, TARGET_GROUP_NOT_FOUND),
            },
            Row {
                case: "a value that is no target group ARN at all",
                arn: "not-an-arn".to_string(),
                target: "10.0.1.12",
                permitted: (400, VALIDATION_ERROR),
            },
            Row {
                case: "a target group ARN of another Region",
                arn: elsewhere.clone(),
                target: "10.0.1.13",
                permitted: (400, VALIDATION_ERROR),
            },
            Row {
                case: "a target id of no form an ip target group's targets take",
                arn: group.clone(),
                target: "i-0123456789abcdef0",
                permitted: (400, VALIDATION_ERROR),
            },
        ];

        for row in rows {
            let Row {
                case,
                arn,
                target,
                permitted,
            } = row;
            let case = format!("{operation}, {case}");
            // What the GROUP reports about this target before the refused call, whichever ARN the
            // row's request carries: a group nothing forwards to reports a stable pair, so the
            // same read afterwards is the evidence the refusal wrote nothing — including for a
            // row whose ARN differs from the group's only in its Region or its form, where a
            // write leaked through would land on this very group.
            let before = health_of(&router, &group, target).await;

            let call: Vec<(&str, &str)> = vec![
                ("TargetGroupArn", arn.as_str()),
                ("Targets.member.1.Id", target),
            ];
            let (status, body) = elbv2(&router, &denied.key, operation, &call).await;
            assert_eq!(
                status, 403,
                "case {case:?}: expected HTTP 403 for a principal under an explicit deny, got \
                 {status} with body {body}"
            );
            let fault = opt_element(&body, "Type").unwrap_or_default();
            assert_eq!(
                fault, SENDER,
                "case {case:?}: expected the {SENDER} fault, got {fault:?} in {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, ACCESS_DENIED,
                "case {case:?}: expected the error code {ACCESS_DENIED}, got {code:?} in {body}"
            );
            let stated = element(&body, "Message");
            let expected = refusal(&denied.principal, &action, &arn, EXPLICIT_DENY);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected IAM's sentence {expected:?}, got {stated:?}"
            );
            let after = health_of(&router, &group, target).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused call changes nothing about the registration it \
                 named, expected {before:?}, got {after:?}"
            );

            // And the SAME request, from a caller the policies permit, is answered by the
            // modelled check the row's defect earns — which is the answer the refusal above
            // preempted.
            let (status, body) = elbv2(&router, ACCOUNT_KEY, operation, &call).await;
            let code = error_code(&body);
            let (expected_status, expected_code) = permitted;
            assert_eq!(
                status, expected_status,
                "case {case:?}: expected HTTP {expected_status} for a permitted caller, got \
                 {status} with body {body}"
            );
            match expected_code.is_empty() {
                true => assert!(
                    !body.contains("<Error>"),
                    "case {case:?}: expected the permitted call to be served, got {body}"
                ),
                false => assert_eq!(
                    code, expected_code,
                    "case {case:?}: expected the permitted caller's own refusal \
                     {expected_code:?}, got {code:?} in {body}"
                ),
            }
        }
    }
}

#[tokio::test]
async fn unused_names_only_an_unregistered_target_or_a_group_nothing_forwards_to() {
    let router = router();

    // The network, through Amazon EC2 itself.
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", VPC_BLOCK)]).await,
        "vpcId",
    );
    let mut subnets: Vec<String> = Vec::new();
    for (block, zone) in SUBNET_BLOCKS {
        let created = ec2(
            &router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", block),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }

    // Two target groups of that VPC: the one a listener will forward to, and one nothing ever
    // forwards to, so what decides the state is the FORWARD rather than the existence of a load
    // balancer in the account.
    let forwarded = given_checked_group(&router, "unused-forwarded", &vpc).await;
    let detached = given_checked_group(&router, "unused-detached", &vpc).await;

    // Nothing is registered yet, so the address is a target the group holds no registration for.
    let both = [
        ("the forwarded group", forwarded.clone()),
        ("the detached group", detached.clone()),
    ];
    for (case, group) in &both {
        let reported = health_of(&router, group, ADDRESS).await;
        assert_eq!(
            reported,
            (UNUSED.to_string(), NOT_REGISTERED.to_string()),
            "case {case:?}: before any registration the target is not registered, got \
             {reported:?}"
        );
    }

    for (case, group) in &both {
        let (status, body) = elbv2(
            &router,
            ACCOUNT_KEY,
            "RegisterTargets",
            &[
                ("TargetGroupArn", group.as_str()),
                ("Targets.member.1.Id", ADDRESS),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from RegisterTargets, got {status} with body {body}"
        );
        let reported = health_of(&router, group, ADDRESS).await;
        assert_eq!(
            reported,
            (UNUSED.to_string(), NOT_IN_USE.to_string()),
            "case {case:?}: with no load balancer forwarding to it the group is not in use, got \
             {reported:?}"
        );
    }

    // A load balancer that forwards nowhere yet: still nothing uses either group.
    let [first_subnet, second_subnet] = &subnets[..] else {
        panic!("setup: expected the two subnets EC2 created, got {subnets:?}")
    };
    let placement: Vec<(&str, &str)> = vec![
        ("Name", "unused-probe-alb"),
        ("Scheme", "internal"),
        ("Subnets.member.1", first_subnet.as_str()),
        ("Subnets.member.2", second_subnet.as_str()),
    ];
    let load_balancer = element(
        &as_account(&router, "CreateLoadBalancer", &placement).await,
        "LoadBalancerArn",
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = as_account(
            &router,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", &load_balancer)],
        )
        .await;
        let state = block_of(&described, "State")
            .and_then(|block| opt_element(block, "Code"))
            .unwrap_or_default();
        if state == "active" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected the load balancer to become active, it is {state:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    for (case, group) in &both {
        let reported = health_of(&router, group, ADDRESS).await;
        assert_eq!(
            reported,
            (UNUSED.to_string(), NOT_IN_USE.to_string()),
            "case {case:?}: a load balancer that forwards nowhere uses no target group, got \
             {reported:?}"
        );
    }

    // The listener's own default action forwards to ONE of the two groups.
    let listener = element(
        &as_account(
            &router,
            "CreateListener",
            &[
                ("LoadBalancerArn", &load_balancer),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("DefaultActions.member.1.Type", "forward"),
                ("DefaultActions.member.1.TargetGroupArn", &forwarded),
            ],
        )
        .await,
        "ListenerArn",
    );

    // From here on the forwarded group's registration is in the health-check lifecycle and is
    // never reported unused again: the state is the `initial` of the checks it still owes, and
    // then the verdict they reach. WHICH of the three a read catches is the clock's and not this
    // suite's, so every read asserts the reachable set, and an `initial` asserts one of the two
    // reasons the model gives it. The group the listener does NOT forward to stays unused
    // throughout.
    let deadline = Instant::now() + DEADLINE;
    let verdict = loop {
        let (state, reason) = health_of(&router, &forwarded, ADDRESS).await;
        assert!(
            REACHABLE_WHEN_FORWARDED.contains(&state.as_str()),
            "case \"the forwarded group\": a registration of a group an active listener \
             forwards to is one of {REACHABLE_WHEN_FORWARDED:?}, got {state:?}/{reason:?}"
        );
        let untouched = health_of(&router, &detached, ADDRESS).await;
        assert_eq!(
            untouched,
            (UNUSED.to_string(), NOT_IN_USE.to_string()),
            "case \"the detached group\": no listener forwards to it, so it is not in use, got \
             {untouched:?}"
        );
        if state != "initial" {
            break (state, reason);
        }
        assert!(
            INITIAL_REASONS.contains(&reason.as_str()),
            "case \"the forwarded group\": an initial registration carries one of \
             {INITIAL_REASONS:?}, got {reason:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case \"the forwarded group\": expected the checks to reach a verdict, it is still \
             {state:?}/{reason:?}"
        );
        tokio::time::sleep(POLL).await;
    };
    assert!(
        verdict.0 == "healthy" || verdict.0 == "unhealthy",
        "case \"the forwarded group\": the verdict its checks reached is healthy or unhealthy, \
         got {verdict:?}"
    );

    // And the association is DERIVED from the listeners that exist: with the listener gone
    // nothing forwards to the group any more, so its registration is unused again.
    as_account(&router, "DeleteListener", &[("ListenerArn", &listener)]).await;
    let released = health_of(&router, &forwarded, ADDRESS).await;
    assert_eq!(
        released,
        (UNUSED.to_string(), NOT_IN_USE.to_string()),
        "case \"the forward deleted\": with no listener forwarding to it the group is not in \
         use again, got {released:?}"
    );
}
