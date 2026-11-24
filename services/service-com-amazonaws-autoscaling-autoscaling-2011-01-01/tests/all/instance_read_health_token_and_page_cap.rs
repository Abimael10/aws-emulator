// SPDX-License-Identifier: CC0-1.0

//! Two things this service publishes ITSELF, rather than reading off the model: the health
//! token the account-level instance read reports a group member's state under, and the
//! sentence a paginated read refuses a `MaxRecords` above its page cap with.
//!
//! RULE 1: `AutoScalingInstanceDetails$HealthStatus` is an unconstrained string whose states
//! the model documents in prose only — "`Healthy` means that the instance is healthy and
//! should remain in service. `Unhealthy` means that the instance is unhealthy and Amazon EC2
//! Auto Scaling should terminate and replace it"
//! (`smithy_query(query="doc",shape="AutoScalingInstanceDetails",member="HealthStatus")`) —
//! so each surface publishes the state in its OWN token: `DescribeAutoScalingInstances`
//! publishes the upper-case one, while the group's own per-instance projection under
//! `AutoScalingGroups[].Instances[]` publishes the mixed-case spelling
//! `SetInstanceHealth$HealthStatus` documents. Both report the ONE state the group holds for
//! the instance, so a state a caller records moves both.
//!
//! RULE 2: `MaxRecords` is a plain integer in this service's model, with no range of its own
//! (`smithy_query(query="shape",shape="MaxRecords")`), and a request's value is weighed
//! against two bounds under the sentence each of them carries: the member's own range, which
//! the paginated reads publish as 1 to 100 and which a value outside is refused as a member
//! value outside its bounds; and the page cap of the read itself, which some reads publish as
//! 50 and a value inside the member's range but past it asks for more records than the read
//! returns — refused with HTTP 400 `ValidationError` carrying the service's own sentence
//! naming that cap.
//!
//! The tables span what each rule generalises over: rule 1 over the two states a group can
//! hold for a member and over recording one of them twice over, rule 2 over the paginated
//! reads of this service, the two caps they publish, and where past a bound the request
//! reaches (one step, far past, and the extremes of the member's own type). Every wait below
//! looks AGAIN until its condition holds or a generous deadline passes.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, emulator, p, query, seed_subnets, tag_value, xml_unescape,
};

/// The bounds the `MaxRecords` MEMBER itself admits on the paginated reads of this service.
///
/// SOURCE: the name-ordered describes publish the member's own range — "The default value is
/// `50` and the maximum value is `100`"
/// (`smithy_query(query="doc",shape="AutoScalingGroupNamesType",member="MaxRecords")`); a page
/// holds items, so it holds at least one.
const MEMBER_CEILING: i32 = 100;
const MEMBER_FLOOR: i32 = 1;

/// How long a case waits for a move the clock owes before calling it a failure. A BACKSTOP,
/// not a wait: every helper below looks again until its condition holds.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The launch template the group of this suite launches from.
const TEMPLATE: &str = "lt-instance-read-tokens";

/// The `LifecycleState` of a member that is in service.
const IN_SERVICE: &str = "InService";

/// A `HealthCheckGracePeriod` long enough that nothing here outlives it, so the replacement a
/// recorded `Unhealthy` leaves owed is held off for the whole of a case and the member stays
/// readable in the state the call wrote.
const HOLDING_GRACE_PERIOD: &str = "600";

/// One state a group can hold for a member: the value a caller records it with, the token the
/// GROUP's own per-instance projection publishes, and the token the ACCOUNT-level instance
/// read publishes. Written out here rather than taken from the crate, so no assertion takes
/// the implementation's own word for either spelling.
struct HealthRow {
    case: &'static str,
    recorded: Option<&'static str>,
    group_token: &'static str,
    instance_read_token: &'static str,
}

/// One paginated read and the page cap it publishes.
struct PagedRead {
    /// The awsQuery action.
    action: &'static str,
    /// Whether the read is group-scoped, so the group this suite holds is named beside the
    /// page size.
    group_scoped: bool,
    /// The largest `MaxRecords` the read serves.
    ///
    /// SOURCE: each operation's own `MaxRecords` documentation —
    /// `smithy_query(query="doc",shape="DescribeAutoScalingInstancesType",member="MaxRecords")`
    /// and `DescribeWarmPoolType` / `DescribeTrafficSourcesRequest` state 50, while
    /// `AutoScalingGroupNamesType` and `DescribeTagsType` state 100, which is the member's own
    /// ceiling ([`MEMBER_CEILING`]).
    cap: i32,
}

/// The group every case of this suite reads, holding one instance in service.
struct Fixture {
    router: MultiServiceRouter,
    group: String,
    instance_id: String,
}

/// An emulator holding one group of one instance, settled in service.
async fn fixture(case: &str) -> Fixture {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let zone = subnets.first().expect("one seeded subnet").zone.clone();
    create_launch_template(&router, TEMPLATE).await;
    let group = "asg-instance-read-tokens".to_string();
    let params = vec![
        p("AutoScalingGroupName", group.clone()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("AvailabilityZones.member.1", zone),
        p("HealthCheckGracePeriod", HOLDING_GRACE_PERIOD),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating {group} expected HTTP 200, got {status} with {body:?}"
    );
    let instance_id = settled_instance(&router, case, &group).await;
    Fixture {
        router,
        group,
        instance_id,
    }
}

/// Look again until the group reports exactly one instance in service, and answer its id.
async fn settled_instance(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        let instances = tag_value(&body, "Instances");
        let members: Vec<&str> = instances
            .split("</member>")
            .filter(|chunk| chunk.contains("<InstanceId"))
            .collect();
        if let [member] = members.as_slice() {
            if tag_value(member, "LifecycleState") == IN_SERVICE {
                return tag_value(member, "InstanceId");
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to hold one instance in service, got {body:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `HealthStatus` the GROUP reports for `instance_id`, through the reader whose subject
/// the group is.
async fn group_health(router: &MultiServiceRouter, case: &str, group: &str, instance_id: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group read expected HTTP 200, got {status} with {body:?}"
    );
    member_health(&tag_value(&body, "Instances"), case, instance_id)
}

/// The `HealthStatus` the ACCOUNT-level instance read reports for `instance_id`.
async fn instance_read_health(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingInstances",
        &[("InstanceIds.member.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the instance read expected HTTP 200, got {status} with {body:?}"
    );
    member_health(&tag_value(&body, "AutoScalingInstances"), case, instance_id)
}

/// The `HealthStatus` the entry for `instance_id` of one reported list carries.
fn member_health(list: &str, case: &str, instance_id: &str) -> String {
    list.split("</member>")
        .filter(|chunk| chunk.contains("<InstanceId"))
        .find(|chunk| tag_value(chunk, "InstanceId") == instance_id)
        .map(|chunk| tag_value(chunk, "HealthStatus"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {instance_id} to be reported, got {list:?}")
        })
}

/// Record `health_status` for `instance_id` and require the call to have succeeded.
async fn record_health(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    health_status: &str,
) {
    let params = vec![
        p("InstanceId", instance_id),
        p("HealthStatus", health_status),
        p("ShouldRespectGracePeriod", "true"),
    ];
    let (status, body) = query(router, "SetInstanceHealth", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: recording {health_status} expected HTTP 200, got {status} with {body:?}"
    );
}

/// Look again until the group reports `wanted` for `instance_id`: a recorded status is what
/// the group reports for the instance, so the two surfaces are compared once the state the
/// case is about is the one the group holds.
async fn group_health_reaching(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = group_health(router, case, group, instance_id).await;
        if reported == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to report {wanted:?} for {instance_id}, got \
             {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The account-level instance read publishes the state the group holds for a member under its
/// OWN token, which is the upper-case one, while the group's projection publishes the
/// mixed-case spelling — for every state a caller can record, and for a state recorded twice
/// over.
#[tokio::test]
async fn the_instance_read_publishes_the_health_state_in_its_own_token() {
    let rows = [
        HealthRow {
            case: "the state a member comes into service in",
            recorded: None,
            group_token: "Healthy",
            instance_read_token: "HEALTHY",
        },
        HealthRow {
            case: "a caller's own health check finding the member out of service",
            recorded: Some("Unhealthy"),
            group_token: "Unhealthy",
            instance_read_token: "UNHEALTHY",
        },
        HealthRow {
            case: "the member returned to service",
            recorded: Some("Healthy"),
            group_token: "Healthy",
            instance_read_token: "HEALTHY",
        },
        HealthRow {
            case: "that same state recorded a second time",
            recorded: Some("Healthy"),
            group_token: "Healthy",
            instance_read_token: "HEALTHY",
        },
    ];

    let fixture = fixture("the group of this suite").await;
    for row in &rows {
        let case = row.case;
        if let Some(health_status) = row.recorded {
            record_health(&fixture.router, case, &fixture.instance_id, health_status).await;
        }
        group_health_reaching(
            &fixture.router,
            case,
            &fixture.group,
            &fixture.instance_id,
            row.group_token,
        )
        .await;
        let from_group = group_health(
            &fixture.router,
            case,
            &fixture.group,
            &fixture.instance_id,
        )
        .await;
        assert_eq!(
            from_group, row.group_token,
            "case {case:?}: expected the group to report {:?}, got {from_group:?}",
            row.group_token
        );
        let from_instance_read =
            instance_read_health(&fixture.router, case, &fixture.instance_id).await;
        assert_eq!(
            from_instance_read, row.instance_read_token,
            "case {case:?}: expected the instance read to report {:?}, got \
             {from_instance_read:?}",
            row.instance_read_token
        );
    }
}

/// A `MaxRecords` is weighed against both bounds, under the sentence each carries: a value
/// inside the member's own range but past the read's page cap is refused with the service's
/// limit sentence naming that cap, while a value outside the member's range is refused with
/// the sentence for a member value outside its bounds — and every value the read serves is
/// served.
#[tokio::test]
async fn a_page_size_is_refused_under_the_sentence_of_the_bound_it_missed() {
    let reads = [
        PagedRead {
            action: "DescribeAutoScalingInstances",
            group_scoped: false,
            cap: 50,
        },
        PagedRead {
            action: "DescribeAutoScalingGroups",
            group_scoped: false,
            cap: MEMBER_CEILING,
        },
        PagedRead {
            action: "DescribeTags",
            group_scoped: false,
            cap: MEMBER_CEILING,
        },
        PagedRead {
            action: "DescribeWarmPool",
            group_scoped: true,
            cap: 50,
        },
        PagedRead {
            action: "DescribeTrafficSources",
            group_scoped: true,
            cap: 50,
        },
    ];

    let fixture = fixture("the group of this suite").await;
    for read in &reads {
        // The page sizes this read refuses, each with the sentence it carries: the values
        // between the read's own cap and the member's ceiling are past the SERVICE's limit for
        // this read, while the values outside the member's range — the floor's far side, one
        // step past the ceiling, and the extremes of the member's own type — are a member
        // value outside its bounds. A read whose cap IS the member's ceiling has no value of
        // the first kind, which is why the first list is built from the cap.
        let mut refused: Vec<(i32, String)> = Vec::new();
        for past_cap in [read.cap + 1, MEMBER_CEILING] {
            if past_cap <= MEMBER_CEILING && past_cap > read.cap {
                refused.push((past_cap, over_page_cap_sentence(read.cap)));
            }
        }
        for outside in [0, -1, i32::MIN] {
            refused.push((outside, member_floor_sentence(outside)));
        }
        for outside in [MEMBER_CEILING + 1, 100_000, i32::MAX] {
            refused.push((outside, member_ceiling_sentence(outside)));
        }

        for (max_records, expected) in &refused {
            let case = format!("{} with a page size of {max_records}", read.action);
            let mut params = vec![p("MaxRecords", max_records.to_string())];
            if read.group_scoped {
                params.push(p("AutoScalingGroupName", fixture.group.clone()));
            }
            let (status, body) = query(&fixture.router, read.action, &borrowed(&params)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {body:?}"
            );
            let code = tag_value(&body, "Code");
            assert_eq!(
                code, "ValidationError",
                "case {case:?}: expected a ValidationError, got {code:?} in {body}"
            );
            let message = xml_unescape(&tag_value(&body, "Message"));
            assert_eq!(
                &message, expected,
                "case {case:?}: expected the whole sentence {expected:?}, got {message:?}"
            );
        }

        // The controls: the cap itself, and the smallest page a request may ask for, are
        // SERVED — so the rows above cannot pass by refusing every page size.
        for served in [1, read.cap] {
            let case = format!("{} with a page size of {served}", read.action);
            let mut params = vec![p("MaxRecords", served.to_string())];
            if read.group_scoped {
                params.push(p("AutoScalingGroupName", fixture.group.clone()));
            }
            let (status, body) = query(&fixture.router, read.action, &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {body:?}"
            );
        }
    }
}

/// The sentence a page size past the cap `cap` a read holds its pages to is refused with.
fn over_page_cap_sentence(cap: i32) -> String {
    format!("The number of records returned is limited to {cap}")
}

/// The sentence a page size below the smallest the member admits is refused with.
fn member_floor_sentence(max_records: i32) -> String {
    format!(
        "1 validation error detected: Value '{max_records}' at 'maxRecords' failed to satisfy \
         constraint: Member must have value greater than or equal to {MEMBER_FLOOR}"
    )
}

/// The sentence a page size above the largest the member admits is refused with.
fn member_ceiling_sentence(max_records: i32) -> String {
    format!(
        "1 validation error detected: Value '{max_records}' at 'maxRecords' failed to satisfy \
         constraint: Member must have value less than or equal to {MEMBER_CEILING}"
    )
}
