// SPDX-License-Identifier: CC0-1.0

//! `DescribeTrafficSources`: the traffic sources an Auto Scaling group is attached to, and
//! the STATE each of those attachments is in.
//!
//! The reader of the OTHER member that carries the same association as the one the parent
//! suite pins, so it is that suite's own concern, in its own file. The rules under test:
//!
//! 1. The read answers exactly the traffic sources the named group holds, in the order it
//!    came to hold them, each one once, carrying its identifier (under `Identifier` and
//!    under the deprecated member it replaced — "this is replaced by `Identifier`",
//!    `TrafficSourceState$TrafficSource`), the type the association is held under, and the
//!    state its REGISTRATIONS put it in: `Adding` while an instance of the group is not
//!    registered with the load balancer or target group yet, `Added` once all of them are
//!    (which a group holding no instance satisfies with none)
//!    (`smithy_query(query="shape",shape="TrafficSourceState")`).
//! 2. `TrafficSourceType` SELECTS among what the group holds and changes nothing else about
//!    the answer: "if you provide a traffic source type, then the results only include that
//!    traffic source type. If you do not provide a traffic source type, then the results
//!    include all the traffic sources for the specified Auto Scaling group".
//! 3. The answer is PAGED: a page holds at most the `MaxRecords` asked for, within this
//!    read's own documented ceiling ("the maximum number of items to return with this call.
//!    The maximum value is 50", `DescribeTrafficSourcesRequest$MaxRecords`), and walking the
//!    `NextToken`s to the end yields every held source exactly once, in that same order.
//! 4. A request that cannot be served is refused with HTTP 400 `ValidationError` carrying
//!    the service's own sentence, and the group's attachments are left exactly as they were.
//! 5. ONE association, ONE state: an `elbv2` traffic source that the group also names under
//!    its own `TargetGroupARNs` reads the same state through both readers, because a client
//!    must not read two states for one attachment.
//!
//! The tables span what each rule generalises over: which type an association is held under
//! and how many of each the group holds, which type the read selects (including one the
//! group holds none of), how many instances owe a registration, the page size asked for
//! (none, every size up to more than the group holds, and both published bounds), and the
//! ways a request can be unusable.

use std::time::Instant;

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, p, query, seed_subnets, tag_value, tag_values, xml_unescape, SeededSubnet, REGION,
};

use super::{
    attachments, create_group, create_launch_template, create_target_group, emulator, poll_budget,
    settled_instance_count, vpc_of, ReportedAttachment, HEALTH_CHECK_EC2, POLL_INTERVAL,
    STATE_ADDED, STATE_ADDING,
};

/// The three `TrafficSourceIdentifier$Type` values an association can be held under, from
/// the model's own catalog for the member
/// (`smithy_query(query="shape",shape="TrafficSourceIdentifier")`): `elb` for the name of a
/// Classic Load Balancer, `elbv2` for an Elastic Load Balancing target group ARN and
/// `vpc-lattice` for a VPC Lattice target group ARN.
const TYPE_ELB: &str = "elb";
const TYPE_ELBV2: &str = "elbv2";
const TYPE_VPC_LATTICE: &str = "vpc-lattice";

/// The Classic Load Balancer an `elb` source of this file names: "for Classic Load
/// Balancers, this will be the name of the Classic Load Balancer in this account and Region"
/// (`TrafficSourceIdentifier$Identifier`), whose own documented example this is.
const CLASSIC_LOAD_BALANCER: &str = "my-classic-load-balancer";

/// The largest page this read serves, which is also the page a request asking for none
/// takes: "the maximum number of items to return with this call. The maximum value is 50"
/// (`DescribeTrafficSourcesRequest$MaxRecords`). Written out here rather than imported, so a
/// wrong ceiling in the handler cannot make a row pass.
const MAX_RECORDS_CEILING: i32 = 50;

/// The port the target groups of this file listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// The launch template the groups of this file launch from.
const TEMPLATE: &str = "lt-traffic-sources";

/// The ARN of a VPC Lattice target group in this account and Region — the form a
/// `vpc-lattice` source names ("VPC Lattice ARN:
/// `arn:aws:vpc-lattice:us-west-2:123456789012:targetgroup/tg-1234567890123456`",
/// `TrafficSourceIdentifier$Identifier`).
fn lattice_target_group(id: &str) -> String {
    engine::emulator_arn("vpc-lattice", REGION, &format!("targetgroup/{id}"))
}

/// One traffic source as a client reads it off the response.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedTrafficSource {
    identifier: String,
    source_type: String,
    state: String,
}

/// The source `identifier`, held under `source_type`, as a group owing no registration
/// reports it: "all Auto Scaling instances are registered with the load balancer or target
/// group", which a group holding no instance satisfies with none.
fn added(identifier: &str, source_type: &str) -> ReportedTrafficSource {
    ReportedTrafficSource {
        identifier: identifier.to_string(),
        source_type: source_type.to_string(),
        state: STATE_ADDED.to_string(),
    }
}

/// Post one `AttachTrafficSources` holding `sources`, each an `(Identifier, Type)` pair, and
/// answer its status and body.
async fn attach_traffic_sources(
    router: &MultiServiceRouter,
    name: &str,
    sources: &[(String, &str)],
) -> (u16, String) {
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
    query(router, "AttachTrafficSources", &borrowed(&params)).await
}

/// Attach `sources` to `name` and require the call to be served: what a row sets up rather
/// than what it asserts.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    sources: &[(String, &str)],
) {
    let (status, body) = attach_traffic_sources(router, name, sources).await;
    assert_eq!(
        status, 200,
        "case {case:?}: attaching {sources:?} to {name:?} expected 200, got {status}: {body}"
    );
}

/// Post one `DescribeTrafficSources` and answer its status and body.
async fn describe_traffic_sources(
    router: &MultiServiceRouter,
    name: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    params.extend(extra.iter().cloned());
    query(router, "DescribeTrafficSources", &borrowed(&params)).await
}

/// The traffic sources the group reports, in the order reported, plus the `NextToken` the
/// page carries (empty when the response holds none).
async fn traffic_sources_page(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &[(String, String)],
) -> (Vec<ReportedTrafficSource>, String) {
    let (status, body) = describe_traffic_sources(router, name, extra).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the traffic sources of {name:?} expected 200, got {status}: {body}"
    );
    let reported = tag_values(&body, "member")
        .into_iter()
        .map(|member| {
            let identifier = tag_value(&member, "Identifier");
            // "This is replaced by `Identifier`" (`TrafficSourceState$TrafficSource`): the
            // deprecated member carries the same value, so a client reading the older member
            // reads the same association rather than nothing.
            let deprecated = tag_value(&member, "TrafficSource");
            assert_eq!(
                deprecated, identifier,
                "case {case:?}: the deprecated member must carry the identifier, got {member:?}"
            );
            ReportedTrafficSource {
                identifier,
                source_type: tag_value(&member, "Type"),
                state: tag_value(&member, "State"),
            }
        })
        .collect();
    (reported, tag_value(&body, "NextToken"))
}

/// Read the group's traffic sources until every one of them is reported in `wanted`, or a
/// generous deadline passes — the CONDITION, never a fixed wait. Answers what it last read.
async fn settled_traffic_sources(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Vec<ReportedTrafficSource> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (reported, _) = traffic_sources_page(router, case, name, &[]).await;
        let settled = !reported.is_empty() && reported.iter().all(|source| source.state == wanted);
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every traffic source of {name:?} to be reported \
                 {wanted:?}, got {reported:?}"
            );
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one group holding no capacity, attached to nothing yet: a group that owes no
/// registration, so the state of every source it comes to hold is the one an attachment with
/// nothing outstanding is in.
async fn create_empty_group(router: &MultiServiceRouter, subnets: &[SeededSubnet], name: &str) {
    create_group(router, name, TEMPLATE, subnets, 0, &[], HEALTH_CHECK_EC2).await;
}

/// RULES 1 and 2: every source the group holds is reported once, in attachment order, with
/// its type and its state — and `TrafficSourceType` narrows that answer to one type without
/// changing anything else about it. Reading again answers the same thing: the read moves
/// nothing.
#[tokio::test]
async fn every_held_traffic_source_is_reported_and_the_type_selects_among_them() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;

    let first = create_target_group(&router, "tg-traffic-read-1", TARGET_GROUP_PORT, &vpc_id).await;
    let second = create_target_group(&router, "tg-traffic-read-2", TARGET_GROUP_PORT, &vpc_id).await;
    let lattice = lattice_target_group("tg-0123456789abcdef0");

    let mixed = "asg-traffic-read-mixed";
    create_empty_group(&router, &subnets, mixed).await;
    // TWO calls, so the order reported is the order the group CAME to hold the sources rather
    // than the order of any one request or a sort of the identifiers.
    attach(
        &router,
        "the first two sources of the mixed group",
        mixed,
        &[
            (first.clone(), TYPE_ELBV2),
            (CLASSIC_LOAD_BALANCER.to_string(), TYPE_ELB),
        ],
    )
    .await;
    attach(
        &router,
        "the next two sources of the mixed group",
        mixed,
        &[
            (lattice.clone(), TYPE_VPC_LATTICE),
            (second.clone(), TYPE_ELBV2),
        ],
    )
    .await;

    let classic_only = "asg-traffic-read-classic";
    create_empty_group(&router, &subnets, classic_only).await;
    attach(
        &router,
        "the only source of the classic group",
        classic_only,
        &[(CLASSIC_LOAD_BALANCER.to_string(), TYPE_ELB)],
    )
    .await;

    // (case, the group read, the type the read selects, what it must then report)
    let rows: Vec<(&str, &str, Option<&str>, Vec<ReportedTrafficSource>)> = vec![
        (
            "no type at all, so every source the group holds",
            mixed,
            None,
            vec![
                added(&first, TYPE_ELBV2),
                added(CLASSIC_LOAD_BALANCER, TYPE_ELB),
                added(&lattice, TYPE_VPC_LATTICE),
                added(&second, TYPE_ELBV2),
            ],
        ),
        (
            "the two elbv2 sources, in the order the group holds them",
            mixed,
            Some(TYPE_ELBV2),
            vec![added(&first, TYPE_ELBV2), added(&second, TYPE_ELBV2)],
        ),
        (
            "the one elb source",
            mixed,
            Some(TYPE_ELB),
            vec![added(CLASSIC_LOAD_BALANCER, TYPE_ELB)],
        ),
        (
            "the one vpc-lattice source",
            mixed,
            Some(TYPE_VPC_LATTICE),
            vec![added(&lattice, TYPE_VPC_LATTICE)],
        ),
        (
            "a type the group holds none of",
            classic_only,
            Some(TYPE_VPC_LATTICE),
            Vec::new(),
        ),
        (
            "no type at all on a group holding one source",
            classic_only,
            None,
            vec![added(CLASSIC_LOAD_BALANCER, TYPE_ELB)],
        ),
    ];

    for (case, group, source_type, expected) in rows {
        let extra: Vec<(String, String)> = source_type
            .map(|wanted| vec![p("TrafficSourceType", wanted)])
            .unwrap_or_default();
        let (reported, token) = traffic_sources_page(&router, case, group, &extra).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
        assert!(
            token.is_empty(),
            "case {case:?}: a page holding every selected source must carry no NextToken, got \
             {token:?}"
        );
        // The read moves nothing, so the same request answers the same thing.
        let (again, _) = traffic_sources_page(&router, case, group, &extra).await;
        assert_eq!(
            again, reported,
            "case {case:?}: a second read must answer the same thing, got {again:?}"
        );
    }
}

/// The page sizes this suite walks the traffic sources with, and the type each row selects:
/// absent (the read's own default), every size from one item to more than the group holds,
/// the published ceiling, and one row paging a SELECTION one item at a time.
fn page_rows() -> Vec<(&'static str, Option<i32>, Option<&'static str>)> {
    vec![
        ("no MaxRecords at all", None, None),
        ("one source per page", Some(1), None),
        ("two sources per page", Some(2), None),
        ("exactly as many as the group holds", Some(4), None),
        ("the published ceiling", Some(MAX_RECORDS_CEILING), None),
        (
            "the elbv2 selection, one source per page",
            Some(1),
            Some(TYPE_ELBV2),
        ),
    ]
}

/// RULE 3: every page size walks to the end yielding each selected source exactly once, in
/// attachment order, with no page larger than asked for and no `NextToken` on the last page.
#[tokio::test]
async fn the_traffic_sources_are_paged_by_max_records() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;

    let first = create_target_group(&router, "tg-traffic-page-1", TARGET_GROUP_PORT, &vpc_id).await;
    let second = create_target_group(&router, "tg-traffic-page-2", TARGET_GROUP_PORT, &vpc_id).await;
    let lattice = lattice_target_group("tg-00112233445566778");

    let name = "asg-traffic-pages";
    create_empty_group(&router, &subnets, name).await;
    attach(
        &router,
        "the sources this suite pages over",
        name,
        &[
            (first.clone(), TYPE_ELBV2),
            (CLASSIC_LOAD_BALANCER.to_string(), TYPE_ELB),
            (lattice.clone(), TYPE_VPC_LATTICE),
            (second.clone(), TYPE_ELBV2),
        ],
    )
    .await;

    for (case, max_records, selected) in page_rows() {
        let expected: Vec<ReportedTrafficSource> = match selected {
            Some(TYPE_ELBV2) => vec![added(&first, TYPE_ELBV2), added(&second, TYPE_ELBV2)],
            Some(other) => panic!("case {case:?}: no row selects {other:?}"),
            None => vec![
                added(&first, TYPE_ELBV2),
                added(CLASSIC_LOAD_BALANCER, TYPE_ELB),
                added(&lattice, TYPE_VPC_LATTICE),
                added(&second, TYPE_ELBV2),
            ],
        };
        let limit = usize::try_from(
            max_records
                .unwrap_or(MAX_RECORDS_CEILING)
                .min(MAX_RECORDS_CEILING),
        )
        .expect("a page size the request could carry");

        let mut walked: Vec<ReportedTrafficSource> = Vec::new();
        let mut token = String::new();
        let mut pages = 0;
        loop {
            let mut extra: Vec<(String, String)> = Vec::new();
            if let Some(max_records) = max_records {
                extra.push(p("MaxRecords", max_records.to_string()));
            }
            if let Some(selected) = selected {
                extra.push(p("TrafficSourceType", selected));
            }
            if !token.is_empty() {
                extra.push(p("NextToken", token.clone()));
            }
            let (page, next) = traffic_sources_page(&router, case, name, &extra).await;
            pages += 1;
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: paging must terminate, walked {pages} page(s) of {walked:?}"
            );
            assert!(
                page.len() <= limit,
                "case {case:?}: a page must hold at most {limit} source(s), got {page:?}"
            );
            walked.extend(page);
            if next.is_empty() {
                break;
            }
            token = next;
        }
        assert_eq!(
            walked, expected,
            "case {case:?}: walking the pages must yield every selected source once, in \
             attachment order, got {walked:?}"
        );
    }
}

/// One unusable request: what it carries beyond the group name, and the refusal it must
/// answer with.
struct RefusalRow {
    case: &'static str,
    group: String,
    extra: Vec<(String, String)>,
    status: u16,
    code: &'static str,
    message: String,
}

/// The ways a request to this read can be unusable: a group the account does not hold, a
/// page size outside the bounds the member documents (one below the floor and one past the
/// ceiling), a token this service never issued, and a token naming a page past the sources
/// the group holds. Every sentence is written out here rather than taken from the crate.
fn refusal_rows(stored_group: &str) -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a group that does not exist",
            group: "asg-traffic-absent".to_string(),
            extra: Vec::new(),
            status: 400,
            code: "ValidationError",
            message: "AutoScalingGroup name not found - Could not find Auto Scaling group \
                      named 'asg-traffic-absent'"
                .to_string(),
        },
        RefusalRow {
            case: "a page size of none at all",
            group: stored_group.to_string(),
            extra: vec![p("MaxRecords", "0")],
            status: 400,
            code: "ValidationError",
            message: "1 validation error detected: Value '0' at 'maxRecords' failed to satisfy \
                      constraint: Member must have value greater than or equal to 1"
                .to_string(),
        },
        RefusalRow {
            case: "a page size one past the documented maximum",
            group: stored_group.to_string(),
            extra: vec![p("MaxRecords", "51")],
            status: 400,
            code: "ValidationError",
            message: "1 validation error detected: Value '51' at 'maxRecords' failed to satisfy \
                      constraint: Member must have value less than or equal to 50"
                .to_string(),
        },
        RefusalRow {
            case: "a token this service never issued",
            group: stored_group.to_string(),
            extra: vec![p("NextToken", "not-a-token")],
            status: 400,
            code: "ValidationError",
            message: "NextToken: not-a-token".to_string(),
        },
        RefusalRow {
            case: "a token naming a page past the sources the group holds",
            group: stored_group.to_string(),
            extra: vec![p("NextToken", "next-9")],
            status: 400,
            code: "ValidationError",
            message: "NextToken: next-9".to_string(),
        },
    ]
}

/// RULE 4: each unusable request is refused with its own status, wire code and whole
/// message; each page size ON the published bounds is served; and the refusals leave the
/// group's own attachments exactly where they were.
#[tokio::test]
async fn an_unusable_request_is_refused_and_the_bounds_are_served() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;

    let target_group =
        create_target_group(&router, "tg-traffic-refusal", TARGET_GROUP_PORT, &vpc_id).await;
    let lattice = lattice_target_group("tg-aabbccddeeff00112");
    let name = "asg-traffic-refusal";
    create_empty_group(&router, &subnets, name).await;
    let held = vec![
        (target_group.clone(), TYPE_ELBV2),
        (CLASSIC_LOAD_BALANCER.to_string(), TYPE_ELB),
        (lattice.clone(), TYPE_VPC_LATTICE),
    ];
    attach(&router, "the sources the refusals must not touch", name, &held).await;
    let expected: Vec<ReportedTrafficSource> = vec![
        added(&target_group, TYPE_ELBV2),
        added(CLASSIC_LOAD_BALANCER, TYPE_ELB),
        added(&lattice, TYPE_VPC_LATTICE),
    ];

    for row in refusal_rows(name) {
        let (status, body) = describe_traffic_sources(&router, &row.group, &row.extra).await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected HTTP {}, got {status}: {body}",
            row.case, row.status
        );
        let code = tag_value(&body, "Code");
        assert_eq!(
            code, row.code,
            "case {:?}: expected the {} code, got {code:?}: {body}",
            row.case, row.code
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, row.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            row.case, row.message
        );
    }

    // Both published bounds of the page size are SERVED: the floor answers one source and a
    // token to resume from, the ceiling answers every source the group holds.
    let (floor, floor_token) = traffic_sources_page(
        &router,
        "a page size of one, the documented floor",
        name,
        &[p("MaxRecords", "1")],
    )
    .await;
    assert_eq!(
        floor,
        expected[..1].to_vec(),
        "a page size of one must answer the first source alone, got {floor:?}"
    );
    assert!(
        !floor_token.is_empty(),
        "a page that stops short of the sources held must carry a NextToken, got none"
    );
    let (ceiling, ceiling_token) = traffic_sources_page(
        &router,
        "a page size at the documented ceiling",
        name,
        &[p("MaxRecords", MAX_RECORDS_CEILING.to_string())],
    )
    .await;
    assert_eq!(
        ceiling, expected,
        "a page size at the ceiling must answer every source the group holds, got {ceiling:?}"
    );
    assert!(
        ceiling_token.is_empty(),
        "the last page must carry no NextToken, got {ceiling_token:?}"
    );

    // And the refused requests wrote nothing: the group holds exactly what it did.
    let (reported, _) = traffic_sources_page(&router, "after the refusals", name, &[]).await;
    assert_eq!(
        reported, expected,
        "the refused requests must leave the attachments of {name:?} in place, got {reported:?}"
    );
}

/// RULE 1's state half: a source whose registrations are NOT in place is reported `Adding`
/// while a group owing none reports `Added` — the state follows the registrations the
/// attachment owes rather than anything stored beside it.
#[tokio::test]
async fn a_source_is_reported_adding_until_its_registrations_are_in_place() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    create_launch_template(&router, TEMPLATE).await;

    // (case, how many instances the group holds, the state its source must be reported in)
    // The source is a VPC Lattice target group, which no emulated service holds a
    // registration in, so an instance of the group leaves a registration outstanding.
    let rows: Vec<(&str, usize, &str)> = vec![
        ("a group that holds no instance owes no registration", 0, STATE_ADDED),
        ("a group whose instance is not registered yet", 1, STATE_ADDING),
    ];

    for (row, (case, desired, expected)) in rows.into_iter().enumerate() {
        let name = format!("asg-traffic-state-{row}");
        create_group(
            &router,
            &name,
            TEMPLATE,
            &subnets,
            desired,
            &[],
            HEALTH_CHECK_EC2,
        )
        .await;
        let lattice = lattice_target_group(&format!("tg-state-{row}"));
        attach(&router, case, &name, &[(lattice.clone(), TYPE_VPC_LATTICE)]).await;
        settled_instance_count(&router, case, &name, desired).await;

        let reported = settled_traffic_sources(&router, case, &name, expected).await;
        assert_eq!(
            reported,
            vec![ReportedTrafficSource {
                identifier: lattice.clone(),
                source_type: TYPE_VPC_LATTICE.to_string(),
                state: expected.to_string(),
            }],
            "case {case:?}: expected the source to be reported {expected:?}, got {reported:?}"
        );
    }
}

/// RULE 5: one association, one state. A target group the group names BOTH under its own
/// `TargetGroupARNs` and as an `elbv2` traffic source reads the same state through both
/// readers, once the registration its instance owes is in place.
#[tokio::test]
async fn one_association_reads_one_state_through_both_readers() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;

    let case = "a target group named under both members";
    let arn = create_target_group(&router, "tg-traffic-both", TARGET_GROUP_PORT, &vpc_id).await;
    let name = "asg-traffic-both";
    create_group(
        &router,
        name,
        TEMPLATE,
        &subnets,
        1,
        std::slice::from_ref(&arn),
        HEALTH_CHECK_EC2,
    )
    .await;
    attach(&router, case, name, &[(arn.clone(), TYPE_ELBV2)]).await;
    settled_instance_count(&router, case, name, 1).await;

    let sources = settled_traffic_sources(&router, case, name, STATE_ADDED).await;
    assert_eq!(
        sources,
        vec![ReportedTrafficSource {
            identifier: arn.clone(),
            source_type: TYPE_ELBV2.to_string(),
            state: STATE_ADDED.to_string(),
        }],
        "case {case:?}: expected the traffic-source reader to report the attachment once in the \
         state its registration put it in, got {sources:?}"
    );
    let reported = attachments(&router, case, name).await;
    assert_eq!(
        reported,
        vec![ReportedAttachment {
            arn: arn.clone(),
            state: STATE_ADDED.to_string(),
        }],
        "case {case:?}: expected the load-balancer reader to report the same state for the same \
         attachment, got {reported:?}"
    );
}
