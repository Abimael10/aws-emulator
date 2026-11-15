// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH stored log events a read serves when the customer managed key
//! they were ingested under is no longer usable.
//!
//! CloudWatch Logs and KMS are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so the key state and the key policy these reads are decided by
//! are KMS's own, written by KMS's own operations, and an event that is served or withheld
//! here is decided by nothing this suite handed to CloudWatch Logs.
//!
//! The rule under test: a log group's stored events are served only while CloudWatch Logs can
//! still use the key they were INGESTED under — the key the group was associated with when
//! each event arrived, since an association re-keys nothing already stored. A key KMS reports
//! in any state but enabled, or whose key policy no longer lets CloudWatch Logs use it for
//! that log group, withholds exactly the events ingested under it: the read still answers 200,
//! with the events it can serve and without the ones it cannot, and reports them all again
//! once the key is usable. An event ingested while the group carried no customer managed key
//! is always served.
//!
//! The tables span what the rule generalises over: which way the key became unusable (its
//! state, its key policy), which operation reads the events (`GetLogEvents`,
//! `FilterLogEvents`), whether the event was ingested under the key, under another key or
//! under none, and whether the group was associated at creation or afterwards.

use serde_json::{json, Value};

use crate::cmk_key_policy_scope::{account_statement, key_policy, log_group_arn, Emulator, REGION};

/// The key policy a usable key carries here: the Regional service principal CloudWatch Logs
/// calls KMS as, granted the encryption operations unconditionally.
fn usable_policy() -> String {
    key_policy(&format!("logs.{REGION}.amazonaws.com"), None)
}

/// A key policy that grants CloudWatch Logs nothing — what a key whose CloudWatch Logs grant
/// has been revoked carries.
fn revoked_policy() -> String {
    json!({"Version": "2012-10-17", "Statement": [account_statement()]}).to_string()
}

/// One KMS call that must succeed — KMS's own operation, over the same engine CloudWatch
/// Logs reads the key through.
async fn kms_ok(aws: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = aws.call("kms", "TrentService", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} {body} must succeed, got {status} {parsed}"
    );
    parsed
}

/// Create a log group, a stream in it, and ingest one event into that stream, answering the
/// event's message.
async fn ingest(
    aws: &Emulator,
    case: &str,
    group: &str,
    stream: &str,
    message: &str,
) -> String {
    aws.logs_ok(
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": [{
                "timestamp": crate::harness::now_ms() - 60 * 1_000,
                "message": message,
            }],
        }),
    )
    .await;
    message.to_string()
}

/// The messages `GetLogEvents` serves for one stream.
async fn read_stream(aws: &Emulator, case: &str, group: &str, stream: &str) -> Vec<String> {
    let page = aws
        .logs_ok(
            case,
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream, "startFromHead": true}),
        )
        .await;
    messages(&page, case)
}

/// The messages `FilterLogEvents` serves for one log group.
async fn search_group(aws: &Emulator, case: &str, group: &str) -> Vec<String> {
    let page = aws
        .logs_ok(case, "FilterLogEvents", &json!({"logGroupName": group}))
        .await;
    messages(&page, case)
}

/// The `message` of every event of one page, in the order the page reports them.
fn messages(page: &Value, case: &str) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a page reports an events list, got {page}"))
        .iter()
        .map(|event| {
            event["message"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an event carries a message, got {event}"))
                .to_string()
        })
        .collect()
}

/// One KMS call a row makes about a key, built from that key's ARN: the operation and its
/// body.
type KeyCall = fn(&str) -> (&'static str, Value);

/// RULE: a read serves a group's events only while CloudWatch Logs can use the key they were
/// ingested under — whichever way the key stopped being usable, and whichever operation reads
/// them — and serves them again as soon as it can.
#[tokio::test]
async fn an_unusable_ingest_key_withholds_the_events_it_holds() {
    // (case, how the key is made unusable, how it is made usable again)
    let rows: Vec<(&str, KeyCall, KeyCall)> = vec![
        (
            "the key is disabled and enabled again",
            |key| ("DisableKey", json!({"KeyId": key})),
            |key| ("EnableKey", json!({"KeyId": key})),
        ),
        (
            "the key policy's CloudWatch Logs grant is revoked and restored",
            |key| (
                "PutKeyPolicy",
                json!({"KeyId": key, "PolicyName": "default", "Policy": revoked_policy()}),
            ),
            |key| (
                "PutKeyPolicy",
                json!({"KeyId": key, "PolicyName": "default", "Policy": usable_policy()}),
            ),
        ),
    ];
    for (case, withdraw, restore) in rows {
        let aws = Emulator::new();
        let group = "cmk/withheld";
        let stream = "stream";
        let key = aws.created_key(case, &usable_policy()).await;
        aws.logs_ok(
            case,
            "CreateLogGroup",
            &json!({"logGroupName": group, "kmsKeyId": key}),
        )
        .await;
        aws.logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
        let message = ingest(&aws, case, group, stream, "bound to the key").await;

        let served = read_stream(&aws, case, group, stream).await;
        assert_eq!(
            served,
            vec![message.clone()],
            "case {case:?}: a usable key serves the group's events, got {served:?}"
        );

        let (operation, body) = withdraw(&key);
        kms_ok(&aws, case, operation, &body).await;
        for (reader, got) in [
            ("GetLogEvents", read_stream(&aws, case, group, stream).await),
            ("FilterLogEvents", search_group(&aws, case, group).await),
        ] {
            assert!(
                got.is_empty(),
                "case {case:?} ({reader}): expected the events to be withheld, got {got:?}"
            );
        }

        let (operation, body) = restore(&key);
        kms_ok(&aws, case, operation, &body).await;
        for (reader, got) in [
            ("GetLogEvents", read_stream(&aws, case, group, stream).await),
            ("FilterLogEvents", search_group(&aws, case, group).await),
        ] {
            assert_eq!(
                got,
                vec![message.clone()],
                "case {case:?} ({reader}): expected the events again once the key is usable, \
                 got {got:?}"
            );
        }
    }
}

/// RULE: the binding is per EVENT, so only the events ingested under the unusable key are
/// withheld: events ingested under another key, and events ingested while the group carried
/// no key at all, keep reading.
#[tokio::test]
async fn only_the_events_of_the_unusable_key_are_withheld() {
    let case = "events bound to the key they were ingested under";
    let aws = Emulator::new();
    let group = "cmk/per-event";
    let stream = "stream";
    let key_a = aws.created_key(case, &usable_policy()).await;
    let key_b = aws.created_key(case, &usable_policy()).await;
    assert_ne!(
        key_a, key_b,
        "case {case:?}: the two keys are different keys"
    );

    // Plaintext first: the group carries no key while this event is ingested.
    aws.logs_ok(case, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.logs_ok(
        case,
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    let plaintext = ingest(&aws, case, group, stream, "under no customer key").await;

    // Then under key A, then under key B: an association re-keys nothing already stored.
    aws.logs_ok(
        case,
        "AssociateKmsKey",
        &json!({"logGroupName": group, "kmsKeyId": key_a}),
    )
    .await;
    let under_a = ingest(&aws, case, group, stream, "under key A").await;
    aws.logs_ok(
        case,
        "AssociateKmsKey",
        &json!({"logGroupName": group, "kmsKeyId": key_b}),
    )
    .await;
    let under_b = ingest(&aws, case, group, stream, "under key B").await;

    let all = vec![plaintext.clone(), under_a.clone(), under_b.clone()];
    let served = read_stream(&aws, case, group, stream).await;
    assert_eq!(
        served, all,
        "case {case:?}: every event reads while both keys are usable, got {served:?}"
    );

    // Revoke key A's grant: exactly the event ingested under key A stops being served.
    kms_ok(
        &aws,
        case,
        "PutKeyPolicy",
        &json!({"KeyId": key_a, "PolicyName": "default", "Policy": revoked_policy()}),
    )
    .await;
    let expected = vec![plaintext.clone(), under_b.clone()];
    for (reader, got) in [
        ("GetLogEvents", read_stream(&aws, case, group, stream).await),
        ("FilterLogEvents", search_group(&aws, case, group).await),
    ] {
        assert_eq!(
            got, expected,
            "case {case:?} ({reader}): expected only key A's event withheld, got {got:?}"
        );
    }

    // And disabling key B withholds its event too, leaving the plaintext one.
    kms_ok(&aws, case, "DisableKey", &json!({"KeyId": key_b}))
        .await;
    let got = read_stream(&aws, case, group, stream).await;
    assert_eq!(
        got,
        vec![plaintext.clone()],
        "case {case:?}: expected only the plaintext event, got {got:?}"
    );

    // Nothing of the store changed on the way: both keys usable again reports all three.
    kms_ok(&aws, case, "EnableKey", &json!({"KeyId": key_b}))
        .await;
    kms_ok(
        &aws,
        case,
        "PutKeyPolicy",
        &json!({"KeyId": key_a, "PolicyName": "default", "Policy": usable_policy()}),
    )
    .await;
    let got = read_stream(&aws, case, group, stream).await;
    assert_eq!(
        got, all,
        "case {case:?}: a withheld read consumed nothing, got {got:?}"
    );
}

/// RULE: a group that never carried a customer managed key is decided by no key at all, so a
/// disabled key elsewhere in the account withholds nothing of it.
#[tokio::test]
async fn a_group_with_no_key_is_served_whatever_the_account_keys_are_in() {
    let case = "a plaintext group beside a disabled key";
    let aws = Emulator::new();
    let group = "cmk/plaintext";
    let stream = "stream";
    let key = aws.created_key(case, &usable_policy()).await;
    aws.logs_ok(case, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.logs_ok(
        case,
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    let message = ingest(&aws, case, group, stream, "plaintext").await;

    kms_ok(&aws, case, "DisableKey", &json!({"KeyId": key})).await;
    let got = read_stream(&aws, case, group, stream).await;
    assert_eq!(
        got,
        vec![message],
        "case {case:?}: expected the group's events whatever other keys are in, got {got:?}"
    );
}

/// The log group ARN a key policy is scoped by is the group's own, so a key scoped to ANOTHER
/// group withholds the events of the group it was associated with before that scoping.
#[tokio::test]
async fn a_key_scoped_to_another_group_withholds_this_groups_events() {
    let case = "a key policy re-scoped to another log group";
    let aws = Emulator::new();
    let group = "cmk/scoped";
    let stream = "stream";
    let key = aws.created_key(case, &usable_policy()).await;
    aws.logs_ok(
        case,
        "CreateLogGroup",
        &json!({"logGroupName": group, "kmsKeyId": key}),
    )
    .await;
    aws.logs_ok(
        case,
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    let message = ingest(&aws, case, group, stream, "scoped").await;

    kms_ok(
        &aws,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": key,
            "PolicyName": "default",
            "Policy": key_policy(
                "logs.amazonaws.com",
                Some(json!({
                    "ArnEquals": {
                        "kms:EncryptionContext:aws:logs:arn": log_group_arn("cmk/elsewhere"),
                    }
                })),
            ),
        }),
    )
    .await;
    let got = read_stream(&aws, case, group, stream).await;
    assert!(
        got.is_empty(),
        "case {case:?}: expected the events to be withheld, got {got:?}"
    );

    // Re-scoped to this group, the same events read again.
    kms_ok(
        &aws,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": key,
            "PolicyName": "default",
            "Policy": key_policy(
                "logs.amazonaws.com",
                Some(json!({
                    "ArnEquals": {"kms:EncryptionContext:aws:logs:arn": log_group_arn(group)}
                })),
            ),
        }),
    )
    .await;
    let got = read_stream(&aws, case, group, stream).await;
    assert_eq!(
        got,
        vec![message],
        "case {case:?}: expected the events once the policy names this group, got {got:?}"
    );
}
