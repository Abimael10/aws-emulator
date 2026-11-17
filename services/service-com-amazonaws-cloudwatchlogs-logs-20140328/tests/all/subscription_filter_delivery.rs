// SPDX-License-Identifier: CC0-1.0

//! The LIVE FILTER SET half of a subscription filter's delivery: WHICH filters a batch is
//! fanned out through, which `subscription_fan_out.rs` (whose harness this suite shares)
//! does not grade — it asserts what ONE filter's envelope looks like.
//!
//! RULE: a batch is delivered through the subscription filters its log group HOLDS at the
//! moment it is ingested, each matching filter getting one envelope naming exactly itself.
//! A log group holds at most five filters at a time (the attached-filter quota; the refusal
//! carries the service's fixed limit sentence), and the quota counts the filters ATTACHED,
//! not the puts made — so a filter the quota refused was never attached and delivers
//! nothing, a filter attached once `DeleteSubscriptionFilter` freed a slot delivers from the
//! next batch on, with no replay of what it missed, and a deleted filter stops delivering
//! while its siblings keep going.
//!
//! The first test spans the fan-out itself: the whole quota's worth of filters on one log
//! group, each configured with a DIFFERENT form of filter pattern that selects the written
//! line (match-everything, an unstructured term, an alternative, an exclusion, a
//! space-delimited pattern), so a fan-out that delivers once per batch rather than once per
//! filter, or one envelope naming every filter, cannot pass. The second spans the lifecycle
//! a client drives the quota through, with two destinations so what a refused, a re-attached
//! and a deleted filter deliver is observed separately. The third spans the ACCOUNT-LEVEL
//! half of the same filter set: "account-level and log-group-level subscription filters are
//! additive" (`PutAccountPolicy`), so a log group the `selectionCriteria` of a
//! `SUBSCRIPTION_FILTER_POLICY` select is delivered through that policy as well as through
//! its own filters, and one the criteria exclude is delivered nothing by it.
//!
//! Both drive the real wire path and read the delivered records where a consumer reads them,
//! through the harness `subscription_fan_out.rs` owns.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::harness::base_ms;
use crate::subscription_fan_out::{
    assert_envelope, past_activation, role_arn, stream_arn, Emulator, BY_LOG_STREAM,
    LOGS_TARGET_PREFIX,
};

/// The subscription filters one log group holds ATTACHED at a time. Source: Service Quotas
/// "Subscription filters per log group" (L-87E7D306), five and non-adjustable — the
/// operation's own prose ("up to two") is stale.
const ATTACHED_FILTER_QUOTA: usize = 5;

/// The fixed text CloudWatch Logs answers a put that would take a log group past the
/// filters it can hold: it names neither the quota nor anything from the request.
const LIMIT_EXCEEDED_MESSAGE: &str = "Resource limit exceeded.";

/// The wire error code of that refusal.
const LIMIT_EXCEEDED: &str = "LimitExceededException";

/// The log group and log stream both tests write to.
const GROUP: &str = "/aws/emulator/live-filter-set";
const LOG_STREAM: &str = "instance-one";

/// The line every case writes, and the forms of filter pattern that select it: the empty
/// pattern, an unstructured term, an alternative (`?`), a term with an exclusion (`-`), and
/// a space-delimited pattern over its three fields. Source: the filter-pattern syntax
/// `PutSubscriptionFilter` references.
const LINE: &str = "ERROR disk full";
const QUOTA_PATTERNS: [&str; ATTACHED_FILTER_QUOTA] = [
    "",
    "ERROR",
    "?ERROR ?FATAL",
    "ERROR -TIMEOUT",
    "[w1 = ERROR, w2, w3]",
];

/// One `PutSubscriptionFilter` body against the one log group these tests use.
fn put_filter(filter: &str, pattern: &str, destination: &str) -> Value {
    json!({
        "logGroupName": GROUP,
        "filterName": filter,
        "filterPattern": pattern,
        "destinationArn": destination,
        "roleArn": role_arn(),
        "distribution": BY_LOG_STREAM,
    })
}

/// The log group and the log stream every case writes into.
async fn given_log_stream(emulator: &Emulator, case: &str) {
    emulator
        .logs(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    emulator
        .logs(
            case,
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": LOG_STREAM}),
        )
        .await;
}

/// Write one line, stamped inside the ingestion window, and answer the timestamp it was
/// written with.
async fn write_line(emulator: &Emulator, case: &str, message: &str) -> i64 {
    let timestamp = base_ms();
    emulator
        .logs(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": LOG_STREAM,
                "logEvents": [{"timestamp": timestamp, "message": message}],
            }),
        )
        .await;
    timestamp
}

/// The filter each delivered record names, and the messages that record carries.
pub(crate) fn delivered_by(case: &str, records: &[(String, Value)]) -> Vec<(String, Vec<String>)> {
    records
        .iter()
        .map(|(_key, envelope)| {
            let named = envelope["subscriptionFilters"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                named.len(),
                1,
                "case {case}: an envelope names exactly one filter, got {envelope}"
            );
            let filter = named[0]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a named filter is a string, got {envelope}"))
                .to_string();
            let messages = envelope["logEvents"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|event| {
                    event["message"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case}: a delivered event carries a message, got {event}")
                        })
                        .to_string()
                })
                .collect();
            (filter, messages)
        })
        .collect()
}

/// The filter names a set of delivered records was delivered for.
fn filters_of(case: &str, records: &[(String, Value)]) -> BTreeSet<String> {
    delivered_by(case, records)
        .into_iter()
        .map(|(filter, _)| filter)
        .collect()
}

/// A log group carrying the whole attached-filter quota fans ONE written line out to every
/// one of those filters: five records, each naming exactly one of them.
#[tokio::test]
async fn the_whole_attached_filter_quota_fans_one_line_out_once_per_filter() {
    let case = "five attached filters on one stream";
    let emulator = Emulator::new();
    let stream = "quota-fanout";
    emulator.create_stream(case, stream).await;
    tokio::time::sleep(past_activation()).await;
    given_log_stream(&emulator, case).await;

    let attached: Vec<String> = (1..=ATTACHED_FILTER_QUOTA)
        .map(|nth| format!("attached-{nth}"))
        .collect();
    for (filter, pattern) in attached.iter().zip(QUOTA_PATTERNS) {
        emulator
            .logs(
                case,
                "PutSubscriptionFilter",
                &put_filter(filter, pattern, &stream_arn(stream)),
            )
            .await;
    }
    // Every one of them is ATTACHED: the quota is reached, not exceeded.
    let listed = emulator
        .logs(
            case,
            "DescribeSubscriptionFilters",
            &json!({"logGroupName": GROUP}),
        )
        .await;
    assert_eq!(
        listed["subscriptionFilters"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        ATTACHED_FILTER_QUOTA,
        "case {case}: the group holds the whole quota, got {listed}"
    );

    let timestamp = write_line(&emulator, case, LINE).await;
    let records = emulator.delivered(case, stream).await;
    assert_eq!(
        records.len(),
        ATTACHED_FILTER_QUOTA,
        "case {case}: one line owes each attached filter its own record, got {records:?}"
    );
    assert_eq!(
        filters_of(case, &records),
        attached.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case}: the records cover every attached filter exactly once, got {records:?}"
    );
    for (key, envelope) in &records {
        let filter = envelope["subscriptionFilters"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: an envelope names its filter, got {envelope}"));
        assert_envelope(
            case,
            envelope,
            GROUP,
            LOG_STREAM,
            filter,
            &[LINE],
            &[timestamp],
        );
        assert_eq!(
            key, LOG_STREAM,
            "case {case}/{filter}: log data grouped by log stream is keyed on the log stream, \
             got {key}"
        );
    }
}

/// The filter set at INGEST is what delivers: a filter the quota refused never delivers, the
/// same put accepted once a slot is freed delivers from the next batch on, and the deleted
/// filter stops while its siblings keep going.
#[tokio::test]
async fn only_the_filters_attached_at_ingest_deliver() {
    let case = "the quota refused, then repaired by freeing a slot";
    let emulator = Emulator::new();
    let accepted = "accepted-side";
    let refused = "refused-side";
    emulator.create_stream(case, accepted).await;
    emulator.create_stream(case, refused).await;
    tokio::time::sleep(past_activation()).await;
    given_log_stream(&emulator, case).await;

    let attached: Vec<String> = (1..=ATTACHED_FILTER_QUOTA)
        .map(|nth| format!("attached-{nth}"))
        .collect();
    for filter in &attached {
        emulator
            .logs(
                case,
                "PutSubscriptionFilter",
                &put_filter(filter, "", &stream_arn(accepted)),
            )
            .await;
    }

    // One past the quota is refused, with the service's fixed limit sentence.
    let sixth = "attached-6";
    let (status, answer) = emulator
        .call(
            "logs",
            LOGS_TARGET_PREFIX,
            "PutSubscriptionFilter",
            &put_filter(sixth, "", &stream_arn(refused)),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case}: a put past the quota is refused, got {status} {answer}"
    );
    assert!(
        answer["__type"]
            .as_str()
            .is_some_and(|code| code.ends_with(LIMIT_EXCEEDED)),
        "case {case}: expected {LIMIT_EXCEEDED}, got {answer}"
    );
    assert_eq!(
        answer["message"],
        json!(LIMIT_EXCEEDED_MESSAGE),
        "case {case}: expected the fixed limit sentence, got {answer}"
    );

    let first = "ERROR while five deliver";
    write_line(&emulator, case, first).await;
    let on_accepted = emulator.delivered(case, accepted).await;
    assert_eq!(
        filters_of(case, &on_accepted),
        attached.iter().cloned().collect::<BTreeSet<String>>(),
        "case {case}: every attached filter delivers the line, got {on_accepted:?}"
    );
    let on_refused = emulator.delivered(case, refused).await;
    assert!(
        on_refused.is_empty(),
        "case {case}: the filter the quota refused was never attached, so its destination \
         holds nothing, got {on_refused:?}"
    );

    // Freeing a slot lets the identical put in.
    let deleted = attached.last().expect("the quota is not empty").clone();
    emulator
        .logs(
            case,
            "DeleteSubscriptionFilter",
            &json!({"logGroupName": GROUP, "filterName": deleted}),
        )
        .await;
    emulator
        .logs(
            case,
            "PutSubscriptionFilter",
            &put_filter(sixth, "", &stream_arn(refused)),
        )
        .await;
    let listed = emulator
        .logs(
            case,
            "DescribeSubscriptionFilters",
            &json!({"logGroupName": GROUP}),
        )
        .await;
    let held: BTreeSet<String> = listed["subscriptionFilters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|filter| {
            filter["filterName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a reported filter is named, got {filter}"))
                .to_string()
        })
        .collect();
    let survivors: BTreeSet<String> = attached
        .iter()
        .filter(|filter| **filter != deleted)
        .cloned()
        .collect();
    let mut expected = survivors.clone();
    expected.insert(sixth.to_string());
    assert_eq!(
        held, expected,
        "case {case}: the group holds the survivors plus the newly attached filter, got \
         {listed}"
    );

    let second = "ERROR after the slot was freed";
    write_line(&emulator, case, second).await;

    // The deleted filter delivers nothing of the second line; its siblings all do.
    let carrying_second: BTreeSet<String> = delivered_by(case, &emulator.delivered(case, accepted).await)
        .into_iter()
        .filter(|(_, messages)| messages == &vec![second.to_string()])
        .map(|(filter, _)| filter)
        .collect();
    assert_eq!(
        carrying_second, survivors,
        "case {case}: the surviving filters carry the second line and the deleted one does \
         not, got {carrying_second:?}"
    );

    // The newly attached filter delivers from the batch after it was attached, and the line
    // written while it was refused does not reach it.
    let on_refused = delivered_by(case, &emulator.delivered(case, refused).await);
    assert_eq!(
        on_refused,
        vec![(sixth.to_string(), vec![second.to_string()])],
        "case {case}: the re-attached filter carries only what was written after it was \
         attached, got {on_refused:?}"
    );
}

/// The ACCOUNT-LEVEL filter set: an account-level subscription filter policy is one more
/// filter of every log group its `selectionCriteria` select.
///
/// "Account-level and log-group-level subscription filters are additive" (`PutAccountPolicy`),
/// and the only criteria such a policy takes is `LogGroupName NOT IN [...]`, so a batch
/// ingested into a group the list does not name is delivered through the policy's own
/// destination — under the policy's name and its own `FilterPattern` — beside whatever the
/// group's own filters deliver, while a group the list DOES name is delivered nothing by it.
///
/// The table spans the three kinds of group the rule distinguishes (excluded, selected with no
/// filter of its own, selected with one), and the policy's pattern selects one of the two
/// written lines, so a policy whose pattern is ignored cannot pass either.
#[tokio::test]
async fn an_account_level_policy_delivers_from_every_group_it_selects() {
    let case = "an account-level subscription filter policy";
    let emulator = Emulator::new();
    let stream = "account-policy-fanout";
    emulator.usable_stream(case, stream).await;

    /// The groups the case writes to: the one the policy's criteria exclude, one it selects,
    /// and one it selects that carries a filter of its own.
    const EXCLUDED: &str = "/aws/emulator/account-policy-excluded";
    const SELECTED: &str = "/aws/emulator/account-policy-selected";
    const SELECTED_WITH_OWN: &str = "/aws/emulator/account-policy-selected-with-own";
    /// The policy, and the filter the third group carries itself.
    const POLICY: &str = "account-wide-subscription";
    const OWN_FILTER: &str = "the-group-s-own";
    /// The two lines every group is written: the policy's `FilterPattern` selects the first.
    const SELECTED_LINE: &str = "ERROR disk full";
    const OTHER_LINE: &str = "INFO all good";

    for group in [EXCLUDED, SELECTED, SELECTED_WITH_OWN] {
        emulator
            .logs(case, "CreateLogGroup", &json!({"logGroupName": group}))
            .await;
        emulator
            .logs(
                case,
                "CreateLogStream",
                &json!({"logGroupName": group, "logStreamName": LOG_STREAM}),
            )
            .await;
    }
    // The third group's OWN filter, which matches everything, so what the policy adds is
    // observable beside it rather than instead of it.
    emulator
        .logs(
            case,
            "PutSubscriptionFilter",
            &json!({
                "logGroupName": SELECTED_WITH_OWN,
                "filterName": OWN_FILTER,
                "filterPattern": "",
                "destinationArn": stream_arn(stream),
                "roleArn": role_arn(),
                "distribution": BY_LOG_STREAM,
            }),
        )
        .await;
    emulator
        .logs(
            case,
            "PutAccountPolicy",
            &json!({
                "policyName": POLICY,
                "policyType": "SUBSCRIPTION_FILTER_POLICY",
                "policyDocument": json!({
                    "DestinationArn": stream_arn(stream),
                    "RoleArn": role_arn(),
                    "FilterPattern": "ERROR",
                    "Distribution": BY_LOG_STREAM,
                })
                .to_string(),
                "selectionCriteria": format!(r#"LogGroupName NOT IN ["{EXCLUDED}"]"#),
            }),
        )
        .await;

    let timestamp = base_ms();
    for group in [EXCLUDED, SELECTED, SELECTED_WITH_OWN] {
        emulator
            .logs(
                case,
                "PutLogEvents",
                &json!({
                    "logGroupName": group,
                    "logStreamName": LOG_STREAM,
                    "logEvents": [
                        {"timestamp": timestamp, "message": SELECTED_LINE},
                        {"timestamp": timestamp + 1, "message": OTHER_LINE},
                    ],
                }),
            )
            .await;
    }

    // What each group's envelopes name and carry: the filter that owes the delivery, and the
    // messages it forwarded.
    let mut delivered: Vec<(String, String, Vec<String>)> = Vec::new();
    for (_key, envelope) in emulator.delivered(case, stream).await {
        let group = envelope["logGroup"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: an envelope names its log group, got {envelope}"))
            .to_string();
        let filter = envelope["subscriptionFilters"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: an envelope names its filter, got {envelope}"))
            .to_string();
        let messages: Vec<String> = envelope["logEvents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|event| event["message"].as_str().unwrap_or_default().to_string())
            .collect();
        delivered.push((group, filter, messages));
    }
    delivered.sort();

    let mut expected: Vec<(String, String, Vec<String>)> = vec![
        // The selected group has no filter of its own, so the policy is its only one, and the
        // policy's pattern selects one of the two lines.
        (
            SELECTED.to_string(),
            POLICY.to_string(),
            vec![SELECTED_LINE.to_string()],
        ),
        // The group that carries its own filter is delivered through BOTH: additive.
        (
            SELECTED_WITH_OWN.to_string(),
            OWN_FILTER.to_string(),
            vec![SELECTED_LINE.to_string(), OTHER_LINE.to_string()],
        ),
        (
            SELECTED_WITH_OWN.to_string(),
            POLICY.to_string(),
            vec![SELECTED_LINE.to_string()],
        ),
    ];
    expected.sort();
    assert_eq!(
        delivered, expected,
        "case {case}: expected {expected:?}, got {delivered:?}"
    );

    // And the policy is reported as the account's, so what delivered is a policy a reader can
    // see rather than a filter nothing holds.
    let described = emulator
        .logs(
            case,
            "DescribeAccountPolicies",
            &json!({"policyType": "SUBSCRIPTION_FILTER_POLICY"}),
        )
        .await;
    let named: Vec<String> = described["accountPolicies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|policy| policy["policyName"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        named,
        vec![POLICY.to_string()],
        "case {case}: the account holds the policy that delivered, got {described}"
    );
}
