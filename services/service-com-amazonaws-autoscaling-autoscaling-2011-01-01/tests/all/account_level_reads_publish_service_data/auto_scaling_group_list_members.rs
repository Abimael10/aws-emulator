// SPDX-License-Identifier: CC0-1.0

//! The LIST members a read of an Auto Scaling group carries, over the awsQuery wire.
//!
//! THE RULE UNDER TEST: a list-typed member of the group a read reports is answered as a
//! LIST — named by the answer, carrying exactly what the group holds, and named and EMPTY
//! when the group holds nothing in it — never left out and never answered as a null. Every
//! collection of the reported group is one: `AvailabilityZones`, `AvailabilityZoneIds`,
//! `LoadBalancerNames`, `TargetGroupARNs`, `Instances`, `SuspendedProcesses`,
//! `EnabledMetrics`, `Tags`, `TerminationPolicies` and `TrafficSources`
//! (`smithy_query(query="shape",shape="AutoScalingGroup")`). A client that iterates one of
//! them reads an empty sequence off a group that holds nothing rather than failing on a
//! member that is not there, and "attached to nothing" is distinguishable from "not
//! reported".
//!
//! The rows span what the rule generalises over: WHICH collection, HOW MANY entries the
//! group holds in it (none, one, several), HOW the entries got there (the create, or a later
//! suspend, enable or attach-shaped member), and whether they were taken away again — a
//! collection emptied by a later call reads back as an empty list, not as an absent member.
//! The reads span the ways one answer carries a group: a describe naming the group, one
//! naming none so every group of the account comes back together, and each page of a
//! one-group-per-page pagination.
//!
//! Auto Scaling, Amazon EC2 and Elastic Load Balancing v2 are mounted on ONE engine and
//! driven over their own wires, so the subnets, launch template and target groups these
//! groups are built from are the ones a client created. Every wait looks AGAIN until the
//! condition holds or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, create_target_group, emulator, p, query, seed_subnets,
    tag_value, tag_values, vpc_of,
};

/// Every list-typed member of the group a read reports, from the model's own definition of
/// the shape (`smithy_query(query="shape",shape="AutoScalingGroup")`): the ten members whose
/// target is a list.
const LIST_MEMBERS: &[&str] = &[
    "AvailabilityZones",
    "AvailabilityZoneIds",
    "LoadBalancerNames",
    "TargetGroupARNs",
    "Instances",
    "SuspendedProcesses",
    "EnabledMetrics",
    "Tags",
    "TerminationPolicies",
    "TrafficSources",
];

/// The launch template every group of this suite launches from, created over Amazon EC2's
/// own wire so the groups launch from a template the account really holds.
const TEMPLATE: &str = "lt-group-lists";

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` whose `Identifier` is an Elastic Load Balancing v2
/// target group ARN (`smithy_query(query="doc",shape="TrafficSourceIdentifier",member="Type")`).
const TRAFFIC_SOURCE_TYPE_ELBV2: &str = "elbv2";

/// The `ResourceType` a tag of an Auto Scaling group is written and deleted under
/// (`smithy_query(query="doc",shape="Tag",member="ResourceType")`: "the only supported value
/// is `auto-scaling-group`").
const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// The termination policy a group that names none is created with, which is why
/// `TerminationPolicies` is a NON-empty list on every row here
/// (`smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="TerminationPolicies")`:
/// the default policy is named `Default`).
const DEFAULT_TERMINATION_POLICY: &str = "Default";

/// How long a wait looks again before it is called a failure. A BACKSTOP, not a schedule.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// One row: how many entries the row's group holds in each of its collections.
struct Row {
    case: &'static str,
    group: &'static str,
    count: usize,
}

/// None, one, several — the counts the rule generalises over on the group side.
const ROWS: &[Row] = &[
    Row {
        case: "a group holding nothing in any of its collections",
        group: "lists-none",
        count: 0,
    },
    Row {
        case: "a group holding one entry in each of its collections",
        group: "lists-one",
        count: 1,
    },
    Row {
        case: "a group holding several entries in each of its collections",
        group: "lists-many",
        count: 2,
    },
];

/// What a row's group was GIVEN, kept so the read-back is compared against what a client
/// asked for rather than against the answer itself.
#[derive(Debug, Default, Clone)]
struct Given {
    load_balancer_names: Vec<String>,
    target_group_arns: Vec<String>,
    tag_keys: Vec<String>,
    processes: Vec<String>,
    metrics: Vec<String>,
    instances: usize,
}

/// The account the rows are read from: one emulator, the zones its groups are placed in, and
/// what each row's group was given.
struct Account {
    router: MultiServiceRouter,
    zones: Vec<String>,
    given: Vec<Given>,
}

/// The XML inside the first element named `tag`: `Some("")` for an element with nothing in
/// it, however the wire spells it (`<Tag/>` or `<Tag></Tag>`), and `None` when the document
/// does not name the element at all.
///
/// The difference between an EMPTY member and an ABSENT one is the whole subject of this
/// suite, so the two are read apart here rather than collapsed into a string. Matching is
/// attribute-tolerant, because the envelope stamps the document's namespace onto the first
/// element of a list, and name-exact, so `AvailabilityZones` never reads
/// `AvailabilityZoneIds`.
fn element_inner<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    loop {
        let at = rest.find(&open)?;
        let after = &rest[at + open.len()..];
        if !(after.starts_with('>') || after.starts_with('/') || after.starts_with(char::is_whitespace))
        {
            // A longer element name that merely begins like this one.
            rest = after;
            continue;
        }
        let open_end = after.find('>')?;
        if after[..open_end].ends_with('/') {
            return Some("");
        }
        let inner = &after[open_end + 1..];
        let end = inner.find(&close)?;
        return Some(&inner[..end]);
    }
}

/// How many times `xml` names the element `tag` — one per group when every group an answer
/// reports carries the member.
fn element_count(xml: &str, tag: &str) -> usize {
    let open = format!("<{tag}");
    let mut count = 0;
    let mut rest = xml;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        if after.starts_with('>') || after.starts_with('/') || after.starts_with(char::is_whitespace)
        {
            count += 1;
        }
        rest = after;
    }
    count
}

/// The `<member>…</member>` entries of the list member `tag`, or `None` when the answer does
/// not name that member at all.
fn entries(xml: &str, tag: &str) -> Option<Vec<String>> {
    element_inner(xml, tag).map(|inner| tag_values(inner, "member"))
}

/// The entries of the list member `tag`, failing the case when the member is ABSENT — which
/// is the defect this suite exists for, so it is named rather than read as empty.
fn list_member(case: &str, xml: &str, tag: &str) -> Vec<String> {
    match entries(xml, tag) {
        Some(found) => found,
        None => panic!(
            "case {case:?}: expected the described group to name {tag} as a list, got {xml}"
        ),
    }
}

/// `values` in ASCII order, so a case asserts the SET a member carries where the order it
/// carries them in is not what that case is about.
fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

/// Post one `DescribeAutoScalingGroups` and answer its body, failing the case when the read
/// itself is refused.
async fn describe(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) -> String {
    let (status, body) = query(router, "DescribeAutoScalingGroups", &borrowed(params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    body
}

/// The answer of a describe naming one group.
async fn described(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    describe(router, case, &[p("AutoScalingGroupNames.member.1", group)]).await
}

/// The scaling processes the service publishes as its own catalog, read over the wire so no
/// row here writes that membership out.
async fn published_processes(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeScalingProcessTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published process catalog expected 200, got {status}: {body}"
    );
    let catalog = tag_values(&body, "ProcessName");
    assert!(
        catalog.len() > 1,
        "the published process catalog must name more than one process, got {catalog:?}"
    );
    catalog
}

/// The group metrics the service publishes, and the granularity it aggregates them at, read
/// over the wire for the same reason.
async fn published_metrics(router: &MultiServiceRouter) -> (Vec<String>, String) {
    let (status, body) = query(router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published metric catalog expected 200, got {status}: {body}"
    );
    let metrics = tag_values(&body, "Metric");
    let granularity = tag_value(&body, "Granularity");
    assert!(
        metrics.len() > 1 && !granularity.is_empty(),
        "the published metric catalog must name more than one metric and a granularity, got \
         {metrics:?} at {granularity:?}"
    );
    (metrics, granularity)
}

/// Look again until the group reports `wanted` instances, so what a case asserts is the
/// membership the clock has settled on rather than a guess about how fast the host is.
async fn instances_settle(router: &MultiServiceRouter, case: &str, group: &str, wanted: usize) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = described(router, case, group).await;
        let held = list_member(case, &body, "Instances");
        if held.len() == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to report {wanted} instance(s), got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Build the account every test here reads: both other services on one engine, two subnets,
/// one launch template, and one group per row holding `row.count` entries in each of its
/// collections — the classic load balancer names and the target groups from the create, the
/// suspensions from `SuspendProcesses` and the metrics from `EnableMetricsCollection`.
async fn account() -> Account {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;
    let processes = published_processes(&router).await;
    let (metrics, granularity) = published_metrics(&router).await;
    let subnet_ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();

    let mut given = Vec::new();
    for row in ROWS {
        let mut holds = Given {
            processes: processes.iter().take(row.count).cloned().collect(),
            metrics: metrics.iter().take(row.count).cloned().collect(),
            instances: row.count,
            ..Given::default()
        };
        for index in 0..row.count {
            holds
                .load_balancer_names
                .push(format!("{}-lb-{index}", row.group));
            holds.tag_keys.push(format!("{}-key-{index}", row.group));
            holds.target_group_arns.push(
                create_target_group(
                    &router,
                    &format!("{}-tg-{index}", row.group),
                    80 + i32::try_from(index).expect("a row's entry count fits in an i32"),
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }

        let mut params = vec![
            p("AutoScalingGroupName", row.group),
            p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", "4"),
            p("DesiredCapacity", row.count.to_string()),
            p("VPCZoneIdentifier", subnet_ids.join(",")),
        ];
        for (index, name) in holds.load_balancer_names.iter().enumerate() {
            params.push(p(
                &format!("LoadBalancerNames.member.{}", index + 1),
                name.clone(),
            ));
        }
        for (index, arn) in holds.target_group_arns.iter().enumerate() {
            params.push(p(
                &format!("TargetGroupARNs.member.{}", index + 1),
                arn.clone(),
            ));
        }
        for (index, key) in holds.tag_keys.iter().enumerate() {
            params.push(p(&format!("Tags.member.{}.Key", index + 1), key.clone()));
            params.push(p(
                &format!("Tags.member.{}.Value", index + 1),
                format!("value-{index}"),
            ));
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        if !holds.processes.is_empty() {
            let mut params = vec![p("AutoScalingGroupName", row.group)];
            for (index, process) in holds.processes.iter().enumerate() {
                params.push(p(
                    &format!("ScalingProcesses.member.{}", index + 1),
                    process.clone(),
                ));
            }
            let (status, body) = query(&router, "SuspendProcesses", &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {:?}: SuspendProcesses expected 200, got {status}: {body}",
                row.case
            );
        }
        if !holds.metrics.is_empty() {
            let mut params = vec![
                p("AutoScalingGroupName", row.group),
                p("Granularity", granularity.as_str()),
            ];
            for (index, metric) in holds.metrics.iter().enumerate() {
                params.push(p(
                    &format!("Metrics.member.{}", index + 1),
                    metric.clone(),
                ));
            }
            let (status, body) = query(&router, "EnableMetricsCollection", &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {:?}: EnableMetricsCollection expected 200, got {status}: {body}",
                row.case
            );
        }
        instances_settle(&router, row.case, row.group, holds.instances).await;
        given.push(holds);
    }

    Account {
        router,
        zones: subnets.iter().map(|subnet| subnet.zone.clone()).collect(),
        given,
    }
}

/// A described group names every list member of the shape, and each one carries exactly what
/// the group holds — nothing, one entry or several — rather than being left out.
#[tokio::test]
async fn every_list_member_of_a_described_group_is_a_list() {
    let account = account().await;

    for (row, holds) in ROWS.iter().zip(&account.given) {
        let case = row.case;
        let body = described(&account.router, case, row.group).await;

        for member in LIST_MEMBERS {
            assert!(
                entries(&body, member).is_some(),
                "case {case:?}: expected the described group to name {member} as a list, got \
                 {body}"
            );
        }

        assert_eq!(
            sorted(list_member(case, &body, "LoadBalancerNames")),
            sorted(holds.load_balancer_names.clone()),
            "case {case:?}: expected LoadBalancerNames to carry {:?}, got {body}",
            holds.load_balancer_names
        );
        assert_eq!(
            sorted(list_member(case, &body, "TargetGroupARNs")),
            sorted(holds.target_group_arns.clone()),
            "case {case:?}: expected TargetGroupARNs to carry {:?}, got {body}",
            holds.target_group_arns
        );
        let suspended: Vec<String> = list_member(case, &body, "SuspendedProcesses")
            .iter()
            .map(|member| tag_value(member, "ProcessName"))
            .collect();
        assert_eq!(
            sorted(suspended),
            sorted(holds.processes.clone()),
            "case {case:?}: expected SuspendedProcesses to carry {:?}, got {body}",
            holds.processes
        );
        let collected: Vec<String> = list_member(case, &body, "EnabledMetrics")
            .iter()
            .map(|member| tag_value(member, "Metric"))
            .collect();
        assert_eq!(
            sorted(collected),
            sorted(holds.metrics.clone()),
            "case {case:?}: expected EnabledMetrics to carry {:?}, got {body}",
            holds.metrics
        );
        let tagged: Vec<String> = list_member(case, &body, "Tags")
            .iter()
            .map(|member| tag_value(member, "Key"))
            .collect();
        assert_eq!(
            sorted(tagged),
            sorted(holds.tag_keys.clone()),
            "case {case:?}: expected Tags to carry {:?}, got {body}",
            holds.tag_keys
        );
        let instances = list_member(case, &body, "Instances");
        assert_eq!(
            instances.len(),
            holds.instances,
            "case {case:?}: expected {} instance(s), got {instances:?}",
            holds.instances
        );
        // The group named no traffic source of its own, but a target group it is attached to
        // IS a traffic source — the same association under its modern spelling — so the
        // member carries one `elbv2` entry per target group the group holds, and the empty
        // list is what a group attached to nothing reports.
        let reported_sources: Vec<String> = list_member(case, &body, "TrafficSources")
            .iter()
            .map(|member| {
                format!(
                    "{}/{}",
                    tag_value(member, "Identifier"),
                    tag_value(member, "Type")
                )
            })
            .collect();
        let expected_sources: Vec<String> = holds
            .target_group_arns
            .iter()
            .map(|arn| format!("{arn}/elbv2"))
            .collect();
        assert_eq!(
            sorted(reported_sources),
            sorted(expected_sources.clone()),
            "case {case:?}: expected TrafficSources to carry {expected_sources:?}, got {body}"
        );
        // The collections the create fills by itself are lists too, and non-empty ones: the
        // zones the group places instances in, their ids, and the termination policy a group
        // naming none is created with.
        assert_eq!(
            sorted(list_member(case, &body, "AvailabilityZones")),
            sorted(account.zones.clone()),
            "case {case:?}: expected AvailabilityZones to carry {:?}, got {body}",
            account.zones
        );
        assert_eq!(
            list_member(case, &body, "AvailabilityZoneIds").len(),
            account.zones.len(),
            "case {case:?}: expected one Availability Zone id per zone, got {body}"
        );
        assert_eq!(
            list_member(case, &body, "TerminationPolicies"),
            vec![DEFAULT_TERMINATION_POLICY.to_string()],
            "case {case:?}: expected the default termination policy, got {body}"
        );
    }
}

/// A group whose attachments are named as TRAFFIC SOURCES rather than as `TargetGroupARNs`
/// reports that collection the same way: a list carrying what it holds, with every other
/// list member still named by the answer.
#[tokio::test]
async fn a_group_reports_its_traffic_sources_as_a_list() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, TEMPLATE).await;
    let case = "a group attached through two elbv2 traffic sources";
    let group = "lists-traffic-sources";

    let mut arns = Vec::new();
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnets[0].id.as_str()),
    ];
    for index in 0..2 {
        let arn = create_target_group(
            &router,
            &format!("lists-ts-{index}"),
            80 + index,
            TARGET_TYPE_INSTANCE,
            &vpc_id,
        )
        .await;
        params.push(p(
            &format!("TrafficSources.member.{}.Identifier", index + 1),
            arn.clone(),
        ));
        params.push(p(
            &format!("TrafficSources.member.{}.Type", index + 1),
            TRAFFIC_SOURCE_TYPE_ELBV2,
        ));
        arns.push(arn);
    }
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let body = described(&router, case, group).await;
    for member in LIST_MEMBERS {
        assert!(
            entries(&body, member).is_some(),
            "case {case:?}: expected the described group to name {member} as a list, got {body}"
        );
    }
    let reported: Vec<String> = list_member(case, &body, "TrafficSources")
        .iter()
        .map(|member| tag_value(member, "Identifier"))
        .collect();
    assert_eq!(
        sorted(reported),
        sorted(arns.clone()),
        "case {case:?}: expected TrafficSources to carry {arns:?}, got {body}"
    );
    // The group is attached to no CLASSIC load balancer, so that collection is the empty
    // list beside the one that carries entries.
    assert_eq!(
        list_member(case, &body, "LoadBalancerNames"),
        Vec::<String>::new(),
        "case {case:?}: expected LoadBalancerNames to be an empty list, got {body}"
    );
}

/// A collection emptied AGAIN reads back as an empty list, not as an absent member: what the
/// group no longer holds is reported as "nothing", on the same footing as a collection
/// nothing was ever put in.
///
/// The three collections driven here are the ones with an implemented remover — the target
/// group attachments, the metrics collection, the tags — plus the instances, emptied by
/// asking for no capacity. (A suspension has no remover yet: `ResumeProcesses` answers 501,
/// so the empty case of `SuspendedProcesses` is the rows above that suspend nothing.)
#[tokio::test]
async fn a_collection_emptied_again_reads_back_as_an_empty_list() {
    let account = account().await;
    let row = ROWS
        .iter()
        .max_by_key(|row| row.count)
        .expect("the table names at least one row");
    let holds = account
        .given
        .get(ROWS.iter().position(|other| other.group == row.group).unwrap_or_default())
        .expect("every row's group was created")
        .clone();
    let case = "a group whose collections were emptied again";
    assert!(
        holds.target_group_arns.len() > 1 && !holds.metrics.is_empty() && holds.instances > 0,
        "this case needs the row that holds several entries, got {holds:?}"
    );

    let mut params = vec![p("AutoScalingGroupName", row.group)];
    for (index, arn) in holds.target_group_arns.iter().enumerate() {
        params.push(p(
            &format!("TargetGroupARNs.member.{}", index + 1),
            arn.clone(),
        ));
    }
    let (status, body) = query(
        &account.router,
        "DetachLoadBalancerTargetGroups",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetachLoadBalancerTargetGroups expected 200, got {status}: {body}"
    );

    let mut params = vec![p("AutoScalingGroupName", row.group)];
    for (index, metric) in holds.metrics.iter().enumerate() {
        params.push(p(
            &format!("Metrics.member.{}", index + 1),
            metric.clone(),
        ));
    }
    let (status, body) = query(
        &account.router,
        "DisableMetricsCollection",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DisableMetricsCollection expected 200, got {status}: {body}"
    );

    let mut params = Vec::new();
    for (index, key) in holds.tag_keys.iter().enumerate() {
        let member = index + 1;
        params.push(p(&format!("Tags.member.{member}.ResourceId"), row.group));
        params.push(p(
            &format!("Tags.member.{member}.ResourceType"),
            GROUP_RESOURCE_TYPE,
        ));
        params.push(p(&format!("Tags.member.{member}.Key"), key.clone()));
    }
    let (status, body) = query(&account.router, "DeleteTags", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTags expected 200, got {status}: {body}"
    );

    let (status, body) = query(
        &account.router,
        "SetDesiredCapacity",
        &borrowed(&[
            p("AutoScalingGroupName", row.group),
            p("DesiredCapacity", "0"),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected 200, got {status}: {body}"
    );
    instances_settle(&account.router, case, row.group, 0).await;

    let body = described(&account.router, case, row.group).await;
    for member in LIST_MEMBERS {
        assert!(
            entries(&body, member).is_some(),
            "case {case:?}: expected the described group to name {member} as a list, got {body}"
        );
    }
    for member in ["TargetGroupARNs", "EnabledMetrics", "Tags", "Instances"] {
        assert_eq!(
            list_member(case, &body, member),
            Vec::<String>::new(),
            "case {case:?}: expected {member} to be an empty list once the group holds \
             nothing in it, got {body}"
        );
    }
}

/// Every read that reports a group carries its list members: a describe naming no group
/// reports each member once per group, and so does each page of a one-group-per-page walk.
#[tokio::test]
async fn every_read_that_reports_a_group_carries_its_list_members() {
    let account = account().await;

    let case = "every group of the account in one answer";
    let body = describe(&account.router, case, &[]).await;
    for member in LIST_MEMBERS {
        assert_eq!(
            element_count(&body, member),
            ROWS.len(),
            "case {case:?}: expected {member} once per group ({} groups), got {body}",
            ROWS.len()
        );
    }

    let case = "one group per page";
    let mut pages = 0;
    let mut next = String::new();
    // One page per group, plus one turn's slack, so a read that never stops is a failure of
    // the loop's bound rather than a hang.
    for _ in 0..=ROWS.len() {
        let mut params = vec![p("MaxRecords", "1")];
        if !next.is_empty() {
            params.push(p("NextToken", next.as_str()));
        }
        let body = describe(&account.router, case, &params).await;
        for member in LIST_MEMBERS {
            assert_eq!(
                element_count(&body, member),
                1,
                "case {case:?}: expected a page of one group to name {member} once, got {body}"
            );
        }
        pages += 1;
        next = tag_value(&body, "NextToken");
        if next.is_empty() {
            break;
        }
    }
    assert_eq!(
        pages,
        ROWS.len(),
        "case {case:?}: expected one page per group, got {pages} page(s)"
    );
}
