// SPDX-License-Identifier: CC0-1.0

//! WHAT Amazon EC2 Auto Scaling tells a caller it refused, and WHICH check decides it.
//!
//! Three rules, each driven end to end over the wire this service speaks (`awsQuery`), with
//! Amazon EC2 and Elastic Load Balancing v2 mounted on the SAME engine (the subnets a group
//! is placed in and the target groups it attaches are their resources):
//!
//! 1. **A refusal about a VALUE the service could not resolve from a published catalog
//!    states the catalog, not the value.** A scheduled action's `TimeZone` is one of "the
//!    canonical names of the IANA time zones"
//!    (`PutScheduledUpdateGroupActionType$TimeZone`), and a value outside that database is
//!    refused with ONE fixed sentence carrying no part of the request — so the rows below
//!    differ in the FORM of the rejected name and every one of them reads the same, while a
//!    canonical name is served and reported back. The GROUP the action belongs to is
//!    resolved first, in the absent-group sentence this write serves.
//!
//! 2. **A resource named by a NAME that is unique only inside a parent needs that parent
//!    stated.** A scaling policy's name is unique within an Auto Scaling group while its
//!    ARN names it outright, so an operation acting on one policy
//!    (`ExecutePolicy`, `DeletePolicy` — `PolicyName` required, `AutoScalingGroupName`
//!    optional) refuses a bare name with no group for the member it LEAVES OUT, before any
//!    lookup, rather than reporting the policy as missing. The policy named is still there
//!    afterwards, and the same request naming the policy's ARN is served.
//!
//! 3. **A request naming a parent resource is refused for that parent before the resources
//!    its other members name are read.** The attach family (`AttachTrafficSources`,
//!    `AttachLoadBalancerTargetGroups`, `AttachLoadBalancers`) changes the named group, so a
//!    group the account does not hold is refused with this service's absent-group sentence
//!    whatever the request says about the traffic sources, target groups or load balancers —
//!    and the resource-resolution refusal still fires for a group that DOES exist.
//!
//! 4. The sentence a paginated read refuses an unusable `NextToken` with is each read's
//!    own, so it is asserted where this crate already crosses every paginated read with
//!    the token forms this service never issued
//!    (`lifecycle_hook_and_notification_refusals.rs`), rather than a second time here.
//!
//! Each table spans what its rule generalises over — the form of the rejected value, the
//! operation, how the resource is named and how many entries travel with it — rather than
//! one recorded row, and every assertion names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_target_group, emulator, list_members, p, query, seed_subnets, tag_value,
    vpc_of, xml_unescape, SeededSubnet, REGION,
};

/// The launch source every group here is created from — a launch TEMPLATE, seeded into the
/// account's own Amazon EC2 store by the shared harness.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The service-common error code this service answers a bad request under.
const VALIDATION_ERROR: &str = "ValidationError";

/// The whole sentence a scheduled-action write refuses a `TimeZone` outside the IANA
/// database with: fixed text, with NO part of the request in it. Spelled out here rather
/// than imported, so a wrong constant in the implementation cannot make a row pass.
const INVALID_TIME_ZONE_MESSAGE: &str =
    "Time zone specified does not conform to standardized time zones found in IANA Time Zone \
     Database";

/// The whole sentence an operation acting on ONE scaling policy refuses a bare policy name
/// carrying no `AutoScalingGroupName` with.
const GROUP_REQUIRED_FOR_POLICY_NAME: &str =
    "The AutoScaling Group name is required when specifying a policy by its name";

/// The whole sentence the attach family refuses a group the account does not hold with: the
/// fixed text every absent-group refusal of this service opens with, and the detail field
/// these operations leave unfilled, so it travels as the literal token `null`.
const ABSENT_GROUP_WITHOUT_DETAIL: &str = "AutoScalingGroup name not found - null";

/// The whole sentence an attach refuses target groups it cannot resolve with.
const UNATTACHABLE_TARGET_GROUPS: &str =
    "Provided Target Groups may not be valid. Please ensure they exist and try again.";

/// The whole sentence an attach refuses Classic Load Balancers it cannot resolve with.
const UNRESOLVABLE_LOAD_BALANCERS: &str =
    "Provided Load Balancers may not be valid. Please ensure they exist and try again.";

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the target group this suite creates listens on.
const TARGET_GROUP_PORT: i32 = 80;

/// The three `TrafficSourceIdentifier$Type` values an association can be held under, from the
/// model's own catalog for the member
/// (`smithy_query(query="doc",shape="TrafficSourceIdentifier",member="Type")`).
const TYPE_ELB: &str = "elb";
const TYPE_ELBV2: &str = "elbv2";
const TYPE_VPC_LATTICE: &str = "vpc-lattice";

/// Create one group holding no capacity, placed in `subnet`, and require the create to be
/// served: what a row sets up rather than what it asserts.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet: &SeededSubnet) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnet.id.clone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body}"
    );
}

/// Post one action and answer its status, wire error code and unescaped message.
async fn refusal(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String, String) {
    let (status, body) = query(router, action, &borrowed(params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Assert one call is refused HTTP 400 with `code` and exactly `message`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    code: &str,
    message: &str,
) {
    let (status, answered_code, answered_message) = refusal(router, action, params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {action} expected HTTP 400, got {status} with {answered_message:?}"
    );
    assert_eq!(
        answered_code, code,
        "case {case:?}: {action} expected the error code {code:?}, got {answered_code:?}"
    );
    assert_eq!(
        answered_message, message,
        "case {case:?}: {action} expected the message {message:?}, got {answered_message:?}"
    );
}

/// The scheduled actions the group reports right now, as `(name, time zone)` pairs.
async fn reported_actions(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the scheduled actions of {group:?} expected 200, got {status} \
         with {body}"
    );
    list_members(&body, "ScheduledUpdateGroupActions")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "ScheduledActionName"),
                tag_value(member, "TimeZone"),
            )
        })
        .collect()
}

/// RULE 1: a `TimeZone` the IANA database does not name is refused with ONE fixed sentence,
/// the same for every such value, and nothing is stored; a canonical name is served and
/// reported back.
#[tokio::test]
async fn an_unknown_time_zone_is_refused_with_one_value_free_sentence() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet");
    let group = "asg-time-zones";
    create_group(&router, group, subnet).await;

    // The forms a value outside the database takes: a region/city pair no database holds, an
    // abbreviation, prose, a UTC offset, and a name carrying the characters the member's
    // pattern admits. None of them is a canonical IANA name, so all read the same sentence.
    let rejected = [
        ("a region and city the database does not hold", "Mars/Olympus_Mons"),
        ("an abbreviation rather than a canonical name", "PST"),
        ("the zone's prose name", "Pacific Standard Time"),
        ("a UTC offset rather than a name", "+05:30"),
        ("a continent the database does not hold", "Europe/Atlantis"),
    ];

    for (case, zone) in rejected {
        let params = vec![
            p("AutoScalingGroupName", group),
            p("ScheduledActionName", "refused-action"),
            p("Recurrence", "0 12 * * *"),
            p("TimeZone", zone),
            p("MinSize", "1"),
        ];
        let (status, code, message) =
            refusal(&router, "PutScheduledUpdateGroupAction", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {message:?}"
        );
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected the error code {VALIDATION_ERROR:?}, got {code:?}"
        );
        assert_eq!(
            message, INVALID_TIME_ZONE_MESSAGE,
            "case {case:?}: expected the message {INVALID_TIME_ZONE_MESSAGE:?}, got {message:?}"
        );
        // The sentence states the catalogue, never the value: a client matching it must not
        // have to strip the zone it sent out of it.
        assert!(
            !message.contains(zone),
            "case {case:?}: expected the refusal to carry no part of the request, got \
             {message:?}"
        );
        let held = reported_actions(&router, case, group).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused put stores nothing, got {held:?}"
        );
    }

    // Canonical names of the IANA time zones — none of them the one the rejected rows above
    // vary on — are served, and the action reports the zone it was stored with.
    let accepted = [
        ("a southern-hemisphere zone", "Australia/Sydney"),
        ("an Asian zone", "Asia/Tokyo"),
        ("the database's own UTC name", "UTC"),
        ("a fixed-offset zone of the Etc area", "Etc/GMT+9"),
    ];
    for (index, (case, zone)) in accepted.iter().enumerate() {
        let name = format!("served-{index:02}");
        let params = vec![
            p("AutoScalingGroupName", group),
            p("ScheduledActionName", name.clone()),
            p("Recurrence", "0 12 * * *"),
            p("TimeZone", *zone),
            p("MinSize", "1"),
        ];
        let (status, body) = query(
            &router,
            "PutScheduledUpdateGroupAction",
            &borrowed(&params),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a canonical IANA name must be served, got {status} with {body}"
        );
        let held = reported_actions(&router, case, group).await;
        assert!(
            held.contains(&(name.clone(), (*zone).to_string())),
            "case {case:?}: expected the action {name:?} to report the zone {zone:?}, got \
             {held:?}"
        );
    }

    // A scheduled action belongs to a GROUP, so a put naming a group the account does not
    // hold is refused for the group before the zone is read at all — in the sentence this
    // write states for an absent group, whose detail field it leaves unfilled. Both rows
    // read the same whether the zone beside it is canonical or not.
    for (case, zone) in [
        ("an absent group, with an unknown zone beside it", "Mars/Olympus_Mons"),
        ("an absent group, with a canonical zone beside it", "Asia/Tokyo"),
    ] {
        assert_refused(
            &router,
            case,
            "PutScheduledUpdateGroupAction",
            &[
                p("AutoScalingGroupName", "no-such-group"),
                p("ScheduledActionName", "refused-action"),
                p("Recurrence", "0 12 * * *"),
                p("TimeZone", zone),
                p("MinSize", "1"),
            ],
            VALIDATION_ERROR,
            ABSENT_GROUP_WITHOUT_DETAIL,
        )
        .await;
    }
}

/// One operation that acts on a single scaling policy, with the member it names the policy
/// in; both take an optional `AutoScalingGroupName`.
const POLICY_OPERATIONS: [&str; 2] = ["ExecutePolicy", "DeletePolicy"];

/// The policies the group reports right now, by name.
async fn reported_policies(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the policies of {group:?} expected 200, got {status} with {body}"
    );
    list_members(&body, "ScalingPolicies")
        .into_iter()
        .map(|member| tag_value(member, "PolicyName"))
        .collect()
}

/// RULE 2: a policy named by its NAME with no group is refused for the missing group, by
/// every operation that acts on one policy — and the policy is still there afterwards.
#[tokio::test]
async fn a_policy_named_by_its_name_needs_its_group() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet");
    let group = "asg-policy-reference";
    create_group(&router, group, subnet).await;

    let put = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", "scale-out"),
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("ScalingAdjustment", "1"),
    ];
    let (status, body) = query(&router, "PutScalingPolicy", &borrowed(&put)).await;
    assert_eq!(
        status, 200,
        "the policy must be put, got {status} with {body}"
    );
    let arn = tag_value(&body, "PolicyARN");
    assert!(
        !arn.is_empty(),
        "PutScalingPolicy must report the policy's ARN, got {body}"
    );

    // The forms a value that is NOT the ARN of a scaling policy takes: the name of a policy
    // the account holds, a name nothing answers to, a name carrying the spaces and
    // punctuation the member's pattern admits, an ARN of ANOTHER service, and an ARN of
    // another of THIS service's resources — the group itself — which names no policy either.
    let named_by_name = [
        ("the name of a policy the account holds", "scale-out".to_string()),
        ("a name nothing answers to", "no-such-policy".to_string()),
        (
            "a name carrying spaces and punctuation",
            "scale out (peak hours)".to_string(),
        ),
        (
            "an ARN of another service",
            format!(
                "arn:aws:elasticloadbalancing:{REGION}:{}:targetgroup/tg/0123456789abcdef",
                ::engine::emulator_account_id()
            ),
        ),
        (
            "an ARN of this service naming the group rather than a policy",
            format!(
                "arn:aws:autoscaling:{REGION}:{}:autoScalingGroup:\
                 0123abcd-0123-4567-89ab-0123456789ab:autoScalingGroupName/{group}",
                ::engine::emulator_account_id()
            ),
        ),
    ];

    for action in POLICY_OPERATIONS {
        for (case, reference) in &named_by_name {
            let case = format!("{action} naming {case}");
            assert_refused(
                &router,
                &case,
                action,
                &[p("PolicyName", reference.clone())],
                VALIDATION_ERROR,
                GROUP_REQUIRED_FOR_POLICY_NAME,
            )
            .await;
            let held = reported_policies(&router, &case, group).await;
            assert_eq!(
                held,
                vec!["scale-out".to_string()],
                "case {case:?}: a refused call leaves the policy where it was, got {held:?}"
            );
        }
    }

    // The ARN names the policy on its own: the same operations, with no group beside it, are
    // SERVED — so the refusal above is about the member that was missing and not about the
    // group being optional.
    let (status, body) = query(&router, "ExecutePolicy", &[("PolicyName", arn.as_str())]).await;
    assert_eq!(
        status, 200,
        "a policy named by its ARN alone must be performed, got {status} with {body}"
    );
    let (status, body) = query(&router, "DeletePolicy", &[("PolicyName", arn.as_str())]).await;
    assert_eq!(
        status, 200,
        "a policy named by its ARN alone must be deleted, got {status} with {body}"
    );
    let held = reported_policies(&router, "after the delete by ARN", group).await;
    assert!(
        held.is_empty(),
        "expected the policy deleted by its ARN to be gone, got {held:?}"
    );
}

/// One attach row: the case, the action, and the members it carries beside the group name.
type AttachRow = (String, &'static str, Vec<(String, String)>);

/// One attach row keyed by OPERATION: the action, the members it carries beside the group
/// name, and the sentence that operation refuses an unresolvable resource with.
type ResourceRow = (&'static str, Vec<(String, String)>, &'static str);

/// The `TrafficSources` members naming each `(identifier, type)` pair, numbered from one.
fn traffic_sources(sources: &[(String, &str)]) -> Vec<(String, String)> {
    let mut params = Vec::new();
    for (index, (identifier, source_type)) in sources.iter().enumerate() {
        let member = index + 1;
        params.push(p(
            format!("TrafficSources.member.{member}.Identifier").as_str(),
            identifier.clone(),
        ));
        params.push(p(
            format!("TrafficSources.member.{member}.Type").as_str(),
            *source_type,
        ));
    }
    params
}

/// RULE 3: the attach family refuses a group the account does not hold for the GROUP,
/// whatever the request names beside it — while the resource-resolution refusal still fires
/// for a group that exists.
#[tokio::test]
async fn an_attach_refuses_an_absent_group_before_the_resources_it_names() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let subnet = subnets.first().expect("one seeded subnet");
    let vpc_id = vpc_of(&router, subnet).await;
    let held_group = "asg-attach-precedence";
    create_group(&router, held_group, subnet).await;
    // One target group the account DOES hold, so a row naming a resolvable resource is
    // refused for the group just as one naming an unresolvable resource is.
    let resolvable = create_target_group(
        &router,
        "tg-attach-precedence",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let account = ::engine::emulator_account_id();
    let unresolvable = format!("arn:aws:elasticloadbalancing:{REGION}:{account}:targetgroup/ghost/0123456789abcdef");
    let lattice = format!("arn:aws:vpc-lattice:{REGION}:{account}:targetgroup/tg-0123456789abcdef0");

    // What the request names beside the absent group: a resource that resolves, one that does
    // not, each kind of traffic source, and several at once.
    let absent_group = "no-such-group";
    let rows: Vec<AttachRow> = vec![
        (
            "a target group ARN the account holds".to_string(),
            "AttachTrafficSources",
            traffic_sources(&[(resolvable.clone(), TYPE_ELBV2)]),
        ),
        (
            "a target group ARN nothing answers to".to_string(),
            "AttachTrafficSources",
            traffic_sources(&[(unresolvable.clone(), TYPE_ELBV2)]),
        ),
        (
            "the name of a Classic Load Balancer".to_string(),
            "AttachTrafficSources",
            traffic_sources(&[("classic-load-balancer".to_string(), TYPE_ELB)]),
        ),
        (
            "a VPC Lattice target group".to_string(),
            "AttachTrafficSources",
            traffic_sources(&[(lattice.clone(), TYPE_VPC_LATTICE)]),
        ),
        (
            "three traffic sources at once".to_string(),
            "AttachTrafficSources",
            traffic_sources(&[
                (resolvable.clone(), TYPE_ELBV2),
                (unresolvable.clone(), TYPE_ELBV2),
                (lattice.clone(), TYPE_VPC_LATTICE),
            ]),
        ),
    ];

    for (case, action, members) in &rows {
        let mut params = vec![p("AutoScalingGroupName", absent_group)];
        params.extend(members.iter().cloned());
        assert_refused(
            &router,
            case,
            action,
            &params,
            VALIDATION_ERROR,
            ABSENT_GROUP_WITHOUT_DETAIL,
        )
        .await;
    }

    // The same precedence in the legacy halves of the family, which state the same
    // absent-group sentence — the one the rows assert in full, so a refusal naming the
    // resources instead fails here.
    let legacy: [ResourceRow; 2] = [
        (
            "AttachLoadBalancerTargetGroups",
            vec![p("TargetGroupARNs.member.1", unresolvable.clone())],
            UNATTACHABLE_TARGET_GROUPS,
        ),
        (
            "AttachLoadBalancers",
            vec![p("LoadBalancerNames.member.1", "no-such-load-balancer")],
            UNRESOLVABLE_LOAD_BALANCERS,
        ),
    ];
    for (action, members, _) in &legacy {
        let case = format!("{action} naming an absent group");
        let mut params = vec![p("AutoScalingGroupName", absent_group)];
        params.extend(members.iter().cloned());
        assert_refused(
            &router,
            &case,
            action,
            &params,
            VALIDATION_ERROR,
            ABSENT_GROUP_WITHOUT_DETAIL,
        )
        .await;
    }

    // A group that EXISTS is still held to the resources its request names: moving the group
    // lookup first did not retire the resolution refusal.
    let resource_rows: [ResourceRow; 3] = [
        (
            "AttachTrafficSources",
            traffic_sources(&[(unresolvable.clone(), TYPE_ELBV2)]),
            UNATTACHABLE_TARGET_GROUPS,
        ),
        (
            "AttachLoadBalancerTargetGroups",
            vec![p("TargetGroupARNs.member.1", unresolvable.clone())],
            UNATTACHABLE_TARGET_GROUPS,
        ),
        (
            "AttachLoadBalancers",
            vec![p("LoadBalancerNames.member.1", "no-such-load-balancer")],
            UNRESOLVABLE_LOAD_BALANCERS,
        ),
    ];
    for (action, members, sentence) in &resource_rows {
        let case = format!("{action} naming a resource nothing answers to");
        let mut params = vec![p("AutoScalingGroupName", held_group)];
        params.extend(members.iter().cloned());
        assert_refused(&router, &case, action, &params, VALIDATION_ERROR, sentence).await;
    }

    // And an attach of what the account does hold is served, so no row above passed on a
    // blanket refusal.
    let mut params = vec![p("AutoScalingGroupName", held_group)];
    params.extend(traffic_sources(&[(resolvable.clone(), TYPE_ELBV2)]));
    let (status, body) = query(&router, "AttachTrafficSources", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "attaching a target group the account holds must be served, got {status} with {body}"
    );
}


