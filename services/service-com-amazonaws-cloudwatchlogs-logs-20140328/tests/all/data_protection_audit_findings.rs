// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AUDIT half of a log group's data protection policy: the findings
//! it delivers to the log group its `FindingsDestination` names.
//!
//! RULE under test: a data protection policy's two blocks act on ONE ingestion and neither
//! replaces the other. The `Deidentify` block masks the sensitive data in the stored event,
//! and the `Audit` block — which "must contain a `FindingsDestination` object"
//! (`PutDataProtectionPolicy$policyDocument`) — records the detection as a log event in the
//! log group named by `FindingsDestination.CloudWatchLogs.LogGroup`, readable through this
//! service's own event reads. The finding is a JSON document naming the instant of the audit,
//! the log group the data was found in and each data identifier found with the count of its
//! terms; it never carries the matched value. An ingested event carrying none of the named
//! data is stored verbatim and produces no finding.
//!
//! The rows span what the rule generalises over: which managed identifier the policy names,
//! how many terms of it one message carries, how many identifiers one message carries, a
//! message carrying none of the named data, several events in one batch, a destination
//! collecting the findings of two source groups, an ACCOUNT-level Audit statement, and a
//! destination log group that has since been deleted.
//!
//! Every case drives the service the way a client does, through the shared harness, so a
//! finding that is not computed from the shared store cannot pass.

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, group_with_stream, messages, Logs};

/// The prefix every AWS-managed data identifier ARN carries, as
/// `PutDataProtectionPolicy$policyDocument`'s `DataIdentifier` array names one.
const DATA_IDENTIFIER_PREFIX: &str = "arn:aws:dataprotection::aws:data-identifier/";

/// The members a finding states: the instant of the audit, the log group the data was found
/// in, and the identifiers found. Spelled here independently of the crate under test.
const AUDIT_TIMESTAMP: &str = "auditTimestamp";
const RESOURCE_ARN: &str = "resourceArn";
const DATA_IDENTIFIERS: &str = "dataIdentifiers";
const IDENTIFIER_NAME: &str = "name";
const IDENTIFIER_COUNT: &str = "count";

/// A policy document over the managed data identifiers `names`, whose Audit action sends its
/// findings to the log group `destination` — or to no CloudWatch Logs destination at all when
/// `destination` is `None`.
fn policy_document(names: &[&str], destination: Option<&str>) -> String {
    let identifiers: Vec<String> = names
        .iter()
        .map(|name| format!("{DATA_IDENTIFIER_PREFIX}{name}"))
        .collect();
    let findings_destination = match destination {
        Some(group) => json!({"CloudWatchLogs": {"LogGroup": group}}),
        None => json!({}),
    };
    json!({
        "Name": "audit-suite-policy",
        "Version": "2021-06-01",
        "Statement": [
            {
                "Sid": "audit-findings",
                "DataIdentifier": identifiers,
                "Operation": {"Audit": {"FindingsDestination": findings_destination}},
            },
            {
                "Sid": "mask-findings",
                "DataIdentifier": identifiers,
                "Operation": {"Deidentify": {"MaskConfig": {}}},
            },
        ],
    })
    .to_string()
}

/// The findings a destination log group holds, read back the way the audit stream is meant to
/// be consumed: `FilterLogEvents` over the whole destination group, paging to the end.
async fn findings(logs: &Logs, case: &str, destination: &str) -> Vec<Value> {
    let mut collected = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut request = json!({"logGroupName": destination});
        if let Some(token) = &next {
            request["nextToken"] = json!(token);
        }
        let page = logs.ok("FilterLogEvents", &request).await;
        for event in page
            .get("events")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: FilterLogEvents reports events, got {page}"))
        {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: every event carries a message, got {event}"));
            let parsed: Value = serde_json::from_str(message).unwrap_or_else(|_| {
                panic!("case {case:?}: a finding is a JSON document, got {message:?}")
            });
            collected.push(parsed);
        }
        next = page
            .get("nextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if next.is_none() {
            break;
        }
    }
    collected
}

/// The `(identifier, count)` pairs one finding states, in the order it lists them.
fn found_in(case: &str, finding: &Value) -> Vec<(String, u64)> {
    finding
        .get(DATA_IDENTIFIERS)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a finding lists identifiers, got {finding}"))
        .iter()
        .map(|entry| {
            (
                entry
                    .get(IDENTIFIER_NAME)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: an identifier is named, got {entry}"))
                    .to_string(),
                entry
                    .get(IDENTIFIER_COUNT)
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| panic!("case {case:?}: an identifier is counted, got {entry}")),
            )
        })
        .collect()
}

/// One row of the findings table: the identifiers the policy names, the messages ingested in
/// one batch, the sensitive terms those messages carry, and the findings expected — one entry
/// per audited event, each listing the `(identifier, count)` pairs it must state.
struct Row {
    case: &'static str,
    identifiers: &'static [&'static str],
    ingested: &'static [&'static str],
    terms: &'static [&'static str],
    expected: &'static [&'static [(&'static str, u64)]],
}

/// The rows differ along what the rule generalises over: which managed identifier is named,
/// how many terms of one identifier a message carries, how many identifiers one message
/// carries, how many events of a batch are audited, and a batch in which only some events
/// carry the named data — including identifiers the issue this suite was written for did not
/// cite (an IP address, a credit card number).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one email address in one event",
            identifiers: &["EmailAddress"],
            ingested: &["login attempt from user jane.doe@example.com failed"],
            terms: &["jane.doe@example.com"],
            expected: &[&[("EmailAddress", 1)]],
        },
        Row {
            case: "two email addresses in one event",
            identifiers: &["EmailAddress"],
            ingested: &["ada@example.com invited bob@example.org"],
            terms: &["ada@example.com", "bob@example.org"],
            expected: &[&[("EmailAddress", 2)]],
        },
        Row {
            case: "two identifiers in one event",
            identifiers: &["EmailAddress", "IpAddress"],
            ingested: &["ada@example.com from 203.0.113.9"],
            terms: &["ada@example.com", "203.0.113.9"],
            expected: &[&[("EmailAddress", 1), ("IpAddress", 1)]],
        },
        Row {
            case: "a credit card number in one event",
            identifiers: &["CreditCardNumber"],
            ingested: &["charged 4111 1111 1111 1111 for order 7"],
            terms: &["4111 1111 1111 1111"],
            expected: &[&[("CreditCardNumber", 1)]],
        },
        Row {
            case: "a batch in which only one event carries the named data",
            identifiers: &["EmailAddress"],
            ingested: &[
                "login attempt from user jane.doe@example.com failed",
                "login attempt from user with no address failed",
            ],
            terms: &["jane.doe@example.com"],
            expected: &[&[("EmailAddress", 1)]],
        },
        Row {
            case: "a batch in which both events carry the named data",
            identifiers: &["EmailAddress"],
            ingested: &["ada@example.com signed in", "bob@example.org signed in"],
            terms: &["ada@example.com", "bob@example.org"],
            expected: &[&[("EmailAddress", 1)], &[("EmailAddress", 1)]],
        },
        Row {
            case: "a batch carrying none of the named data",
            identifiers: &["EmailAddress"],
            ingested: &["login attempt from user with no address failed"],
            terms: &[],
            expected: &[],
        },
        Row {
            case: "a policy naming an identifier the message does not carry",
            identifiers: &["IpAddress"],
            ingested: &["login attempt from user jane.doe@example.com failed"],
            terms: &[],
            expected: &[],
        },
    ]
}

/// RULE: every audited ingestion delivers one finding per event the policy found data in,
/// into the destination group, naming the audited group and what was found — and never the
/// matched value; while the SAME ingestion is stored with that data masked, and an event
/// carrying none of it is stored verbatim and audited to nothing.
#[tokio::test]
async fn an_audited_ingestion_delivers_a_finding_naming_what_it_found() {
    for (index, row) in rows().into_iter().enumerate() {
        let logs = Logs::new();
        let case = row.case;
        let destination = format!("/audit/findings-{index}");
        let protected = format!("/audit/protected-{index}");
        let stream = "ingest";
        // The destination group exists before the policy names it, so what the findings are
        // read back from is a log group the account really holds.
        logs.ok(
            "CreateLogGroup",
            &json!({"logGroupName": destination.clone()}),
        )
        .await;
        group_with_stream(&logs, &protected, stream, Some(1)).await;
        logs.ok(
            "PutDataProtectionPolicy",
            &json!({
                "logGroupIdentifier": protected.clone(),
                "policyDocument": policy_document(row.identifiers, Some(&destination)),
            }),
        )
        .await;

        let at = base_ms();
        let events: Vec<(i64, &str)> = row
            .ingested
            .iter()
            .enumerate()
            .map(|(offset, message)| (at + i64::try_from(offset).unwrap_or(0), *message))
            .collect();
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": protected.clone(),
                "logStreamName": stream,
                "logEvents": batch(&events),
            }),
        )
        .await;

        // The findings the destination group received, one per audited event.
        let delivered = findings(&logs, case, &destination).await;
        assert_eq!(
            delivered.len(),
            row.expected.len(),
            "case {case:?}: expected {} finding(s) in the destination group, got {delivered:?}",
            row.expected.len()
        );
        let (_, protected_arn) = logs.group_arns(&protected).await;
        for (finding, expected) in delivered.iter().zip(row.expected) {
            let found = found_in(case, finding);
            let wanted: Vec<(String, u64)> = expected
                .iter()
                .map(|(name, count)| ((*name).to_string(), *count))
                .collect();
            assert_eq!(
                found, wanted,
                "case {case:?}: expected the finding to state {wanted:?}, got {found:?}"
            );
            assert_eq!(
                finding.get(RESOURCE_ARN).and_then(Value::as_str),
                Some(protected_arn.as_str()),
                "case {case:?}: expected the finding to name the audited log group, got {finding}"
            );
            let audited_at = finding
                .get(AUDIT_TIMESTAMP)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a finding names its audit instant, got {finding}"));
            assert!(
                audited_at.len() == "2026-01-01T00:00:00Z".len() && audited_at.ends_with('Z'),
                "case {case:?}: expected a UTC instant to the second, got {audited_at:?}"
            );
            // The finding is a detection record, not a copy of the message: no matched term,
            // and no text of the message, is in it.
            let rendered = finding.to_string();
            for term in row.terms {
                assert!(
                    !rendered.contains(term),
                    "case {case:?}: expected the finding to carry no matched value, got {rendered}"
                );
            }
        }

        // The Deidentify half fired on the same ingestion: each stored event carries the
        // matched spans masked and the rest of its message verbatim.
        let page = logs
            .ok(
                "GetLogEvents",
                &json!({
                    "logGroupName": protected.clone(),
                    "logStreamName": stream,
                    "startFromHead": true,
                }),
            )
            .await;
        let stored = messages(&page);
        assert_eq!(
            stored.len(),
            row.ingested.len(),
            "case {case:?}: expected the ingested events to be stored, got {stored:?}"
        );
        for (read, ingested) in stored.iter().zip(row.ingested) {
            let expected_masked = row.terms.iter().fold(ingested.to_string(), |text, term| {
                text.replace(term, &"*".repeat(term.chars().count()))
            });
            assert_eq!(
                read, &expected_masked,
                "case {case:?}: expected the stored event to read {expected_masked:?}, got {read:?}"
            );
        }
    }
}

/// RULE: a destination collects the findings of every group audited to it, keeping each source
/// group's findings in a stream of its own — so a reader can tell which group a finding came
/// from by the stream it arrived in as well as by the ARN it names.
#[tokio::test]
async fn one_destination_collects_the_findings_of_every_group_audited_to_it() {
    let logs = Logs::new();
    let destination = "/audit/shared-findings";
    let stream = "ingest";
    logs.ok("CreateLogGroup", &json!({"logGroupName": destination}))
        .await;
    let first = "/audit/source-one";
    let second = "/audit/source-two";
    for group in [first, second] {
        group_with_stream(&logs, group, stream, Some(1)).await;
        logs.ok(
            "PutDataProtectionPolicy",
            &json!({
                "logGroupIdentifier": group,
                "policyDocument": policy_document(&["EmailAddress"], Some(destination)),
            }),
        )
        .await;
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": batch(&[(base_ms(), "ada@example.com signed in")]),
            }),
        )
        .await;
    }

    // Each source group's findings arrive in a stream named by that group.
    let described = logs
        .ok(
            "DescribeLogStreams",
            &json!({"logGroupName": destination}),
        )
        .await;
    let streams: Vec<String> = described
        .get("logStreams")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("DescribeLogStreams reports logStreams, got {described}"))
        .iter()
        .filter_map(|held| {
            held.get("logStreamName")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    assert_eq!(
        streams,
        vec![first.to_string(), second.to_string()],
        "case \"two groups audited to one destination\": expected one stream per source \
         group, got {streams:?}"
    );

    // ...and each stream's finding names its own source group.
    for group in [first, second] {
        let case = format!("the findings of {group}");
        let (_, group_arn) = logs.group_arns(group).await;
        let page = logs
            .ok(
                "GetLogEvents",
                &json!({
                    "logGroupName": destination,
                    "logStreamName": group,
                    "startFromHead": true,
                }),
            )
            .await;
        let read = messages(&page);
        assert_eq!(
            read.len(),
            1,
            "case {case:?}: expected one finding in the stream, got {read:?}"
        );
        let finding: Value = serde_json::from_str(&read[0])
            .unwrap_or_else(|_| panic!("case {case:?}: a finding is JSON, got {:?}", read[0]));
        assert_eq!(
            finding.get(RESOURCE_ARN).and_then(Value::as_str),
            Some(group_arn.as_str()),
            "case {case:?}: expected the finding to name its own source group, got {finding}"
        );
        assert_eq!(
            found_in(&case, &finding),
            vec![("EmailAddress".to_string(), 1u64)],
            "case {case:?}: expected the finding to state what was found, got {finding}"
        );
    }
}

/// RULE: an ACCOUNT-level data protection policy audits the groups it applies to, to its own
/// findings destination — the two policies being cumulative, so a group with a policy of its
/// own is audited to both destinations.
#[tokio::test]
async fn an_account_level_audit_statement_delivers_findings_too() {
    let logs = Logs::new();
    let account_destination = "/audit/account-findings";
    let group_destination = "/audit/group-findings";
    let stream = "ingest";
    for destination in [account_destination, group_destination] {
        logs.ok("CreateLogGroup", &json!({"logGroupName": destination}))
            .await;
    }
    let covered = "/audit/covered";
    let both = "/audit/both";
    for group in [covered, both] {
        group_with_stream(&logs, group, stream, Some(1)).await;
    }
    logs.ok(
        "PutAccountPolicy",
        &json!({
            "policyName": "audit-account-policy",
            "policyType": "DATA_PROTECTION_POLICY",
            "policyDocument": policy_document(&["EmailAddress"], Some(account_destination)),
        }),
    )
    .await;
    logs.ok(
        "PutDataProtectionPolicy",
        &json!({
            "logGroupIdentifier": both,
            "policyDocument": policy_document(&["EmailAddress"], Some(group_destination)),
        }),
    )
    .await;
    for group in [covered, both] {
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": batch(&[(base_ms(), "ada@example.com signed in")]),
            }),
        )
        .await;
    }

    // (case, the destination, the source groups whose findings it must hold)
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "the account policy's destination",
            account_destination,
            &["/audit/both", "/audit/covered"],
        ),
        (
            "the group policy's own destination",
            group_destination,
            &["/audit/both"],
        ),
    ];
    for (case, destination, sources) in cases {
        let delivered = findings(&logs, case, destination).await;
        let mut named: Vec<String> = Vec::new();
        for finding in &delivered {
            named.push(
                finding
                    .get(RESOURCE_ARN)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: a finding names a group, got {finding}"))
                    .to_string(),
            );
        }
        named.sort();
        let mut wanted: Vec<String> = Vec::new();
        for source in *sources {
            wanted.push(logs.group_arns(source).await.1);
        }
        wanted.sort();
        assert_eq!(
            named, wanted,
            "case {case:?}: expected findings for {wanted:?}, got {named:?}"
        );
    }
}

/// RULE: a finding is delivered only where this service OWNS the destination, and only one
/// that exists takes it: an Audit action naming no CloudWatch Logs destination at all
/// delivers nothing, and one whose destination log group has since been DELETED delivers
/// nothing either — neither keeping the masking from firing.
#[tokio::test]
async fn an_audit_statement_this_service_cannot_deliver_to_stores_no_finding() {
    let logs = Logs::new();
    let stream = "ingest";
    let deleted_destination = "/audit/deleted-destination";
    logs.ok(
        "CreateLogGroup",
        &json!({"logGroupName": deleted_destination}),
    )
    .await;
    // (case, the group, the destination the policy names)
    let cases: &[(&str, &str, Option<&str>)] = &[
        (
            "a destination log group deleted after the policy was put",
            "/audit/orphaned",
            Some(deleted_destination),
        ),
        (
            "an Audit action naming no CloudWatch Logs destination",
            "/audit/no-destination",
            None,
        ),
    ];
    for (case, group, destination) in cases {
        group_with_stream(&logs, group, stream, Some(1)).await;
        logs.ok(
            "PutDataProtectionPolicy",
            &json!({
                "logGroupIdentifier": group,
                "policyDocument": policy_document(&["EmailAddress"], *destination),
            }),
        )
        .await;
        // The destination the first case names is taken away before anything is ingested, so
        // the policy outlives the group it was pointed at.
        if destination.is_some() {
            logs.ok(
                "DeleteLogGroup",
                &json!({"logGroupName": deleted_destination}),
            )
            .await;
        }
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": batch(&[(base_ms(), "ada@example.com signed in")]),
            }),
        )
        .await;
        // The ingestion is still masked...
        let page = logs
            .ok(
                "GetLogEvents",
                &json!({
                    "logGroupName": group,
                    "logStreamName": stream,
                    "startFromHead": true,
                }),
            )
            .await;
        let read = messages(&page);
        assert_eq!(
            read,
            vec!["*************** signed in".to_string()],
            "case {case:?}: expected the stored event to be masked, got {read:?}"
        );
        // ...and nothing was stored under the destination the policy named: the account holds
        // no log group there, so there is nothing to read findings from.
        let code = logs
            .err(
                "FilterLogEvents",
                &json!({"logGroupName": deleted_destination}),
            )
            .await;
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected the destination group to stay absent, got {code}"
        );
    }
}
