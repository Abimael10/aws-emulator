// SPDX-License-Identifier: CC0-1.0

//! What the BUCKET POLICY of a delivery channel's bucket decides about a snapshot delivery
//! Config PERFORMS through that channel.
//!
//! The rule under test: **Config writes a delivered object as its own service principal, so
//! a snapshot delivery writes its object only while the policy in force on the channel's
//! bucket at the moment of the write allows `config.amazonaws.com` the `s3:PutObject` on
//! the key the object goes to — the policy the bucket carries THEN, not the one it carried
//! when the channel was taken — and a delivery that policy withholds writes nothing and is
//! reported through the channel's `configSnapshotDeliveryInfo` as a failed delivery, with
//! the error code and the sentence Config carries for a delivery policy that does not admit
//! it.**
//!
//! `DeliverConfigSnapshot` models no error for the delivery itself, so the channel's status
//! is the only place that outcome is raised: these cases read it there.
//!
//! Config and S3 are mounted on ONE engine (the harness
//! `config_recorded_resource_compliance` builds), so a case that writes a policy with S3's
//! own `PutBucketPolicy`, or takes it away with `DeleteBucketPolicy`, is writing the
//! document the delivery's own write is judged against.
//!
//! The rows differ along what the rule generalises over: whether the bucket carries a policy
//! at all when the delivery is performed, which resource its delivery statement covers (the
//! whole bucket, this account's Config path, this Region's objects alone, another Region's,
//! another account's), which principal the grant names, whether Config is denied outright,
//! whether the channel delivers under a key prefix, and which CONDITION the delivery
//! statement carries.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config_recorded_resource_compliance::{
    config_delivery_bucket_policy, Emulator, ACCOUNT_ID, REGION,
};
use crate::delivery_channel_bucket_policy::{
    allow, policy, CONFIG_PRINCIPAL, GET_BUCKET_ACL, INSUFFICIENT_POLICY, LIST_BUCKET, PUT_OBJECT,
};

/// The bucket every case delivers its snapshots into.
const DELIVERY_BUCKET: &str = "config-delivery-bucket";

/// The recorder and the channel every case sets up.
const RECORDER_NAME: &str = "the-recorder";
/// See [`RECORDER_NAME`].
const CHANNEL_NAME: &str = "the-channel";

/// The `lastStatus` a delivery that wrote its object reports, and the one a delivery that
/// did not. The service answers the members of `com.amazonaws.configservice#DeliveryStatus`
/// upper-cased.
const SUCCESS: &str = "SUCCESS";
/// See [`SUCCESS`].
const FAILURE: &str = "FAILURE";

/// How long a poll waits for the work the clock owes — the delivery, due a fraction of a
/// second after the call that scheduled it. Many passes' worth of slack, not a span the
/// behaviour depends on.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(15);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The ARN of the delivery bucket, which a statement's `Resource` names.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{DELIVERY_BUCKET}")
}

/// The ONE place a row's policy is built: Config granted the bucket-level calls on the
/// bucket and the object write on `objects`, with `condition` in place of the condition the
/// documented policy gives its delivery statement where a row replaces it — the documented
/// delivery policy with its `AWSConfigBucketDelivery` statement pointed and conditioned
/// wherever a row points and conditions it.
fn delivery_policy(objects: String, condition: Option<Value>) -> String {
    let bucket = bucket_arn();
    let mut delivery = allow(
        CONFIG_PRINCIPAL,
        &[PUT_OBJECT],
        std::slice::from_ref(&objects),
    );
    if let Some(condition) = condition {
        delivery["Condition"] = condition;
    }
    policy(vec![
        allow(
            CONFIG_PRINCIPAL,
            &[GET_BUCKET_ACL, LIST_BUCKET],
            std::slice::from_ref(&bucket),
        ),
        delivery,
    ])
}

/// A policy whose delivery statement covers `objects`, conditioned the way the documented
/// policy conditions it.
fn policy_writing(objects: String) -> String {
    delivery_policy(objects, None)
}

/// Create the delivery bucket carrying the documented delivery policy, put the recorder and
/// a channel delivering through it under `prefix`, and start recording — the established
/// channel every case then changes the bucket's policy under.
async fn channel_through(emulator: &Emulator, prefix: Option<&str>) {
    emulator.create_delivery_bucket(DELIVERY_BUCKET).await;
    emulator
        .config(
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": RECORDER_NAME,
                    "roleARN": format!("arn:aws:iam::{ACCOUNT_ID}:role/config-role"),
                    "recordingGroup": { "allSupported": true },
                }
            }),
        )
        .await;
    let mut channel = json!({ "name": CHANNEL_NAME, "s3BucketName": DELIVERY_BUCKET });
    if let Some(prefix) = prefix {
        channel["s3KeyPrefix"] = json!(prefix);
    }
    emulator
        .config("PutDeliveryChannel", &json!({ "DeliveryChannel": channel }))
        .await;
    emulator
        .config(
            "StartConfigurationRecorder",
            &json!({ "ConfigurationRecorderName": RECORDER_NAME }),
        )
        .await;
}

/// Put `document` as the delivery bucket's policy.
async fn put_policy(case: &str, emulator: &Emulator, document: String) {
    emulator
        .s3()
        .put_bucket_policy()
        .bucket(DELIVERY_BUCKET)
        .policy(document)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: writing the bucket policy must succeed, got {error:?}")
        });
}

/// Take the delivery bucket's policy away.
async fn delete_policy(case: &str, emulator: &Emulator) {
    emulator
        .s3()
        .delete_bucket_policy()
        .bucket(DELIVERY_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: deleting the bucket policy must succeed, got {error:?}")
        });
}

/// Schedule a delivery and answer the `configSnapshotId` it was given.
async fn deliver(case: &str, emulator: &Emulator) -> String {
    let response = emulator
        .config(
            "DeliverConfigSnapshot",
            &json!({ "deliveryChannelName": CHANNEL_NAME }),
        )
        .await;
    response["configSnapshotId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a configSnapshotId, got {response}"))
        .to_string()
}

/// Read the channel's `configSnapshotDeliveryInfo` until the delivery reports a terminal
/// status, and answer it.
async fn await_settled_delivery(case: &str, emulator: &Emulator) -> Value {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let response = emulator
            .config(
                "DescribeDeliveryChannelStatus",
                &json!({ "DeliveryChannelNames": [CHANNEL_NAME] }),
            )
            .await;
        let info = response["DeliveryChannelsStatus"][0]["configSnapshotDeliveryInfo"].clone();
        if info["lastStatus"].is_string() {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the delivery to reach a terminal lastStatus, got {info}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every key the delivery bucket holds.
async fn delivered_keys(case: &str, emulator: &Emulator) -> Vec<String> {
    let listed = emulator
        .s3()
        .list_objects_v2()
        .bucket(DELIVERY_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: listing {DELIVERY_BUCKET} must succeed, got {error:?}")
        });
    listed
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// The sentence a delivery the bucket's policy withholds is reported with, which names the
/// bucket and the call that was denied, the key prefix the channel asked for with it.
fn denied_message(prefix: Option<&str>) -> String {
    format!(
        "Insufficient delivery policy to s3 bucket: {DELIVERY_BUCKET}, unable to write to \
         bucket, provided s3 key prefix is '{}'.",
        prefix.unwrap_or("null")
    )
}

/// One row: the policy in force on the bucket when the delivery is performed, the prefix the
/// channel delivers under, and whether the rule says the object has to be written.
struct Row {
    case: &'static str,
    /// The policy the bucket carries at the moment of the write, or `None` for a bucket whose
    /// policy was taken away.
    document: fn() -> Option<String>,
    /// The `s3KeyPrefix` the channel configures.
    prefix: Option<&'static str>,
    /// Whether the delivery's write is allowed by that policy.
    delivered: bool,
}

#[tokio::test]
async fn the_policy_in_force_at_the_write_decides_whether_a_snapshot_is_delivered() {
    let rows: Vec<Row> = vec![
        Row {
            case: "the documented policy, still in force",
            document: || Some(config_delivery_bucket_policy(DELIVERY_BUCKET)),
            prefix: None,
            delivered: true,
        },
        Row {
            case: "a delivery statement covering this account's Config path",
            document: || {
                Some(policy_writing(format!(
                    "{}/AWSLogs/{ACCOUNT_ID}/Config/*",
                    bucket_arn()
                )))
            },
            prefix: None,
            delivered: true,
        },
        Row {
            // The key a delivery goes to carries the Region it was delivered for, so a
            // statement covering only this Region's objects still covers it — which the
            // writability check Config makes before it takes a channel is NOT under.
            case: "a delivery statement covering this Region's delivered objects alone",
            document: || {
                Some(policy_writing(format!(
                    "{}/AWSLogs/{ACCOUNT_ID}/Config/{REGION}/*",
                    bucket_arn()
                )))
            },
            prefix: None,
            delivered: true,
        },
        Row {
            case: "a delivery statement covering the channel's own prefix",
            document: || {
                Some(policy_writing(format!(
                    "{}/audit/AWSLogs/{ACCOUNT_ID}/Config/*",
                    bucket_arn()
                )))
            },
            prefix: Some("audit"),
            delivered: true,
        },
        Row {
            case: "no policy at all, taken away under the established channel",
            document: || None,
            prefix: None,
            delivered: false,
        },
        Row {
            case: "a delivery statement covering another Region's objects",
            document: || {
                Some(policy_writing(format!(
                    "{}/AWSLogs/{ACCOUNT_ID}/Config/eu-west-1/*",
                    bucket_arn()
                )))
            },
            prefix: None,
            delivered: false,
        },
        Row {
            case: "a delivery statement covering another account's Config path",
            document: || {
                Some(policy_writing(format!(
                    "{}/AWSLogs/999999999999/Config/*",
                    bucket_arn()
                )))
            },
            prefix: None,
            delivered: false,
        },
        Row {
            case: "a delivery statement that does not reach under the channel's prefix",
            document: || {
                Some(policy_writing(format!(
                    "{}/AWSLogs/{ACCOUNT_ID}/Config/*",
                    bucket_arn()
                )))
            },
            prefix: Some("audit"),
            delivered: false,
        },
        Row {
            case: "the write granted to another service principal",
            document: || {
                let bucket = bucket_arn();
                Some(policy(vec![allow(
                    "cloudtrail.amazonaws.com",
                    &[GET_BUCKET_ACL, LIST_BUCKET, PUT_OBJECT],
                    &[bucket.clone(), format!("{bucket}/*")],
                )]))
            },
            prefix: None,
            delivered: false,
        },
        Row {
            case: "the write denied to Config outright",
            document: || {
                let bucket = bucket_arn();
                Some(json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        allow(
                            CONFIG_PRINCIPAL,
                            &[GET_BUCKET_ACL, LIST_BUCKET, PUT_OBJECT],
                            &[bucket.clone(), format!("{bucket}/*")],
                        ),
                        json!({
                            "Effect": "Deny",
                            "Principal": { "Service": CONFIG_PRINCIPAL },
                            "Action": PUT_OBJECT,
                            "Resource": format!("{bucket}/*"),
                        }),
                    ],
                })
                .to_string())
            },
            prefix: None,
            delivered: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        channel_through(&emulator, row.prefix).await;

        // The policy the WRITE is judged against is the one the bucket carries now, which
        // every row sets after the channel was established under the documented one.
        match (row.document)() {
            Some(document) => put_policy(case, &emulator, document).await,
            None => delete_policy(case, &emulator).await,
        }

        let snapshot_id = deliver(case, &emulator).await;
        let info = await_settled_delivery(case, &emulator).await;
        let keys = delivered_keys(case, &emulator).await;

        if row.delivered {
            assert_eq!(
                info["lastStatus"],
                json!(SUCCESS),
                "case {case:?}: a delivery the policy admits reports {SUCCESS}, got {info}"
            );
            for member in ["lastErrorCode", "lastErrorMessage"] {
                assert!(
                    info.get(member).is_none(),
                    "case {case:?}: a delivery that succeeded reports no {member}, got {info}"
                );
            }
            assert!(
                info["lastSuccessfulTime"].is_number(),
                "case {case:?}: a delivery that wrote its object reports when it did, got \
                 {info}"
            );
            assert!(
                keys.iter().any(|key| key.contains(&snapshot_id)),
                "case {case:?}: expected an object naming {snapshot_id}, got {keys:?}"
            );
            continue;
        }

        let expected = denied_message(row.prefix);
        assert_eq!(
            (
                info["lastStatus"].as_str(),
                info["lastErrorCode"].as_str(),
                info["lastErrorMessage"].as_str(),
            ),
            (
                Some(FAILURE),
                Some(INSUFFICIENT_POLICY),
                Some(expected.as_str()),
            ),
            "case {case:?}: expected {FAILURE} {INSUFFICIENT_POLICY} and {expected:?}, got \
             {info}"
        );
        assert!(
            info.get("lastSuccessfulTime").is_none(),
            "case {case:?}: a delivery that wrote nothing claims no success, got {info}"
        );
        assert!(
            info["lastAttemptTime"].is_number(),
            "case {case:?}: the delivery that failed was still attempted, got {info}"
        );
        assert!(
            keys.is_empty(),
            "case {case:?}: a delivery the policy withholds writes no object, got {keys:?}"
        );
    }
}

/// A delivery denied by a policy taken away under the established channel fails, and a
/// delivery made after that policy is put back succeeds: the decision is read per delivery,
/// so neither leg is decided by the other. The rows differ along the prefix the channel
/// delivers under.
#[tokio::test]
async fn a_delivery_denied_by_a_removed_policy_fails_and_the_next_one_succeeds_once_it_is_back() {
    let cases: [(&str, Option<&str>); 2] =
        [("no key prefix", None), ("a key prefix", Some("audit"))];

    for (case, prefix) in cases {
        let emulator = Emulator::new();
        channel_through(&emulator, prefix).await;

        // The denied leg: the policy that let the channel be taken is gone.
        delete_policy(case, &emulator).await;
        let denied_id = deliver(case, &emulator).await;
        let denied = await_settled_delivery(case, &emulator).await;
        let expected = denied_message(prefix);
        assert_eq!(
            (
                denied["lastStatus"].as_str(),
                denied["lastErrorCode"].as_str(),
                denied["lastErrorMessage"].as_str(),
            ),
            (
                Some(FAILURE),
                Some(INSUFFICIENT_POLICY),
                Some(expected.as_str()),
            ),
            "case {case:?}: expected {FAILURE} {INSUFFICIENT_POLICY} and {expected:?}, got \
             {denied}"
        );
        let after_denial = delivered_keys(case, &emulator).await;
        assert!(
            !after_denial.iter().any(|key| key.contains(&denied_id)),
            "case {case:?}: the denied delivery {denied_id} wrote nothing, got \
             {after_denial:?}"
        );

        // The restored leg: the same policy is put back, and the next delivery is written.
        put_policy(
            case,
            &emulator,
            config_delivery_bucket_policy(DELIVERY_BUCKET),
        )
        .await;
        let delivered_id = deliver(case, &emulator).await;
        let delivered = await_settled_delivery(case, &emulator).await;
        assert_eq!(
            delivered["lastStatus"],
            json!(SUCCESS),
            "case {case:?}: the delivery made under the restored policy reports {SUCCESS}, \
             got {delivered}"
        );
        for member in ["lastErrorCode", "lastErrorMessage"] {
            assert!(
                delivered.get(member).is_none(),
                "case {case:?}: the delivery that succeeded reports no {member}, got \
                 {delivered}"
            );
        }
        // The success is the LATER delivery's: it landed at or after the denied attempt.
        let denied_attempt = denied["lastAttemptTime"].as_f64().unwrap_or_default();
        let successful = delivered["lastSuccessfulTime"].as_f64().unwrap_or_default();
        assert!(
            denied_attempt > 0.0 && successful >= denied_attempt,
            "case {case:?}: expected a success at or after the denied attempt \
             {denied_attempt}, got {successful} with {delivered}"
        );
        let keys = delivered_keys(case, &emulator).await;
        assert!(
            keys.iter().any(|key| key.contains(&delivered_id)),
            "case {case:?}: expected an object naming {delivered_id}, got {keys:?}"
        );
        assert!(
            !keys.iter().any(|key| key.contains(&denied_id)),
            "case {case:?}: the denied delivery is still not in the bucket, got {keys:?}"
        );
    }
}

/// An account that is not this one, which a row conditions a grant on to point it away from
/// the delivery actually being made.
const ANOTHER_ACCOUNT: &str = "999999999999";

/// A policy whose delivery statement covers every object of the bucket, carrying `condition`
/// in place of the condition the documented policy gives that statement.
fn policy_conditioned(condition: Value) -> String {
    delivery_policy(format!("{}/*", bucket_arn()), Some(condition))
}

/// The ARN Config assigned the customer managed configuration recorder a delivery is made
/// for, read back through `DescribeConfigurationRecorders` — the value a delivery statement
/// conditioned on `AWS:SourceArn` is compared against, which is minted per recorder and so
/// cannot be written into a row.
async fn described_recorder_arn(case: &str, emulator: &Emulator) -> String {
    let described = emulator
        .config("DescribeConfigurationRecorders", &json!({}))
        .await;
    described["ConfigurationRecorders"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the recorder's arn, got {described}")
        })
        .to_string()
}

/// One row: the `Condition` the bucket's delivery statement carries when the delivery is
/// performed, and whether the rule says the object has to be written.
struct ConditionRow {
    case: &'static str,
    /// The condition the delivery statement carries, built from the ARN of the recorder the
    /// delivery is made for.
    condition: fn(&str) -> Value,
    /// Whether the write that delivery makes is allowed under that condition.
    delivered: bool,
}

/// The CONDITION on the delivery statement decides the same thing its resource and principal
/// do: a snapshot delivery writes its object only while the statement that grants
/// `s3:PutObject` to `config.amazonaws.com` holds for the delivery actually being made, and
/// a delivery whose condition does not hold writes nothing and is reported as a failure.
///
/// The three keys the rows condition on are the ones a Config delivery carries, which the
/// delivery bucket policy AWS documents for a delivery channel constrains: the delivering
/// account (`AWS:SourceAccount`), the resource of Config's the delivery is made for
/// (`AWS:SourceArn`) and the canned ACL Config writes a delivered object with
/// (`s3:x-amz-acl`). A row also conditions on a key no delivery carries, which decides
/// nothing and so grants nothing.
#[tokio::test]
async fn the_delivery_statements_condition_decides_whether_a_snapshot_is_delivered() {
    let rows: Vec<ConditionRow> = vec![
        ConditionRow {
            case: "the delivering account, as the documented policy conditions it",
            condition: |_| json!({ "StringEquals": { "AWS:SourceAccount": ACCOUNT_ID } }),
            delivered: true,
        },
        ConditionRow {
            case: "the canned ACL Config writes a delivered object with",
            condition: |_| {
                json!({ "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" } })
            },
            delivered: true,
        },
        ConditionRow {
            case: "the delivering account and the canned ACL together",
            condition: |_| {
                json!({
                    "StringEquals": {
                        "AWS:SourceAccount": ACCOUNT_ID,
                        "s3:x-amz-acl": "bucket-owner-full-control",
                    }
                })
            },
            delivered: true,
        },
        ConditionRow {
            case: "the recorder the delivery is made for, as the source ARN",
            condition: |recorder| json!({ "StringEquals": { "AWS:SourceArn": recorder } }),
            delivered: true,
        },
        ConditionRow {
            case: "any configuration recorder of this account, as a source ARN pattern",
            condition: |_| {
                json!({
                    "ArnLike": {
                        "AWS:SourceArn":
                            format!("arn:aws:config:{REGION}:{ACCOUNT_ID}:configuration-recorder/*")
                    }
                })
            },
            delivered: true,
        },
        ConditionRow {
            case: "another account as the delivering account",
            condition: |_| json!({ "StringEquals": { "AWS:SourceAccount": ANOTHER_ACCOUNT } }),
            delivered: false,
        },
        ConditionRow {
            case: "a canned ACL other than the one a delivered object is written with",
            condition: |_| json!({ "StringEquals": { "s3:x-amz-acl": "private" } }),
            delivered: false,
        },
        ConditionRow {
            case: "another account's recorder as the source ARN",
            condition: |_| {
                json!({
                    "ArnLike": {
                        "AWS:SourceArn":
                            format!(
                                "arn:aws:config:{REGION}:{ANOTHER_ACCOUNT}:\
                                 configuration-recorder/*"
                            )
                    }
                })
            },
            delivered: false,
        },
        ConditionRow {
            case: "this account's recorder with one character of its ARN changed",
            condition: |recorder| {
                json!({ "StringEquals": { "AWS:SourceArn": format!("{recorder}x") } })
            },
            delivered: false,
        },
        ConditionRow {
            case: "a condition key a Config delivery carries no value for",
            condition: |_| {
                json!({ "StringEquals": { "s3:x-amz-server-side-encryption": "aws:kms" } })
            },
            delivered: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        channel_through(&emulator, None).await;

        // The condition is compared against the delivery being made, so the row's statement
        // is written against the recorder this account really holds.
        let recorder = described_recorder_arn(case, &emulator).await;
        put_policy(case, &emulator, policy_conditioned((row.condition)(&recorder))).await;

        let snapshot_id = deliver(case, &emulator).await;
        let info = await_settled_delivery(case, &emulator).await;
        let keys = delivered_keys(case, &emulator).await;

        if row.delivered {
            assert_eq!(
                info["lastStatus"],
                json!(SUCCESS),
                "case {case:?}: a delivery whose condition holds reports {SUCCESS}, got {info}"
            );
            for member in ["lastErrorCode", "lastErrorMessage"] {
                assert!(
                    info.get(member).is_none(),
                    "case {case:?}: a delivery that succeeded reports no {member}, got {info}"
                );
            }
            assert!(
                keys.iter().any(|key| key.contains(&snapshot_id)),
                "case {case:?}: expected an object naming {snapshot_id}, got {keys:?}"
            );
            continue;
        }

        let expected = denied_message(None);
        assert_eq!(
            (
                info["lastStatus"].as_str(),
                info["lastErrorCode"].as_str(),
                info["lastErrorMessage"].as_str(),
            ),
            (
                Some(FAILURE),
                Some(INSUFFICIENT_POLICY),
                Some(expected.as_str()),
            ),
            "case {case:?}: expected {FAILURE} {INSUFFICIENT_POLICY} and {expected:?}, got \
             {info}"
        );
        assert!(
            info.get("lastSuccessfulTime").is_none(),
            "case {case:?}: a delivery that wrote nothing claims no success, got {info}"
        );
        assert!(
            keys.is_empty(),
            "case {case:?}: a delivery the condition withholds writes no object, got {keys:?}"
        );
    }
}
