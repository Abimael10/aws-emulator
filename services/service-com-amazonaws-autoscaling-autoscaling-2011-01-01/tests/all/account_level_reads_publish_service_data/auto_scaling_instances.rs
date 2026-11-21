// SPDX-License-Identifier: CC0-1.0

//! `DescribeAutoScalingInstances` — the account's own read of its Auto Scaling instances,
//! over the awsQuery wire.
//!
//! THE RULE UNDER TEST: an account-level instance read reports EVERY instance of EVERY Auto
//! Scaling group the account holds, each as the owning group's own view of it plus the name
//! of that group, filtered by the instance ids the request names ("If you omit this
//! property, all Auto Scaling instances are described. If you specify an ID that does not
//! exist, it is ignored with no error",
//! `smithy_query(query="operation",operation="DescribeAutoScalingInstances")`), served one
//! page of at most `MaxRecords` entries at a time with a `NextToken` that resumes exactly
//! where the page stopped, and it changes nothing a later caller sees.
//!
//! The tables span what that rule generalises over: how many groups the account holds and
//! how many instances each of them holds, which ids a request names (none, one, one per
//! group, an id the account holds nothing for, the same id twice), and the page size — every
//! size the read pages over this account with, the published bounds of `MaxRecords` ("The
//! default value is 50 and the maximum value is 50",
//! `smithy_query(query="doc",shape="DescribeAutoScalingInstancesType",member="MaxRecords")`)
//! and one past each of them. Membership is read back as the clock moves it: every wait here
//! looks AGAIN until the condition holds or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, emulator, p, query, seed_subnets, tag_value, xml_unescape,
};

/// How long a case waits for a move the clock owes before calling it a failure. A BACKSTOP,
/// not a wait: every helper below looks again until its condition holds.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The launch template every group of this suite launches from, created over Amazon EC2's
/// own wire so the groups launch from a template the account really holds.
const TEMPLATE: &str = "lt-instance-read";

/// The `LifecycleState` an instance of a group reaches once it is in service.
const IN_SERVICE: &str = "InService";

/// The largest `MaxRecords` this read honours, and the smallest a request may ask for.
///
/// SOURCE: the model's own documentation for `DescribeAutoScalingInstancesType$MaxRecords` —
/// "The maximum number of items to return with this call. The default value is 50 and the
/// maximum value is 50"; a page holds items, so it holds at least one.
const MAX_RECORDS_CEILING: usize = 50;
const MAX_RECORDS_FLOOR: usize = 1;

/// An instance id of the issued form that the whole account holds nothing for.
const ABSENT_INSTANCE_ID: &str = "i-00000000000000099";

/// How many pages one walk may take before it is called a failure rather than a loop: far
/// more than any case here needs, so only a read that never stops trips it.
const PAGE_CAP: usize = 32;

/// One instance as a read reports it: every member of the reported shape, plus the group it
/// belongs to.
///
/// The same reader serves both reads, so the account-level read's
/// `AutoScalingInstanceDetails` and the group's own `Instances` member can be compared as
/// whole records: `AutoScalingInstanceDetails` is "the same facts the group reports for the
/// instance plus the name of the group"
/// (`smithy_query(query="shape",shape="AutoScalingInstanceDetails")` beside
/// `smithy_query(query="shape",shape="Instance")`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    instance_id: String,
    group: String,
    instance_type: String,
    availability_zone: String,
    availability_zone_id: String,
    lifecycle_state: String,
    health_status: String,
    launch_template_name: String,
    image_id: String,
    protected_from_scale_in: String,
    weighted_capacity: String,
}

/// The members of one reported list. `group` names the owning group for a list that does not
/// carry it (a group's own `Instances`), and is read off each entry otherwise.
fn members(list: &str, group: Option<&str>) -> Vec<Member> {
    list.split("</member>")
        .filter(|chunk| chunk.contains("<InstanceId"))
        .map(|chunk| Member {
            instance_id: tag_value(chunk, "InstanceId"),
            group: group.map_or_else(
                || tag_value(chunk, "AutoScalingGroupName"),
                str::to_string,
            ),
            instance_type: tag_value(chunk, "InstanceType"),
            availability_zone: tag_value(chunk, "AvailabilityZone"),
            availability_zone_id: tag_value(chunk, "AvailabilityZoneId"),
            lifecycle_state: tag_value(chunk, "LifecycleState"),
            health_status: tag_value(chunk, "HealthStatus"),
            launch_template_name: tag_value(chunk, "LaunchTemplateName"),
            image_id: tag_value(chunk, "ImageId"),
            protected_from_scale_in: tag_value(chunk, "ProtectedFromScaleIn"),
            weighted_capacity: tag_value(chunk, "WeightedCapacity"),
        })
        .collect()
}

/// POST one `DescribeAutoScalingInstances` and answer its status and body.
async fn read(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    query(router, "DescribeAutoScalingInstances", &borrowed(params)).await
}

/// The entries one accepted page of the account-level read carries, and the `NextToken` it
/// issued for a further page.
async fn page(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> (Vec<Member>, Option<String>) {
    let (status, body) = read(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the account-level instance read, got {status} \
         with {body:?}"
    );
    let token = tag_value(&body, "NextToken");
    (
        members(&tag_value(&body, "AutoScalingInstances"), None),
        (!token.is_empty()).then_some(token),
    )
}

/// Every page the account-level read serves for `params`, in order, each page followed by
/// resuming from the token it issued until it issues none.
async fn pages(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<Vec<Member>> {
    let mut served: Vec<Vec<Member>> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut this_page = params.to_vec();
        if let Some(token) = &token {
            this_page.push(p("NextToken", token.clone()));
        }
        let (entries, next) = page(router, case, &this_page).await;
        served.push(entries);
        match next {
            None => return served,
            Some(next) => token = Some(next),
        }
        assert!(
            served.len() < PAGE_CAP,
            "case {case:?}: the read issued a further token after {} pages, which is more \
             than this account holds entries for",
            served.len()
        );
    }
}

/// What the group `name` reports for its own instances, under `AutoScalingGroups[].Instances`.
async fn group_members(router: &MultiServiceRouter, name: &str) -> Vec<Member> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?} expected HTTP 200, got {status} with {body:?}"
    );
    members(&tag_value(&body, "Instances"), Some(name))
}

/// Look again until the group `name` reports exactly `wanted` instances, all of them in
/// service, and answer that group's view of them.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<Member> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = group_members(router, name).await;
        if reported.len() == wanted
            && reported
                .iter()
                .all(|member| member.lifecycle_state == IN_SERVICE)
        {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to report {wanted} instance(s) in service, got \
             {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create one group over the wire, from the account's own launch template, placed in `zone`.
async fn create_group(router: &MultiServiceRouter, name: &str, zone: &str, capacity: usize) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "6"),
        p("DesiredCapacity", capacity.to_string()),
        p("AvailabilityZones.member.1", zone),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// One account of this suite: the emulator, and the groups it holds with the instances each
/// of them launched, settled in service.
struct Account {
    router: MultiServiceRouter,
    /// The groups in the order the account-level read reports them, each with its own view of
    /// its instances.
    groups: Vec<(String, Vec<Member>)>,
}

impl Account {
    /// Every instance of the account, in the order the groups above hold them: what a read
    /// naming no ids and no page size must report.
    fn every_member(&self) -> Vec<Member> {
        self.groups
            .iter()
            .flat_map(|(_, members)| members.clone())
            .collect()
    }

    /// The id of the `index`-th instance of the account.
    fn id(&self, index: usize) -> String {
        self.every_member()
            .get(index)
            .expect("the account holds this many instances")
            .instance_id
            .clone()
    }
}

/// An account holding one group per entry of `capacities`, each with that many instances in
/// service. The group names are in ascending order, which is the order the account-level
/// read reports the groups in.
async fn account(case: &str, capacities: &[usize]) -> Account {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let zone = subnets.first().expect("one seeded subnet").zone.clone();
    create_launch_template(&router, TEMPLATE).await;

    // Every group is created before any of them is waited on, so the account settles in
    // about one span rather than one per group.
    let mut names: Vec<String> = Vec::new();
    for (index, capacity) in capacities.iter().enumerate() {
        let name = format!("asg-instance-read-{index}");
        create_group(&router, &name, &zone, *capacity).await;
        names.push(name);
    }
    let mut groups = Vec::new();
    for (name, capacity) in names.into_iter().zip(capacities) {
        let members = settled(&router, case, &name, *capacity).await;
        groups.push((name, members));
    }
    Account { router, groups }
}

/// The account-level read reports every instance of every group of the account, each as the
/// owning group's own view of it plus the name of that group.
#[tokio::test]
async fn the_account_read_reports_every_group_member_as_its_group_does() {
    // The rows differ in how the account's instances are spread over its groups: one group
    // holding one, one group holding several, and several groups holding different counts.
    let rows: [(&str, Vec<usize>); 4] = [
        ("one group holding one instance", vec![1]),
        ("one group holding three instances", vec![3]),
        ("two groups holding one instance each", vec![1, 1]),
        ("three groups of different sizes", vec![2, 1, 3]),
    ];

    for (case, capacities) in rows {
        let account = account(case, &capacities).await;
        let (reported, token) = page(&account.router, case, &[]).await;
        assert_eq!(
            token, None,
            "case {case:?}: an account of {} instance(s) fits in one page, got the token \
             {token:?}",
            account.every_member().len()
        );
        // Each entry is the owning group's own record of that instance, in the order the
        // groups hold them: one instance, two reads, one answer.
        assert_eq!(
            reported,
            account.every_member(),
            "case {case:?}: expected each group's own view of its instances, got {reported:?}"
        );
        // And every entry names the group it belongs to, which the group's own read does not
        // carry.
        for (name, members) in &account.groups {
            for member in members {
                assert_eq!(
                    &member.group, name,
                    "case {case:?}: expected {} to be reported under {name}, got {member:?}",
                    member.instance_id
                );
            }
        }
    }
}

/// The read serves one page of at most `MaxRecords` entries and a token that resumes exactly
/// where the page stopped, so the pages of a walk are the account's instances once each, in
/// the order one unpaged read reports them.
#[tokio::test]
async fn the_account_read_pages_over_every_group_and_each_page_follows_the_last() {
    let case = "two groups holding four instances between them";
    let account = account(case, &[3, 1]).await;
    let all = account.every_member();
    let total = all.len();
    assert_eq!(
        total, 4,
        "case {case:?}: expected the account to hold four instances, got {all:?}"
    );

    // The rows span the page size: a size below the count, one at it, one above it, the
    // published bounds of `MaxRecords`, and no page size at all (the read's own default).
    let sizes: [Option<usize>; 6] = [
        None,
        Some(MAX_RECORDS_FLOOR),
        Some(2),
        Some(total),
        Some(total + 1),
        Some(MAX_RECORDS_CEILING),
    ];

    for size in sizes {
        let case = &format!("{case}, read {size:?} at a time");
        let params: Vec<(String, String)> = size
            .map(|size| vec![p("MaxRecords", size.to_string())])
            .unwrap_or_default();
        let served = pages(&account.router, case, &params).await;
        // The limit the read holds a page to: the size asked for, or its own default.
        let limit = size.unwrap_or(MAX_RECORDS_CEILING);
        let expected_pages = total.div_ceil(limit);
        assert_eq!(
            served.len(),
            expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {total} entries at {limit} \
             per page, got {:?}",
            served.iter().map(Vec::len).collect::<Vec<usize>>()
        );
        for (index, entries) in served.iter().enumerate() {
            let expected = limit.min(total - index * limit);
            assert_eq!(
                entries.len(),
                expected,
                "case {case:?}: expected page {index} to hold {expected} entries, got \
                 {entries:?}"
            );
        }
        let walked: Vec<Member> = served.into_iter().flatten().collect();
        assert_eq!(
            walked, all,
            "case {case:?}: expected the walk to serve every instance once, in the order one \
             unpaged read reports them, got {walked:?}"
        );
    }
}

/// One row of the filter table: which ids the request names, and which of the account's
/// instances the read must report for them.
struct FilterRow {
    case: &'static str,
    /// The indices of the account's instances the request names, in the order it names them.
    named: Vec<usize>,
    /// Whether the request also names an id the account holds nothing for.
    names_absent: bool,
    /// The indices of the instances the read must report.
    expected: Vec<usize>,
}

/// The ids a request names are a FILTER and nothing else: an id the account holds nothing for
/// is ignored with no error, and an id named twice is one instance still.
#[tokio::test]
async fn the_account_read_filters_by_the_ids_it_is_given_and_ignores_the_rest() {
    let case = "two groups holding three instances between them";
    let account = account(case, &[2, 1]).await;
    let all = account.every_member();
    assert_eq!(
        all.len(),
        3,
        "case {case:?}: expected the account to hold three instances, got {all:?}"
    );

    let rows = [
        FilterRow {
            case: "no ids: every instance of the account",
            named: vec![],
            names_absent: false,
            expected: vec![0, 1, 2],
        },
        FilterRow {
            case: "one id: that instance alone",
            named: vec![1],
            names_absent: false,
            expected: vec![1],
        },
        FilterRow {
            case: "one id from each group: both instances",
            named: vec![0, 2],
            names_absent: false,
            expected: vec![0, 2],
        },
        FilterRow {
            case: "the ids in the reverse of the reported order: the reported order still",
            named: vec![2, 0],
            names_absent: false,
            expected: vec![0, 2],
        },
        FilterRow {
            case: "the same id twice: one instance",
            named: vec![1, 1],
            names_absent: false,
            expected: vec![1],
        },
        FilterRow {
            case: "an id the account holds nothing for, beside one it holds",
            named: vec![0],
            names_absent: true,
            expected: vec![0],
        },
        FilterRow {
            case: "only an id the account holds nothing for: no instances, no error",
            named: vec![],
            names_absent: true,
            expected: vec![],
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut ids: Vec<String> = row.named.iter().map(|index| account.id(*index)).collect();
        if row.names_absent {
            ids.push(ABSENT_INSTANCE_ID.to_string());
        }
        let params: Vec<(String, String)> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| p(&format!("InstanceIds.member.{}", index + 1), id.clone()))
            .collect();
        let (reported, token) = page(&account.router, case, &params).await;
        let expected: Vec<Member> = row
            .expected
            .iter()
            .map(|index| {
                all.get(*index)
                    .expect("an index into the account's instances")
                    .clone()
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: naming {ids:?} must report {expected:?}, got {reported:?}"
        );
        assert_eq!(
            token, None,
            "case {case:?}: a filtered page that holds every match issues no token, got \
             {token:?}"
        );
    }
}

/// One row of the page-size table: the `MaxRecords` a request carries, and the whole
/// sentence the read refuses it with — `None` for a value it must accept.
struct BoundRow {
    case: &'static str,
    max_records: String,
    refusal: Option<String>,
}

/// A `MaxRecords` outside the bounds this read publishes is refused with the sentence for a
/// member value outside its bounds, and a value inside them is served.
#[tokio::test]
async fn the_account_read_holds_its_page_size_to_the_bounds_it_publishes() {
    let case = "one group holding two instances";
    let account = account(case, &[2]).await;

    let rows = [
        BoundRow {
            case: "the published floor",
            max_records: MAX_RECORDS_FLOOR.to_string(),
            refusal: None,
        },
        BoundRow {
            case: "the published ceiling",
            max_records: MAX_RECORDS_CEILING.to_string(),
            refusal: None,
        },
        BoundRow {
            case: "one below the published floor",
            max_records: (MAX_RECORDS_FLOOR - 1).to_string(),
            refusal: Some(format!(
                "1 validation error detected: Value '{}' at 'maxRecords' failed to satisfy \
                 constraint: Member must have value greater than or equal to \
                 {MAX_RECORDS_FLOOR}",
                MAX_RECORDS_FLOOR - 1
            )),
        },
        BoundRow {
            case: "a negative page size",
            max_records: "-3".to_string(),
            refusal: Some(format!(
                "1 validation error detected: Value '-3' at 'maxRecords' failed to satisfy \
                 constraint: Member must have value greater than or equal to \
                 {MAX_RECORDS_FLOOR}"
            )),
        },
        BoundRow {
            case: "one past the published ceiling",
            max_records: (MAX_RECORDS_CEILING + 1).to_string(),
            refusal: Some(format!(
                "1 validation error detected: Value '{}' at 'maxRecords' failed to satisfy \
                 constraint: Member must have value less than or equal to \
                 {MAX_RECORDS_CEILING}",
                MAX_RECORDS_CEILING + 1
            )),
        },
    ];

    for row in &rows {
        let case = row.case;
        let params = vec![p("MaxRecords", row.max_records.clone())];
        let (status, body) = read(&account.router, &params).await;
        match &row.refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a MaxRecords of {} is served, got {status} with {body:?}",
                    row.max_records
                );
            }
            Some(message) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: a MaxRecords of {} is refused, got {status} with {body:?}",
                    row.max_records
                );
                let code = tag_value(&body, "Code");
                assert_eq!(
                    code, "ValidationError",
                    "case {case:?}: expected a ValidationError, got {code:?} in {body}"
                );
                let reported = xml_unescape(&tag_value(&body, "Message"));
                assert_eq!(
                    &reported, message,
                    "case {case:?}: expected the whole sentence {message:?}, got {reported:?}"
                );
            }
        }
    }
}

/// A `NextToken` this read did not issue, and one naming a page past the end of the list it
/// was issued for, name no page: both are this operation's own modelled `InvalidNextToken`,
/// while the token a page issued resumes that page.
#[tokio::test]
async fn the_account_read_refuses_a_token_that_names_no_page() {
    let case = "one group holding two instances";
    let account = account(case, &[2]).await;

    // The token a served page issues IS usable, so no row below can pass by refusing every
    // token.
    let (first, token) = page(
        &account.router,
        case,
        &[p("MaxRecords", MAX_RECORDS_FLOOR.to_string())],
    )
    .await;
    let token = token.expect("a page below the account's instance count issues a token");
    let (second, _) = page(
        &account.router,
        case,
        &[
            p("MaxRecords", MAX_RECORDS_FLOOR.to_string()),
            p("NextToken", token.clone()),
        ],
    )
    .await;
    assert_ne!(
        first, second,
        "case {case:?}: the token a page issues must resume AFTER that page, got {second:?}"
    );

    // The rows differ in why the token names no page: text no page token is, a token of the
    // issued form naming a page past the end of the list, and the empty token.
    let rows: [(&str, String); 3] = [
        ("text this read never issued", "not-a-token".to_string()),
        (
            "a token of the issued form, naming a page past the end",
            autoscaling::state::encode_next_token(MAX_RECORDS_CEILING),
        ),
        (
            "a token of the issued form, naming the first page, which is reached with no \
             token at all",
            autoscaling::state::encode_next_token(0),
        ),
    ];

    for (case, token) in &rows {
        let params = vec![p("NextToken", token.clone())];
        let (status, body) = read(&account.router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the token {token:?} names no page, got {status} with {body:?}"
        );
        let code = tag_value(&body, "Code");
        assert_eq!(
            code, "InvalidNextToken",
            "case {case:?}: expected the modelled InvalidNextToken, got {code:?} in {body}"
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message,
            format!("The token '{token}' is invalid"),
            "case {case:?}: expected the refusal to quote the token the caller sent, got \
             {message:?}"
        );
    }
}

/// The read reports the membership the groups hold NOW — an instance a capacity change added
/// is reported, one a termination removed is not — and reading changes nothing a later caller
/// sees.
#[tokio::test]
async fn the_account_read_reflects_what_the_groups_hold_and_changes_nothing() {
    let case = "a group whose membership moves";
    let account = account(case, &[1, 1]).await;
    let grown = account
        .groups
        .first()
        .expect("the account holds two groups")
        .0
        .clone();

    // Reading twice over answers the same thing twice: a report is not a write.
    let (before, _) = page(&account.router, case, &[]).await;
    let (again, _) = page(&account.router, case, &[]).await;
    assert_eq!(
        before, again,
        "case {case:?}: a second read must answer what the first did, got {again:?}"
    );
    assert_eq!(
        before.len(),
        2,
        "case {case:?}: expected the account's two instances, got {before:?}"
    );

    // A capacity change gives the group another instance, and the account-level read reports
    // it as soon as the group holds it.
    let params = vec![p("AutoScalingGroupName", grown.clone()), p("DesiredCapacity", "3")];
    let (status, body) = query(&account.router, "SetDesiredCapacity", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected HTTP 200, got {status} with {body:?}"
    );
    let members = settled(&account.router, case, &grown, 3).await;
    let reported = wait_for_account_read(&account.router, case, 4).await;
    for member in &members {
        assert!(
            reported.contains(member),
            "case {case:?}: expected the account read to report {member:?}, got {reported:?}"
        );
    }

    // And a termination takes the instance out of the account's own read, the same way the
    // group stops reporting it.
    let retired = members
        .first()
        .expect("the grown group holds three instances")
        .instance_id
        .clone();
    let params = vec![
        p("InstanceId", retired.clone()),
        p("ShouldDecrementDesiredCapacity", "true"),
    ];
    let (status, body) = query(
        &account.router,
        "TerminateInstanceInAutoScalingGroup",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TerminateInstanceInAutoScalingGroup expected HTTP 200, got {status} \
         with {body:?}"
    );
    let reported = wait_for_account_read(&account.router, case, 3).await;
    assert!(
        reported
            .iter()
            .all(|member| member.instance_id != retired),
        "case {case:?}: expected the account read to drop {retired}, got {reported:?}"
    );
    // The instances the account still holds are each reported under their own group.
    for member in &reported {
        assert!(
            account
                .groups
                .iter()
                .any(|(name, _)| *name == member.group),
            "case {case:?}: expected {member:?} to be reported under a group of the account, \
             got the groups {:?}",
            account
                .groups
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<String>>()
        );
    }
}

/// Look again until the account-level read reports exactly `wanted` instances, all of them in
/// service, and answer them.
async fn wait_for_account_read(
    router: &MultiServiceRouter,
    case: &str,
    wanted: usize,
) -> Vec<Member> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (reported, _) = page(router, case, &[]).await;
        if reported.len() == wanted
            && reported
                .iter()
                .all(|member| member.lifecycle_state == IN_SERVICE)
        {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the account read to report {wanted} instance(s) in \
             service, got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
