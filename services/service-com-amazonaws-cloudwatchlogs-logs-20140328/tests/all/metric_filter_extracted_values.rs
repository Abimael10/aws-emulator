// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `extractedValues` member of a `TestMetricFilter` match.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.TestMetricFilter` — so a response that is not computed by the
//! service cannot pass. WHICH events a pattern selects, and which values each form of the
//! syntax extracts, is the subject of `tests/all/metric_filter_testing.rs`; this suite is
//! about the member being there at all.
//!
//! The rule: every reported match carries `extractedValues` — "the values extracted from the
//! event data by the filter" — so a pattern that names no field of the event reports the EMPTY
//! map it extracted rather than no map, and a client iterating what a filter would emit never
//! dereferences a missing member.
//!
//! The table spans what the rule generalises over: the pattern FORM (unstructured terms, a
//! space-delimited field list, a JSON condition and the empty pattern), whether that form
//! names a field of the event at all, and whether the matched event is JSON.

use serde_json::json;

use crate::harness::Logs;

/// The operation every case calls.
const OP: &str = "TestMetricFilter";

/// One row: the pattern, the sample it runs over, and the names each match must report as
/// extracted — empty for a pattern that names no field of the event.
struct ExtractionCase {
    /// What this row varies, for the assertion messages.
    case: &'static str,
    /// The `filterPattern` under test.
    pattern: &'static str,
    /// The sample of log event messages.
    sample: &'static [&'static str],
    /// The names every reported match must carry as extracted values.
    extracted: &'static [&'static str],
}

/// Every match of every pattern form reports `extractedValues`, which is empty exactly when
/// the pattern names no field of the event.
#[tokio::test]
async fn every_match_reports_the_values_it_extracted() {
    let logs = Logs::new();

    let cases: [ExtractionCase; 5] = [
        ExtractionCase {
            case: "an unstructured term, which names no field",
            pattern: "ERROR",
            sample: &["ERROR order 1 failed", "INFO order 2 accepted"],
            extracted: &[],
        },
        ExtractionCase {
            case: "the empty pattern, which matches every event and names no field",
            pattern: "",
            sample: &["ERROR order 1 failed", "{\"level\":\"INFO\"}"],
            extracted: &[],
        },
        ExtractionCase {
            case: "an unstructured term over a JSON event, which still names no field",
            pattern: "WARN",
            sample: &["{\"level\":\"WARN\",\"latency\":12}"],
            extracted: &[],
        },
        ExtractionCase {
            case: "a space-delimited list naming the event's three fields",
            pattern: "[ip, user, code]",
            sample: &["10.0.0.1 anna 200", "10.0.0.2 bob 404"],
            extracted: &["$code", "$ip", "$user"],
        },
        ExtractionCase {
            case: "a JSON condition, which selects the leaves of the event document",
            pattern: "{ $.level = \"WARN\" }",
            sample: &["{\"level\":\"WARN\"}", "{\"level\":\"INFO\"}"],
            extracted: &["$.level"],
        },
    ];

    for row in &cases {
        let reported = logs
            .ok(
                OP,
                &json!({"filterPattern": row.pattern, "logEventMessages": row.sample}),
            )
            .await;
        let matched = reported["matches"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {:?}: {OP} reports matches, got {reported}", row.case)
            })
            .clone();
        assert!(
            !matched.is_empty(),
            "case {:?}: the pattern must match the sample, got {reported}",
            row.case
        );
        for entry in &matched {
            let values = entry["extractedValues"].as_object().unwrap_or_else(|| {
                panic!(
                    "case {:?}: every match carries extractedValues, got {entry}",
                    row.case
                )
            });
            let mut names: Vec<&str> = values.keys().map(String::as_str).collect();
            names.sort_unstable();
            let mut wanted: Vec<&str> = row.extracted.to_vec();
            wanted.sort_unstable();
            assert_eq!(
                names, wanted,
                "case {:?}: expected exactly the names the pattern extracts, got {entry}",
                row.case
            );
            for (name, value) in values {
                assert!(
                    value.is_string(),
                    "case {:?}: an extracted value is text, got {name}={value}",
                    row.case
                );
            }
        }
    }
}
