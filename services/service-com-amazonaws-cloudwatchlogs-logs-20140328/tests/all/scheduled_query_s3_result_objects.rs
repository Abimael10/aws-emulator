// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OBJECT a scheduled query's run delivers to its Amazon S3
//! destination: the key it is written under and the document it holds.
//!
//! CloudWatch Logs and Amazon S3 are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so every object read back here is one the object store itself holds,
//! written by the run over the in-process callback and not by anything in this suite.
//!
//! The rule under test: a settled run of a scheduled query whose
//! `destinationConfiguration.s3Configuration` names `s3://<bucket>/<prefix>` delivers ONE object
//! to that bucket, keyed `<prefix>/<name>/<queryId>.json` — the query the results belong to and
//! the `queryId` `GetScheduledQueryHistory` reports for the run, as the JSON document a consumer
//! of a published result set parses — and the destination's `processedIdentifier` is that
//! object's `s3://` URI. The object's body is a JSON array holding one object per result row of
//! the run, each carrying exactly the fields the query projected, in the order the query ordered
//! the rows — the same result set `GetQueryResults` answers under that `queryId`.
//!
//! The table spans what the rule generalises over: the forms the destination URI's prefix takes
//! (a nested prefix with a trailing slash, one segment without it, and no prefix at all, where
//! the object lands under the query's own name at the bucket's root), how many fields the
//! pipeline projects, how it orders its rows, whether it aggregates them, and how many rows it
//! produces — including none at all, where the delivered document is the empty array.
//!
//! The fields a run delivers are the ones its query projected at EVERY destination the model's
//! `DestinationConfiguration` carries, so the second test here reads the same rule off the other
//! one: the lookup table a run refreshes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::export_task_objects::Emulator;

/// The operations under test, and the reads a delivery is proved through.
const CREATE: &str = "CreateScheduledQuery";
const HISTORY: &str = "GetScheduledQueryHistory";
const QUERY_RESULTS: &str = "GetQueryResults";
const GET_LOOKUP_TABLE: &str = "GetLookupTable";

/// The pointer `GetQueryResults` adds to each row of log records it answers, which a DELIVERED
/// result set does not carry.
const PTR: &str = "@ptr";

/// The log group every case queries, and the stream its events are written to.
const GROUP: &str = "/aws/emulator/scheduled-query-objects";
const STREAM: &str = "runs";

/// The IAM roles a scheduled query executes and delivers under.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-execution";
const DELIVERY_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-delivery";

/// The messages seeded into the group — two of them ERROR, which is what a filtering pipeline
/// of the table selects, in the order they are seeded.
const MESSAGES: [&str; 3] = [
    "ERROR order 1 failed",
    "INFO order 2 accepted",
    "ERROR order 3 failed",
];

/// How long a case is given to see the run the clock has made due, and how long it waits between
/// two looks. Generous on purpose: the condition — a record whose Amazon S3 destination has been
/// processed — is what is waited for, never a fixed span.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// Seed [`GROUP`] with [`MESSAGES`] an hour back, so every run's lookback window holds them.
async fn seed_events(emulator: &Emulator, case: &str) {
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    emulator
        .logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
        )
        .await;
    let first_ms = crate::harness::now_ms() - HOUR;
    let events: Vec<Value> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": first_ms + index as i64 * 1_000, "message": message})
        })
        .collect();
    emulator
        .logs_ok(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": events,
            }),
        )
        .await;
}

/// One row of the table: the pipeline a scheduled query runs, where its results are delivered,
/// and what the delivered document must then hold.
struct Row {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The name the scheduled query is created under, which also names its bucket.
    name: &'static str,
    /// The key prefix the destination URI names, as it appears in the URI — so a row can state
    /// it with a trailing slash, without one, or not at all.
    stated_prefix: &'static str,
    /// The pipeline the query runs.
    query_string: &'static str,
    /// The fields every delivered row carries, which are the ones the pipeline projects.
    fields: &'static [&'static str],
    /// The value of `probe` in each delivered row, in the order the rows must be delivered in.
    probe: &'static str,
    /// Those values, or none at all for a pipeline that selects nothing.
    probed: &'static [&'static str],
    /// Whether `probed` holds the values verbatim or the decimal each row's `probe` must parse
    /// to, for an aggregating pipeline whose count is a number rather than text.
    numeric: bool,
}

impl Row {
    /// The bucket this row's results are delivered to. One bucket per row, so the WHOLE bucket
    /// listing is what the delivered key is compared against: an object written anywhere else,
    /// or under any other key, shows up.
    fn bucket(&self) -> String {
        format!("scheduled-query-objects-{}", self.name)
    }

    /// The `destinationIdentifier` this row's `s3Configuration` names.
    fn destination_uri(&self) -> String {
        format!("s3://{}{}", self.bucket(), self.stated_prefix)
    }

    /// The key the run's object must be written under: the prefix the URI names, then the
    /// scheduled query the results belong to and the run's own `queryId` as the JSON document it
    /// delivers.
    fn expected_key(&self, query_id: &str) -> String {
        let prefix = self.stated_prefix.trim_matches('/');
        if prefix.is_empty() {
            format!("{}/{query_id}.json", self.name)
        } else {
            format!("{prefix}/{}/{query_id}.json", self.name)
        }
    }
}

/// The first record of `name`'s history whose Amazon S3 destination has been processed, polled
/// for because the run that processes it is owed by the clock.
async fn settled_record(emulator: &Emulator, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let page = emulator
            .logs_ok(
                case,
                HISTORY,
                &json!({
                    "identifier": name,
                    "startTime": 0,
                    "endTime": crate::harness::now_ms() / 1_000 + 3_600,
                }),
            )
            .await;
        let settled = page["triggerHistory"]
            .as_array()
            .and_then(|records| records.first())
            .filter(|record| {
                record["destinations"]
                    .as_array()
                    .is_some_and(|destinations| {
                        destinations.iter().any(|destination| {
                            destination["destinationType"].as_str() == Some("S3")
                                && destination["status"].as_str() != Some("IN_PROGRESS")
                        })
                    })
            })
            .cloned();
        if let Some(settled) = settled {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a run of {name} with its Amazon S3 destination processed, \
             got {page}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rows a delivered object holds: the JSON array's objects, each as its field/value pairs
/// sorted by field name, so two readings of one row compare whatever order the document names
/// its fields in.
fn delivered_rows(case: &str, body: &[u8]) -> Vec<Vec<(String, String)>> {
    let rendered = String::from_utf8_lossy(body).into_owned();
    let document: Value = serde_json::from_slice(body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the delivered object to be a JSON document, got \
             {rendered:?}: {error}"
        )
    });
    let array = document.as_array().unwrap_or_else(|| {
        panic!("case {case:?}: expected a JSON array of result rows, got {document}")
    });
    array
        .iter()
        .map(|row| {
            let fields = row.as_object().unwrap_or_else(|| {
                panic!("case {case:?}: expected every delivered row to be an object, got {row}")
            });
            let mut pairs: Vec<(String, String)> = fields
                .iter()
                .map(|(field, value)| {
                    let text = value.as_str().unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the value of {field} to be a string, got \
                             {value}"
                        )
                    });
                    (field.clone(), text.to_string())
                })
                .collect();
            pairs.sort();
            pairs
        })
        .collect()
}

/// The rows `GetQueryResults` answers under `query_id`, in the same shape [`delivered_rows`]
/// reads an object in — the result set the delivered document must equal.
///
/// The `@ptr` of the interactive answer is dropped: "the fields you specify in your query are
/// returned, along with a @ptr field, which is the identifier for the log record. You can use the
/// value of @ptr in a GetLogRecord operation to get the full log record" (`GetQueryResults`), so
/// that pointer is a handle this read hands its caller rather than a field of the result set.
async fn result_rows(emulator: &Emulator, case: &str, query_id: &str) -> Vec<Vec<(String, String)>> {
    let answer = emulator
        .logs_ok(case, QUERY_RESULTS, &json!({"queryId": query_id}))
        .await;
    answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueryResults answers a results list, got {answer}"))
        .iter()
        .map(|row| {
            let mut pairs: Vec<(String, String)> = row
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: every result row is a list, got {row}"))
                .iter()
                .map(|field| {
                    (
                        field["field"].as_str().unwrap_or_default().to_string(),
                        field["value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .filter(|(field, _)| field != PTR)
                .collect();
            pairs.sort();
            pairs
        })
        .collect()
}

/// The value of `field` in one read row, or the empty string when the row does not carry it.
fn value(row: &[(String, String)], field: &str) -> String {
    row.iter()
        .find(|(name, _)| name == field)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// A settled run delivers its result set to Amazon S3 as the JSON document keyed by its own
/// `queryId` under the prefix its destination names.
#[tokio::test]
async fn a_runs_results_are_delivered_as_the_json_object_its_query_id_keys() {
    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;

    let rows: [Row; 5] = [
        Row {
            case: "a nested prefix with a trailing slash, two projected fields",
            name: "nested-prefix",
            stated_prefix: "/results/errors/",
            query_string: "fields @timestamp, @message | filter @message like /ERROR/",
            fields: &["@timestamp", "@message"],
            probe: "@message",
            // A pipeline that orders nothing itself answers the newest event first.
            probed: &["ERROR order 3 failed", "ERROR order 1 failed"],
            numeric: false,
        },
        Row {
            case: "one prefix segment without a trailing slash, rows ordered oldest first",
            name: "flat-prefix",
            stated_prefix: "/history",
            query_string:
                "fields @timestamp, @message | filter @message like /ERROR/ | sort @timestamp asc",
            fields: &["@timestamp", "@message"],
            probe: "@message",
            probed: &["ERROR order 1 failed", "ERROR order 3 failed"],
            numeric: false,
        },
        Row {
            case: "no prefix at all, one projected field, one row",
            name: "bucket-root",
            stated_prefix: "",
            query_string: "fields @message | filter @message like /ERROR/ | limit 1",
            fields: &["@message"],
            probe: "@message",
            probed: &["ERROR order 3 failed"],
            numeric: false,
        },
        Row {
            case: "an aggregating pipeline, one row of its own field",
            name: "aggregated",
            stated_prefix: "/counts/",
            query_string: "stats count(*) as total",
            fields: &["total"],
            probe: "total",
            // Every seeded message is counted: the pipeline filters none of them out.
            probed: &["3"],
            numeric: true,
        },
        Row {
            case: "a pipeline that selects nothing",
            name: "no-rows",
            stated_prefix: "/empty/",
            query_string: "fields @timestamp, @message | filter @message like /NOTHING HERE/",
            fields: &[],
            probe: "@message",
            probed: &[],
            numeric: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let bucket = row.bucket();
        emulator.create_bucket(case, &bucket).await;
        emulator
            .logs_ok(
                case,
                CREATE,
                &json!({
                    "name": row.name,
                    "queryLanguage": "CWLI",
                    "queryString": row.query_string,
                    "scheduleExpression": "rate(1 day)",
                    "executionRoleArn": EXECUTION_ROLE,
                    "logGroupIdentifiers": [GROUP],
                    "startTimeOffset": 7_200,
                    "destinationConfiguration": {
                        "s3Configuration": {
                            "destinationIdentifier": row.destination_uri(),
                            "roleArn": DELIVERY_ROLE,
                        },
                    },
                }),
            )
            .await;

        let record = settled_record(&emulator, case, row.name).await;
        let query_id = record["queryId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: an execution reports the queryId it ran under, got {record}")
            })
            .to_string();
        let destination = record["destinations"]
            .as_array()
            .and_then(|destinations| {
                destinations
                    .iter()
                    .find(|destination| destination["destinationType"].as_str() == Some("S3"))
                    .cloned()
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: the execution reports its Amazon S3 destination, got {record}")
            });
        assert_eq!(
            destination["status"].as_str(),
            Some("COMPLETE"),
            "case {case:?}: expected the delivery to the bucket the store holds to complete, got \
             {destination}"
        );

        // The key the rule names, and the URI the destination reports for what it produced.
        let expected_key = row.expected_key(&query_id);
        let expected_uri = format!("s3://{bucket}/{expected_key}");
        assert_eq!(
            destination["processedIdentifier"].as_str(),
            Some(expected_uri.as_str()),
            "case {case:?}: expected the processed identifier {expected_uri:?}, got {destination}"
        );

        // The object store holds that key and nothing else: no differently-suffixed object and
        // nothing written under another key.
        let held = emulator.all_keys(case, &bucket).await;
        assert_eq!(
            held,
            vec![expected_key.clone()],
            "case {case:?}: expected {bucket} to hold exactly {expected_key:?}, got {held:?}"
        );
        let delivered_key = held.first().cloned().unwrap_or_default();
        assert!(
            delivered_key.ends_with(&format!("{query_id}.json")),
            "case {case:?}: expected the delivered key to end with the run's queryId and .json, \
             got {delivered_key:?}"
        );

        let body = emulator.object(case, &bucket, &expected_key).await;
        let delivered = delivered_rows(case, &body);
        assert_eq!(
            delivered.len(),
            row.probed.len(),
            "case {case:?}: expected {} delivered row(s), got {delivered:?}",
            row.probed.len()
        );
        for (index, (delivered_row, expected)) in delivered.iter().zip(row.probed).enumerate() {
            let mut names: Vec<&str> = delivered_row
                .iter()
                .map(|(field, _)| field.as_str())
                .collect();
            names.sort_unstable();
            let mut projected: Vec<&str> = row.fields.to_vec();
            projected.sort_unstable();
            assert_eq!(
                names, projected,
                "case {case:?}: expected row {index} to carry exactly {projected:?}, got \
                 {delivered_row:?}"
            );
            let actual = value(delivered_row, row.probe);
            if row.numeric {
                let expected_number = expected.parse::<f64>().unwrap_or_default();
                let actual_number = actual.parse::<f64>().unwrap_or_else(|error| {
                    panic!(
                        "case {case:?}: expected row {index}'s {} to be a number, got \
                         {actual:?}: {error}",
                        row.probe
                    )
                });
                assert!(
                    (actual_number - expected_number).abs() < f64::EPSILON,
                    "case {case:?}: expected row {index}'s {} to be {expected_number}, got \
                     {actual_number}",
                    row.probe
                );
            } else {
                assert_eq!(
                    actual, *expected,
                    "case {case:?}: expected row {index}'s {} to be {expected:?}, got {actual:?}",
                    row.probe
                );
            }
        }
        // No event the pipeline did not select is in the document, whatever it projected.
        let rendered = String::from_utf8_lossy(&body).into_owned();
        assert!(
            !rendered.contains("INFO order 2 accepted"),
            "case {case:?}: expected the delivered rows to be the ones the run produced, got \
             {rendered:?}"
        );

        // The delivered document IS the run's result set: the rows `GetQueryResults` answers
        // under the run's own queryId, in that order.
        let answered = result_rows(&emulator, case, &query_id).await;
        assert_eq!(
            delivered, answered,
            "case {case:?}: expected the delivered document to hold the rows the run produced, \
             got {delivered:?} against {answered:?}"
        );
    }
}

/// The fields a run DELIVERS are the ones its query projected at EVERY destination the model's
/// `DestinationConfiguration` carries, so the lookup table a run refreshes names those same
/// fields in its header row and no pointer of the interactive read.
#[tokio::test]
async fn a_delivered_lookup_table_names_the_fields_the_query_projected() {
    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;

    // (case, the query's name, the table it refreshes, the pipeline it runs, the fields the
    // delivered header row must name)
    let cases: [(&str, &str, &str, &str, &[&str]); 2] = [
        (
            "two projected fields",
            "table-two-fields",
            "scheduled_query_projected_two",
            "fields @timestamp, @message | filter @message like /ERROR/",
            &["@timestamp", "@message"],
        ),
        (
            "one projected field",
            "table-one-field",
            "scheduled_query_projected_one",
            "fields @message | filter @message like /ERROR/",
            &["@message"],
        ),
    ];

    for (case, name, table, query_string, fields) in cases {
        emulator
            .logs_ok(
                case,
                CREATE,
                &json!({
                    "name": name,
                    "queryLanguage": "CWLI",
                    "queryString": query_string,
                    "scheduleExpression": "rate(1 day)",
                    "executionRoleArn": EXECUTION_ROLE,
                    "logGroupIdentifiers": [GROUP],
                    "startTimeOffset": 7_200,
                    "destinationConfiguration": {
                        "lookupTableConfiguration": {
                            "tableName": table,
                            "roleArn": DELIVERY_ROLE,
                        },
                    },
                }),
            )
            .await;

        let record = settled_lookup_table_record(&emulator, case, name).await;
        let arn = record["destinations"]
            .as_array()
            .and_then(|destinations| {
                destinations
                    .iter()
                    .find(|destination| {
                        destination["destinationType"].as_str() == Some("LOOKUP_TABLE")
                    })
                    .and_then(|destination| destination["processedIdentifier"].as_str())
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: a processed delivery names the table it wrote, got {record}")
            })
            .to_string();
        let held = emulator
            .logs_ok(case, GET_LOOKUP_TABLE, &json!({"lookupTableArn": arn}))
            .await;
        let body = held["tableBody"].as_str().unwrap_or_default().to_string();
        let header: Vec<&str> = body
            .lines()
            .next()
            .unwrap_or_default()
            .split(',')
            .collect();
        let mut named = header.clone();
        named.sort_unstable();
        let mut projected: Vec<&str> = fields.to_vec();
        projected.sort_unstable();
        assert_eq!(
            named, projected,
            "case {case:?}: expected the header row to name exactly {projected:?}, got \
             {header:?}"
        );
    }
}

/// The first record of `name`'s history whose lookup-table destination has been processed,
/// polled for because the run that processes it is owed by the clock.
async fn settled_lookup_table_record(emulator: &Emulator, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let page = emulator
            .logs_ok(
                case,
                HISTORY,
                &json!({
                    "identifier": name,
                    "startTime": 0,
                    "endTime": crate::harness::now_ms() / 1_000 + 3_600,
                }),
            )
            .await;
        let settled = page["triggerHistory"]
            .as_array()
            .and_then(|records| records.first())
            .filter(|record| {
                record["destinations"]
                    .as_array()
                    .is_some_and(|destinations| {
                        destinations.iter().any(|destination| {
                            destination["destinationType"].as_str() == Some("LOOKUP_TABLE")
                                && destination["status"].as_str() != Some("IN_PROGRESS")
                        })
                    })
            })
            .cloned();
        if let Some(settled) = settled {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a run of {name} with its lookup table refreshed, got {page}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
