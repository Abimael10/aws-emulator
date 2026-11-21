// SPDX-License-Identifier: CC0-1.0

//! `DetachTrafficSources` over the awsQuery wire.
//!
//! The rule under test: a traffic-source detach is ONE unit of work over the traffic sources
//! the named Auto Scaling group holds — every identifier the request names must be one the
//! group currently holds, so a call naming one it does not hold detaches NONE of them and is
//! refused HTTP 400 `ValidationError` naming the identifiers that are not part of the group,
//! while a call whose every identifier is attached removes exactly those, leaves the rest in
//! the order the group came to hold them, and deregisters the group's instances from what it
//! detached ("when you detach a traffic source, it enters the Removing state while
//! deregistering the instances in the group ... the instances continue to run").
//!
//! The tables below span what that rule generalises over: how many traffic sources the group
//! holds, which kind each is held under (`elbv2`, `elb`, `vpc-lattice` — the catalog of
//! `TrafficSourceIdentifier$Type`), how many the request names and in which order, whether it
//! repeats one, whether the type it states for an attachment matches the one the group holds
//! it under or is left out altogether, where in the request an unheld identifier sits, and the
//! documented per-call bound. Every row reads the group back through `DescribeTrafficSources`
//! — the implemented reader of the very collection this call removes from — so no row can pass
//! on an accepted-but-unstored change.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_target_group, elbv2_query, emulator, list_members, p, query, seed_subnets,
    tag_value, vpc_of, xml_unescape, SeededSubnet, REGION,
};

/// The launch source every group here is created from — a launch TEMPLATE, because no account
/// can create a launch configuration (see `launch_configurations_are_unavailable.rs`).
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The most traffic sources one call may name, from the model's own documentation for
/// `DetachTrafficSourcesType$TrafficSources`: "You can specify up to 10 traffic sources."
const MAX_TRAFFIC_SOURCES_PER_CALL: usize = 10;

/// The three `TrafficSourceIdentifier$Type` values an association can be held under, from the
/// model's own catalog for the member
/// (`smithy_query(query="doc",shape="TrafficSourceIdentifier",member="Type")`): `elb` for the
/// name of a Classic Load Balancer, `elbv2` for an Elastic Load Balancing target group ARN and
/// `vpc-lattice` for a VPC Lattice target group ARN.
const TYPE_ELB: &str = "elb";
const TYPE_ELBV2: &str = "elbv2";
const TYPE_VPC_LATTICE: &str = "vpc-lattice";

/// The state an attachment of a group owing no registration is reported in: "all Auto Scaling
/// instances are registered with the load balancer or target group"
/// (`smithy_query(query="doc",shape="TrafficSourceState",member="State")`), which a group
/// holding no instance satisfies with none.
const STATE_ADDED: &str = "Added";

/// The port the target groups of this suite listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// How often a poll re-reads what another service holds.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span this service owes a
/// transition, with ample margin for a loaded test machine. Read from the service's own span
/// rather than written out, so the two cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(30)
}

/// One traffic source as a client reads it off `DescribeTrafficSources`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedSource {
    identifier: String,
    source_type: String,
    state: String,
}

/// The ARN of a VPC Lattice target group in this account and Region — the form a
/// `vpc-lattice` source names ("VPC Lattice ARN:
/// `arn:aws:vpc-lattice:us-west-2:123456789012:targetgroup/tg-1234567890123456`",
/// `TrafficSourceIdentifier$Identifier`).
fn lattice_target_group(id: &str) -> String {
    engine::emulator_arn("vpc-lattice", REGION, &format!("targetgroup/{id}"))
}

/// The traffic sources this suite's rows name, numbered from one as the rows do: two Elastic
/// Load Balancing v2 target groups the account HOLDS (an `elbv2` association names a target
/// group that resolves), one Classic Load Balancer name, one VPC Lattice target group, and
/// enough further Classic Load Balancer names to fill a call at the documented bound and name
/// two more the group never held.
async fn seeded_sources(router: &MultiServiceRouter, vpc_id: &str) -> Vec<(String, &'static str)> {
    let mut sources: Vec<(String, &'static str)> = vec![
        (
            create_target_group(
                router,
                "tg-traffic-detach-1",
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                vpc_id,
            )
            .await,
            TYPE_ELBV2,
        ),
        (
            create_target_group(
                router,
                "tg-traffic-detach-2",
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                vpc_id,
            )
            .await,
            TYPE_ELBV2,
        ),
        ("my-classic-load-balancer".to_string(), TYPE_ELB),
        (lattice_target_group("tg-0123456789abcdef0"), TYPE_VPC_LATTICE),
    ];
    // The Classic Load Balancer names that fill out the set, so the bound rows have more
    // sources to name than one call may carry.
    for index in sources.len()..=MAX_TRAFFIC_SOURCES_PER_CALL + 2 {
        sources.push((format!("classic-load-balancer-{index}"), TYPE_ELB));
    }
    sources
}

/// The `index`-th seeded source, numbered from one as the rows do.
fn source(seeded: &[(String, &'static str)], index: usize) -> (String, &'static str) {
    seeded[index - 1].clone()
}

/// The sources at `positions`, in that order.
fn sources_at(
    seeded: &[(String, &'static str)],
    positions: &[usize],
) -> Vec<(String, &'static str)> {
    positions
        .iter()
        .map(|position| source(seeded, *position))
        .collect()
}

/// Create one group with no capacity, so it owes no registration and nothing is launched.
async fn create_group(router: &MultiServiceRouter, name: &str, subnets: &[SeededSubnet]) {
    create_sized_group(router, name, subnets, 0).await;
}

/// Create one group holding `desired` instances, placed over `subnets`.
async fn create_sized_group(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[SeededSubnet],
    desired: usize,
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", ids.join(",")),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Attach `sources` to `name` and require the call to be served: what a row sets up rather
/// than what it asserts.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    sources: &[(String, &'static str)],
) {
    let mut params = vec![p("AutoScalingGroupName", name)];
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
    let (status, body) = query(router, "AttachTrafficSources", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: attaching {sources:?} to {name:?} must succeed, got {status} with \
         {body:?}"
    );
}

/// Post one detach naming each source by its identifier and, where the row states one, the
/// type it states for it — and answer its status, wire error code and message.
async fn detach_stating(
    router: &MultiServiceRouter,
    name: &str,
    sources: &[(String, Option<&str>)],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, (identifier, source_type)) in sources.iter().enumerate() {
        let member = index + 1;
        params.push(p(
            format!("TrafficSources.member.{member}.Identifier").as_str(),
            identifier.clone(),
        ));
        if let Some(stated) = source_type {
            params.push(p(
                format!("TrafficSources.member.{member}.Type").as_str(),
                *stated,
            ));
        }
    }
    let (status, body) = query(router, "DetachTrafficSources", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one detach naming each source under the type the group holds it under.
async fn detach(
    router: &MultiServiceRouter,
    name: &str,
    sources: &[(String, &'static str)],
) -> (u16, String, String) {
    let stated: Vec<(String, Option<&str>)> = sources
        .iter()
        .map(|(identifier, source_type)| (identifier.clone(), Some(*source_type)))
        .collect();
    detach_stating(router, name, &stated).await
}

/// The traffic sources the group reports through `DescribeTrafficSources`, in the order
/// reported.
async fn reported_sources(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<ReportedSource> {
    let (status, body) = query(
        router,
        "DescribeTrafficSources",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the traffic sources of {name:?} expected 200, got {status} with \
         {body:?}"
    );
    list_members(&body, "TrafficSources")
        .into_iter()
        .map(|member| ReportedSource {
            identifier: tag_value(member, "Identifier"),
            source_type: tag_value(member, "Type"),
            state: tag_value(member, "State"),
        })
        .collect()
}

/// What the group must report for the sources it still holds: each one once, under the type it
/// is held as, in attachment order, in the state an attachment owing no registration is in.
fn expected_sources(held: &[(String, &'static str)]) -> Vec<ReportedSource> {
    held.iter()
        .map(|(identifier, source_type)| ReportedSource {
            identifier: identifier.clone(),
            source_type: (*source_type).to_string(),
            state: STATE_ADDED.to_string(),
        })
        .collect()
}

/// The message Amazon EC2 Auto Scaling refuses a detach naming associations the group does not
/// hold with, as the recorded refusal words it: fixed text plus the identifiers from the
/// request that are not attached, in request order.
fn unattached_message(identifiers: &[String]) -> String {
    format!(
        "Trying to remove Target Groups that are not part of the group: {}",
        identifiers.join(", ")
    )
}

/// One accepted-detach row: the case, the positions of the sources the group holds (in
/// attachment order) and the positions the request names, in request order.
type AcceptedRow = (&'static str, Vec<usize>, Vec<usize>);

fn accepted_rows() -> Vec<AcceptedRow> {
    vec![
        ("the only traffic source of the group", vec![1], vec![1]),
        ("the first of three", vec![1, 2, 3], vec![1]),
        ("the last of three", vec![1, 2, 3], vec![3]),
        ("the middle of three", vec![1, 2, 3], vec![2]),
        (
            "two of three, in reverse attachment order",
            vec![1, 2, 3],
            vec![3, 1],
        ),
        ("all three at once", vec![1, 2, 3], vec![1, 2, 3]),
        (
            "the Classic Load Balancer beside a target group",
            vec![3, 1],
            vec![3],
        ),
        (
            "the VPC Lattice source among all three kinds",
            vec![1, 3, 4],
            vec![4],
        ),
        (
            "the target group among all three kinds",
            vec![1, 3, 4],
            vec![1],
        ),
        (
            "the same attached traffic source named twice",
            vec![1, 2],
            vec![1, 1],
        ),
        (
            "every traffic source of a group holding the documented maximum",
            (1..=MAX_TRAFFIC_SOURCES_PER_CALL).collect(),
            (1..=MAX_TRAFFIC_SOURCES_PER_CALL).collect(),
        ),
        (
            "one of a group holding the documented maximum",
            (1..=MAX_TRAFFIC_SOURCES_PER_CALL).collect(),
            vec![MAX_TRAFFIC_SOURCES_PER_CALL],
        ),
    ]
}

/// Every row: the named traffic sources are removed and only they are, and the group reports
/// what is left — in the order it came to hold them — through the reader of that very
/// collection.
#[tokio::test]
async fn a_detach_removes_exactly_the_traffic_sources_it_names() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    for (row, (case, held_positions, detached_positions)) in
        accepted_rows().into_iter().enumerate()
    {
        let name = format!("asg-traffic-detach-{row}");
        let held = sources_at(&seeded, &held_positions);
        create_group(&router, &name, &subnets).await;
        attach(&router, case, &name, &held).await;

        let requested = sources_at(&seeded, &detached_positions);
        let (status, code, message) = detach(&router, &name, &requested).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );

        let left: Vec<(String, &'static str)> = held
            .iter()
            .filter(|(identifier, _)| {
                !requested
                    .iter()
                    .any(|(detached, _)| detached == identifier)
            })
            .cloned()
            .collect();
        let expected = expected_sources(&left);
        let actual = reported_sources(&router, case, &name).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the group to hold {expected:?}, got {actual:?}"
        );
    }
}

/// The identifier is what the association IS: a detach names a held attachment by naming its
/// identifier, whether it states the type the group holds it under, states another one, or
/// leaves the member out altogether (it is "required if the identifier is the name of a
/// Classic Load Balancer", `TrafficSourceIdentifier$Type`).
#[tokio::test]
async fn a_detach_names_an_attachment_by_its_identifier() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    // (case, the type the request states for the attachment it detaches)
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("the type the group holds it under", Some(TYPE_ELBV2)),
        ("another type altogether", Some(TYPE_VPC_LATTICE)),
        ("no type at all", None),
    ];

    for (row, (case, stated)) in rows.into_iter().enumerate() {
        let name = format!("asg-traffic-detach-typed-{row}");
        let held = sources_at(&seeded, &[1, 3]);
        create_group(&router, &name, &subnets).await;
        attach(&router, case, &name, &held).await;

        let (identifier, _) = source(&seeded, 1);
        let (status, code, message) =
            detach_stating(&router, &name, &[(identifier, stated)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );

        let expected = expected_sources(&sources_at(&seeded, &[3]));
        let actual = reported_sources(&router, case, &name).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the group to hold {expected:?}, got {actual:?}"
        );
    }
}

/// One refused-detach row: the case, the positions the group holds, the positions the request
/// names and the positions the refusal must name, in request order.
type RefusalRow = (&'static str, Vec<usize>, Vec<usize>, Vec<usize>);

fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        (
            "a traffic source the group never held, alone",
            vec![1],
            vec![11],
            vec![11],
        ),
        (
            "an attached traffic source followed by one never attached",
            vec![1],
            vec![1, 11],
            vec![11],
        ),
        (
            "a never-attached traffic source followed by an attached one",
            vec![1],
            vec![11, 1],
            vec![11],
        ),
        (
            "two never-attached traffic sources mixed between two attached ones",
            vec![1, 2],
            vec![1, 11, 2, 12],
            vec![11, 12],
        ),
        (
            "the same never-attached traffic source named twice",
            vec![2],
            vec![11, 11],
            vec![11, 11],
        ),
        (
            "every traffic source of a group that holds none",
            Vec::new(),
            vec![1, 2],
            vec![1, 2],
        ),
        (
            "a target group the group holds no association with at all",
            vec![3],
            vec![2],
            vec![2],
        ),
    ]
}

/// Every row: HTTP 400, `ValidationError`, the refusal naming the identifiers that are not
/// part of the group, and NOTHING detached — the group still holds every attachment it held
/// before the call, which is what makes the refusal whole.
#[tokio::test]
async fn a_detach_naming_a_traffic_source_the_group_does_not_hold_is_refused_whole() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    for (row, (case, held_positions, requested_positions, unattached_positions)) in
        refusal_rows().into_iter().enumerate()
    {
        let name = format!("asg-traffic-refused-{row}");
        let held = sources_at(&seeded, &held_positions);
        create_group(&router, &name, &subnets).await;
        if !held.is_empty() {
            attach(&router, case, &name, &held).await;
        }

        let requested = sources_at(&seeded, &requested_positions);
        let (status, code, message) = detach(&router, &name, &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        let unattached: Vec<String> = sources_at(&seeded, &unattached_positions)
            .into_iter()
            .map(|(identifier, _)| identifier)
            .collect();
        let expected = unattached_message(&unattached);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        let still_held = expected_sources(&held);
        let actual = reported_sources(&router, case, &name).await;
        assert_eq!(
            actual, still_held,
            "case {case:?}: expected the group to still hold {still_held:?}, got {actual:?}"
        );
    }
}

/// A traffic source a first detach removed is no longer part of the group, so detaching it
/// again is refused exactly as one that was never attached is — and the attachment the first
/// call left in place is still there.
#[tokio::test]
async fn detaching_an_already_detached_traffic_source_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    let held = sources_at(&seeded, &[1, 3]);
    let name = "asg-traffic-detach-twice";
    create_group(&router, name, &subnets).await;
    attach(&router, "the two sources detached twice", name, &held).await;

    let first = vec![source(&seeded, 1)];
    let (status, code, message) = detach(&router, name, &first).await;
    assert_eq!(
        status, 200,
        "the first detach must succeed, got {status} with {code:?} {message:?}"
    );

    for (case, requested) in [
        ("the detached traffic source alone", first.clone()),
        (
            "the detached traffic source beside the one still attached",
            sources_at(&seeded, &[3, 1]),
        ),
    ] {
        let (status, code, message) = detach(&router, name, &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        let expected = unattached_message(&[source(&seeded, 1).0]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        let still_held = expected_sources(&sources_at(&seeded, &[3]));
        let actual = reported_sources(&router, case, name).await;
        assert_eq!(
            actual, still_held,
            "case {case:?}: expected the group to still hold {still_held:?}, got {actual:?}"
        );
    }
}

/// The documented per-call bound: a call naming one traffic source more than the maximum is
/// refused without detaching anything, and one naming exactly the maximum is served.
#[tokio::test]
async fn a_detach_past_the_documented_per_call_maximum_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    let name = "asg-traffic-detach-bound";
    create_group(&router, name, &subnets).await;
    // The group holds one more than a single call may name, attached in two calls of at most
    // the per-call bound each.
    let held = sources_at(
        &seeded,
        &(1..=MAX_TRAFFIC_SOURCES_PER_CALL + 1).collect::<Vec<usize>>(),
    );
    attach(&router, "the first ten sources", name, &held[..MAX_TRAFFIC_SOURCES_PER_CALL]).await;
    attach(&router, "the eleventh source", name, &held[MAX_TRAFFIC_SOURCES_PER_CALL..]).await;

    // One past the bound: refused, and every attachment survives.
    let (status, code, message) = detach(&router, name, &held).await;
    assert_eq!(
        status, 400,
        "a detach of {} traffic sources must be refused, got {status} with {message:?}",
        held.len()
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code past the bound, got {code:?} {message:?}"
    );
    let expected = format!(
        "Trying to update too many Target Groups at once. The limit is \
         {MAX_TRAFFIC_SOURCES_PER_CALL}"
    );
    assert_eq!(
        message, expected,
        "expected the message {expected:?} past the bound, got {message:?}"
    );
    let still_held = expected_sources(&held);
    let actual = reported_sources(&router, "one past the bound", name).await;
    assert_eq!(
        actual, still_held,
        "expected every attachment to survive the refusal, got {actual:?}"
    );

    // AT the bound: served, and only the source the call did not name is left.
    let at_bound = held[..MAX_TRAFFIC_SOURCES_PER_CALL].to_vec();
    let (status, code, message) = detach(&router, name, &at_bound).await;
    assert_eq!(
        status, 200,
        "a detach of {MAX_TRAFFIC_SOURCES_PER_CALL} traffic sources must succeed, got {status} \
         with {code:?} {message:?}"
    );
    let left = expected_sources(&held[MAX_TRAFFIC_SOURCES_PER_CALL..]);
    let actual = reported_sources(&router, "at the bound", name).await;
    assert_eq!(
        actual, left,
        "expected only the traffic source the call did not name to be left, got {actual:?}"
    );
}

/// A detach naming a group the account does not hold is refused like every other group-scoped
/// mutation of this service, and names the group that is missing.
#[tokio::test]
async fn a_detach_of_an_absent_group_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    let present = "asg-traffic-present";
    let held = sources_at(&seeded, &[1]);
    create_group(&router, present, &subnets).await;
    attach(&router, "the source of the present group", present, &held).await;

    for name in ["asg-traffic-absent", "ASG-Traffic-Present", "asg-traffic-present-but-longer"] {
        let (status, code, message) = detach(&router, name, &held).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {name:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        // The absent-group refusal this operation answers with: the fixed sentence whose detail
        // field is left unfilled, so it travels as the literal token `null` and no part of the
        // request appears in it.
        let expected = "AutoScalingGroup name not found - null";
        assert_eq!(
            message, expected,
            "case {name:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The group that DOES exist still holds its attachment: the refusals above touched nothing.
    let expected = expected_sources(&held);
    let actual = reported_sources(&router, "the present group", present).await;
    assert_eq!(
        actual, expected,
        "expected the stored group to keep its traffic source, got {actual:?}"
    );
}

/// The instance ids Elastic Load Balancing v2 reports as registered with `arn`, sorted — read
/// through its own `DescribeTargetHealth`, the service that owns the registration.
async fn registered_ids(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    let mut ids: Vec<String> = list_members(&body, "TargetHealthDescriptions")
        .into_iter()
        .map(|member| tag_value(member, "Id"))
        .collect();
    ids.sort();
    ids
}

/// Read the target group until it registers exactly `wanted`, or a generous deadline passes —
/// the CONDITION, never a fixed wait.
async fn settled_registrations(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = registered_ids(router, arn).await;
        if held == wanted || Instant::now() >= deadline {
            assert_eq!(
                held, wanted,
                "case {case:?}: expected {arn:?} to register {wanted:?}, got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ids of the instances the group reports, sorted — what every target group it is attached
/// to must hold a target for.
async fn group_instance_ids(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    let mut ids: Vec<String> = list_members(&body, "Instances")
        .into_iter()
        .map(|member| tag_value(member, "InstanceId"))
        .collect();
    ids.sort();
    ids
}

/// Read the group until it reports `wanted` instances, or a generous deadline passes.
async fn settled_instance_ids(
    router: &MultiServiceRouter,
    name: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let ids = group_instance_ids(router, name).await;
        if ids.len() == wanted || Instant::now() >= deadline {
            assert_eq!(
                ids.len(),
                wanted,
                "expected {name:?} to hold {wanted} instance(s), got {ids:?}"
            );
            return ids;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The detach is BACKED: a traffic source the group no longer holds deregisters the group's
/// instances from it — "while deregistering the instances in the group" — read back from the
/// service that owns the registration, while the attachment the call left in place keeps its
/// own registrations and the instances keep running.
#[tokio::test]
async fn a_detached_traffic_source_deregisters_the_groups_instances() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let seeded = seeded_sources(&router, &vpc_id).await;

    let name = "asg-traffic-detach-registrations";
    create_sized_group(&router, name, &subnets, 1).await;
    let held = sources_at(&seeded, &[1, 2]);
    attach(&router, "the two target groups", name, &held).await;

    let instances = settled_instance_ids(&router, name, 1).await;
    let (detached, _) = source(&seeded, 1);
    let (kept, _) = source(&seeded, 2);
    settled_registrations(&router, "before the detach", &detached, &instances).await;
    settled_registrations(&router, "before the detach", &kept, &instances).await;

    let (status, code, message) = detach(&router, name, &sources_at(&seeded, &[1])).await;
    assert_eq!(
        status, 200,
        "the detach must succeed, got {status} with {code:?} {message:?}"
    );

    settled_registrations(&router, "after the detach", &detached, &[]).await;
    settled_registrations(&router, "after the detach", &kept, &instances).await;

    // "The instances continue to run": the group still holds the very instances it did.
    let after = group_instance_ids(&router, name).await;
    assert_eq!(
        after, instances,
        "expected the group to keep its instances, got {after:?}"
    );
}
