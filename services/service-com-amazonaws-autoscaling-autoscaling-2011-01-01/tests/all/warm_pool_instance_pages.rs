// SPDX-License-Identifier: CC0-1.0

//! `DescribeWarmPool` as the PAGINATED REPORT of one Auto Scaling group's warm pool.
//!
//! The rule under test: `DescribeWarmPool` reports the warm pool of the group it names —
//! the stored configuration, plus at most `MaxRecords` of that pool's instances in a stable
//! order, followed by the `NextToken` that resumes the read at the first instance the page
//! did not carry — and the last page carries no token, so walking the chain yields every
//! instance of the pool exactly once whatever the page size. A group that holds NO warm
//! pool is REPORTED rather than refused: the answer carries neither a configuration nor an
//! instance, and the instances the group runs for itself are never reported as the pool's.
//!
//! The tables below span what that rule generalises over: a page narrower than the pool, a
//! page exactly its size, a page wider than it, the published ceiling of `MaxRecords`, a
//! request that omits the member altogether, and — for the group with no pool — both a
//! group running nothing and a group running instances of its own. Every row walks the
//! whole token chain and compares it against the one unbounded read, so no row can pass on
//! a page that repeats an instance, drops one, or re-orders them between pages.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, emulator, p, query, seed_subnets, tag_value, tag_values,
    xml_unescape,
};

/// How often a poll re-reads the pool.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span this service owes a
/// transition, with ample margin for a loaded test machine. Derived from the service's own
/// span rather than written out, so the two cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(30)
}

/// The published ceiling of this read's page size: "The maximum number of instances to
/// return with this call. The maximum value is 50"
/// (`smithy_query(query="doc",shape="DescribeWarmPoolType",member="MaxRecords")`).
const MAX_MAX_RECORDS: i32 = 50;

/// How many instances the pool under test holds — enough that a page of one, a page of two
/// and a page of all of them are three different walks, and small enough that the pool
/// settles quickly.
const POOL_SIZE: usize = 3;

/// The `PoolState` the pool under test is put in, one of the model's own `WarmPoolState`
/// values (`smithy_query(query="shape",shape="WarmPoolState")`).
const POOL_STATE: &str = "Stopped";

/// The `LifecycleState` an instance of a warm pool in `pool_state` is reported in once its
/// transition is due: the pool state under the prefix the model's `LifecycleState` enum
/// spells warm-pool states with.
fn warmed(pool_state: &str) -> String {
    format!("Warmed:{pool_state}")
}

/// Create one group placed in `subnet`, launching from `template`.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnet: &str,
    max_size: i32,
    desired_capacity: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired_capacity.to_string()),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Read one page of `group`'s warm pool, with the page size and the resume token a case
/// states.
async fn describe_warm_pool(
    router: &MultiServiceRouter,
    group: &str,
    max_records: Option<i32>,
    next_token: Option<&str>,
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    if let Some(max_records) = max_records {
        params.push(p("MaxRecords", max_records.to_string()));
    }
    if let Some(token) = next_token {
        params.push(p("NextToken", token));
    }
    query(router, "DescribeWarmPool", &borrowed(&params)).await
}

/// The instance ids the one unbounded read reports for `group`, once the pool holds
/// `wanted` of them and every one of them is in `state` — the CONDITION, never a fixed
/// wait.
async fn settled_pool_instances(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
    state: &str,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) = describe_warm_pool(router, group, None, None).await;
        assert_eq!(
            status, 200,
            "DescribeWarmPool for {group:?} expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let settled = ids.len() == wanted
            && states.len() == wanted
            && states.iter().all(|held| held == state);
        if settled || Instant::now() >= deadline {
            assert_eq!(
                ids.len(),
                wanted,
                "the warm pool of {group:?} must hold {wanted} instance(s), got {body}"
            );
            assert!(
                states.iter().all(|held| held == state),
                "every instance of {group:?}'s warm pool must report {state}, got {body}"
            );
            return ids;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The instance ids the group itself reports, through its own reader.
async fn group_instances(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
}

/// Wait until the group itself holds `wanted` instances, or a generous deadline passes.
async fn settled_group_instances(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let ids = group_instances(router, group).await;
        if ids.len() == wanted || Instant::now() >= deadline {
            assert_eq!(
                ids.len(),
                wanted,
                "the group {group:?} must hold {wanted} instance(s) of its own, got {ids:?}"
            );
            return ids;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One emulator holding ONE group whose warm pool has settled at [`POOL_SIZE`] instances:
/// the router, the group's name and the instance ids the pool holds, in the order the one
/// unbounded read reports them.
async fn a_settled_pool(suffix: &str) -> (MultiServiceRouter, String, Vec<String>) {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let template = format!("lt-warm-page-{suffix}");
    create_launch_template(&router, &template).await;
    let group = format!("asg-warm-page-{suffix}");
    let pool_size = i32::try_from(POOL_SIZE).expect("the pool size fits in an i32");
    create_group(&router, &group, &template, &subnets[0].id, pool_size, 0).await;
    let params = vec![
        p("AutoScalingGroupName", group.as_str()),
        p("MinSize", pool_size.to_string()),
        p("PoolState", POOL_STATE),
    ];
    let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "PutWarmPool for {group:?} expected 200, got {status}: {body}"
    );
    let ids = settled_pool_instances(&router, &group, POOL_SIZE, &warmed(POOL_STATE)).await;
    (router, group, ids)
}

/// One row of the page-size table: the `MaxRecords` a case sends, and the sizes of the
/// pages the chain must then answer — the pool's [`POOL_SIZE`] instances cut into pages of
/// at most that many, which is what bounding a page by `MaxRecords` means.
struct PageRow {
    case: &'static str,
    max_records: Option<i32>,
    expected_page_sizes: &'static [usize],
}

/// Every page of a warm-pool read carries the pool's configuration and at most
/// `MaxRecords` of its instances, and the token chain yields every instance exactly once,
/// in the order the one unbounded read reports them.
#[tokio::test]
async fn a_warm_pool_read_pages_its_instances_behind_a_resume_token() {
    let rows = [
        PageRow {
            case: "one instance per page: a page for each, the last without a token",
            max_records: Some(1),
            expected_page_sizes: &[1, 1, 1],
        },
        PageRow {
            case: "a page narrower than the pool, not dividing it",
            max_records: Some(2),
            expected_page_sizes: &[2, 1],
        },
        PageRow {
            case: "a page exactly the size of the pool: one page, no token",
            max_records: Some(3),
            expected_page_sizes: &[3],
        },
        PageRow {
            case: "the published ceiling, wider than the pool",
            max_records: Some(MAX_MAX_RECORDS),
            expected_page_sizes: &[3],
        },
        PageRow {
            case: "no page size at all",
            max_records: None,
            expected_page_sizes: &[3],
        },
    ];

    // One emulator for the whole table: the pool is READ by every row and written by none,
    // so every row reads the same settled pool and a row that changed it would be caught
    // by the next.
    let (router, group, all_ids) = a_settled_pool("chain").await;

    for row in &rows {
        let case = row.case;
        let mut token: Option<String> = None;
        let mut page_sizes: Vec<usize> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        loop {
            let (status, body) =
                describe_warm_pool(&router, &group, row.max_records, token.as_deref()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeWarmPool expected 200, got {status}: {body}"
            );
            // The configuration is the POOL's, not the page's, so every page of the chain
            // reports it.
            let configuration = tag_value(&body, "WarmPoolConfiguration");
            assert_eq!(
                tag_value(&configuration, "PoolState"),
                POOL_STATE,
                "case {case:?}: every page must report the stored PoolState {POOL_STATE}, \
                 got {body}"
            );
            let ids = tag_values(&body, "InstanceId");
            if let Some(limit) = row.max_records {
                let limit = usize::try_from(limit).expect("a positive page size");
                assert!(
                    ids.len() <= limit,
                    "case {case:?}: a page holds at most {limit} instance(s), got {}: {body}",
                    ids.len()
                );
            }
            for id in &ids {
                assert!(
                    !seen.contains(id),
                    "case {case:?}: instance {id} was reported on two pages, got {seen:?} \
                     then {ids:?}"
                );
            }
            page_sizes.push(ids.len());
            seen.extend(ids);
            let next = xml_unescape(&tag_value(&body, "NextToken"));
            if next.is_empty() {
                break;
            }
            assert!(
                page_sizes.len() <= POOL_SIZE,
                "case {case:?}: the token chain must end within {POOL_SIZE} page(s), \
                 got {page_sizes:?} and another token {next:?}"
            );
            token = Some(next);
        }
        assert_eq!(
            page_sizes, row.expected_page_sizes,
            "case {case:?}: expected pages of {:?}, got {page_sizes:?}",
            row.expected_page_sizes
        );
        assert_eq!(
            seen, all_ids,
            "case {case:?}: the chain must yield every instance of the pool once, in order; \
             expected {all_ids:?}, got {seen:?}"
        );
    }
}

/// One row of the absent-pool table: what the group runs for itself while holding no warm
/// pool.
struct AbsentRow {
    case: &'static str,
    max_size: i32,
    desired_capacity: i32,
}

/// A group that holds no warm pool is REPORTED, not refused: the answer carries neither a
/// configuration nor an instance — not even the instances the group runs for itself — and
/// reading it again answers the same, because a read changes nothing.
#[tokio::test]
async fn a_group_with_no_warm_pool_is_reported_rather_than_refused() {
    let rows = [
        AbsentRow {
            case: "a group running nothing",
            max_size: 1,
            desired_capacity: 0,
        },
        AbsentRow {
            case: "a group running instances of its own, which are not the pool's",
            max_size: 2,
            desired_capacity: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One emulator per row: each row is its own account, so the instances of one row
        // never explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let template = format!("lt-warm-absent-{index}");
        create_launch_template(&router, &template).await;
        let group = format!("asg-warm-absent-{index}");
        create_group(
            &router,
            &group,
            &template,
            &subnets[0].id,
            row.max_size,
            row.desired_capacity,
        )
        .await;
        let owned = settled_group_instances(
            &router,
            &group,
            usize::try_from(row.desired_capacity).expect("a non-negative desired capacity"),
        )
        .await;

        for read in ["first", "second"] {
            let (status, body) = describe_warm_pool(&router, &group, None, None).await;
            assert_eq!(
                status, 200,
                "case {case:?} ({read} read): DescribeWarmPool expected 200, got {status}: \
                 {body}"
            );
            assert_eq!(
                tag_value(&body, "WarmPoolConfiguration"),
                "",
                "case {case:?} ({read} read): a group with no warm pool reports no \
                 configuration, got {body}"
            );
            assert_eq!(
                tag_values(&body, "InstanceId"),
                Vec::<String>::new(),
                "case {case:?} ({read} read): a group with no warm pool reports no pool \
                 instance; the group's own are {owned:?}, got {body}"
            );
            assert_eq!(
                tag_value(&body, "NextToken"),
                "",
                "case {case:?} ({read} read): there is no further page to resume at, \
                 got {body}"
            );
        }
        // The group still runs exactly what it ran before the two reads: reading the pool
        // neither launched an instance nor took one away.
        assert_eq!(
            group_instances(&router, &group).await,
            owned,
            "case {case:?}: the group's own instances must be untouched by a pool read"
        );
    }
}
