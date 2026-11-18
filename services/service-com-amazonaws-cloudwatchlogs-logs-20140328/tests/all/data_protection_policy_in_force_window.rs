// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a data protection policy masks: the span it is in force for,
//! whichever level holds it.
//!
//! RULE under test, as `PutDataProtectionPolicy` states it: "Sensitive data is detected and
//! masked when it is ingested into the log group. When you set a data protection policy, log
//! events ingested into the log group before that time are not masked." `PutAccountPolicy`
//! states the same of the account-level policy, whose data protection half is cumulative with
//! the group's. So the masking of ONE event is decided once, at the instant it is ingested,
//! from the policies in force then — an event ingested before the policy arrived is served
//! verbatim, one ingested while it was in force is served with the matched span replaced by
//! asterisks for good, and one ingested after the policy was REMOVED is served verbatim
//! again, at either level.
//!
//! What the suites next door cover: `tests/all/data_protection_masking.rs` which spans are
//! masked and the group-level delete, `tests/all/unmask_authorization.rs` the permission a
//! read needs to see the ingested bytes. This suite's subject is the WINDOW, and it spans
//! what the rule generalises over: the level the policy is held at (the group's own, the
//! account's) and the removal that ends it (`DeleteDataProtectionPolicy`,
//! `DeleteAccountPolicy`), over identifiers whose data takes a different form each.
//!
//! Every case drives the service the way a client does, through the shared harness, so a
//! response that is not computed from the shared store cannot pass.

use serde_json::json;

use crate::account_policies::{data_protection_document, delete_body, put_body};
use crate::harness::{base_ms, batch, group_with_stream, messages, Logs};

/// The `policyType` of the account-level policy whose half this is, as
/// `PutAccountPolicy$policyType` names it.
const DATA_PROTECTION_POLICY: &str = "DATA_PROTECTION_POLICY";

/// The name the account-level policy of a case is held under. "Each account can have only one
/// account-level data protection policy", so one name is all a case needs.
const ACCOUNT_POLICY_NAME: &str = "in-force-window";

/// WHICH level a case's policy is held at — the two the rule covers, each put and removed
/// through its own pair of operations.
#[derive(Debug, Clone, Copy)]
enum Level {
    /// The log group's own policy: `PutDataProtectionPolicy` / `DeleteDataProtectionPolicy`.
    Group,
    /// The account-level policy that applies to the group: `PutAccountPolicy` /
    /// `DeleteAccountPolicy`.
    Account,
}

/// One row of the table: the level the policy is held at, the managed data identifier it
/// names, and one term of that identifier's data for the ingested messages to carry.
struct Row {
    case: &'static str,
    level: Level,
    identifier: &'static str,
    term: &'static str,
}

/// The rows span the two levels a data protection policy is held at, each over a managed data
/// identifier whose data has a form of its own.
///
/// The identifiers are the ones this emulator detects, published by "Types of data that you
/// can mask", which `PutDataProtectionPolicy$policyDocument` links to for its
/// `DataIdentifier` array — including ones the issue did not cite (an IP address, a credit
/// card number).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a group-level policy naming an email address",
            level: Level::Group,
            identifier: "EmailAddress",
            term: "ada@example.com",
        },
        Row {
            case: "an account-level policy naming an email address",
            level: Level::Account,
            identifier: "EmailAddress",
            term: "grace.hopper@navy.example.org",
        },
        Row {
            case: "a group-level policy naming an IPv4 address",
            level: Level::Group,
            identifier: "IpAddress",
            term: "198.51.100.7",
        },
        Row {
            case: "an account-level policy naming a credit card number",
            level: Level::Account,
            identifier: "CreditCardNumber",
            term: "4111111111111111",
        },
    ]
}

/// Put the case's policy at its own level, over the one managed data identifier it names.
async fn put_policy(logs: &Logs, row: &Row, group: &str) {
    let document = data_protection_document(row.identifier);
    match row.level {
        Level::Group => {
            logs.ok(
                "PutDataProtectionPolicy",
                &json!({"logGroupIdentifier": group, "policyDocument": document}),
            )
            .await;
        }
        Level::Account => {
            logs.ok(
                "PutAccountPolicy",
                &put_body(
                    ACCOUNT_POLICY_NAME,
                    DATA_PROTECTION_POLICY,
                    &document,
                    None,
                ),
            )
            .await;
        }
    }
}

/// Remove the case's policy, through the delete that owns its level.
async fn remove_policy(logs: &Logs, row: &Row, group: &str) {
    match row.level {
        Level::Group => {
            logs.ok(
                "DeleteDataProtectionPolicy",
                &json!({"logGroupIdentifier": group}),
            )
            .await;
        }
        Level::Account => {
            logs.ok(
                "DeleteAccountPolicy",
                &delete_body(ACCOUNT_POLICY_NAME, DATA_PROTECTION_POLICY),
            )
            .await;
        }
    }
}

/// An event ingested before the policy arrived, one ingested while it was in force and one
/// ingested after it was removed: only the middle one is masked, at either level, and `unmask`
/// recovers the ingested bytes of all three.
#[tokio::test]
async fn only_the_events_ingested_while_a_policy_was_in_force_are_masked_at_either_level() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let logs = Logs::new();
        let group = format!("in-force-window-{index}");
        let stream = "ingest";
        group_with_stream(&logs, &group, stream, Some(1)).await;

        // Each phase's message carries the row's term, so what separates the phases is only
        // WHEN the event was ingested.
        let ingested = |label: &str| format!("{label}: contact {} now", row.term);
        let put_event = |timestamp: i64, text: String| {
            let logs = &logs;
            let group = group.clone();
            async move {
                logs.ok(
                    "PutLogEvents",
                    &json!({
                        "logGroupName": group,
                        "logStreamName": stream,
                        "logEvents": batch(&[(timestamp, text.as_str())]),
                    }),
                )
                .await;
            }
        };

        let at = base_ms();
        put_event(at, ingested("before")).await;
        put_policy(&logs, &row, &group).await;
        put_event(at + 1, ingested("during")).await;
        remove_policy(&logs, &row, &group).await;
        put_event(at + 2, ingested("after")).await;

        // The matched span, and nothing else, is asterisks: "the sensitive data is replaced
        // by asterisks".
        let masked_term = "*".repeat(row.term.chars().count());
        let expected = vec![
            ingested("before"),
            ingested("during").replace(row.term, &masked_term),
            ingested("after"),
        ];
        let page = logs
            .ok(
                "GetLogEvents",
                &json!({"logGroupName": group.clone(), "logStreamName": stream}),
            )
            .await;
        let read = messages(&page);
        assert_eq!(
            read, expected,
            "case {case:?}: expected {expected:?}, got {read:?}"
        );

        // The ingested bytes are kept whatever the policy did, so an unmasked read reports
        // every phase verbatim.
        let page = logs
            .ok(
                "GetLogEvents",
                &json!({
                    "logGroupName": group.clone(),
                    "logStreamName": stream,
                    "unmask": true,
                }),
            )
            .await;
        let read = messages(&page);
        let verbatim = vec![
            ingested("before"),
            ingested("during"),
            ingested("after"),
        ];
        assert_eq!(
            read, verbatim,
            "case {case:?}: expected unmask to recover every ingested message, got {read:?}"
        );
    }
}

/// A policy put, removed and put AGAIN masks the events of each span it was in force for and
/// leaves the span between them verbatim — so the window is the policy's own presence and not
/// a one-way switch the first delete throws for good.
#[tokio::test]
async fn a_policy_put_again_masks_the_events_ingested_under_it() {
    let case = "a group-level policy put, removed, and put again";
    let logs = Logs::new();
    let group = "in-force-window-repeat";
    let stream = "ingest";
    group_with_stream(&logs, group, stream, Some(1)).await;
    let term = "ada@example.com";
    let ingested = |label: &str| format!("{label}: contact {term} now");

    let at = base_ms();
    // Each phase of the policy's presence, and the label of the event ingested inside it.
    let phases: [(bool, &str); 4] = [
        (true, "first-under"),
        (false, "between"),
        (true, "second-under"),
        (false, "after"),
    ];
    for (offset, (in_force, label)) in phases.into_iter().enumerate() {
        if in_force {
            logs.ok(
                "PutDataProtectionPolicy",
                &json!({
                    "logGroupIdentifier": group,
                    "policyDocument": data_protection_document("EmailAddress"),
                }),
            )
            .await;
        } else {
            logs.ok(
                "DeleteDataProtectionPolicy",
                &json!({"logGroupIdentifier": group}),
            )
            .await;
        }
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": batch(&[(
                    at + i64::try_from(offset).expect("a phase index fits in an i64"),
                    ingested(label).as_str(),
                )]),
            }),
        )
        .await;
    }

    let masked_term = "*".repeat(term.chars().count());
    let expected: Vec<String> = phases
        .iter()
        .map(|(in_force, label)| {
            if *in_force {
                ingested(label).replace(term, &masked_term)
            } else {
                ingested(label)
            }
        })
        .collect();
    let page = logs
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
    let read = messages(&page);
    assert_eq!(
        read, expected,
        "case {case:?}: expected {expected:?}, got {read:?}"
    );
}
