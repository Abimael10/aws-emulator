// SPDX-License-Identifier: CC0-1.0

//! What `CreateFeatureGroup` does with the Amazon S3 location its group's records are
//! REPLICATED into and the IAM role that replication is made as — driven end to end over the
//! real wire path (SageMaker, Amazon S3 and IAM mounted on ONE shared engine, as the
//! production router mounts every service), so what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: a feature group whose `OfflineStoreConfig` names a location the
//! permissions in force do not let its `RoleArn` write is CREATED — the call is answered — and
//! then FAILS: the clock settles it into the modelled `FeatureGroupStatus` `CreateFailed`
//! carrying a `FailureReason`, which is the member the model gives exactly this condition
//! ("The reason that the `FeatureGroup` failed to be replicated in the `OfflineStore`. This is
//! failure can occur because: The `FeatureGroup` could not be created in the `OfflineStore`" —
//! `DescribeFeatureGroup$FailureReason`). A group whose write IS permitted settles `Created`
//! with no reason, and a group that failed cannot be updated.
//!
//! The two documents in force are weighed as IAM weighs them within one account: an `Allow`
//! from EITHER the destination bucket's own policy or the role's identity-based policies
//! grants, and an explicit `Deny` from either refuses. Only what IAM and Amazon S3
//! ESTABLISHED fails a group: a role this account's IAM holds nothing under, and a bucket
//! carrying no policy, establish nothing.
//!
//! The table spans what the rule generalises over: WHICH resource the role's one grant names
//! (the whole bucket, exactly the configured prefix, another bucket), WHICH action it names,
//! whether IAM holds the role at all, the destination bucket policy's own effect, the SHAPE of
//! the configured location (a prefix, a nested prefix, the bucket's root) and whether the
//! create names an offline store at all.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model_artifact_admission::{error_code, message, role_arn, trust_for, Emulator};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The IAM action an object write is authorized under, as this service names it.
const WRITE_ACTION: &str = service_com_amazonaws_sagemaker_sagemaker::state::PUT_OBJECT_ACTION;

/// An Amazon S3 action that is NOT the one a record write is authorized under, so a role
/// allowed only this one cannot write the store.
const UNRELATED_ACTION: &str = "s3:DeleteObject";

/// The `FeatureGroupStatus` a group whose offline store could be created settles in.
const CREATED: &str = service_com_amazonaws_sagemaker_sagemaker::state::FEATURE_GROUP_STATUS_CREATED;

/// The `FeatureGroupStatus` a group whose offline store could NOT be created settles in.
const CREATE_FAILED: &str =
    service_com_amazonaws_sagemaker_sagemaker::state::FEATURE_GROUP_STATUS_CREATE_FAILED;

/// The group's record-identifier feature.
const IDENTIFIER_FEATURE: &str = "customer-id";

/// The group's event-time feature.
const EVENT_TIME_FEATURE: &str = "event-time";

/// How long a case waits for the terminal status the clock already owes a created group.
///
/// GENEROUS on purpose: what is under test is WHICH status the group settles in, never how
/// fast it gets there, so each case polls the reader for the condition it needs and this is
/// only the deadline that stops a broken emulator from hanging the suite.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits between two reads while polling for a settled status.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One create the rule is asserted over.
struct Row {
    case: &'static str,
    /// The key prefix the create's `S3Uri` names inside the row's own bucket (`""` for the
    /// bucket's root), and `None` for a create that names no offline store at all.
    prefix: Option<&'static str>,
    /// Whether IAM holds the role the create supplies.
    role_held: bool,
    /// The action and resource of the held role's one inline statement, or `None` for a role
    /// carrying no policy at all. `{bucket}` stands for the row's own bucket.
    grant: Option<(&'static str, &'static str)>,
    /// The effect, action and resource of the destination bucket policy's one statement, which
    /// names the row's role as its principal; `None` for a bucket carrying no policy.
    bucket_policy: Option<(&'static str, &'static str, &'static str)>,
    /// Whether the group's creation must FAIL.
    failed: bool,
}

/// The `CreateFeatureGroup` body for `group`, with the offline store `offline` describes.
fn create_body(group: &str, offline: Option<(&str, &str)>) -> Value {
    let mut body = json!({
        "FeatureGroupName": group,
        "RecordIdentifierFeatureName": IDENTIFIER_FEATURE,
        "EventTimeFeatureName": EVENT_TIME_FEATURE,
        "FeatureDefinitions": [
            { "FeatureName": IDENTIFIER_FEATURE, "FeatureType": "String" },
            { "FeatureName": EVENT_TIME_FEATURE, "FeatureType": "String" },
        ],
        "OnlineStoreConfig": { "EnableOnlineStore": true },
    });
    let object = body.as_object_mut().expect("a JSON object body");
    if let Some((s3_uri, supplied_role)) = offline {
        object.insert(
            "OfflineStoreConfig".to_string(),
            // `DisableGlueTableCreation`: no suite here manages a Glue table.
            json!({
                "S3StorageConfig": { "S3Uri": s3_uri },
                "DisableGlueTableCreation": true,
            }),
        );
        object.insert("RoleArn".to_string(), json!(supplied_role));
    }
    body
}

/// A policy document with one statement, as both IAM and Amazon S3 read one.
fn one_statement(effect: &str, principal: Option<&str>, action: &str, resource: &str) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Action": action,
        "Resource": resource,
    });
    if let Some(principal) = principal {
        statement["Principal"] = json!({ "AWS": principal });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// `DescribeFeatureGroup` for `group`, polled until its `FeatureGroupStatus` is one the clock
/// no longer owes a transition from.
async fn settled(emulator: &Emulator, case: &str, group: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .sagemaker_ok(case, "DescribeFeatureGroup", &json!({ "FeatureGroupName": group }))
            .await;
        let status = described
            .get("FeatureGroupStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == CREATED || status == CREATE_FAILED {
            return described;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group never left {status:?}, last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a created feature group settles `CreateFailed`, carrying a `FailureReason` that names
/// the role and the location, exactly when the permissions in force establish that its role
/// may not write the offline location the request named — and settles `Created` with no reason
/// otherwise. The CALL is answered either way.
#[tokio::test]
async fn a_feature_group_whose_role_may_not_write_its_offline_store_fails() {
    let rows = [
        Row {
            case: "a role granted the write on the whole destination bucket",
            prefix: Some("offline"),
            role_held: true,
            grant: Some((WRITE_ACTION, "arn:aws:s3:::{bucket}/*")),
            bucket_policy: None,
            failed: false,
        },
        Row {
            case: "a role granted the write on exactly the configured nested prefix",
            prefix: Some("offline/store"),
            role_held: true,
            grant: Some((WRITE_ACTION, "arn:aws:s3:::{bucket}/offline/store/*")),
            bucket_policy: None,
            failed: false,
        },
        Row {
            case: "a location that is the destination bucket's root",
            prefix: Some(""),
            role_held: true,
            grant: Some((WRITE_ACTION, "arn:aws:s3:::{bucket}/*")),
            bucket_policy: None,
            failed: false,
        },
        Row {
            case: "a role whose one grant names another bucket",
            prefix: Some("offline"),
            role_held: true,
            grant: Some((WRITE_ACTION, "arn:aws:s3:::somewhere-else/*")),
            bucket_policy: None,
            failed: true,
        },
        Row {
            case: "a role carrying no policy at all",
            prefix: Some("offline"),
            role_held: true,
            grant: None,
            bucket_policy: None,
            failed: true,
        },
        Row {
            case: "a role whose one grant names another action on the location",
            prefix: Some("offline"),
            role_held: true,
            grant: Some((UNRELATED_ACTION, "arn:aws:s3:::{bucket}/*")),
            bucket_policy: None,
            failed: true,
        },
        Row {
            case: "a role this account's IAM holds nothing under",
            prefix: Some("offline"),
            role_held: false,
            grant: None,
            bucket_policy: None,
            failed: false,
        },
        Row {
            case: "a destination bucket whose policy denies the write to the role",
            prefix: Some("offline"),
            role_held: true,
            grant: Some((WRITE_ACTION, "arn:aws:s3:::{bucket}/*")),
            bucket_policy: Some(("Deny", WRITE_ACTION, "arn:aws:s3:::{bucket}/*")),
            failed: true,
        },
        Row {
            case: "a destination bucket whose policy allows the write to a role with no grant",
            prefix: Some("offline"),
            role_held: true,
            grant: None,
            bucket_policy: Some(("Allow", WRITE_ACTION, "arn:aws:s3:::{bucket}/*")),
            failed: false,
        },
        Row {
            case: "a create that names no offline store at all",
            prefix: None,
            role_held: true,
            grant: None,
            bucket_policy: None,
            failed: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let bucket = format!("features-admission-{index}");
        let group = format!("offlineadmission{index}");
        let role_name = format!("offline-store-{index}");
        let role = role_arn(&role_name);
        emulator.given_bucket(&bucket).await;
        if row.role_held {
            emulator
                .given_role(&role_name, &trust_for(SERVICE_PRINCIPAL))
                .await;
            if let Some((action, resource)) = row.grant {
                emulator
                    .given_role_policy(
                        &role_name,
                        &one_statement(
                            "Allow",
                            None,
                            action,
                            &resource.replace("{bucket}", &bucket),
                        ),
                    )
                    .await;
            }
        }
        if let Some((effect, action, resource)) = row.bucket_policy {
            emulator
                .given_bucket_policy(
                    &bucket,
                    &one_statement(
                        effect,
                        Some(&role),
                        action,
                        &resource.replace("{bucket}", &bucket),
                    ),
                )
                .await;
        }

        let s3_uri = row.prefix.map(|prefix| {
            if prefix.is_empty() {
                format!("s3://{bucket}")
            } else {
                format!("s3://{bucket}/{prefix}/")
            }
        });
        let body = create_body(&group, s3_uri.as_deref().map(|uri| (uri, role.as_str())));
        // The CALL is answered whatever the permissions say: the failure travels on the
        // resource, never as a refusal of the create.
        let created = emulator.sagemaker_ok(case, "CreateFeatureGroup", &body).await;

        let described = settled(&emulator, case, &group).await;
        let status = described
            .get("FeatureGroupStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reason = described
            .get("FailureReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            described.get("FeatureGroupArn"),
            created.get("FeatureGroupArn"),
            "case {case:?}: expected the created ARN to read back, got {described}"
        );
        if row.failed {
            assert_eq!(
                status, CREATE_FAILED,
                "case {case:?}: expected the group to settle {CREATE_FAILED}, got {described}"
            );
            let named = s3_uri.clone().unwrap_or_default();
            for expected in [role.as_str(), WRITE_ACTION, named.as_str()] {
                assert!(
                    reason.contains(expected),
                    "case {case:?}: expected the FailureReason to name {expected:?}, got \
                     {reason:?}"
                );
            }
        } else {
            assert_eq!(
                status, CREATED,
                "case {case:?}: expected the group to settle {CREATED}, got {described}"
            );
            assert!(
                reason.is_empty(),
                "case {case:?}: expected no FailureReason, got {reason:?}"
            );
        }
    }
}

/// RULE (the control pair): ONE request and ONE role — the group fails while the permissions in
/// force give the role no write at the offline location, and the identical create made once the
/// same role is granted that write settles `Created` with its offline store reporting `Active`.
#[tokio::test]
async fn granting_the_write_turns_the_same_failed_create_into_a_healthy_group() {
    let emulator = Emulator::new();
    let bucket = "features-control-pair";
    let role_name = "offline-store-control";
    let role = emulator
        .given_role(role_name, &trust_for(SERVICE_PRINCIPAL))
        .await;
    emulator.given_bucket(bucket).await;
    let s3_uri = format!("s3://{bucket}/offline/");

    let case = "the role carries no grant at all";
    let failed_group = "offlinecontrolfailed";
    emulator
        .sagemaker_ok(
            case,
            "CreateFeatureGroup",
            &create_body(failed_group, Some((&s3_uri, role.as_str()))),
        )
        .await;
    let described = settled(&emulator, case, failed_group).await;
    assert_eq!(
        described.get("FeatureGroupStatus").and_then(Value::as_str),
        Some(CREATE_FAILED),
        "case {case:?}: expected the group's creation to fail, got {described}"
    );
    // A group whose creation failed holds no provisioned store for an update to change.
    let (status, refused) = emulator
        .sagemaker(
            "UpdateFeatureGroup",
            &json!({
                "FeatureGroupName": failed_group,
                "ThroughputConfig": { "ThroughputMode": "OnDemand" },
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected updating a failed group to be refused, got {status} with \
         {refused}"
    );
    let refusal = message(&refused);
    for expected in [failed_group, &CREATE_FAILED.to_uppercase()] {
        assert!(
            refusal.contains(expected),
            "case {case:?}: expected the refusal to name {expected:?}, got {refusal:?}"
        );
    }

    let case = "the same role granted the write";
    let healthy_group = "offlinecontrolhealthy";
    emulator
        .given_role_policy(
            role_name,
            &one_statement(
                "Allow",
                None,
                WRITE_ACTION,
                &format!("arn:aws:s3:::{bucket}/*"),
            ),
        )
        .await;
    emulator
        .sagemaker_ok(
            case,
            "CreateFeatureGroup",
            &create_body(healthy_group, Some((&s3_uri, role.as_str()))),
        )
        .await;
    let described = settled(&emulator, case, healthy_group).await;
    assert_eq!(
        described.get("FeatureGroupStatus").and_then(Value::as_str),
        Some(CREATED),
        "case {case:?}: expected the group to be created, got {described}"
    );
    assert_eq!(
        described.get("FailureReason"),
        None,
        "case {case:?}: expected no FailureReason, got {described}"
    );
    // The permission the create resolved is the one the group's own status is reported from,
    // so a group this create did not fail reports its offline store as replicating.
    assert_eq!(
        described
            .pointer("/OfflineStoreStatus/Status")
            .and_then(Value::as_str),
        Some("Active"),
        "case {case:?}: expected the offline store reported Active, got {described}"
    );
}
