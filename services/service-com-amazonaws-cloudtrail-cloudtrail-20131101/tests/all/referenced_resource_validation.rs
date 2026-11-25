// SPDX-License-Identifier: CC0-1.0

//! The rule over every resource a trail's settings REFERENCE, and the one every advanced
//! event selector has to state.
//!
//! A trail is a delivery pipeline, so each resource it is configured against belongs to
//! another service and has to exist there AND let CloudTrail use it before the trail is
//! accepted: the destination bucket and its bucket policy, the notification topic's
//! access policy, the `KmsKeyId` and the `CloudWatchLogsLogGroupArn`. Each failure has
//! its own modelled error, and a refused create or update persists nothing.
//!
//! Every table below spans the CLASS rather than one row: a reference is defective
//! because the resource is absent, because its policy grants another principal, another
//! action, another resource or another key prefix, because it denies the call outright,
//! or because the identifier names nothing anywhere — and each table also carries the
//! rows where the reference HOLDS, in each form the member accepts.

use serde_json::{json, Value};

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state::referenced_resources as referenced;

use super::emulator::{Emulator, HOME_REGION};

/// The Region no case in this suite calls in, for the rows whose subject is a reference
/// pinned to another Region.
const OTHER_REGION: &str = "eu-west-1";

/// The HTTP status CloudTrail answers a refused reference with when the model's own
/// annotation is not the status on the wire: `S3BucketDoesNotExistException` is
/// `@httpError(404)` and answered 400, and `InsufficientS3BucketPolicyException` is
/// `@httpError(403)` and answered 400
/// (`referenced::INSUFFICIENT_S3_BUCKET_POLICY_STATUS`).
const BUCKET_ABSENT_STATUS: u16 = 400;

/// This account's id, as every ARN in this suite carries it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The trail ARN `CreateTrail` mints for `name` in [`HOME_REGION`].
fn trail_arn(name: &str) -> String {
    format!(
        "arn:aws:cloudtrail:{HOME_REGION}:{}:trail/{name}",
        account()
    )
}

/// A bucket policy granting `principal` the action `action` on `resource`, optionally
/// under `condition` — the one shape every destination row below varies.
fn bucket_policy(principal: Value, action: &str, resource: &str, condition: Option<Value>) -> String {
    let mut statement = json!({
        "Sid": "Grant",
        "Effect": "Allow",
        "Principal": principal,
        "Action": action,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// The CloudTrail service principal, as a policy's `Principal` element names it.
fn cloudtrail_principal() -> Value {
    json!({"Service": "cloudtrail.amazonaws.com"})
}

/// The canned-ACL condition a CloudTrail bucket policy scopes its write grant by.
fn acl_condition() -> Value {
    json!({"StringEquals": {"s3:x-amz-acl": "bucket-owner-full-control"}})
}

/// What the account holds for the destination a row points its trail at.
#[derive(Debug, Clone)]
enum Destination {
    /// No bucket of that name at all.
    Absent,
    /// A bucket created and never policied — the state `CreateBucket` leaves one in.
    Unpolicied,
    /// A bucket carrying this policy document.
    Policied(String),
}

/// `CreateTrail` accepts a destination only when the account holds the bucket AND its
/// policy lets CloudTrail write the log files this trail would deliver.
#[tokio::test]
async fn a_destination_is_accepted_only_when_its_policy_permits_the_delivery() {
    // The harness provisions nothing here: each row states what the account holds.
    let emulator = Emulator::without_provisioned_destinations();

    // (case, what the account holds, the S3KeyPrefix the trail carries, the expected
    //  error code or "" for a success)
    let rows: Vec<(&str, Destination, Option<&str>, &str)> = vec![
        (
            "no bucket of that name",
            Destination::Absent,
            None,
            "S3BucketDoesNotExistException",
        ),
        (
            "a bucket created and never policied",
            Destination::Unpolicied,
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write to another principal",
            Destination::Policied(bucket_policy(
                json!({"Service": "config.amazonaws.com"}),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/*",
                Some(acl_condition()),
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting CloudTrail a read instead of the write",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:GetObject",
                "arn:aws:s3:::REPLACED/*",
                None,
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write on another bucket's objects",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::somewhere-else/*",
                None,
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write under a key prefix this trail does not use",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/elsewhere/*",
                None,
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write under a condition the delivery does not satisfy",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/*",
                Some(json!({"StringEquals": {"s3:x-amz-acl": "public-read"}})),
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write and then denying it",
            Destination::Policied(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Sid": "Grant",
                            "Effect": "Allow",
                            "Principal": cloudtrail_principal(),
                            "Action": "s3:PutObject",
                            "Resource": "arn:aws:s3:::REPLACED/*",
                        },
                        {
                            "Sid": "Revoke",
                            "Effect": "Deny",
                            "Principal": cloudtrail_principal(),
                            "Action": "s3:PutObject",
                            "Resource": "arn:aws:s3:::REPLACED/*",
                        },
                    ],
                })
                .to_string(),
            ),
            None,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a policy granting the write over the whole bucket",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/*",
                Some(acl_condition()),
            )),
            None,
            "",
        ),
        (
            "a policy granting the write under the CloudTrail log path",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                &format!("arn:aws:s3:::REPLACED/AWSLogs/{}/*", account()),
                Some(acl_condition()),
            )),
            None,
            "",
        ),
        (
            "a policy granting the write under the trail's own key prefix",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                &format!("arn:aws:s3:::REPLACED/logs/prod/AWSLogs/{}/*", account()),
                Some(acl_condition()),
            )),
            Some("logs/prod"),
            "",
        ),
        (
            "a policy whose grant is scoped to the trail it is written for",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/*",
                Some(json!({
                    "StringEquals": {"aws:SourceArn": trail_arn("destination-11")},
                })),
            )),
            None,
            "",
        ),
        (
            "a policy whose grant is scoped to another trail",
            Destination::Policied(bucket_policy(
                cloudtrail_principal(),
                "s3:PutObject",
                "arn:aws:s3:::REPLACED/*",
                Some(json!({
                    "StringEquals": {"aws:SourceArn": trail_arn("some-other-trail")},
                })),
            )),
            None,
            "InsufficientS3BucketPolicyException",
        ),
    ];

    for (index, (case, destination, prefix, expected)) in rows.into_iter().enumerate() {
        let bucket = format!("destination-bucket-{index}");
        let name = format!("destination-{index}");
        match &destination {
            Destination::Absent => {}
            Destination::Unpolicied => {
                emulator.provision_bucket_with_policy(&bucket, HOME_REGION, None);
            }
            Destination::Policied(policy) => {
                // The row writes the bucket it is about as `REPLACED`, so one document
                // serves every row and names the bucket the row really created.
                let policy = policy.replace("REPLACED", &bucket);
                emulator.provision_bucket_with_policy(&bucket, HOME_REGION, Some(&policy));
            }
        }

        let mut request = json!({"Name": name, "S3BucketName": bucket});
        if let Some(prefix) = prefix {
            request["S3KeyPrefix"] = json!(prefix);
        }
        let reply = emulator.call("CreateTrail", &request).await;

        if expected.is_empty() {
            let created = reply.expect_ok(case).clone();
            assert_eq!(
                created.get("S3BucketName").and_then(Value::as_str),
                Some(bucket.as_str()),
                "case {case}: expected the created trail to report {bucket}, got {created:?}"
            );
            // The round trip: the accepted destination is read back through `GetTrail`.
            let read = emulator.call("GetTrail", &json!({"Name": name})).await;
            let trail = read.expect_ok(case)["Trail"].clone();
            assert_eq!(
                trail.get("S3BucketName").and_then(Value::as_str),
                Some(bucket.as_str()),
                "case {case}: expected GetTrail to report {bucket}, got {trail:?}"
            );
            continue;
        }

        let status = if expected == "S3BucketDoesNotExistException" {
            BUCKET_ABSENT_STATUS
        } else {
            referenced::INSUFFICIENT_S3_BUCKET_POLICY_STATUS
        };
        reply.expect_error(case, status, expected);
        if expected == "InsufficientS3BucketPolicyException" {
            assert_eq!(
                reply.message(),
                referenced::insufficient_s3_bucket_policy_message(&bucket),
                "case {case}: expected the refusal to name the bucket whose policy is \
                 insufficient, got {:?}",
                reply.body
            );
        }
        // A refused reference persists no trail.
        let absent = emulator.call("GetTrail", &json!({"Name": name})).await;
        absent.expect_error(
            &format!("{case}: GetTrail after the refusal"),
            BUCKET_ABSENT_STATUS,
            "TrailNotFoundException",
        );
    }
}

/// `CreateTrail` accepts a notification topic only when its access policy lets
/// CloudTrail publish to it.
#[tokio::test]
async fn a_notification_topic_is_accepted_only_when_its_policy_permits_the_publish() {
    let emulator = Emulator::without_provisioned_destinations();

    /// A topic policy granting `principal` the action `action` on the topic, optionally
    /// under `condition`.
    fn topic_policy(
        principal: Value,
        action: &str,
        topic_arn: &str,
        condition: Option<Value>,
    ) -> String {
        let mut statement = json!({
            "Sid": "Grant",
            "Effect": "Allow",
            "Principal": principal,
            "Action": action,
            "Resource": topic_arn,
        });
        if let Some(condition) = condition {
            statement["Condition"] = condition;
        }
        json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
    }

    // (case, the policy the topic carries — `None` for a topic never created, `Some(None)`
    //  for one carrying no policy of its own — the expected error code or "" for success)
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, Option<Option<fn(&str) -> String>>, &str)> = vec![
        (
            "no topic of that name",
            None,
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a topic carrying no policy of its own",
            Some(None),
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a policy granting the publish to another principal",
            Some(Some(|arn: &str| {
                topic_policy(
                    json!({"Service": "events.amazonaws.com"}),
                    "SNS:Publish",
                    arn,
                    None,
                )
            })),
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a policy granting CloudTrail another action on the topic",
            Some(Some(|arn: &str| {
                topic_policy(cloudtrail_principal(), "SNS:Subscribe", arn, None)
            })),
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a policy granting the publish and then denying it",
            Some(Some(|arn: &str| {
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": {"Service": "cloudtrail.amazonaws.com"},
                            "Action": "SNS:Publish",
                            "Resource": arn,
                        },
                        {
                            "Effect": "Deny",
                            "Principal": {"Service": "cloudtrail.amazonaws.com"},
                            "Action": "SNS:Publish",
                            "Resource": arn,
                        },
                    ],
                })
                .to_string()
            })),
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a policy granting CloudTrail the publish",
            Some(Some(|arn: &str| {
                topic_policy(cloudtrail_principal(), "SNS:Publish", arn, None)
            })),
            "",
        ),
        (
            "a policy granting the publish under the account that owns the trail",
            Some(Some(|arn: &str| {
                topic_policy(
                    cloudtrail_principal(),
                    "SNS:Publish",
                    arn,
                    Some(json!({
                        "StringEquals": {"aws:SourceAccount": engine::emulator_account_id()},
                    })),
                )
            })),
            "",
        ),
        (
            "a policy granting the publish for every resource",
            Some(Some(|_: &str| {
                topic_policy(cloudtrail_principal(), "SNS:Publish", "*", None)
            })),
            "",
        ),
    ];

    for (index, (case, policy, expected)) in rows.into_iter().enumerate() {
        let bucket = format!("topic-bucket-{index}");
        emulator.provision_bucket(&bucket, HOME_REGION);
        let topic = format!("notification-topic-{index}");
        let topic_arn = format!("arn:aws:sns:{HOME_REGION}:{}:{topic}", account());
        match policy {
            None => {}
            Some(None) => emulator.provision_topic_with_policy(&topic_arn, None),
            Some(Some(build)) => {
                let document = build(&topic_arn);
                emulator.provision_topic_with_policy(&topic_arn, Some(&document));
            }
        }

        let name = format!("topic-trail-{index}");
        let reply = emulator
            .call(
                "CreateTrail",
                &json!({"Name": name, "S3BucketName": bucket, "SnsTopicName": topic}),
            )
            .await;

        if expected.is_empty() {
            let created = reply.expect_ok(case).clone();
            assert_eq!(
                created.get("SnsTopicARN").and_then(Value::as_str),
                Some(topic_arn.as_str()),
                "case {case}: expected the created trail to report {topic_arn}, got {created:?}"
            );
            let read = emulator.call("GetTrail", &json!({"Name": name})).await;
            let trail = read.expect_ok(case)["Trail"].clone();
            assert_eq!(
                trail.get("SnsTopicARN").and_then(Value::as_str),
                Some(topic_arn.as_str()),
                "case {case}: expected GetTrail to report {topic_arn}, got {trail:?}"
            );
            continue;
        }

        reply.expect_error(case, 403, expected);
        assert_eq!(
            reply.message(),
            referenced::INSUFFICIENT_SNS_TOPIC_POLICY_MESSAGE,
            "case {case}: expected the one fixed topic-policy refusal text, got {:?}",
            reply.body
        );
    }
}

/// `CreateTrail` accepts a `KmsKeyId` only when it resolves to a key of this account and
/// Region, in every form the member documents — reports it back as the fully specified
/// key ARN whichever form was sent, and splits the two refusals on whether this Region's
/// endpoint could have served the identifier at all.
#[tokio::test]
async fn an_encryption_key_is_accepted_only_when_it_resolves_to_a_key() {
    let emulator = Emulator::without_provisioned_destinations();
    let bucket = "encryption-bucket";
    emulator.provision_bucket(bucket, HOME_REGION);

    // One key the account really holds, reachable by every form of identifier.
    let held_id = "11111111-2222-4333-8444-555555555555";
    emulator.provision_kms_key(held_id, HOME_REGION);
    emulator.provision_kms_key("alias/trail-key", HOME_REGION);

    // (case, the KmsKeyId the request carries, the expected error code or "" for success)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a key id naming no key",
            "99999999-8888-4777-8666-555555555555".to_string(),
            "InsufficientEncryptionPolicyException",
        ),
        (
            "a fully specified key ARN naming no key",
            format!(
                "arn:aws:kms:{HOME_REGION}:{}:key/99999999-8888-4777-8666-555555555555",
                account()
            ),
            "InsufficientEncryptionPolicyException",
        ),
        (
            "the all-zeros key ARN",
            format!(
                "arn:aws:kms:{HOME_REGION}:{}:key/00000000-0000-0000-0000-000000000000",
                account()
            ),
            "InsufficientEncryptionPolicyException",
        ),
        (
            "an alias naming no key",
            "alias/no-such-alias".to_string(),
            "InsufficientEncryptionPolicyException",
        ),
        (
            "an alias ARN naming no key",
            format!("arn:aws:kms:{HOME_REGION}:{}:alias/no-such-alias", account()),
            "InsufficientEncryptionPolicyException",
        ),
        (
            "a key ARN of another Region",
            format!("arn:aws:kms:{OTHER_REGION}:{}:key/{held_id}", account()),
            "KmsKeyNotFoundException",
        ),
        (
            "an alias ARN of another Region",
            format!("arn:aws:kms:{OTHER_REGION}:{}:alias/trail-key", account()),
            "KmsKeyNotFoundException",
        ),
        (
            "a key ARN of another account",
            format!("arn:aws:kms:{HOME_REGION}:999999999999:key/{held_id}"),
            "KmsKeyNotFoundException",
        ),
        (
            "a value in no key-id form at all",
            "not-a-key-id".to_string(),
            "KmsKeyNotFoundException",
        ),
        (
            "an ARN that is not a KMS ARN at all",
            "arn:aws:s3:::not-a-key".to_string(),
            "KmsKeyNotFoundException",
        ),
        (
            "a KMS ARN whose resource is neither a key nor an alias",
            format!("arn:aws:kms:{HOME_REGION}:{}:grant/abc", account()),
            "KmsKeyNotFoundException",
        ),
        ("the held key by its key id", held_id.to_string(), ""),
        (
            "the held key by its fully specified ARN",
            format!("arn:aws:kms:{HOME_REGION}:{}:key/{held_id}", account()),
            "",
        ),
        ("a held alias", "alias/trail-key".to_string(), ""),
        (
            "a held alias by its ARN",
            format!("arn:aws:kms:{HOME_REGION}:{}:alias/trail-key", account()),
            "",
        ),
    ];

    for (index, (case, key_id, expected)) in rows.into_iter().enumerate() {
        let name = format!("encryption-trail-{index}");
        let reply = emulator
            .call(
                "CreateTrail",
                &json!({"Name": name, "S3BucketName": bucket, "KmsKeyId": key_id}),
            )
            .await;

        if expected.is_empty() {
            // Whichever form the request wrote, the trail reports the fully specified
            // ARN of the key it resolved to.
            let key_arn = emulator
                .kms_key_arn(HOME_REGION, &key_id)
                .unwrap_or_else(|| panic!("case {case}: the fixture key is held"));
            let created = reply.expect_ok(case).clone();
            assert_eq!(
                created.get("KmsKeyId").and_then(Value::as_str),
                Some(key_arn.as_str()),
                "case {case}: expected the created trail to report {key_arn}, got {created:?}"
            );
            let read = emulator.call("GetTrail", &json!({"Name": name})).await;
            let trail = read.expect_ok(case)["Trail"].clone();
            assert_eq!(
                trail.get("KmsKeyId").and_then(Value::as_str),
                Some(key_arn.as_str()),
                "case {case}: expected GetTrail to report {key_arn}, got {trail:?}"
            );
            continue;
        }

        reply.expect_error(case, 400, expected);
        let named = referenced::fully_specified_key_arn(HOME_REGION, &key_id);
        let wanted = if expected == "KmsKeyNotFoundException" {
            referenced::kms_key_not_found_message(bucket, &named)
        } else {
            referenced::insufficient_encryption_policy_message(bucket, &named)
        };
        assert_eq!(
            reply.message(),
            wanted,
            "case {case}: expected the refusal to name the trail's bucket and the key \
             ARN, got {:?}",
            reply.body
        );
    }
}

/// `CreateTrail` accepts a CloudWatch Logs destination only as the PAIR it is, and only
/// when the log-group half is a log-group ARN of this account and Region naming a log
/// group that exists — each failure carrying the sentence that says which half is wrong.
#[tokio::test]
async fn a_cloudwatch_logs_destination_is_accepted_only_when_the_log_group_exists() {
    let emulator = Emulator::without_provisioned_destinations();
    let bucket = "log-group-bucket";
    emulator.provision_bucket(bucket, HOME_REGION);

    let held = format!(
        "arn:aws:logs:{HOME_REGION}:{}:log-group:/aws/cloudtrail/held:*",
        account()
    );
    emulator.provision_log_group(&held);
    let role = format!("arn:aws:iam::{}:role/CloudTrail_CloudWatchLogs_Role", account());

    /// What a row expects of the pair it sends.
    enum Expected {
        /// The pair is accepted and read back.
        Accepted,
        /// Refused with this error code and this refusal text.
        Refused(&'static str, &'static str),
    }
    use Expected::{Accepted, Refused};

    // (case, the CloudWatchLogsLogGroupArn the request carries, the role half, expected)
    let rows: Vec<(&str, Option<String>, bool, Expected)> = vec![
        (
            "an ARN naming no log group",
            Some(format!(
                "arn:aws:logs:{HOME_REGION}:{}:log-group:never-created:*",
                account()
            )),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::LOG_GROUP_ABSENT_MESSAGE,
            ),
        ),
        (
            "a bare log group name rather than an ARN",
            Some("/aws/cloudtrail/held".to_string()),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::UNREADABLE_LOG_GROUP_ARN_MESSAGE,
            ),
        ),
        (
            "a log-group ARN of another Region",
            Some(format!(
                "arn:aws:logs:{OTHER_REGION}:{}:log-group:/aws/cloudtrail/held:*",
                account()
            )),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::UNREADABLE_LOG_GROUP_ARN_MESSAGE,
            ),
        ),
        (
            "a log-group ARN of another account",
            Some(format!(
                "arn:aws:logs:{HOME_REGION}:999999999999:log-group:/aws/cloudtrail/held:*"
            )),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::UNREADABLE_LOG_GROUP_ARN_MESSAGE,
            ),
        ),
        (
            "an ARN of another service",
            Some(format!("arn:aws:sns:{HOME_REGION}:{}:a-topic", account())),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::UNREADABLE_LOG_GROUP_ARN_MESSAGE,
            ),
        ),
        (
            "a logs ARN that addresses no log group",
            Some(format!(
                "arn:aws:logs:{HOME_REGION}:{}:destination:/aws/cloudtrail/held",
                account()
            )),
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::UNREADABLE_LOG_GROUP_ARN_MESSAGE,
            ),
        ),
        (
            "the held log group with no role to write it with",
            Some(held.clone()),
            false,
            Refused(
                "InvalidCloudWatchLogsRoleArnException",
                referenced::LOG_GROUP_AND_ROLE_REQUIRED_MESSAGE,
            ),
        ),
        (
            "a role with no log group to write to",
            None,
            true,
            Refused(
                "InvalidCloudWatchLogsLogGroupArnException",
                referenced::LOG_GROUP_AND_ROLE_REQUIRED_MESSAGE,
            ),
        ),
        (
            "neither half of the pair",
            None,
            false,
            Accepted,
        ),
        (
            "the held log group, with the wildcard suffix",
            Some(held.clone()),
            true,
            Accepted,
        ),
        (
            "the held log group, without the wildcard suffix",
            Some(held.trim_end_matches(":*").to_string()),
            true,
            Accepted,
        ),
    ];

    for (index, (case, arn, with_role, expected)) in rows.into_iter().enumerate() {
        let name = format!("log-group-trail-{index}");
        let mut request = json!({"Name": name, "S3BucketName": bucket});
        if let Some(arn) = &arn {
            request["CloudWatchLogsLogGroupArn"] = json!(arn);
        }
        if with_role {
            request["CloudWatchLogsRoleArn"] = json!(role);
        }
        let reply = emulator.call("CreateTrail", &request).await;

        match expected {
            Expected::Accepted => {
                let created = reply.expect_ok(case).clone();
                assert_eq!(
                    created
                        .get("CloudWatchLogsLogGroupArn")
                        .and_then(Value::as_str),
                    arn.as_deref(),
                    "case {case}: expected the created trail to report {arn:?}, got \
                     {created:?}"
                );
            }
            Expected::Refused(code, message) => {
                reply.expect_error(case, 400, code);
                assert_eq!(
                    reply.message(),
                    message,
                    "case {case}: expected the refusal that says which half is wrong, \
                     got {:?}",
                    reply.body
                );
            }
        }
    }
}

/// `UpdateTrail` holds every reference to the same rule `CreateTrail` does, and a refused
/// update leaves the trail configured exactly as it was.
#[tokio::test]
async fn an_update_holds_every_reference_to_the_same_rule_and_changes_nothing_when_refused() {
    let emulator = Emulator::without_provisioned_destinations();
    const NAME: &str = "update-references";
    let held_bucket = "update-held-bucket";
    let held_key = "22222222-3333-4444-8555-666666666666";
    let held_topic_arn = format!("arn:aws:sns:{HOME_REGION}:{}:update-held-topic", account());
    let held_log_group = format!(
        "arn:aws:logs:{HOME_REGION}:{}:log-group:/aws/cloudtrail/update-held:*",
        account()
    );

    emulator.provision_bucket(held_bucket, HOME_REGION);
    emulator.provision_kms_key(held_key, HOME_REGION);
    emulator.provision_topic(&held_topic_arn);
    emulator.provision_log_group(&held_log_group);
    emulator
        .call(
            "CreateTrail",
            &json!({"Name": NAME, "S3BucketName": held_bucket}),
        )
        .await
        .expect_ok("CreateTrail against the held destination");

    // A bucket the account holds but never policied, and one whose policy grants the
    // write only under a key prefix the trail does not deliver under.
    emulator.provision_bucket_with_policy("update-unpolicied", HOME_REGION, None);
    emulator.provision_bucket_with_policy(
        "update-wrong-prefix",
        HOME_REGION,
        Some(&bucket_policy(
            cloudtrail_principal(),
            "s3:PutObject",
            &format!("arn:aws:s3:::update-wrong-prefix/elsewhere/AWSLogs/{}/*", account()),
            Some(acl_condition()),
        )),
    );
    emulator.provision_topic_with_policy("arn:aws:sns:us-east-1:000000000000:update-unpolicied", None);

    // (case, the members the update carries besides Name, (status, error code))
    let rows: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a destination no bucket backs",
            json!({"S3BucketName": "update-never-created"}),
            BUCKET_ABSENT_STATUS,
            "S3BucketDoesNotExistException",
        ),
        (
            "a destination whose bucket carries no policy",
            json!({"S3BucketName": "update-unpolicied"}),
            referenced::INSUFFICIENT_S3_BUCKET_POLICY_STATUS,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a destination whose policy grants the write elsewhere",
            json!({"S3BucketName": "update-wrong-prefix"}),
            referenced::INSUFFICIENT_S3_BUCKET_POLICY_STATUS,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a key prefix the held destination's policy does not cover",
            json!({"S3BucketName": "update-wrong-prefix", "S3KeyPrefix": "logs"}),
            referenced::INSUFFICIENT_S3_BUCKET_POLICY_STATUS,
            "InsufficientS3BucketPolicyException",
        ),
        (
            "a notification topic the account does not hold",
            json!({"SnsTopicName": "update-never-created-topic"}),
            403,
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a notification topic carrying no policy of its own",
            json!({"SnsTopicName": "update-unpolicied"}),
            403,
            "InsufficientSnsTopicPolicyException",
        ),
        (
            "a key identifier of this Region naming no key",
            json!({"KmsKeyId": "77777777-6666-4555-8444-333333333333"}),
            400,
            "InsufficientEncryptionPolicyException",
        ),
        (
            "a key identifier that is not a KMS identifier at all",
            json!({"KmsKeyId": "arn:aws:s3:::not-a-key"}),
            400,
            "KmsKeyNotFoundException",
        ),
        (
            "a log-group ARN naming no log group",
            json!({
                "CloudWatchLogsLogGroupArn":
                    format!("arn:aws:logs:{HOME_REGION}:{}:log-group:update-absent:*", account()),
                "CloudWatchLogsRoleArn":
                    format!("arn:aws:iam::{}:role/CloudTrail_CloudWatchLogs_Role", account()),
            }),
            400,
            "InvalidCloudWatchLogsLogGroupArnException",
        ),
        (
            "a log group with no role to write it with",
            json!({"CloudWatchLogsLogGroupArn": held_log_group.clone()}),
            400,
            "InvalidCloudWatchLogsRoleArnException",
        ),
        (
            "a role with no log group to write to",
            json!({
                "CloudWatchLogsRoleArn":
                    format!("arn:aws:iam::{}:role/CloudTrail_CloudWatchLogs_Role", account()),
            }),
            400,
            "InvalidCloudWatchLogsLogGroupArnException",
        ),
    ];

    let before = emulator
        .call("GetTrail", &json!({"Name": NAME}))
        .await
        .expect_ok("the trail before any refused update")["Trail"]
        .clone();

    for (case, members, status, code) in rows {
        let mut request = json!({"Name": NAME});
        for (member, value) in members.as_object().expect("the row is an object") {
            request[member] = value.clone();
        }
        let reply = emulator.call("UpdateTrail", &request).await;
        reply.expect_error(case, status, code);

        let after = emulator.call("GetTrail", &json!({"Name": NAME})).await;
        assert_eq!(
            &after.expect_ok(case)["Trail"],
            &before,
            "case {case}: a refused update must leave the trail configured as it was, got \
             {:?}",
            after.body
        );
    }

    // Every reference the account really holds is accepted, in one update, and read back.
    let updated = emulator
        .call(
            "UpdateTrail",
            &json!({
                "Name": NAME,
                "SnsTopicName": held_topic_arn,
                "KmsKeyId": held_key,
                "CloudWatchLogsLogGroupArn": held_log_group,
                "CloudWatchLogsRoleArn":
                    format!("arn:aws:iam::{}:role/CloudTrail_CloudWatchLogs_Role", account()),
            }),
        )
        .await;
    let updated = updated.expect_ok("every held reference at once").clone();
    // The key is reported as the fully specified ARN of the key the bare id resolved to.
    let held_key_arn = emulator
        .kms_key_arn(HOME_REGION, held_key)
        .expect("the fixture key is held");
    for (member, want) in [
        ("SnsTopicARN", held_topic_arn.as_str()),
        ("KmsKeyId", held_key_arn.as_str()),
        ("CloudWatchLogsLogGroupArn", held_log_group.as_str()),
    ] {
        assert_eq!(
            updated.get(member).and_then(Value::as_str),
            Some(want),
            "case every held reference at once: expected {member} = {want}, got {updated:?}"
        );
    }
    let read = emulator.call("GetTrail", &json!({"Name": NAME})).await;
    let trail = read.expect_ok("the updated trail")["Trail"].clone();
    for (member, want) in updated.as_object().expect("an object") {
        assert_eq!(
            &trail[member], want,
            "case the updated trail: GetTrail reports {member} = {}, where UpdateTrail \
             reported {want}",
            trail[member]
        );
    }
}

/// Every advanced event selector states which CATEGORY of events it selects, and the
/// fields the model admits only `Equals` for carry no other operator — on every operation
/// that stores an advanced selector set.
#[tokio::test]
async fn an_advanced_event_selector_states_its_event_category_and_only_the_operators_its_fields_take()
{
    use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state::{
        EQUALS_ONLY_SELECTOR_FIELDS, EVENT_CATEGORY_SELECTOR_FIELD,
    };

    let emulator = Emulator::new();
    emulator
        .call(
            "CreateTrail",
            &json!({"Name": "selector-rules", "S3BucketName": "selector-rules-bucket"}),
        )
        .await
        .expect_ok("CreateTrail for the selector cases");

    // (case, the AdvancedEventSelectors the request carries, accepted?)
    let mut rows: Vec<(String, Value, bool)> = vec![
        (
            "a selector whose only field selector is readOnly".to_string(),
            json!([{
                "Name": "no-event-category",
                "FieldSelectors": [{"Field": "readOnly", "Equals": ["false"]}],
            }]),
            false,
        ),
        (
            "a selector that names resources but no event category".to_string(),
            json!([{
                "Name": "resources-only",
                "FieldSelectors": [
                    {"Field": "resources.type", "Equals": ["AWS::S3::Object"]},
                    {"Field": "resources.ARN", "StartsWith": ["arn:aws:s3:::b/"]},
                ],
            }]),
            false,
        ),
        (
            "one selector of two stating no event category".to_string(),
            json!([
                {
                    "Name": "complete",
                    "FieldSelectors": [{"Field": "eventCategory", "Equals": ["Management"]}],
                },
                {
                    "Name": "incomplete",
                    "FieldSelectors": [{"Field": "eventSource", "NotEquals": ["kms.amazonaws.com"]}],
                },
            ]),
            false,
        ),
        (
            "a selector whose event category is spelt in another case".to_string(),
            json!([{
                "Name": "mis-cased",
                "FieldSelectors": [{"Field": "EventCategory", "Equals": ["Management"]}],
            }]),
            false,
        ),
        (
            "a selector stating its event category".to_string(),
            json!([{
                "Name": "management",
                "FieldSelectors": [{"Field": "eventCategory", "Equals": ["Management"]}],
            }]),
            true,
        ),
        (
            "a selector stating its event category beside other fields".to_string(),
            json!([{
                "Name": "data",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": ["Data"]},
                    {"Field": "resources.type", "Equals": ["AWS::S3::Object"]},
                    {"Field": "resources.ARN", "NotStartsWith": ["arn:aws:s3:::b/"]},
                ],
            }]),
            true,
        ),
    ];
    // One row per field the model admits `Equals` for and nothing else, each stating a
    // condition under a different operator — taken from the model's own catalog rather
    // than written out here.
    for field in EQUALS_ONLY_SELECTOR_FIELDS {
        for operator in ["StartsWith", "EndsWith", "NotEquals", "NotStartsWith", "NotEndsWith"] {
            let mut field_selectors =
                vec![json!({"Field": EVENT_CATEGORY_SELECTOR_FIELD, "Equals": ["Management"]})];
            field_selectors.push(json!({"Field": field, operator: ["Management"]}));
            rows.push((
                format!("the {field} field under {operator}"),
                json!([{"Name": "operator-row", "FieldSelectors": field_selectors}]),
                false,
            ));
        }
    }

    for (case, selectors, accepted) in rows {
        let reply = emulator
            .call(
                "PutEventSelectors",
                &json!({"TrailName": "selector-rules", "AdvancedEventSelectors": selectors}),
            )
            .await;
        if accepted {
            let applied = reply.expect_ok(&case).clone();
            assert_eq!(
                applied.get("AdvancedEventSelectors"),
                Some(&selectors),
                "case {case}: expected the selector set to be stored as given, got {applied:?}"
            );
        } else {
            reply.expect_error(&case, 400, "InvalidEventSelectorsException");
            assert!(
                !reply.message().is_empty(),
                "case {case}: a refusal must say what was wrong with the request, got {reply:?}"
            );
        }

        // The very same rule holds for an event data store's selector set: one reading,
        // one answer.
        let store = emulator
            .call(
                "CreateEventDataStore",
                &json!({
                    "Name": format!("eds-{}", case.replace(' ', "-")),
                    "AdvancedEventSelectors": selectors,
                }),
            )
            .await;
        if accepted {
            store.expect_ok(&format!("CreateEventDataStore with {case}"));
        } else {
            store.expect_error(
                &format!("CreateEventDataStore with {case}"),
                400,
                "InvalidEventSelectorsException",
            );
        }
    }
}
