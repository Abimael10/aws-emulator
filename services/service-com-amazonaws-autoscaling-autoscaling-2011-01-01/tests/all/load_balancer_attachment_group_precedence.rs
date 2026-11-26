// SPDX-License-Identifier: CC0-1.0

//! WHICH check decides a Classic Load Balancer attachment, when the Auto Scaling group the
//! request names is not one the account holds.
//!
//! The rule driven end to end here: **an attachment write resolves the Auto Scaling group it
//! changes BEFORE it examines the load-balancer names travelling with it**, so a name no group
//! answers to is refused for the GROUP — this service's absent-group `ValidationError`,
//! whose detail these operations leave unfilled — whatever those load-balancer names are, and
//! whether or not Elastic Load Balancing holds a load balancer under them. A client reads the
//! refusal to decide what to fix: told the load balancers are suspect it re-checks or
//! re-creates load balancers that are fine and retries against a group that will never exist,
//! while told the group is gone it re-creates or re-resolves the group.
//!
//! The table spans what the rule generalises over: the operation (`AttachLoadBalancers` and
//! `DetachLoadBalancers`, the two writes whose other member is a `LoadBalancerNames` list),
//! the group name's length, case, character set and spelling (a name spelled like the
//! unfilled detail token among them) and whether that name never existed or was created and
//! then deleted, and the load-balancer names the request carries: one the account holds, one
//! nothing answers to, a mix of the two, several held ones, one name given twice, a name no
//! load-balancer name could be, and the most names one call may give.
//!
//! The controls run on a group that EXISTS, so no row can pass by refusing everything: the
//! unresolvable-load-balancer refusal still fires there, a detach of a load balancer the
//! group does not hold is still refused for that load balancer, an attach of held load
//! balancers is served and read back through `DescribeLoadBalancers`, and the SAME request
//! that was served flips to the absent-group refusal once the group is deleted.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_classic_load_balancer, emulator_with_classic_elb, p, query, tag_value,
    tag_values, xml_unescape, REGION,
};

/// The service-common error code this service answers a bad request under.
const VALIDATION_ERROR: &str = "ValidationError";

/// The WHOLE sentence these writes refuse a group the account does not hold with: the fixed
/// text every absent-group refusal of this service opens with, and the detail field they
/// leave unfilled, which therefore travels as the literal token below. Spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make a row pass.
const ABSENT_GROUP_MESSAGE: &str = "AutoScalingGroup name not found - null";

/// The fixed opening of that sentence, which a refusal about anything OTHER than the group
/// may not carry.
const ABSENT_GROUP_PREFIX: &str = "AutoScalingGroup name not found - ";

/// The unfilled detail token, which one row also submits as the group's name: the refusal for
/// a group named like it reads exactly like the refusal for any other absent group.
const UNFILLED_DETAIL: &str = "null";

/// The whole sentence an attach refuses Classic Load Balancers it cannot resolve with — the
/// one a request whose only defect is the group may never be answered with.
const UNRESOLVABLE_LOAD_BALANCERS: &str =
    "Provided Load Balancers may not be valid. Please ensure they exist and try again.";

/// The Availability Zone the group here is placed in and every load balancer it could attach
/// is enabled for, in the Region the requests are signed for.
const ZONE: &str = "us-west-2a";

/// The launch source the group here is created from, seeded into the account's own Amazon EC2
/// store by the shared harness.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The most load balancers ONE attach or detach may name, from the model's own documentation
/// for `AttachLoadBalancersType$LoadBalancerNames`: "You can specify up to 10 load
/// balancers."
const MAX_LOAD_BALANCERS_PER_CALL: usize = 10;

/// A load-balancer name Elastic Load Balancing holds nothing under.
const ABSENT_LOAD_BALANCER: &str = "clb-no-such-load-balancer";

/// The two writes whose request carries a `LoadBalancerNames` list beside the group name.
const ATTACH: &str = "AttachLoadBalancers";
const DETACH: &str = "DetachLoadBalancers";

/// The Classic Load Balancers this suite creates in the account, through Elastic Load
/// Balancing's own wire: as many as one call may name, so the row carrying the documented
/// per-call bound names load balancers that all exist.
fn held_load_balancers() -> Vec<String> {
    (1..=MAX_LOAD_BALANCERS_PER_CALL)
        .map(|index| format!("clb-precedence-{index}"))
        .collect()
}

/// Group names a client can submit for a group the account does not hold, differing in
/// length, case, character set, separators and spelling. `AutoScalingGroupName` is
/// `XmlStringMaxLen255` (len 1..255,
/// `smithy_query(query="operation",operation="AttachLoadBalancers")`), so the boundary
/// validator admits every row here and only a handler can refuse it.
fn absent_group_names() -> Vec<(&'static str, String)> {
    let mut names: Vec<(&'static str, String)> = [
        ("the shortest name the model allows", "a"),
        (
            "a name of the shape the recorded calls used",
            "probe-asg-0123456789ab",
        ),
        ("an upper-case name", "ASG-UPPER-CASE"),
        (
            "a name carrying every separator the model allows",
            "asg.name_with-separators 1",
        ),
        ("a name spelled like the unfilled detail token", UNFILLED_DETAIL),
        ("a non-ASCII name", "グループ-ünïcode"),
    ]
    .into_iter()
    .map(|(case, name)| (case, name.to_string()))
    .collect();
    // The longest name the member's length constraint admits.
    names.push(("the longest name the model allows", "z".repeat(255)));
    names
}

/// The `LoadBalancerNames` lists a request can carry beside the group name, built from the
/// load balancers the account holds: each one would decide the call on its own if the group
/// were resolved after it.
fn load_balancer_name_sets(held: &[String]) -> Vec<(&'static str, Vec<String>)> {
    let first = held.first().expect("one held load balancer").clone();
    let second = held.get(1).expect("two held load balancers").clone();
    vec![
        ("one load balancer the account holds", vec![first.clone()]),
        (
            "a name nothing answers to",
            vec![ABSENT_LOAD_BALANCER.to_string()],
        ),
        (
            "one that exists and one that does not",
            vec![first.clone(), ABSENT_LOAD_BALANCER.to_string()],
        ),
        (
            "two load balancers the account holds",
            vec![first.clone(), second],
        ),
        ("one held name given twice", vec![first.clone(), first]),
        (
            "a name no load-balancer name could be",
            vec!["<clb & 'name' \"quoted\">".to_string()],
        ),
        (
            "the most names one call may give, all held",
            held.to_vec(),
        ),
    ]
}

/// Create the group this suite's controls run on, holding no capacity, and require the create
/// to be served: what the controls set up rather than what they assert.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("AvailabilityZones.member.1", ZONE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "0"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body}"
    );
}

/// Post one `AttachLoadBalancers` or `DetachLoadBalancers` call and answer its status, wire
/// error code and unescaped message.
async fn load_balancer_call(
    router: &MultiServiceRouter,
    action: &str,
    group: &str,
    names: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            &format!("LoadBalancerNames.member.{}", index + 1),
            name.clone(),
        ));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The Classic Load Balancer names `DescribeLoadBalancers` reports for one group, in the
/// order reported.
async fn attached_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeLoadBalancers",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLoadBalancers for {group:?} expected 200, got {status} with {body}"
    );
    tag_values(&body, "LoadBalancerName")
}

/// Assert one call naming an absent group is refused for the GROUP: HTTP 400, the
/// service-common code, and the whole absent-group sentence.
async fn assert_absent_group_refusal(
    router: &MultiServiceRouter,
    action: &str,
    name_case: &str,
    group: &str,
    names_case: &str,
    names: &[String],
) {
    let case = format!("{action} for {name_case} ({group:?}) carrying {names_case}");
    let (status, code, message) = load_balancer_call(router, action, group, names).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {message:?}"
    );
    assert_eq!(
        code, VALIDATION_ERROR,
        "case {case:?}: expected the error code {VALIDATION_ERROR:?}, got {code:?} with \
         {message:?}"
    );
    assert_eq!(
        message, ABSENT_GROUP_MESSAGE,
        "case {case:?}: expected the message {ABSENT_GROUP_MESSAGE:?}, got {message:?}"
    );
}

/// THE RULE: both Classic Load Balancer writes resolve the group before the load-balancer
/// names, for a name that never existed and for one created and then deleted — while a group
/// that EXISTS is still held to the load balancers its request names.
#[tokio::test]
async fn a_load_balancer_write_resolves_the_group_before_its_load_balancer_names() {
    let (router, _engine) = emulator_with_classic_elb();
    let held = held_load_balancers();
    for name in &held {
        create_classic_load_balancer(&router, REGION, ZONE, name).await;
    }
    let name_sets = load_balancer_name_sets(&held);

    for (name_case, group) in absent_group_names() {
        for (names_case, names) in &name_sets {
            for action in [ATTACH, DETACH] {
                assert_absent_group_refusal(
                    &router, action, name_case, &group, names_case, names,
                )
                .await;
            }
        }
    }

    // A name the account HELD and no longer does is refused exactly like one that never
    // existed: the refusal is about what the store holds now.
    let deleted = "asg-precedence-deleted";
    create_group(&router, deleted).await;
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", deleted)],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting {deleted:?} must succeed, got {status} with {body}"
    );
    for (names_case, names) in &name_sets {
        for action in [ATTACH, DETACH] {
            assert_absent_group_refusal(
                &router,
                action,
                "a name created and then deleted",
                deleted,
                names_case,
                names,
            )
            .await;
        }
    }

    // The controls, on a group that EXISTS. It starts out attached to nothing, which also
    // shows the rows above attached nothing anywhere.
    let group = "asg-precedence-held";
    create_group(&router, group).await;
    let attached = attached_names(&router, group).await;
    assert!(
        attached.is_empty(),
        "the control group must start out attached to nothing, got {attached:?}"
    );

    // A load balancer nothing answers to is still refused for the LOAD BALANCER where the
    // group is resolvable: resolving the group first did not retire that refusal.
    let unresolvable = vec![ABSENT_LOAD_BALANCER.to_string()];
    let (status, code, message) =
        load_balancer_call(&router, ATTACH, group, &unresolvable).await;
    assert_eq!(
        status, 400,
        "case \"an unresolvable load balancer on a stored group\": expected HTTP 400, got \
         {status} with {message:?}"
    );
    assert_eq!(
        code, VALIDATION_ERROR,
        "case \"an unresolvable load balancer on a stored group\": expected the error code \
         {VALIDATION_ERROR:?}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, UNRESOLVABLE_LOAD_BALANCERS,
        "case \"an unresolvable load balancer on a stored group\": expected the message \
         {UNRESOLVABLE_LOAD_BALANCERS:?}, got {message:?}"
    );

    // And a detach of a load balancer the group does not hold is refused for THAT load
    // balancer, naming it — never as an absent group.
    let first = held.first().expect("one held load balancer").clone();
    let (status, _code, message) =
        load_balancer_call(&router, DETACH, group, std::slice::from_ref(&first)).await;
    let expected =
        format!("Trying to remove Load Balancers that are not part of the group: {first}");
    assert_eq!(
        status, 400,
        "case \"a detach of an unattached load balancer\": expected HTTP 400, got {status} \
         with {message:?}"
    );
    assert_eq!(
        message, expected,
        "case \"a detach of an unattached load balancer\": expected the message {expected:?}, \
         got {message:?}"
    );
    assert!(
        !message.starts_with(ABSENT_GROUP_PREFIX),
        "case \"a detach of an unattached load balancer\": a stored group must not be reported \
         as absent, got {message:?}"
    );

    // An attach of load balancers the account holds is SERVED on that group, and read back
    // through this service's own read.
    let second = held.get(1).expect("two held load balancers").clone();
    let pair = vec![first.clone(), second.clone()];
    let (status, code, message) = load_balancer_call(&router, ATTACH, group, &pair).await;
    assert_eq!(
        status, 200,
        "case \"an attach of held load balancers\": expected HTTP 200, got {status} \
         {code:?} {message:?}"
    );
    let attached = attached_names(&router, group).await;
    assert_eq!(
        attached, pair,
        "case \"an attach of held load balancers\": expected {pair:?} reported for the group, \
         got {attached:?}"
    );

    // The SAME request that was just served flips to the absent-group refusal once the group
    // is deleted, and nothing about the load balancers it names has changed: the group is
    // what decides it.
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting {group:?} must succeed, got {status} with {body}"
    );
    assert_absent_group_refusal(
        &router,
        ATTACH,
        "the control group after its delete",
        group,
        "the same two held load balancers the served attach named",
        &pair,
    )
    .await;
}
