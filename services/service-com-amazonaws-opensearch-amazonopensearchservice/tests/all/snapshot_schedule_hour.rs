// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the hour a domain's daily automated snapshot can be scheduled
//! at, on both paths that set it: `CreateDomain` and `UpdateDomainConfig`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`];
//! this suite adds no second copy of it.
//!
//! The rule under test: `SnapshotOptions$AutomatedSnapshotStartHour` is "the time, in UTC
//! format, when OpenSearch Service takes a daily automated snapshot of the specified
//! domain" (`smithy_query(query="shape",shape="SnapshotOptions")`) — an hour of the day,
//! so the values it admits are the 24 hours a day has and one outside them schedules a
//! snapshot at no time at all. Such a request is refused with the modelled
//! `ValidationException` (HTTP 400), worded as the constraint report every value outside
//! its member's bounds is refused with: the value the request carried, the member it was
//! carried at as the wire names it, and the bound it broke.
//!
//! So the assertions span SETS of inputs: a table of values past each end of the range —
//! each bound plus one, and values well past both — refused and storing nothing; every
//! hour of the day, each accepted and read back; and the update path, which refuses the
//! same values and keeps the stored schedule.

use serde_json::json;

use super::domain_lifecycle::{
    error_code, settled_domain, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};

/// The hours of the day, as this suite's OWN statement of the range the member admits:
/// the 24 hours a UTC day has.
const FIRST_HOUR: i64 = 0;
const LAST_HOUR: i64 = 23;

/// The values outside that range: each bound plus one past it, and values well past both
/// ends.
const OUTSIDE: [i64; 5] = [24, 25, 99, -1, -12];

/// This suite's OWN statement of the refusal text: the envelope and clause AWS states a
/// constraint violation in, naming the value, the member path and the bound it broke.
/// Restated here rather than imported from the service, so changing it in the service
/// fails this suite instead of being asserted against itself.
fn expected_message(hour: i64) -> String {
    let requirement = if hour < FIRST_HOUR {
        format!("Member must have value greater than or equal to {FIRST_HOUR}")
    } else {
        format!("Member must have value less than or equal to {LAST_HOUR}")
    };
    format!(
        "1 validation error detected: Value '{hour}' at \
         'snapshotOptions.automatedSnapshotStartHour' failed to satisfy constraint: \
         {requirement}"
    )
}

/// A domain name of the modelled shape (`^[a-z][a-z0-9\-]+$`, 3..28) for one row.
fn domain_name(prefix: &str, row: usize) -> String {
    format!("{prefix}-{row}")
}

/// Assert one answer IS the refusal the rule prescribes for `hour`.
fn assert_refused(answer: &Answer, hour: i64, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "ValidationException",
        "case {case}: expected the modelled validation refusal, got {:?}",
        answer.body
    );
    let actual = answer.body["message"].as_str().unwrap_or_default();
    assert_eq!(
        actual,
        expected_message(hour),
        "case {case}: expected the constraint report for the hour, got {actual:?}"
    );
}

#[tokio::test]
async fn an_hour_no_day_has_is_refused_on_create() {
    let emulator = Emulator::new();

    for (index, hour) in OUTSIDE.into_iter().enumerate() {
        let name = domain_name("snapshot-refused", index);
        let case = format!("CreateDomain({name}, AutomatedSnapshotStartHour={hour})");
        let answer = emulator
            .create(json!({
                "DomainName": name,
                "SnapshotOptions": { "AutomatedSnapshotStartHour": hour },
            }))
            .await;
        assert_refused(&answer, hour, &case);

        // A refused create mints nothing: the name is still free of any record.
        let read = emulator.describe(&name).await;
        assert_eq!(
            read.status, RECORDED_NOT_FOUND_STATUS,
            "case {case}: a refused create stores no domain, got {} {:?}",
            read.status, read.body
        );
    }
}

#[tokio::test]
async fn every_hour_of_the_day_is_accepted_and_reads_back() {
    let emulator = Emulator::new();

    for hour in FIRST_HOUR..=LAST_HOUR {
        let name = domain_name("snapshot-hour", usize::try_from(hour).unwrap_or(0));
        let case = format!("CreateDomain({name}, AutomatedSnapshotStartHour={hour})");
        let answer = emulator
            .create(json!({
                "DomainName": name,
                "SnapshotOptions": { "AutomatedSnapshotStartHour": hour },
            }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected an hour of the day to be accepted, got {} {:?}",
            answer.status, answer.body
        );

        // Round trip: the scheduled hour reads back from the record.
        let read = emulator.describe(&name).await;
        assert_eq!(
            read.body["DomainStatus"]["SnapshotOptions"]["AutomatedSnapshotStartHour"],
            json!(hour),
            "case {case}: expected the stored schedule to carry the hour, got {:?}",
            read.body["DomainStatus"]["SnapshotOptions"]
        );
    }
}

#[tokio::test]
async fn an_update_refuses_the_same_hours_and_keeps_the_stored_schedule() {
    let emulator = Emulator::new();
    let name = "snapshot-update";
    let created = emulator
        .create(json!({
            "DomainName": name,
            "SnapshotOptions": { "AutomatedSnapshotStartHour": 7 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({name}): expected 200, got {} {:?}",
        created.status, created.body
    );
    // The service applies one change of a domain at a time, so the change the create
    // started has to have landed before an update can start another: the case waits for
    // THAT condition rather than for a fixed stretch of time.
    settled_domain(&emulator, name).await;

    for hour in OUTSIDE {
        let case = format!("UpdateDomainConfig({name}, AutomatedSnapshotStartHour={hour})");
        let answer = emulator
            .update_config(
                name,
                json!({ "SnapshotOptions": { "AutomatedSnapshotStartHour": hour } }),
            )
            .await;
        assert_refused(&answer, hour, &case);

        let stored = emulator.describe_config(name).await;
        assert_eq!(
            stored.body["DomainConfig"]["SnapshotOptions"]["Options"]
                ["AutomatedSnapshotStartHour"],
            json!(7),
            "case {case}: a refused update leaves the stored schedule alone, got {:?}",
            stored.body["DomainConfig"]["SnapshotOptions"]
        );
    }
}
