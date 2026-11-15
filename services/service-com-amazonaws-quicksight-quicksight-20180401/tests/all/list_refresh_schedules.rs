// SPDX-License-Identifier: CC0-1.0

//! `ListRefreshSchedules`: the refresh schedules one data set holds, over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the route, the status, and a body the handler computed from the shared store.
//!
//! The rule under test: a refresh schedule is a child of one data set, so the list a
//! data set reports is exactly the schedules stored under `(AwsAccountId, DataSetId)`,
//! each reported as the single stored record the create persisted (the same structure
//! `DescribeRefreshSchedule` answers with, carrying the ARN the service minted); a data
//! set holding none reports an empty list, a data set the account does not hold is
//! refused as not found, and listing — a report — leaves every later caller's view
//! unchanged.
//!
//! So the rows below vary what the rule generalises over — how many schedules the data
//! set holds (none up to the documented allowance), the interval and refresh type, which
//! optional frequency members each schedule carries, the order the schedules were
//! created in, the id's form, the data set, the account and the region — rather than
//! re-running one recorded call.

use serde_json::{json, Value};

use service_com_amazonaws_quicksight_quicksight_20180401::state as qs_state;

use super::data_source_data_set_lifecycle::Harness;
use super::refresh_schedule_lifecycle::{
    create_schedule_in, daily_schedule, data_set_fixture_in, MAX_SCHEDULES_PER_DATA_SET,
};

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// The region most rows sign for.
const REGION: &str = "us-west-2";

/// `ListRefreshSchedules`:
/// `GET /accounts/{AwsAccountId}/data-sets/{DataSetId}/refresh-schedules`. Source:
/// `smithy_query(query="operation", operation="ListRefreshSchedules")`.
async fn list_schedules_in(
    harness: &Harness,
    region: &str,
    account: &str,
    data_set_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "GET",
            &format!("/accounts/{account}/data-sets/{data_set_id}/refresh-schedules"),
            None,
        )
        .await
}

/// `DescribeRefreshSchedule`, the single-schedule read the list must agree with. Source:
/// `smithy_query(query="operation", operation="DescribeRefreshSchedule")`.
async fn describe_schedule_in(
    harness: &Harness,
    region: &str,
    account: &str,
    data_set_id: &str,
    schedule_id: &str,
) -> (u16, Value) {
    harness
        .call_in_region(
            region,
            "GET",
            &format!("/accounts/{account}/data-sets/{data_set_id}/refresh-schedules/{schedule_id}"),
            None,
        )
        .await
}

fn refresh_schedule_arn(region: &str, account: &str, data_set_id: &str, schedule_id: &str) -> String {
    format!(
        "arn:aws:quicksight:{region}:{account}:dataset/{data_set_id}/refresh-schedule/{schedule_id}"
    )
}

/// The entries of a `ListRefreshSchedules` body, ordered by `ScheduleId` so a row can
/// compare two lists without depending on which order the schedules were created in.
fn listed_schedules(body: &Value, case: &str) -> Vec<Value> {
    let mut entries = body["RefreshSchedules"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a RefreshSchedules list, got {body}"))
        .clone();
    entries.sort_by_key(|entry| entry["ScheduleId"].as_str().unwrap_or_default().to_string());
    entries
}

/// One data set, and the `Schedule` bodies it holds.
struct ListCase {
    /// Case name, reported by every assertion below.
    name: &'static str,
    /// The region the row signs for — the region in every ARN the row expects.
    region: &'static str,
    /// The 12-digit account the row addresses.
    account: &'static str,
    /// The data set the row hangs its schedules on.
    data_set_id: &'static str,
}

impl ListCase {
    /// The `Schedule` (`RefreshSchedule`) bodies this row creates, in creation order.
    fn schedules(&self) -> Vec<Value> {
        match self.name {
            // A data set that holds nothing: the list is empty, not absent.
            "no schedules" => vec![],
            // One schedule, carrying every optional frequency member plus
            // `StartAfterDateTime`.
            "one fully specified daily schedule" => vec![json!({
                "ScheduleId": "nightly",
                "RefreshType": "FULL_REFRESH",
                "ScheduleFrequency": {
                    "Interval": "DAILY",
                    "TimeOfTheDay": "03:15",
                    "Timezone": "Europe/Berlin",
                },
                // Epoch seconds: restJson1 renders a timestamp as an epoch number.
                "StartAfterDateTime": 1_767_225_600,
            })],
            // Three schedules created in an order the sorted list does not keep, so the
            // list is read back by identity rather than by arrival.
            "three schedules created out of order" => vec![
                daily_schedule("zulu-monthly", "MONTHLY"),
                daily_schedule("alpha.daily_v2", "DAILY"),
                json!({
                    "ScheduleId": "mike-weekly",
                    "RefreshType": "FULL_REFRESH",
                    "ScheduleFrequency": {
                        "Interval": "WEEKLY",
                        "RefreshOnDay": { "DayOfWeek": "THURSDAY" },
                        "TimeOfTheDay": "23:59",
                    },
                }),
            ],
            // The documented allowance, one schedule per sub-daily interval plus two
            // longer ones: the largest list a data set can report.
            "the whole documented allowance" => vec![
                json!({
                    "ScheduleId": "every-15",
                    "RefreshType": "INCREMENTAL_REFRESH",
                    "ScheduleFrequency": { "Interval": "MINUTE15", "Timezone": "America/New_York" },
                }),
                json!({
                    "ScheduleId": "every-30",
                    "RefreshType": "INCREMENTAL_REFRESH",
                    "ScheduleFrequency": { "Interval": "MINUTE30" },
                }),
                json!({
                    "ScheduleId": "hourly",
                    "RefreshType": "INCREMENTAL_REFRESH",
                    "ScheduleFrequency": { "Interval": "HOURLY" },
                }),
                daily_schedule("daily", "DAILY"),
                json!({
                    "ScheduleId": "month-end",
                    "RefreshType": "FULL_REFRESH",
                    "ScheduleFrequency": {
                        "Interval": "MONTHLY",
                        // The model's `DayOfMonth` pattern allows this sentinel.
                        "RefreshOnDay": { "DayOfMonth": "LAST_DAY_OF_MONTH" },
                    },
                }),
            ],
            // Another region, another account, a one-character data set id.
            "another account in another region" => vec![
                daily_schedule("i", "DAILY"),
                daily_schedule("weekly", "WEEKLY"),
            ],
            other => panic!("no schedules declared for case {other}"),
        }
    }
}

/// Rows varying the count, the intervals, the refresh types, the optional members, the
/// creation order, the id forms, the account and the region. The `RefreshInterval`
/// members each row names come from
/// `smithy_query(query="shape", shape="RefreshInterval")`.
const LIST_CASES: &[ListCase] = &[
    ListCase {
        name: "no schedules",
        region: REGION,
        account: ACCOUNT,
        data_set_id: "orders",
    },
    ListCase {
        name: "one fully specified daily schedule",
        region: REGION,
        account: ACCOUNT,
        data_set_id: "orders.by_day-2026",
    },
    ListCase {
        name: "three schedules created out of order",
        region: "us-east-1",
        account: ACCOUNT,
        data_set_id: "revenue_rollup",
    },
    ListCase {
        name: "the whole documented allowance",
        region: REGION,
        account: ACCOUNT,
        data_set_id: "sales.q3_2026-rollup",
    },
    ListCase {
        name: "another account in another region",
        region: "eu-central-1",
        account: OTHER_ACCOUNT,
        data_set_id: "i",
    },
];

/// Create every schedule `case` declares, and answer the created ids in creation order.
async fn fill(harness: &Harness, case: &ListCase) -> Vec<String> {
    let name = case.name;
    let mut ids = Vec::new();
    for schedule in case.schedules() {
        let schedule_id = schedule["ScheduleId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name}: a row's schedule carries no ScheduleId"))
            .to_string();
        let (status, body) = create_schedule_in(
            harness,
            case.region,
            case.account,
            case.data_set_id,
            schedule.clone(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name}/{schedule_id}: create expected 200, got {status} with body {body}"
        );
        ids.push(schedule_id);
    }
    assert!(
        ids.len() <= MAX_SCHEDULES_PER_DATA_SET,
        "case {name}: a row may not exceed the documented allowance, got {} schedule(s)",
        ids.len()
    );
    ids
}

#[tokio::test]
async fn a_data_set_lists_exactly_the_schedules_it_holds() {
    for case in LIST_CASES {
        let name = case.name;
        let harness = Harness::new();
        data_set_fixture_in(&harness, case.region, case.account, case.data_set_id).await;
        let created = fill(&harness, case).await;

        let (status, body) =
            list_schedules_in(&harness, case.region, case.account, case.data_set_id).await;
        assert_eq!(
            status, 200,
            "case {name}: list expected 200, got {status} with body {body}"
        );
        // `Status` is `@httpResponseCode`-bound, so it IS the HTTP status asserted above
        // rather than a body member.
        assert_eq!(
            body["Status"],
            Value::Null,
            "case {name}: Status is bound to the HTTP status, so it is not a body member: {body}"
        );
        let request_id = body["RequestId"].as_str().unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {name}: expected a RequestId, got {body}"
        );

        let entries = listed_schedules(&body, name);
        assert_eq!(
            entries.len(),
            created.len(),
            "case {name}: expected {} schedule(s), got {} in {body}",
            created.len(),
            entries.len()
        );

        let mut expected_ids = created.clone();
        expected_ids.sort();
        let listed_ids: Vec<String> = entries
            .iter()
            .map(|entry| entry["ScheduleId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            listed_ids, expected_ids,
            "case {name}: expected the ids {expected_ids:?}, got {listed_ids:?}"
        );

        // Each listed entry is the stored record, which is what the single-schedule read
        // reports too — including the ARN the service minted for the schedule.
        for entry in &entries {
            let schedule_id = entry["ScheduleId"].as_str().unwrap_or_default().to_string();
            let expected_arn =
                refresh_schedule_arn(case.region, case.account, case.data_set_id, &schedule_id);
            assert_eq!(
                entry["Arn"],
                json!(expected_arn),
                "case {name}/{schedule_id}: expected the data-set-scoped ARN {expected_arn:?}, \
                 got {entry}"
            );
            let (status, described) = describe_schedule_in(
                &harness,
                case.region,
                case.account,
                case.data_set_id,
                &schedule_id,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name}/{schedule_id}: describe expected 200, got {status} with body \
                 {described}"
            );
            assert_eq!(
                &described["RefreshSchedule"], entry,
                "case {name}/{schedule_id}: the list and the single read must report the same \
                 stored schedule: describe gave {}, list gave {entry}",
                described["RefreshSchedule"]
            );
        }
    }
}

#[tokio::test]
async fn a_list_reports_only_the_addressed_data_sets_schedules() {
    let harness = Harness::new();
    // Three data sets sharing an id across two accounts, plus a sibling in the first
    // account: each holds its own schedules, so each list must report only its own.
    data_set_fixture_in(&harness, REGION, ACCOUNT, "orders").await;
    data_set_fixture_in(&harness, REGION, ACCOUNT, "orders-sibling").await;
    data_set_fixture_in(&harness, REGION, OTHER_ACCOUNT, "orders").await;

    let held: [(&str, &str, &[&str]); 3] = [
        (ACCOUNT, "orders", &["nightly", "weekly"]),
        (ACCOUNT, "orders-sibling", &["sibling-daily"]),
        (OTHER_ACCOUNT, "orders", &["other-account-daily"]),
    ];
    for (account, data_set_id, schedule_ids) in held {
        for schedule_id in schedule_ids {
            let interval = if schedule_id.contains("weekly") {
                "WEEKLY"
            } else {
                "DAILY"
            };
            let (status, body) = create_schedule_in(
                &harness,
                REGION,
                account,
                data_set_id,
                daily_schedule(schedule_id, interval),
            )
            .await;
            assert_eq!(
                status, 200,
                "fixture {account}/{data_set_id}/{schedule_id}: create expected 200, got \
                 {status} with body {body}"
            );
        }
    }

    for (account, data_set_id, schedule_ids) in held {
        let case = format!("{account}/{data_set_id}");
        let (status, body) = list_schedules_in(&harness, REGION, account, data_set_id).await;
        assert_eq!(
            status, 200,
            "case {case}: list expected 200, got {status} with body {body}"
        );
        let listed: Vec<String> = listed_schedules(&body, &case)
            .iter()
            .map(|entry| entry["ScheduleId"].as_str().unwrap_or_default().to_string())
            .collect();
        let mut expected: Vec<String> = schedule_ids.iter().map(|id| (*id).to_string()).collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case}: expected only its own schedules {expected:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let harness = Harness::new();
    data_set_fixture_in(&harness, REGION, ACCOUNT, "orders").await;
    // One short of the documented allowance, so a consumed allowance would show up as a
    // refused create below.
    let mut created = Vec::new();
    for index in 0..MAX_SCHEDULES_PER_DATA_SET - 1 {
        let schedule_id = format!("fill-{index}");
        let (status, body) = create_schedule_in(
            &harness,
            REGION,
            ACCOUNT,
            "orders",
            daily_schedule(&schedule_id, "DAILY"),
        )
        .await;
        assert_eq!(
            status, 200,
            "fixture {schedule_id}: create expected 200, got {status} with body {body}"
        );
        created.push(schedule_id);
    }

    let (status, first) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
    assert_eq!(
        status, 200,
        "the first list expected 200, got {status} with body {first}"
    );
    let first_entries = listed_schedules(&first, "repeated list");
    // Repeated reads report the same schedules; only the per-call request id differs.
    for round in 1..4 {
        let (status, again) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
        assert_eq!(
            status, 200,
            "case round {round}: list expected 200, got {status} with body {again}"
        );
        assert_eq!(
            listed_schedules(&again, "repeated list"),
            first_entries,
            "case round {round}: a repeated list must report the same schedules, got {again}"
        );
    }

    let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
    let held = state.lock().refresh_schedules_in(ACCOUNT, "orders").len();
    assert_eq!(
        held,
        created.len(),
        "the lists must not have changed the stored schedules: expected {}, got {held}",
        created.len()
    );

    // The allowance was not consumed by reading: the data set still takes its last
    // schedule, and only then is it full.
    let (status, body) = create_schedule_in(
        &harness,
        REGION,
        ACCOUNT,
        "orders",
        daily_schedule("last-one", "DAILY"),
    )
    .await;
    assert_eq!(
        status, 200,
        "the schedule the allowance still had room for expected 200, got {status} with body {body}"
    );
    let (status, body) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
    assert_eq!(
        status, 200,
        "the list after the last create expected 200, got {status} with body {body}"
    );
    assert_eq!(
        listed_schedules(&body, "after the last create").len(),
        MAX_SCHEDULES_PER_DATA_SET,
        "expected the allowance's worth of schedules, got {body}"
    );
}

#[tokio::test]
async fn each_create_adds_exactly_its_schedule_to_the_list() {
    let harness = Harness::new();
    data_set_fixture_in(&harness, REGION, ACCOUNT, "orders").await;
    // Ids whose sorted order is not their creation order, so a list that merely echoed
    // arrivals would not match.
    let ids = ["zulu", "alpha", "mike", "bravo"];

    let (status, body) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
    assert_eq!(
        status, 200,
        "case before any create: list expected 200, got {status} with body {body}"
    );
    assert!(
        listed_schedules(&body, "before any create").is_empty(),
        "case before any create: a data set holding no schedules lists none, got {body}"
    );

    let mut expected: Vec<String> = Vec::new();
    for schedule_id in ids {
        let (status, body) = create_schedule_in(
            &harness,
            REGION,
            ACCOUNT,
            "orders",
            daily_schedule(schedule_id, "DAILY"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {schedule_id}: create expected 200, got {status} with body {body}"
        );
        expected.push(schedule_id.to_string());
        expected.sort();

        let (status, body) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
        assert_eq!(
            status, 200,
            "case {schedule_id}: list after the create expected 200, got {status} with body {body}"
        );
        let listed: Vec<String> = listed_schedules(&body, schedule_id)
            .iter()
            .map(|entry| entry["ScheduleId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            listed, expected,
            "case {schedule_id}: after the create expected {expected:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn listing_a_data_set_the_account_does_not_hold_is_refused() {
    let harness = Harness::new();
    data_set_fixture_in(&harness, REGION, ACCOUNT, "orders").await;
    let (status, body) = create_schedule_in(
        &harness,
        REGION,
        ACCOUNT,
        "orders",
        daily_schedule("nightly", "DAILY"),
    )
    .await;
    assert_eq!(
        status, 200,
        "fixture nightly: create expected 200, got {status} with body {body}"
    );

    // An id nothing created, and the held id read by another account: each names a data
    // set that account does not hold. (The store keys a data set by account and id, not
    // by region, so another region is the SAME data set and is not a miss.)
    let misses: [(&str, &str, &str, &str); 3] = [
        ("an id nothing created", REGION, ACCOUNT, "never-created"),
        ("another account", REGION, OTHER_ACCOUNT, "orders"),
        (
            "another account in another region",
            "ap-southeast-2",
            OTHER_ACCOUNT,
            "orders",
        ),
    ];
    for (case, region, account, data_set_id) in misses {
        let (status, body) = list_schedules_in(&harness, region, account, data_set_id).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} with body {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {body}"
        );
        // The data-set not-found wording: `DataSet <arn> is not found`, with the ARN built
        // from the signed region and the request's own path labels.
        let expected = format!(
            "DataSet arn:aws:quicksight:{region}:{account}:dataset/{data_set_id} is not found"
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case}: expected the message {expected:?}, got {:?}",
            body["Message"]
        );
    }

    // The refusals reported on state they did not disturb: the held data set still lists
    // its schedule.
    let (status, body) = list_schedules_in(&harness, REGION, ACCOUNT, "orders").await;
    assert_eq!(
        status, 200,
        "the held data set must stay listable, got {status} with body {body}"
    );
    let listed = listed_schedules(&body, "after the refusals");
    assert_eq!(
        listed.len(),
        1,
        "the held data set expected its one schedule, got {body}"
    );
    assert_eq!(
        listed[0]["ScheduleId"],
        json!("nightly"),
        "the held data set expected to still list `nightly`, got {body}"
    );
}
