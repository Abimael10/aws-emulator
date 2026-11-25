// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a trail's referenced resource is rejected with, in whose words, and
//! under which HTTP status.
//!
//! A trail is a delivery pipeline, so `CreateTrail` and `UpdateTrail` resolve every
//! resource its settings name before they accept it. The rule this suite pins is not
//! THAT a defective reference is refused — `referenced_resource_validation` covers that
//! — but how the refusal is reported, because that is all a caller has to act on:
//!
//! * the encryption key: an identifier this Region's endpoint could have served that
//!   resolves to nothing is a key-or-key-policy problem
//!   (`InsufficientEncryptionPolicyException`), while one it could never serve — a value
//!   in no key-identifier form, or an ARN of another Region or account — is a key that is
//!   not there (`KmsKeyNotFoundException`); each names the trail's own destination bucket
//!   and the identifier as a fully specified ARN;
//! * the CloudWatch Logs destination: three fixed sentences, one per way the pair is
//!   wrong — the log group does not exist, the ARN cannot be read, or only one half of
//!   the pair was configured — reported as the error of the half at fault;
//! * the notification topic: one fixed sentence, naming nothing of the request;
//! * the destination bucket's policy: HTTP 400, whatever the model's `@httpError` says,
//!   on every operation that resolves the destination;
//! * and the key a trail KEEPS: the fully specified key ARN, whichever of the four forms
//!   the `KmsKeyId` member accepts was sent.
//!
//! Every table spans its class rather than restating one row: each form of the member,
//! each way the reference fails, and the row where it holds. The expected sentences are
//! written out here rather than read from the handler, so this suite pins the wording.

use serde_json::{json, Value};

use super::emulator::{Emulator, HOME_REGION};

/// The Region no case in this suite calls in, for a reference pinned to another Region.
const OTHER_REGION: &str = "eu-west-1";

/// This account's id, as every ARN in this suite carries it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// An IAM role ARN of this account, the role half of a CloudWatch Logs configuration.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/CloudTrail_CloudWatchLogs_Role", account())
}

/// A trail delivering to a bucket whose policy permits the delivery — what every row
/// below starts from — and the bucket it delivers to.
async fn trail_on_a_writable_destination(emulator: &Emulator, name: &str) -> String {
    let bucket = format!("{name}-destination");
    emulator.provision_bucket(&bucket, HOME_REGION);
    emulator
        .call(
            "CreateTrail",
            &json!({"Name": name, "S3BucketName": bucket}),
        )
        .await
        .expect_ok(&format!("CreateTrail {name} against {bucket}"));
    bucket
}

/// The `KmsKeyId` a refused call left on the trail, read back through `GetTrail`.
async fn stored_key(emulator: &Emulator, name: &str, case: &str) -> Value {
    let read = emulator.call("GetTrail", &json!({"Name": name})).await;
    read.expect_ok(case)["Trail"]
        .get("KmsKeyId")
        .cloned()
        .unwrap_or(Value::Null)
}

/// A `KmsKeyId` that names no key is refused as a KEY-OR-POLICY problem when this
/// Region's endpoint could have served the identifier, and as a key that is NOT THERE
/// when it could not — each in the sentence that names the trail's bucket and the
/// identifier as a fully specified ARN, and neither leaving a key on the trail.
#[tokio::test]
async fn an_unresolvable_key_identifier_is_refused_as_the_class_its_form_decides() {
    let emulator = Emulator::without_provisioned_destinations();
    const NAME: &str = "key-class-trail";
    let bucket = trail_on_a_writable_destination(&emulator, NAME).await;

    // One key the account really holds, so a row about another Region's ARN is about the
    // Region rather than about a key nobody created.
    let held_id = "33333333-4444-4555-8666-777777777777";
    emulator.provision_kms_key(held_id, HOME_REGION);

    // (case, the KmsKeyId the request carries, whether this Region could have served it)
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "a fully specified key ARN of this Region naming no key",
            format!(
                "arn:aws:kms:{HOME_REGION}:{}:key/88888888-9999-4000-8111-222222222222",
                account()
            ),
            true,
        ),
        (
            "a bare key id naming no key",
            "88888888-9999-4000-8111-222222222222".to_string(),
            true,
        ),
        ("an alias naming no key", "alias/absent-key".to_string(), true),
        (
            "an alias ARN of this Region naming no key",
            format!("arn:aws:kms:{HOME_REGION}:{}:alias/absent-key", account()),
            true,
        ),
        (
            "a multi-Region key id naming no key",
            "mrk-1234567890abcdef1234567890abcdef".to_string(),
            true,
        ),
        (
            "a key ARN of another Region",
            format!("arn:aws:kms:{OTHER_REGION}:{}:key/{held_id}", account()),
            false,
        ),
        (
            "an alias ARN of another Region",
            format!("arn:aws:kms:{OTHER_REGION}:{}:alias/absent-key", account()),
            false,
        ),
        (
            "a key ARN of another account",
            format!("arn:aws:kms:{HOME_REGION}:999999999999:key/{held_id}"),
            false,
        ),
        ("a value in no key-id form", "not-a-key-id".to_string(), false),
        (
            "an ARN of another service entirely",
            "arn:aws:s3:::not-a-key".to_string(),
            false,
        ),
    ];

    for (index, (case, key_id, servable_here)) in rows.into_iter().enumerate() {
        // The ARN the refusal names the identifier by: an identifier that is already an
        // ARN as given, a bare `alias/<name>` as this Region's alias ARN, anything else
        // as this Region's key ARN.
        let named = if key_id.starts_with("arn:") {
            key_id.clone()
        } else {
            format!(
                "arn:aws:kms:{HOME_REGION}:{}:{}",
                account(),
                if key_id.starts_with("alias/") {
                    key_id.clone()
                } else {
                    format!("key/{key_id}")
                }
            )
        };
        let (code, message) = if servable_here {
            (
                "InsufficientEncryptionPolicyException",
                format!("Insufficient permissions to access S3 bucket {bucket} or KMS key {named}."),
            )
        } else {
            (
                "KmsKeyNotFoundException",
                format!(
                    "KMS key ID {named} does not exist, or s3 bucket {bucket} and key are \
                     not in the same region."
                ),
            )
        };

        for operation in ["CreateTrail", "UpdateTrail"] {
            let case = format!("{operation} with {case}");
            let request = if operation == "CreateTrail" {
                json!({
                    "Name": format!("{NAME}-create-{index}"),
                    "S3BucketName": bucket,
                    "KmsKeyId": key_id,
                })
            } else {
                json!({"Name": NAME, "KmsKeyId": key_id})
            };
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, 400, code);
            assert_eq!(
                reply.message(),
                message,
                "case {case}: expected the refusal to name the trail's bucket and the \
                 key ARN, got {:?}",
                reply.body
            );
        }

        // A refused reference leaves the trail with no key at all.
        assert_eq!(
            stored_key(&emulator, NAME, case).await,
            Value::Null,
            "case {case}: a refused update must leave the trail without a KmsKeyId"
        );
    }
}

/// A trail REPORTS its encryption key as the fully specified ARN of the key the
/// identifier resolved to — on the create or update that stored it and on every later
/// read — whichever of the four forms the member accepts was sent.
#[tokio::test]
async fn a_trail_reports_its_encryption_key_as_a_fully_specified_key_arn() {
    let emulator = Emulator::without_provisioned_destinations();
    let bucket = "key-form-bucket";
    emulator.provision_bucket(bucket, HOME_REGION);

    let key_id = "44444444-5555-4666-8777-888888888888";
    emulator.provision_kms_key(key_id, HOME_REGION);
    emulator.provision_kms_key("alias/form-key", HOME_REGION);
    let key_arn = emulator
        .kms_key_arn(HOME_REGION, key_id)
        .expect("the fixture key is held");
    let alias_key_arn = emulator
        .kms_key_arn(HOME_REGION, "alias/form-key")
        .expect("the fixture alias names a held key");

    // (case, the KmsKeyId the request carries, the fully specified ARN it resolves to)
    let rows: Vec<(&str, String, &str)> = vec![
        ("a globally unique identifier", key_id.to_string(), &key_arn),
        (
            "a fully specified ARN to a key",
            format!("arn:aws:kms:{HOME_REGION}:{}:key/{key_id}", account()),
            &key_arn,
        ),
        (
            "an alias name prefixed by alias/",
            "alias/form-key".to_string(),
            &alias_key_arn,
        ),
        (
            "a fully specified ARN to an alias",
            format!("arn:aws:kms:{HOME_REGION}:{}:alias/form-key", account()),
            &alias_key_arn,
        ),
    ];

    for (index, (case, given, expected)) in rows.into_iter().enumerate() {
        // The create stores it, and the read reports what the create reported.
        let created_name = format!("key-form-create-{index}");
        let created = emulator
            .call(
                "CreateTrail",
                &json!({"Name": created_name, "S3BucketName": bucket, "KmsKeyId": given}),
            )
            .await;
        let created = created.expect_ok(case).clone();
        assert_eq!(
            created.get("KmsKeyId").and_then(Value::as_str),
            Some(expected),
            "case CreateTrail with {case}: expected {expected}, got {created:?}"
        );
        let read = emulator
            .call("GetTrail", &json!({"Name": created_name}))
            .await;
        let trail = read.expect_ok(case)["Trail"].clone();
        assert_eq!(
            trail.get("KmsKeyId").and_then(Value::as_str),
            Some(expected),
            "case GetTrail after CreateTrail with {case}: expected {expected}, got \
             {trail:?}"
        );

        // And so does the update, on a trail created without a key.
        let updated_name = format!("key-form-update-{index}");
        emulator
            .call(
                "CreateTrail",
                &json!({"Name": updated_name, "S3BucketName": bucket}),
            )
            .await
            .expect_ok(case);
        let updated = emulator
            .call(
                "UpdateTrail",
                &json!({"Name": updated_name, "KmsKeyId": given}),
            )
            .await;
        let updated = updated.expect_ok(case).clone();
        assert_eq!(
            updated.get("KmsKeyId").and_then(Value::as_str),
            Some(expected),
            "case UpdateTrail with {case}: expected {expected}, got {updated:?}"
        );
        let read = emulator
            .call("GetTrail", &json!({"Name": updated_name}))
            .await;
        let trail = read.expect_ok(case)["Trail"].clone();
        assert_eq!(
            trail.get("KmsKeyId").and_then(Value::as_str),
            Some(expected),
            "case GetTrail after UpdateTrail with {case}: expected {expected}, got \
             {trail:?}"
        );
    }
}

/// The CloudWatch Logs configuration is refused in the sentence that says which half is
/// wrong, reported as the modelled error of that half — and accepted as the pair it is.
#[tokio::test]
async fn a_cloudwatch_logs_refusal_says_which_half_of_the_pair_is_wrong() {
    let emulator = Emulator::without_provisioned_destinations();
    const NAME: &str = "cwl-class-trail";
    let bucket = trail_on_a_writable_destination(&emulator, NAME).await;

    let held = format!(
        "arn:aws:logs:{HOME_REGION}:{}:log-group:/aws/cloudtrail/cwl-class:*",
        account()
    );
    emulator.provision_log_group(&held);

    /// One row of the table below: a case name, the log-group half and the role half the
    /// request carries, and the error code and text the pair is refused with.
    type Row = (&'static str, Option<String>, Option<String>, &'static str, &'static str);

    // (case, the log-group half, the role half, the expected code, the expected text)
    let rows: Vec<Row> = vec![
        (
            "a log-group ARN of this account and Region naming no log group",
            Some(format!(
                "arn:aws:logs:{HOME_REGION}:{}:log-group:never-created:*",
                account()
            )),
            Some(role_arn()),
            "InvalidCloudWatchLogsLogGroupArnException",
            "You must specify an existing log group.",
        ),
        (
            "a log-group half that is no ARN at all",
            Some("not-an-arn".to_string()),
            Some(role_arn()),
            "InvalidCloudWatchLogsLogGroupArnException",
            "CloudTrail cannot validate the specified log group ARN.",
        ),
        (
            "a log-group ARN of another Region",
            Some(format!(
                "arn:aws:logs:{OTHER_REGION}:{}:log-group:/aws/cloudtrail/cwl-class:*",
                account()
            )),
            Some(role_arn()),
            "InvalidCloudWatchLogsLogGroupArnException",
            "CloudTrail cannot validate the specified log group ARN.",
        ),
        (
            "a role half that is no ARN at all, beside a log group that exists",
            Some(held.clone()),
            Some("not-an-arn".to_string()),
            "InvalidCloudWatchLogsRoleArnException",
            "CloudTrail cannot validate the specified role ARN.",
        ),
        (
            "a role half naming an IAM user rather than a role",
            Some(held.clone()),
            Some(format!("arn:aws:iam::{}:user/not-a-role", account())),
            "InvalidCloudWatchLogsRoleArnException",
            "CloudTrail cannot validate the specified role ARN.",
        ),
        (
            "a role half of another account",
            Some(held.clone()),
            Some("arn:aws:iam::999999999999:role/Elsewhere".to_string()),
            "InvalidCloudWatchLogsRoleArnException",
            "CloudTrail cannot validate the specified role ARN.",
        ),
        (
            "a role that is at fault beside a log group that is too",
            Some("not-an-arn".to_string()),
            Some("not-an-arn".to_string()),
            "InvalidCloudWatchLogsRoleArnException",
            "CloudTrail cannot validate the specified role ARN.",
        ),
        (
            "a log group with no role to write it with",
            Some(held.clone()),
            None,
            "InvalidCloudWatchLogsRoleArnException",
            "You must specify a log group and a role ARN.",
        ),
        (
            "a role with no log group to write to",
            None,
            Some(role_arn()),
            "InvalidCloudWatchLogsLogGroupArnException",
            "You must specify a log group and a role ARN.",
        ),
    ];

    for (index, (case, log_group, role, code, message)) in rows.into_iter().enumerate() {
        for operation in ["CreateTrail", "UpdateTrail"] {
            let case = format!("{operation} with {case}");
            let mut request = if operation == "CreateTrail" {
                json!({"Name": format!("{NAME}-create-{index}"), "S3BucketName": bucket})
            } else {
                json!({"Name": NAME})
            };
            if let Some(arn) = &log_group {
                request["CloudWatchLogsLogGroupArn"] = json!(arn);
            }
            if let Some(arn) = &role {
                request["CloudWatchLogsRoleArn"] = json!(arn);
            }
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, 400, code);
            assert_eq!(
                reply.message(),
                message,
                "case {case}: expected the sentence that says which half is wrong, got \
                 {:?}",
                reply.body
            );
        }
    }

    // The pair the account really holds is accepted and read back, on the same trail
    // every row above failed to configure.
    let updated = emulator
        .call(
            "UpdateTrail",
            &json!({
                "Name": NAME,
                "CloudWatchLogsLogGroupArn": held,
                "CloudWatchLogsRoleArn": role_arn(),
            }),
        )
        .await;
    let updated = updated.expect_ok("the held pair at once").clone();
    assert_eq!(
        updated
            .get("CloudWatchLogsLogGroupArn")
            .and_then(Value::as_str),
        Some(held.as_str()),
        "case the held pair at once: expected {held}, got {updated:?}"
    );
    let read = emulator.call("GetTrail", &json!({"Name": NAME})).await;
    let trail = read.expect_ok("the configured trail")["Trail"].clone();
    assert_eq!(
        trail
            .get("CloudWatchLogsLogGroupArn")
            .and_then(Value::as_str),
        Some(held.as_str()),
        "case the configured trail: expected GetTrail to report {held}, got {trail:?}"
    );

    // And the pair rule is about the REQUEST, not about what the trail already carries:
    // a request naming one half alone is refused on a trail configured with both.
    for (case, members, code) in [
        (
            "an update naming only the log group of a configured pair",
            json!({"CloudWatchLogsLogGroupArn": held.clone()}),
            "InvalidCloudWatchLogsRoleArnException",
        ),
        (
            "an update naming only the role of a configured pair",
            json!({"CloudWatchLogsRoleArn": role_arn()}),
            "InvalidCloudWatchLogsLogGroupArnException",
        ),
    ] {
        let mut request = json!({"Name": NAME});
        for (member, value) in members.as_object().expect("the row is an object") {
            request[member] = value.clone();
        }
        let reply = emulator.call("UpdateTrail", &request).await;
        reply.expect_error(case, 400, code);
        assert_eq!(
            reply.message(),
            "You must specify a log group and a role ARN.",
            "case {case}: expected the sentence that states the pair rule, got {:?}",
            reply.body
        );
    }
}

/// A notification topic CloudTrail may not publish to is refused in ONE fixed sentence,
/// naming nothing of the request — and a topic NAME that is no topic name is the other,
/// separate refusal, which does name the value.
#[tokio::test]
async fn a_topic_refusal_is_one_fixed_sentence_whichever_way_the_topic_fails() {
    use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state;

    let emulator = Emulator::without_provisioned_destinations();
    const NAME: &str = "topic-class-trail";
    let bucket = trail_on_a_writable_destination(&emulator, NAME).await;

    /// The access policy Amazon SNS reports for a topic its owner never policied, and
    /// the one that grants another service the publish: neither lets CloudTrail publish.
    fn owner_only_policy(topic_arn: &str) -> String {
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "__default_statement_ID",
                "Effect": "Allow",
                "Principal": {"AWS": "*"},
                "Action": "SNS:Publish",
                "Resource": topic_arn,
                "Condition": {
                    "StringEquals": {"AWS:SourceOwner": engine::emulator_account_id()},
                },
            }],
        })
        .to_string()
    }

    // (case, the policy the topic carries — `None` for a topic never created)
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, Option<Option<fn(&str) -> String>>)> = vec![
        ("a topic the account does not hold", None),
        ("a topic carrying no policy of its own", Some(None)),
        (
            "a topic whose policy grants only its owner the publish",
            Some(Some(owner_only_policy as fn(&str) -> String)),
        ),
    ];

    for (index, (case, policy)) in rows.into_iter().enumerate() {
        let topic = format!("topic-class-{index}");
        let topic_arn = format!("arn:aws:sns:{HOME_REGION}:{}:{topic}", account());
        match policy {
            None => {}
            Some(None) => emulator.provision_topic_with_policy(&topic_arn, None),
            Some(Some(build)) => {
                emulator.provision_topic_with_policy(&topic_arn, Some(&build(&topic_arn)));
            }
        }

        for operation in ["CreateTrail", "UpdateTrail"] {
            let case = format!("{operation} with {case}");
            let request = if operation == "CreateTrail" {
                json!({
                    "Name": format!("{NAME}-create-{index}"),
                    "S3BucketName": bucket,
                    "SnsTopicName": topic,
                })
            } else {
                json!({"Name": NAME, "SnsTopicName": topic})
            };
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, 403, "InsufficientSnsTopicPolicyException");
            assert_eq!(
                reply.message(),
                "SNS Topic does not exist or the topic policy is incorrect!",
                "case {case}: expected the one fixed topic-policy sentence, got {:?}",
                reply.body
            );
        }
    }

    // A value that is no topic name at all is the separate refusal, which names it. The
    // name rule is Amazon SNS's — up to 256 characters of letters, digits, `_` and `-`
    // (`CreateTopicInput$Name`) — so a value outside it names no topic CloudTrail could
    // be notified through, whichever way it leaves the rule.
    let too_long = "t".repeat(state::SNS_TOPIC_NAME_MAX_LEN + 1);
    let rows: Vec<(&str, String)> = vec![
        ("a topic name one character over the documented maximum", too_long),
        ("a topic name carrying a space", "not a topic".to_string()),
        ("a topic name carrying a slash", "topics/mine".to_string()),
        ("a topic name carrying a period", "my.topic".to_string()),
        (
            "an ARN of another service in the topic member",
            format!("arn:aws:s3:::{}-bucket", account()),
        ),
    ];
    for (case, value) in rows {
        for operation in ["CreateTrail", "UpdateTrail"] {
            let case = format!("{operation} with {case}");
            let request = if operation == "CreateTrail" {
                json!({
                    "Name": format!("{NAME}-name-{}", value.len()),
                    "S3BucketName": bucket,
                    "SnsTopicName": value,
                })
            } else {
                json!({"Name": NAME, "SnsTopicName": value})
            };
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, 400, "InvalidSnsTopicNameException");
            assert_eq!(
                reply.message(),
                format!("Invalid SNS topic name: {value} is found!"),
                "case {case}: expected the sentence that names the value, got {:?}",
                reply.body
            );
        }
    }

    // A topic name at the documented maximum IS a topic name, so it is resolved rather
    // than refused on its form: the refusal it takes is the policy one.
    let at_bound = "t".repeat(state::SNS_TOPIC_NAME_MAX_LEN);
    let reply = emulator
        .call("UpdateTrail", &json!({"Name": NAME, "SnsTopicName": at_bound}))
        .await;
    reply.expect_error(
        "a topic name at the documented maximum",
        403,
        "InsufficientSnsTopicPolicyException",
    );

    // The remedy: once the topic's policy grants CloudTrail the publish, the same call
    // holds and the trail reports the topic's ARN.
    let topic = "topic-class-remedied";
    let topic_arn = format!("arn:aws:sns:{HOME_REGION}:{}:{topic}", account());
    emulator.provision_topic(&topic_arn);
    let updated = emulator
        .call("UpdateTrail", &json!({"Name": NAME, "SnsTopicName": topic}))
        .await;
    assert_eq!(
        updated
            .expect_ok("the remedied topic")
            .get("SnsTopicARN")
            .and_then(Value::as_str),
        Some(topic_arn.as_str()),
        "case the remedied topic: expected {topic_arn}, got {:?}",
        updated.body
    );
}

/// An insufficient destination-bucket policy is answered HTTP 400 — the status a client
/// branches on before the payload — on every operation that resolves the destination,
/// and the identical call holds once the delivery policy is attached.
#[tokio::test]
async fn an_insufficient_bucket_policy_is_refused_with_a_bad_request_status() {
    /// The call `operation` makes on the trail `trail` and the bucket `bucket`: the two
    /// trail writes designate the destination, and the Insights write is judged against
    /// the destination the trail already carries.
    fn request(operation: &str, trail: &str, bucket: &str) -> Value {
        if operation == "PutInsightSelectors" {
            return json!({
                "TrailName": trail,
                "InsightSelectors": [{"InsightType": "ApiCallRateInsight"}],
            });
        }
        json!({"Name": trail, "S3BucketName": bucket})
    }

    for operation in ["CreateTrail", "UpdateTrail", "PutInsightSelectors"] {
        let emulator = Emulator::without_provisioned_destinations();
        let trail = "bucket-policy-status";
        // `CreateTrail` is the call under test for its own row, so that row starts with
        // no trail at all; the other two need the trail to exist first, on a destination
        // that was writable then.
        let unpolicied = format!("{trail}-unpolicied");
        emulator.provision_bucket_with_policy(&unpolicied, HOME_REGION, None);
        let bucket = if operation == "CreateTrail" {
            unpolicied.clone()
        } else {
            let held = trail_on_a_writable_destination(&emulator, trail).await;
            if operation == "PutInsightSelectors" {
                // The destination the trail already delivers to stops permitting the
                // delivery, which is what the Insights write is judged against.
                emulator.provision_bucket_with_policy(&held, HOME_REGION, None);
                held
            } else {
                unpolicied.clone()
            }
        };

        let case = format!("{operation} against a bucket carrying no delivery policy");
        let reply = emulator
            .call(operation, &request(operation, trail, &bucket))
            .await;
        reply.expect_error(&case, 400, "InsufficientS3BucketPolicyException");
        assert_eq!(
            reply.message(),
            format!("Incorrect S3 bucket policy is detected for bucket: {bucket}"),
            "case {case}: expected the sentence that names the bucket, got {:?}",
            reply.body
        );

        // A refused `CreateTrail` lists no trail; the other two left the trail there.
        let listed = emulator.call("ListTrails", &json!({})).await;
        let names: Vec<String> = listed
            .expect_ok(&case)["Trails"]
            .as_array()
            .map(|trails| {
                trails
                    .iter()
                    .filter_map(|entry| entry.get("Name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names.contains(&trail.to_string()),
            operation != "CreateTrail",
            "case {case}: ListTrails reported {names:?}"
        );

        // The remedy is the bucket policy, and nothing else about the call changes.
        emulator.provision_bucket(&bucket, HOME_REGION);
        emulator
            .call(operation, &request(operation, trail, &bucket))
            .await
            .expect_ok(&format!("{case}, once the delivery policy is attached"));
    }
}
