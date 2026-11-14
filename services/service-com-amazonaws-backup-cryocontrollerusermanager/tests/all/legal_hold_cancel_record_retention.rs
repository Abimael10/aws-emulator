// SPDX-License-Identifier: CC0-1.0

//! What `CancelLegalHold` RECORDS on the hold it releases, driven as real HTTP
//! through the router.
//!
//! The RULE under test: cancelling a legal hold that is in force records on that
//! hold the terminal `LegalHoldStatus`, the reason the request stated, the
//! instant the cancellation was made and — when, and only when, the request
//! states a day count — the instant that many days after that cancellation at
//! which the cancelled hold's record stops being retained
//! (`CancelLegalHoldInput$RetainRecordInDays`, "The integer amount, in days,
//! after which to remove legal hold", `smithy_query` operation
//! `CancelLegalHold`). The hold's record is KEPT rather than removed, so every
//! later read of it — the hold's own `GetLegalHold` and the account's
//! `ListLegalHolds` — reports those same recorded values, and reading it again
//! changes none of them.
//!
//! The rows of the table differ along what the rule generalises over: the day
//! count the request states (none at all, and counts from the same day out to a
//! century, none of them the one the recorded case used) and the form of the
//! reason it states (one word, a sentence with spaces, a sentence with
//! punctuation, and one far longer than any of them) — because what the
//! cancellation records is derived from the request and the clock, not from the
//! shape of either.
//!
//! The request harness (`router`, `call`, `call_reported`, `epoch_seconds`,
//! `string_at`) is the one `vault_and_backup_job.rs` exposes to this test
//! binary, so there is ONE wire path for the service's suites.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, call_reported, epoch_seconds, router, string_at};

/// How long a poll loop looks for the activation the CLOCK owes a fresh hold
/// before failing the case.
///
/// Generous on purpose: the transition falls due on the emulator's own clock, so
/// each case waits for the STATUS and this deadline only bounds a hold that
/// never comes into force at all — a loaded or a slow host still sees one that
/// does.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between two looks. A poll INTERVAL, not a budget:
/// the loop gives up only at [`POLL_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Seconds in a day — the unit `RetainRecordInDays` states its amount in.
const SECONDS_PER_DAY: f64 = 24.0 * 60.0 * 60.0;

/// How far apart two instants this suite compares may be and still be the same
/// instant: the protocol serialises a timestamp as fractional epoch seconds, so
/// a comparison allows for that rounding rather than demanding bit equality.
const INSTANT_SLACK: f64 = 0.01;

/// The `LegalHoldStatus` values a hold of this suite passes through, as the
/// model lists them (`smithy_query` shape `LegalHoldStatus`:
/// `CREATING | ACTIVE | CANCELING | CANCELED`).
pub(crate) const STATUS_CREATING: &str = "CREATING";
pub(crate) const STATUS_ACTIVE: &str = "ACTIVE";
pub(crate) const STATUS_CANCELED: &str = "CANCELED";

/// The vault name every case's recovery-point selection names: a hold IS the
/// hold it places on the recovery points its selection names, so a create states
/// one.
const RETENTION_VAULT: &str = "cancel-retention-vault";

/// One row of the cancellation table: what the request states, and so what the
/// hold has to report back afterwards.
struct CancelCase {
    name: &'static str,
    /// The `retainRecordInDays` the request states, or `None` when it states
    /// none.
    retain_record_in_days: Option<i64>,
    /// The `cancelDescription` the request states, percent-encoded as it travels
    /// on the query string.
    encoded_description: String,
    /// That same reason decoded, which is what the hold must report.
    description: String,
}

/// The rows of the table, spanning the day counts a request may state and the
/// forms its reason may take.
fn cancel_cases() -> Vec<CancelCase> {
    // A reason far longer than a sentence, spelled from characters a query
    // string carries unencoded, so the row differs in LENGTH and in nothing
    // else.
    let long_reason = "retention-schedule-reviewed-and-the-hold-is-no-longer-needed-".repeat(4);
    vec![
        CancelCase {
            name: "no day count, a one-word reason",
            retain_record_in_days: None,
            encoded_description: "released".to_string(),
            description: "released".to_string(),
        },
        CancelCase {
            name: "the same day, so the retention falls due at the cancellation",
            retain_record_in_days: Some(0),
            encoded_description: "retention%20due%20at%20once".to_string(),
            description: "retention due at once".to_string(),
        },
        CancelCase {
            name: "one day, a sentence with spaces",
            retain_record_in_days: Some(1),
            encoded_description: "audit%20closed%20on%20request".to_string(),
            description: "audit closed on request".to_string(),
        },
        CancelCase {
            name: "a fortnight, a sentence with punctuation",
            retain_record_in_days: Some(14),
            encoded_description: "hold%20lifted%3A%20matter%20settled%2C%20no%20appeal."
                .to_string(),
            description: "hold lifted: matter settled, no appeal.".to_string(),
        },
        CancelCase {
            name: "a year",
            retain_record_in_days: Some(365),
            encoded_description: "annual%20review%20complete".to_string(),
            description: "annual review complete".to_string(),
        },
        CancelCase {
            name: "a century, with a reason far longer than a sentence",
            retain_record_in_days: Some(36_500),
            encoded_description: long_reason.clone(),
            description: long_reason,
        },
    ]
}

impl CancelCase {
    /// The query string this row's cancellation travels with: the reason it
    /// states always, the day count only when the row states one.
    fn query(&self) -> String {
        match self.retain_record_in_days {
            None => format!("?cancelDescription={}", self.encoded_description),
            Some(days) => format!(
                "?cancelDescription={}&retainRecordInDays={days}",
                self.encoded_description
            ),
        }
    }
}

/// `POST /legal-holds` — `CreateLegalHold`'s HTTP binding — answering the id the
/// hold was stored under.
async fn create_legal_hold(router: &MultiServiceRouter, case: &str) -> String {
    let request = json!({
        "Title": format!("retention case: {case}"),
        "Description": format!("the hold this case cancels: {case}"),
        "RecoveryPointSelection": {"VaultNames": [RETENTION_VAULT]},
    });
    let (status, created) = call(router, "POST", "/legal-holds", Some(&request.to_string())).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLegalHold 200, got {status} {created:?}"
    );
    string_at(&created, "LegalHoldId")
}

/// `GET /legal-holds/{LegalHoldId}` — `GetLegalHold`'s HTTP binding, which is
/// what the recorded cancellation has to be visible through.
pub(crate) async fn get_legal_hold(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> Value {
    let (status, read) = call(router, "GET", &format!("/legal-holds/{id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetLegalHold 200, got {status} {read:?}"
    );
    read
}

/// `GET /legal-holds` — `ListLegalHolds`'s HTTP binding: the holds the account
/// still holds, which a kept record stays among.
pub(crate) async fn list_legal_holds(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, listed) = call(router, "GET", "/legal-holds", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListLegalHolds 200, got {status} {listed:?}"
    );
    assert!(
        listed.get("NextToken").is_none(),
        "case {case:?}: this suite's holds fit one page, got {listed:?}"
    );
    listed
        .get("LegalHolds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `DELETE /legal-holds/{LegalHoldId}` — `CancelLegalHold`'s HTTP binding, with
/// the query string its `cancelDescription` and `retainRecordInDays` bind to.
/// Answers with the error shape the response NAMES beside its status and body.
pub(crate) async fn cancel_legal_hold(
    router: &MultiServiceRouter,
    id: &str,
    query: &str,
) -> (u16, String, Value) {
    call_reported(router, "DELETE", &format!("/legal-holds/{id}{query}"), None).await
}

/// The hold `id` once the clock has put it in force, looked for again until it
/// has or until [`POLL_DEADLINE`] passes: the activation falls due on the
/// emulator's own clock, so the case waits for the STATUS rather than for a
/// span.
pub(crate) async fn hold_in_force(router: &MultiServiceRouter, case: &str, id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_legal_hold(router, case, id).await;
        let status = string_at(&read, "Status");
        if status == STATUS_ACTIVE {
            return;
        }
        assert_eq!(
            status, STATUS_CREATING,
            "case {case:?}: a hold on its way into force reports {STATUS_CREATING:?} until it \
             gets there, got {read:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the hold to come into force within {POLL_DEADLINE:?}, \
             got {read:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A timestamp member as the fractional epoch seconds this protocol serialises
/// it as, or `None` when the answer omits it.
fn instant_at(body: &Value, member: &str) -> Option<f64> {
    body.get(member).and_then(Value::as_f64)
}

/// Whether an answer carries no members at all. `CancelLegalHoldOutput` declares
/// none (`smithy_query` operation `CancelLegalHold`), so everything the
/// cancellation recorded is reported by the hold's own reads rather than by the
/// call that made it.
fn carries_no_members(body: &Value) -> bool {
    body.is_null() || body.as_object().is_some_and(serde_json::Map::is_empty)
}

/// A cancellation records, on the hold it releases, the terminal status, the
/// reason the request stated, the instant it was made, and the record retention
/// the request's day count makes due — whatever that day count is, and whatever
/// form the reason takes.
#[tokio::test]
async fn a_cancellation_records_the_reason_and_the_retention_its_day_count_makes_due() {
    let router = router();
    let cases = cancel_cases();

    // Every row's hold is created BEFORE any of them is waited for, so the
    // activations the clock owes them all fall due together rather than one
    // after another.
    let mut holds = Vec::with_capacity(cases.len());
    for case in &cases {
        holds.push(create_legal_hold(&router, case.name).await);
    }

    for (case, id) in cases.iter().zip(&holds) {
        hold_in_force(&router, case.name, id).await;

        // The cancellation instant is the one the handler read between these
        // two, so the recorded instant has to fall inside the window.
        let before = SystemTime::now();
        let (status, errortype, answered) = cancel_legal_hold(&router, id, &case.query()).await;
        let after = SystemTime::now();
        assert_eq!(
            (status, errortype.as_str()),
            (201, "<unset>"),
            "case {:?}: expected the hold in force to be released, got {status} {answered:?}",
            case.name
        );
        assert!(
            carries_no_members(&answered),
            "case {:?}: a released hold's answer carries no members, got {answered:?}",
            case.name
        );

        let read = get_legal_hold(&router, case.name, id).await;
        assert_eq!(
            string_at(&read, "Status"),
            STATUS_CANCELED,
            "case {:?}: expected the released hold's terminal status, got {read:?}",
            case.name
        );
        assert_eq!(
            string_at(&read, "CancelDescription"),
            case.description,
            "case {:?}: expected the reason the request stated, got {read:?}",
            case.name
        );

        let cancelled_at = instant_at(&read, "CancellationDate").unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the cancellation instant to be recorded, got {read:?}",
                case.name
            )
        });
        let window = (epoch_seconds(before) - INSTANT_SLACK)..=(epoch_seconds(after) + INSTANT_SLACK);
        assert!(
            window.contains(&cancelled_at),
            "case {:?}: expected the cancellation instant inside {window:?}, got {cancelled_at} \
             from {read:?}",
            case.name
        );

        match case.retain_record_in_days {
            // A request that states no day count makes no record retention due.
            None => assert!(
                read.get("RetainRecordUntil").is_none(),
                "case {:?}: a cancellation stating no day count records no retention, \
                 got {read:?}",
                case.name
            ),
            Some(days) => {
                let until = instant_at(&read, "RetainRecordUntil").unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected the record retention to be recorded, got {read:?}",
                        case.name
                    )
                });
                let owed = days as f64 * SECONDS_PER_DAY;
                let recorded = until - cancelled_at;
                assert!(
                    (recorded - owed).abs() <= INSTANT_SLACK,
                    "case {:?}: expected the record retention {owed} s after the cancellation, \
                     got {recorded} s (RetainRecordUntil {until}, CancellationDate \
                     {cancelled_at})",
                    case.name
                );
            }
        }

        // The record is KEPT and reading it changes nothing: the same read
        // answers the same members again.
        let again = get_legal_hold(&router, case.name, id).await;
        assert_eq!(
            again, read,
            "case {:?}: a second read of the released hold reports the same record, \
             got {again:?}",
            case.name
        );
    }

    // Every hold this suite cancelled is still one the account holds, each
    // reporting its own cancellation.
    let listed = list_legal_holds(&router, "every released hold").await;
    assert_eq!(
        listed.len(),
        holds.len(),
        "expected every released hold's record to be kept, got {listed:?}"
    );
    for (case, id) in cases.iter().zip(&holds) {
        let entry = listed
            .iter()
            .find(|hold| string_at(hold, "LegalHoldId") == *id)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the released hold to still be listed, got {listed:?}",
                    case.name
                )
            });
        assert_eq!(
            string_at(entry, "Status"),
            STATUS_CANCELED,
            "case {:?}: expected the listed hold to report its cancellation, got {entry:?}",
            case.name
        );
        assert!(
            instant_at(entry, "CancellationDate").is_some(),
            "case {:?}: expected the listed hold to report when it was cancelled, got {entry:?}",
            case.name
        );
    }
}

/// What a cancellation records comes from the REQUEST, not from the operation:
/// two holds of the same account cancelled with different day counts record
/// retentions that differ by exactly the difference between those counts, and a
/// hold nothing cancelled records no cancellation at all.
#[tokio::test]
async fn two_cancellations_record_the_retention_each_one_asked_for() {
    // (case, the day count the request states)
    let asked: [(&str, i64); 3] = [
        ("the shortest retention", 1),
        ("a quarter's retention", 90),
        ("a decade's retention", 3_650),
    ];

    let router = router();
    let mut holds = Vec::with_capacity(asked.len() + 1);
    for (case, _days) in asked {
        holds.push(create_legal_hold(&router, case).await);
    }
    // The control: a hold this test never cancels.
    let untouched = create_legal_hold(&router, "never cancelled").await;

    let mut recorded = Vec::with_capacity(asked.len());
    for ((case, days), id) in asked.iter().zip(&holds) {
        hold_in_force(&router, case, id).await;
        let query = format!("?cancelDescription=recorded-retention&retainRecordInDays={days}");
        let (status, _errortype, answered) = cancel_legal_hold(&router, id, &query).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected the hold in force to be released, got {status} {answered:?}"
        );

        let read = get_legal_hold(&router, case, id).await;
        let cancelled_at = instant_at(&read, "CancellationDate").unwrap_or_else(|| {
            panic!("case {case:?}: expected the cancellation instant, got {read:?}")
        });
        let until = instant_at(&read, "RetainRecordUntil").unwrap_or_else(|| {
            panic!("case {case:?}: expected the record retention, got {read:?}")
        });
        recorded.push((*case, *days, until - cancelled_at));
    }

    for (case, days, span) in &recorded {
        let owed = *days as f64 * SECONDS_PER_DAY;
        assert!(
            (span - owed).abs() <= INSTANT_SLACK,
            "case {case:?}: expected a retention {owed} s after the cancellation, got {span} s"
        );
    }
    // Two different day counts cannot record the same span, or the retention
    // would not be derived from what the request asked for.
    for pair in recorded.windows(2) {
        let (first, second) = (&pair[0], &pair[1]);
        assert!(
            (first.2 - second.2).abs() > INSTANT_SLACK,
            "cases {:?} and {:?} asked for {} and {} days, so their retentions must differ, \
             got {} s and {} s",
            first.0,
            second.0,
            first.1,
            second.1,
            first.2,
            second.2
        );
    }

    // The hold nothing cancelled records nothing a cancellation writes, so the
    // records above are what those calls wrote rather than a default.
    hold_in_force(&router, "never cancelled", &untouched).await;
    let read = get_legal_hold(&router, "never cancelled", &untouched).await;
    assert_eq!(
        string_at(&read, "Status"),
        STATUS_ACTIVE,
        "a hold nothing cancelled is still in force, got {read:?}"
    );
    for member in ["CancellationDate", "CancelDescription", "RetainRecordUntil"] {
        assert!(
            read.get(member).is_none(),
            "a hold nothing cancelled records no {member}, got {read:?}"
        );
    }
}
