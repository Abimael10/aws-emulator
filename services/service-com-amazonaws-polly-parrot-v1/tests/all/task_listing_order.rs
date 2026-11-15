// SPDX-License-Identifier: CC0-1.0

//! The ORDER `ListSpeechSynthesisTasks` serves its report in, driven over HTTP through
//! the same router the emulator serves with.
//!
//! RULE: the report is ordered by creation date MOST RECENTLY CREATED FIRST — of two
//! tasks the service holds, the one with the later `CreationTime` occupies the lower
//! index in the concatenation of every page, and that order agrees with the
//! `CreationTime` each task itself reports through `GetSpeechSynthesisTask`. Following
//! `NextToken` walks that same descending sequence, whatever page size the caller asked
//! for and whatever `Status` it filtered on.
//!
//! The rule is checked over a TABLE rather than the two tasks a report happens to quote:
//! the rows differ along what the rule generalises over — how far apart the tasks were
//! created (a microsecond, a fraction of a millisecond, a millisecond, a second, minutes,
//! hours, and a tie), whether the report is filtered by status, and the page size the
//! caller bounded it with (both published bounds of `MaxResults`, sizes in between, and no
//! bound at all). One row drives the start call itself, so the order is checked over
//! creation times the SERVICE minted as well as over fixed ones.
//!
//! The harness and the readers come from `crate::lexicons_and_speech`, the listing
//! helpers from `crate::synthesis_tasks` and the task fixtures from `crate::support`: a
//! row that fixes a creation time seeds the shared store directly, the way
//! `StartSpeechSynthesisTask` writes it.

use std::time::{Duration, SystemTime};

use crate::lexicons_and_speech::{json, Polly};
use crate::support::{fixture_task_id, get_task, seed_task, stored_task};
use crate::synthesis_tasks::{list, paged_task_ids, task_ids};

/// How many tasks one row of the table holds: enough that several page sizes cut the
/// report in different places and that a status filter leaves more than one task behind.
const TASKS_PER_ROW: u64 = 6;

/// How far apart two consecutive tasks of a row were created. The spans cover the class
/// the rule generalises over, from a fraction of a millisecond — where the order is owed
/// at a finer precision than a whole millisecond, which is what a truncating order key
/// gets wrong — up to hours, and `Duration::ZERO`, where the tasks share a creation
/// instant and the rule demands no order between them.
const SPACINGS: [(&str, Duration); 7] = [
    ("a microsecond apart", Duration::from_micros(1)),
    ("a fraction of a millisecond apart", Duration::from_micros(100)),
    ("a millisecond apart", Duration::from_millis(1)),
    ("a second apart", Duration::from_secs(1)),
    ("minutes apart", Duration::from_secs(90)),
    ("an hour apart", Duration::from_secs(3_600)),
    ("created at the same instant", Duration::ZERO),
];

/// The page bounds a row lists under: both published bounds of `MaxResults` (1 and 100),
/// a size in between that cuts the report mid-way, and no bound at all.
const PAGE_BOUNDS: [Option<usize>; 4] = [None, Some(1), Some(2), Some(100)];

/// The `CreationTime` a task itself reports, as `GetSpeechSynthesisTask` answers it:
/// epoch seconds.
async fn reported_creation_time(polly: &Polly, task_id: &str) -> f64 {
    let response = get_task(polly, task_id).await;
    assert_eq!(
        response.status(),
        200,
        "task {task_id}: the task read must succeed, body {:?}",
        json(&response)
    );
    let body = json(&response);
    body["SynthesisTask"]["CreationTime"]
        .as_f64()
        .unwrap_or_else(|| panic!("task {task_id}: no CreationTime in {body:?}"))
}

/// Pair every id of a reported sequence with the `CreationTime` the task reports for
/// itself, so the order of the listing is judged against the tasks' OWN timestamps.
async fn with_creation_times(polly: &Polly, ids: &[String]) -> Vec<(String, f64)> {
    let mut paired: Vec<(String, f64)> = Vec::new();
    for task_id in ids {
        let created = reported_creation_time(polly, task_id).await;
        paired.push((task_id.clone(), created));
    }
    paired
}

/// Assert that a reported sequence is newest first: no task is reported ahead of one
/// created later than it.
fn assert_newest_first(case: &str, reported: &[(String, f64)]) {
    for pair in reported.windows(2) {
        let (ahead_id, ahead_created) = &pair[0];
        let (behind_id, behind_created) = &pair[1];
        assert!(
            ahead_created >= behind_created,
            "case {case:?}: {ahead_id:?} (CreationTime {ahead_created}) is reported ahead of \
             {behind_id:?} (CreationTime {behind_created}), but the report is newest first; \
             got {reported:?}"
        );
    }
}

/// Seed one row's tasks, oldest first, alternating between `completed` and `inProgress`,
/// and answer their ids in creation order (oldest first).
fn seed_row(polly: &Polly, label: &str, spacing: Duration, now: SystemTime) -> Vec<String> {
    (0..TASKS_PER_ROW)
        .map(|index| {
            let task_id = fixture_task_id(&format!("{label}-{index}"));
            let created = now - spacing * u32::try_from(TASKS_PER_ROW - index).unwrap_or(1);
            let mut task = stored_task(&task_id, created);
            task.task_status = if index % 2 == 0 {
                "completed"
            } else {
                "inProgress"
            }
            .to_string();
            seed_task(polly, &task);
            task_id
        })
        .collect()
}

/// RULE: of two tasks the service holds, the one created later is reported at a lower
/// index in the concatenation of every `ListSpeechSynthesisTasks` page — under every page
/// size and every `Status` filter — and that order agrees with the `CreationTime` each
/// task reports for itself.
#[tokio::test]
async fn the_report_lists_the_most_recently_created_task_first() {
    for (spacing_case, spacing) in SPACINGS {
        // One store per row, so the row's tasks are the whole report and the order
        // checked is the row's own.
        let polly = Polly::new();
        let now = SystemTime::now();
        let label = spacing_case.replace(' ', "-");
        let oldest_first = seed_row(&polly, &label, spacing, now);

        for status in [None, Some("completed"), Some("inProgress")] {
            // What the store holds for this filter, newest first — the order the report
            // owes, which for a row created at one instant is only a set.
            let mut expected: Vec<String> = oldest_first
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    status.is_none_or(|status| {
                        (index % 2 == 0) == (status == "completed")
                    })
                })
                .map(|(_, task_id)| task_id.clone())
                .collect();
            expected.reverse();

            for max_results in PAGE_BOUNDS {
                let case = format!(
                    "{spacing_case}, Status={status:?}, MaxResults={max_results:?}"
                );
                let reported = paged_task_ids(&polly, status, max_results).await;
                let paired = with_creation_times(&polly, &reported).await;
                assert_newest_first(&case, &paired);

                if spacing.is_zero() {
                    // A tie orders nothing, so only the set and the count are owed —
                    // and no task may be reported twice or dropped by the paging.
                    let mut sorted = reported.clone();
                    sorted.sort();
                    let mut wanted = expected.clone();
                    wanted.sort();
                    assert_eq!(
                        sorted, wanted,
                        "case {case:?}: the pages together must be every matching task once, \
                         got {reported:?}"
                    );
                } else {
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected the tasks newest first, got {reported:?}"
                    );
                }
            }

            // The client that reads only the FIRST page to find its latest task: the
            // first task of the first page is the newest one the filter matches.
            if !spacing.is_zero() {
                let case = format!("{spacing_case}, Status={status:?}, first page only");
                let first_page = task_ids(&json(
                    &list(
                        &polly,
                        &match status {
                            Some(status) => format!("?MaxResults=1&Status={status}"),
                            None => "?MaxResults=1".to_string(),
                        },
                    )
                    .await,
                ));
                assert_eq!(
                    first_page.first(),
                    expected.first(),
                    "case {case:?}: expected the newest matching task, got {first_page:?}"
                );
            }
        }
    }
}

/// RULE: the same order holds for creation times the SERVICE minted — the task the
/// listing reports first is the one with the latest `CreationTime` of those the service
/// holds, and no task is reported ahead of one created after it.
#[tokio::test]
async fn a_started_task_is_reported_ahead_of_the_tasks_started_before_it() {
    let polly = Polly::new();
    let mut started: Vec<String> = Vec::new();
    // Different text per start, so the rows are distinct tasks rather than one request
    // repeated; the creation time of each is the service's own.
    for (index, text) in [
        "First task started for the ordering check.",
        "Second task.",
        "Third task, a little longer than the others.",
        "Fourth.",
        "Fifth task started for the ordering check.",
    ]
    .into_iter()
    .enumerate()
    {
        let request = serde_json::json!({
            "Text": text,
            "VoiceId": "Joanna",
            "OutputFormat": "mp3",
            "OutputS3BucketName": "polly-output",
        });
        let response = polly
            .call("POST", "/v1/synthesisTasks", &request.to_string())
            .await;
        assert_eq!(
            response.status(),
            200,
            "start {index}: the task must be created, body {:?}",
            json(&response)
        );
        let body = json(&response);
        started.push(
            body["SynthesisTask"]["TaskId"]
                .as_str()
                .unwrap_or_else(|| panic!("start {index}: no TaskId in {body:?}"))
                .to_string(),
        );
    }

    for max_results in PAGE_BOUNDS {
        let case = format!("started tasks, MaxResults={max_results:?}");
        let reported = paged_task_ids(&polly, None, max_results).await;
        assert_eq!(
            reported.len(),
            started.len(),
            "case {case:?}: every started task must be reported, got {reported:?}"
        );
        let paired = with_creation_times(&polly, &reported).await;
        assert_newest_first(&case, &paired);

        // The newest task the service holds leads the report, whether or not any pair of
        // starts landed in the same millisecond. The comparison is the exact one: both
        // values come from the same set of reported timestamps.
        let latest = paired
            .iter()
            .map(|(_, created)| *created)
            .fold(f64::NEG_INFINITY, f64::max);
        let leading = paired
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the report is empty"));
        assert_eq!(
            leading.1.total_cmp(&latest),
            std::cmp::Ordering::Equal,
            "case {case:?}: the report leads with {:?} (CreationTime {}), but the latest \
             CreationTime it holds is {latest}; got {paired:?}",
            leading.0,
            leading.1
        );
    }
}
