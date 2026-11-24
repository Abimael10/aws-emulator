// SPDX-License-Identifier: CC0-1.0

//! The permissions the log delivery service needs on the Amazon S3 bucket a user pool
//! exports its logs to — written where Amazon S3 OWNS them, in that bucket's own policy.
//!
//! `S3ConfigurationType.BucketArn` is "the ARN of an Amazon S3 bucket that's the destination
//! for threat protection log export", and the exported records are written into that bucket
//! by the log delivery service rather than by the caller or by this service. What lets it
//! write is not a fact of this service's own store: the bucket belongs to Amazon S3, and so
//! does the policy on it. So a `SetLogDeliveryConfiguration` that ACCEPTS an Amazon S3
//! destination writes those permissions into that bucket's policy, instead of reporting a
//! log export whose delivery nothing could ever authorise.
//!
//! The grant is two statements for the delivery principal
//! ([`LOG_DELIVERY_SERVICE_PRINCIPAL`]): the write of the objects the exported records land
//! in, under the key prefix the delivery writes them to for the pool's own account
//! ([`delivery_write_grant`]), and the read of the destination bucket's ACL the delivery
//! checks before it writes ([`delivery_acl_check_grant`]). Both are MERGED into whatever
//! document the bucket already carries: a statement ALLOWING the delivery the same action is
//! the grant this write replaces, so a pool configured twice over one bucket leaves it
//! granting exactly once, and every other statement — the bucket owner's enforce-TLS deny,
//! or a refusal of the delivery principal itself — is left exactly where and as it was.
//!
//! The document the grants land in is NAMED the way the log delivery writes it: it carries
//! the log-delivery grant document's own `Id` ([`DELIVERY_POLICY_DOCUMENT_ID`]) unless it
//! already carries one of the owner's. Nothing else about the document is rewritten, so a
//! policy read back after a set is the one the bucket carries.
//!
//! A destination the pool stops exporting to keeps its grant: the policy is the bucket
//! owner's document, and this service only ever ADDS the delivery's statements to it.
//!
//! Amazon S3 is reached the way the framework documents a handler reaching another emulated
//! service ([`smithy_server_runtime::OperationContext::callback`]), through the tree's one
//! construction of that client (`service_common::s3_client`), so nothing here opens a
//! socket, leaves the process, or names another service crate.
//!
//! The one caller-supplied value that reaches the client's authority is the Region, which
//! [`super::log_destinations::destination_region`] has already narrowed to a Region label, so
//! no request can re-point this client at a host of its own choosing.

use super::log_destinations::{destination_region, s3_destination, ConfiguredPool, S3_ARN_NAMESPACE};

#[allow(unused_imports)]
use super::super::types::*;

/// The service principal that writes a user pool's exported logs into the destination bucket
/// — the identity the bucket's policy grants.
pub const LOG_DELIVERY_SERVICE_PRINCIPAL: &str = "delivery.logs.amazonaws.com";

/// The `Sid` of the statement granting that principal the object write.
pub const DELIVERY_WRITE_SID: &str = "AWSLogDeliveryWrite";

/// The `Sid` of the statement granting it the bucket-ACL read.
pub const DELIVERY_ACL_CHECK_SID: &str = "AWSLogDeliveryAclCheck";

/// The action the exported records are written with.
pub const DELIVERY_WRITE_ACTION: &str = "s3:PutObject";

/// The action the delivery reads the destination bucket's ACL with, before it writes.
pub const DELIVERY_ACL_CHECK_ACTION: &str = "s3:GetBucketAcl";

/// The `Id` the log-delivery grant document carries — the NAME of that document, which the
/// delivery writes on the policy it leaves on a destination bucket.
///
/// A fixed part of the document rather than anything read from a request: no bucket, account
/// or Region appears in it, so every bucket the delivery writes grants into reports the same
/// name back. A document that already carries an `Id` of its own KEEPS it — the grants are
/// merged into the owner's document, and naming it is the owner's.
const DELIVERY_POLICY_DOCUMENT_ID: &str = "AWSLogDeliveryWrite20150319";

/// The canned ACL every delivered object has to be written with, so the objects the delivery
/// puts in the bucket belong to the bucket's owner.
const BUCKET_OWNER_FULL_CONTROL: &str = "bucket-owner-full-control";

/// The key segment the delivery writes every exported log object under, followed by the
/// account the records are of.
const DELIVERED_LOGS_KEY_SEGMENT: &str = "AWSLogs";

/// The version every AWS resource policy declares, as Amazon S3's own bucket-policy examples
/// spell it.
const POLICY_VERSION: &str = "2012-10-17";

/// The effect a GRANT carries, as IAM spells it — the one the delivery's own statements have,
/// and what tells a grant apart from the bucket owner's refusal of the same principal.
const ALLOW_EFFECT: &str = "Allow";

/// The error code Amazon S3 answers a read of the policy of a bucket that carries none with,
/// which is "this bucket has no policy yet" rather than a failure to ask: the grants are then
/// the first statements the document holds.
const NO_SUCH_BUCKET_POLICY_CODE: &str = "NoSuchBucketPolicy";

/// What Amazon S3 answered about the policy a bucket carries, in the three cases the
/// amendment has to tell apart.
///
/// Only a bucket S3 reports carrying NO policy is one whose policy the grants may become the
/// whole of; a document S3 did report is merged into; and a question S3 left unanswered — a
/// read that failed, or a body that is no JSON document — establishes nothing, so the policy
/// in force stays as it is rather than being overwritten with the grants alone.
#[derive(Debug)]
enum CarriedPolicy {
    /// Amazon S3 reports the bucket carrying no policy at all.
    None,
    /// The policy document Amazon S3 reports the bucket carrying.
    Document(::serde_json::Value),
    /// Amazon S3 did not say what the bucket carries.
    Unknown,
}

/// Write the permissions the log delivery needs into the own policy of every Amazon S3
/// bucket `configurations` exports to.
///
/// Each bucket is granted ONCE however many entries of the collection name it, because the
/// two statements are of the bucket and of the account rather than of one entry. A collection
/// naming no Amazon S3 destination — the empty one a set clears a pool's logging with
/// included — grants nothing: there is no bucket to grant on.
///
/// Call it with NO guard of this service's store held: it awaits.
pub(super) async fn grant_s3_log_export_delivery(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    pool: &ConfiguredPool<'_>,
    configurations: &[LogConfigurationType],
) {
    let mut granted: Vec<&str> = Vec::new();
    for arn in exported_bucket_arns(configurations) {
        if granted.contains(&arn) {
            continue;
        }
        granted.push(arn);
        grant_on_destination(ctx, pool, arn).await;
    }
}

/// The `S3ConfigurationType.BucketArn` of every entry of `configurations` that names one, in
/// the order the collection names them.
fn exported_bucket_arns(configurations: &[LogConfigurationType]) -> Vec<&str> {
    configurations
        .iter()
        .filter_map(|configuration| configuration.s3_configuration.as_ref())
        .filter_map(|s3| s3.bucket_arn.as_deref())
        .collect()
}

/// Write the delivery's grants into the policy of the bucket `arn` names.
///
/// Does nothing at all for a value that names no Amazon S3 bucket: there is no bucket to
/// grant on. Such a value never reaches an ACCEPTED collection — the destination resolution
/// refuses it first ([`super::log_destinations`]) — so this only keeps the two readings of
/// what an Amazon S3 destination is from having to be the same one.
async fn grant_on_destination(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    pool: &ConfiguredPool<'_>,
    arn: &str,
) {
    let Some(destination) = s3_destination(arn) else {
        return;
    };
    let region = destination_region(arn, S3_ARN_NAMESPACE, pool.region);
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    let carried = match client
        .get_bucket_policy()
        .bucket(destination.bucket)
        .send()
        .await
    {
        Ok(answer) => match answer.policy.as_deref() {
            None => CarriedPolicy::None,
            Some(text) => match ::serde_json::from_str::<::serde_json::Value>(text) {
                Ok(document) => CarriedPolicy::Document(document),
                Err(error) => {
                    ungranted(destination.bucket, &error);
                    CarriedPolicy::Unknown
                }
            },
        },
        Err(error) => {
            // A bucket that carries no policy yet is the ordinary case of a first log
            // export, not a failure: the grants become the document's first statements.
            // Anything else leaves the question of what the bucket carries unanswered.
            if ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error)
                == Some(NO_SUCH_BUCKET_POLICY_CODE)
            {
                CarriedPolicy::None
            } else {
                ungranted(destination.bucket, &error);
                CarriedPolicy::Unknown
            }
        }
    };
    let carried = match carried {
        CarriedPolicy::None => None,
        CarriedPolicy::Document(document) => Some(document),
        // A policy this emulator could not read is not one it may replace: the write below
        // would put a document holding the grants alone, dropping statements the bucket's
        // owner wrote. Nothing is written, and the read that failed is already traced.
        CarriedPolicy::Unknown => return,
    };
    let merged = merged_delivery_grants(
        carried,
        destination.partition,
        destination.bucket,
        region,
        pool.account,
    );
    let document = match ::serde_json::to_string(&merged) {
        Ok(document) => document,
        Err(error) => {
            ungranted(destination.bucket, &error);
            return;
        }
    };
    if let Err(error) = client
        .put_bucket_policy()
        .bucket(destination.bucket)
        .policy(document)
        .send()
        .await
    {
        ungranted(destination.bucket, &error);
    }
}

/// The policy the destination bucket is to carry: the one it carries now with the log
/// delivery's two grants merged in.
///
/// A statement already granting the delivery one of the two actions is REPLACED where it
/// sits, so the grants are written once however often a pool is configured to export to one
/// bucket, and every other statement is left exactly as it was. A document that is not an
/// object holding statements is not one this service can merge into, so the grants stand
/// alone in a fresh document, which is the state a bucket carrying no policy is in.
fn merged_delivery_grants(
    carried: Option<::serde_json::Value>,
    partition: &str,
    bucket: &str,
    region: &str,
    account: &str,
) -> ::serde_json::Value {
    let bucket_arn = bucket_arn(partition, bucket);
    let source_arn = delivery_source_arn(partition, region, account);
    let mut document = match carried {
        Some(::serde_json::Value::Object(document)) => document,
        _ => ::serde_json::Map::new(),
    };
    let mut statements: Vec<::serde_json::Value> = match document.get("Statement") {
        Some(::serde_json::Value::Array(statements)) => statements.clone(),
        Some(statement @ ::serde_json::Value::Object(_)) => vec![statement.clone()],
        _ => Vec::new(),
    };

    // The ACL check first, which is the order the log delivery's own grant document lists
    // the two in: the delivery reads the bucket's ACL before it writes anything into it.
    for grant in [
        delivery_acl_check_grant(&bucket_arn, &source_arn, account),
        delivery_write_grant(&bucket_arn, &source_arn, account),
    ] {
        let action = grant
            .get("Action")
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        match statements
            .iter()
            .position(|statement| grants_delivery(statement, &action))
        {
            Some(at) => statements[at] = grant,
            None => statements.push(grant),
        }
    }

    document.insert(
        "Version".to_string(),
        ::serde_json::Value::String(POLICY_VERSION.to_string()),
    );
    document.insert(
        "Statement".to_string(),
        ::serde_json::Value::Array(statements),
    );
    // The document the delivery writes carries its own name, and a document that already
    // carries one keeps it: the grants are merged into the owner's document.
    document
        .entry("Id".to_string())
        .or_insert_with(|| ::serde_json::Value::String(DELIVERY_POLICY_DOCUMENT_ID.to_string()));
    ::serde_json::Value::Object(document)
}

/// The statement granting the delivery the write of the objects the exported records land in.
///
/// The conditions are what bound the grant: the objects have to be written so their content
/// belongs to the bucket's owner, and the delivery has to be acting for the pool's own
/// account's logging ([`delivery_source_arn`]).
fn delivery_write_grant(
    bucket_arn: &str,
    source_arn: &str,
    account: &str,
) -> ::serde_json::Value {
    ::serde_json::json!({
        "Sid": DELIVERY_WRITE_SID,
        "Effect": ALLOW_EFFECT,
        "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
        "Action": DELIVERY_WRITE_ACTION,
        "Resource": delivered_objects_arn(bucket_arn, account),
        "Condition": {
            "StringEquals": {
                "s3:x-amz-acl": BUCKET_OWNER_FULL_CONTROL,
                "aws:SourceAccount": account,
            },
            "ArnLike": { "aws:SourceArn": source_arn },
        },
    })
}

/// The statement granting the delivery the read of the destination bucket's ACL, which it
/// makes against the bucket itself rather than against any object of it.
fn delivery_acl_check_grant(
    bucket_arn: &str,
    source_arn: &str,
    account: &str,
) -> ::serde_json::Value {
    ::serde_json::json!({
        "Sid": DELIVERY_ACL_CHECK_SID,
        "Effect": ALLOW_EFFECT,
        "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
        "Action": DELIVERY_ACL_CHECK_ACTION,
        "Resource": bucket_arn,
        "Condition": {
            "StringEquals": { "aws:SourceAccount": account },
            "ArnLike": { "aws:SourceArn": source_arn },
        },
    })
}

/// The ARN of the destination bucket itself, in the partition its own ARN named.
fn bucket_arn(partition: &str, bucket: &str) -> String {
    format!("arn:{partition}:{S3_ARN_NAMESPACE}:::{bucket}")
}

/// The ARN of the objects the delivery writes into the destination bucket: the segment it
/// writes every exported log under ([`DELIVERED_LOGS_KEY_SEGMENT`]) and the account the
/// records are of.
fn delivered_objects_arn(bucket_arn: &str, account: &str) -> String {
    format!("{bucket_arn}/{DELIVERED_LOGS_KEY_SEGMENT}/{account}/*")
}

/// The ARN pattern the delivery acts under: the logging of `account` in `region`, which is
/// what scopes the grant to this account's own exported logs rather than to anything the
/// delivery service is asked to write.
fn delivery_source_arn(partition: &str, region: &str, account: &str) -> String {
    format!("arn:{partition}:logs:{region}:{account}:*")
}

/// Whether `statement` ALLOWS `action` to the delivery principal — the statement this
/// service's own grant for that action replaces.
///
/// A statement that DENIES the delivery principal is not a grant and is never one of these:
/// it is the bucket owner's own refusal, which the merge leaves exactly where it is rather
/// than taking its place.
fn grants_delivery(statement: &::serde_json::Value, action: &str) -> bool {
    let allows =
        statement.get("Effect").and_then(::serde_json::Value::as_str) == Some(ALLOW_EFFECT);
    let granted = statement
        .get("Principal")
        .and_then(|named| named.get("Service"))
        .is_some_and(|service| names(service, LOG_DELIVERY_SERVICE_PRINCIPAL));
    allows
        && granted
        && statement
            .get("Action")
            .is_some_and(|actions| names(actions, action))
}

/// Whether a policy element written as one string or a list of them names `value`.
fn names(element: &::serde_json::Value, value: &str) -> bool {
    match element {
        ::serde_json::Value::String(named) => named == value,
        ::serde_json::Value::Array(named) => named.iter().any(|named| named.as_str() == Some(value)),
        _ => false,
    }
}

/// Trace a destination bucket whose policy this emulator could not amend, which is a
/// deployment with no object store mounted rather than a bucket reported missing.
///
/// The caller's own value is rendered in its escaped `Debug` form, so a name carrying a
/// newline cannot forge a log line.
fn ungranted<E: ::std::error::Error + 'static>(bucket: &str, error: &E) {
    ::tracing::warn!(
        bucket_name = ?bucket,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "the log delivery grants of a user pool's log export could not be written into the \
         destination bucket's policy"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account every document below is granted for, and the Region its source ARN names.
    const ACCOUNT: &str = "000000000000";
    const REGION: &str = "us-east-1";

    /// The document a bucket carrying no policy is left with: the delivery's two statements
    /// and nothing else, each scoped to this account's own exported logs.
    #[test]
    fn a_bucket_with_no_policy_is_left_carrying_the_two_delivery_grants() {
        let document = merged_delivery_grants(None, "aws", "pool-logs", REGION, ACCOUNT);
        assert_eq!(
            document,
            ::serde_json::json!({
                "Version": POLICY_VERSION,
                "Id": DELIVERY_POLICY_DOCUMENT_ID,
                "Statement": [
                    {
                        "Sid": DELIVERY_ACL_CHECK_SID,
                        "Effect": ALLOW_EFFECT,
                        "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
                        "Action": DELIVERY_ACL_CHECK_ACTION,
                        "Resource": "arn:aws:s3:::pool-logs",
                        "Condition": {
                            "StringEquals": { "aws:SourceAccount": ACCOUNT },
                            "ArnLike": { "aws:SourceArn": "arn:aws:logs:us-east-1:000000000000:*" },
                        },
                    },
                    {
                        "Sid": DELIVERY_WRITE_SID,
                        "Effect": ALLOW_EFFECT,
                        "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
                        "Action": DELIVERY_WRITE_ACTION,
                        "Resource": "arn:aws:s3:::pool-logs/AWSLogs/000000000000/*",
                        "Condition": {
                            "StringEquals": {
                                "s3:x-amz-acl": BUCKET_OWNER_FULL_CONTROL,
                                "aws:SourceAccount": ACCOUNT,
                            },
                            "ArnLike": { "aws:SourceArn": "arn:aws:logs:us-east-1:000000000000:*" },
                        },
                    },
                ],
            }),
            "a bucket carrying no policy must be left with the two delivery grants, \
             got {document}"
        );
    }

    /// Which ARNs the grants name: the partition and the bucket the destination's own ARN
    /// named, and the account the pool is in.
    #[test]
    fn the_grants_name_the_bucket_and_the_account_they_were_written_for() {
        // (case, partition, bucket, region, account)
        type ArnRow<'a> = (&'a str, &'a str, &'a str, &'a str, &'a str);
        let cases: [ArnRow<'_>; 4] = [
            ("the commercial partition", "aws", "pool-logs", "us-east-1", ACCOUNT),
            ("another partition", "aws-us-gov", "gov-pool-logs", "us-gov-west-1", "123456789012"),
            ("a third partition", "aws-cn", "cn.pool.logs", "cn-north-1", "111122223333"),
            ("a bucket name with digits and hyphens", "aws", "pool-logs-2026", "eu-west-2", "444455556666"),
        ];
        for (case, partition, bucket, region, account) in cases {
            let document = merged_delivery_grants(None, partition, bucket, region, account);
            let statements = document["Statement"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a statement list: {document}"))
                .clone();
            let actual: Vec<(&str, &str)> = statements
                .iter()
                .map(|statement| {
                    (
                        statement["Action"].as_str().unwrap_or_default(),
                        statement["Resource"].as_str().unwrap_or_default(),
                    )
                })
                .collect();
            let acl_check_resource = format!("arn:{partition}:s3:::{bucket}");
            let write_resource = format!(
                "arn:{partition}:s3:::{bucket}/{DELIVERED_LOGS_KEY_SEGMENT}/{account}/*"
            );
            let expected: Vec<(&str, &str)> = vec![
                (DELIVERY_ACL_CHECK_ACTION, acl_check_resource.as_str()),
                (DELIVERY_WRITE_ACTION, write_resource.as_str()),
            ];
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the grants {expected:?}, got {actual:?}"
            );
            for statement in &statements {
                let source = statement["Condition"]["ArnLike"]["aws:SourceArn"]
                    .as_str()
                    .unwrap_or_default();
                let expected = format!("arn:{partition}:logs:{region}:{account}:*");
                assert_eq!(
                    source, expected,
                    "case {case:?}: expected the source ARN {expected:?}, got {source:?}"
                );
            }
        }
    }

    /// What the merge does to a document the bucket already carries: the owner's statements
    /// stay, a grant of the same action is replaced where it sits, and a DENY of the delivery
    /// principal is not a grant and is left alone.
    #[test]
    fn the_grants_are_merged_into_whatever_policy_the_bucket_carries() {
        let owners_deny = ::serde_json::json!({
            "Sid": "DenyInsecureTransport",
            "Effect": "Deny",
            "Principal": "*",
            "Action": "s3:*",
            "Resource": "arn:aws:s3:::pool-logs/*",
            "Condition": { "Bool": { "aws:SecureTransport": "false" } },
        });
        let delivery_deny = ::serde_json::json!({
            "Sid": "DenyTheDelivery",
            "Effect": "Deny",
            "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
            "Action": DELIVERY_WRITE_ACTION,
            "Resource": "arn:aws:s3:::pool-logs/*",
        });
        let stale_write = ::serde_json::json!({
            "Sid": DELIVERY_WRITE_SID,
            "Effect": ALLOW_EFFECT,
            "Principal": { "Service": LOG_DELIVERY_SERVICE_PRINCIPAL },
            "Action": DELIVERY_WRITE_ACTION,
            "Resource": "arn:aws:s3:::pool-logs/AWSLogs/999999999999/*",
        });

        // (case, the document the bucket carries, the statement ids the merge leaves, in order)
        type MergeRow<'a> = (&'a str, ::serde_json::Value, Vec<&'a str>);
        let cases: [MergeRow<'_>; 5] = [
            (
                "a document the owner wrote",
                ::serde_json::json!({
                    "Version": POLICY_VERSION,
                    "Id": "TheOwnersDocument",
                    "Statement": [owners_deny.clone()],
                }),
                vec!["DenyInsecureTransport", DELIVERY_ACL_CHECK_SID, DELIVERY_WRITE_SID],
            ),
            (
                "a grant of the same action already written, replaced where it sits",
                ::serde_json::json!({
                    "Version": POLICY_VERSION,
                    "Statement": [stale_write.clone(), owners_deny.clone()],
                }),
                vec![DELIVERY_WRITE_SID, "DenyInsecureTransport", DELIVERY_ACL_CHECK_SID],
            ),
            (
                "a refusal of the delivery principal, which is not a grant",
                ::serde_json::json!({
                    "Version": POLICY_VERSION,
                    "Statement": [delivery_deny.clone()],
                }),
                vec!["DenyTheDelivery", DELIVERY_ACL_CHECK_SID, DELIVERY_WRITE_SID],
            ),
            (
                "a document carrying one statement rather than a list",
                ::serde_json::json!({ "Version": POLICY_VERSION, "Statement": owners_deny.clone() }),
                vec!["DenyInsecureTransport", DELIVERY_ACL_CHECK_SID, DELIVERY_WRITE_SID],
            ),
            (
                "a document that is not an object at all",
                ::serde_json::json!("not a policy"),
                vec![DELIVERY_ACL_CHECK_SID, DELIVERY_WRITE_SID],
            ),
        ];

        for (case, carried, expected) in cases {
            let document =
                merged_delivery_grants(Some(carried), "aws", "pool-logs", REGION, ACCOUNT);
            let actual: Vec<&str> = document["Statement"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a statement list: {document}"))
                .iter()
                .map(|statement| statement["Sid"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the statements {expected:?}, got {actual:?}"
            );
            // The owner's own name for the document is kept; a document carrying none is
            // named by the delivery.
            let id = document["Id"].as_str().unwrap_or_default();
            let expected_id = if case == "a document the owner wrote" {
                "TheOwnersDocument"
            } else {
                DELIVERY_POLICY_DOCUMENT_ID
            };
            assert_eq!(
                id, expected_id,
                "case {case:?}: expected the document id {expected_id:?}, got {id:?}"
            );
        }
    }

    /// Granting twice over one bucket leaves it granting once: the second merge replaces the
    /// statements the first wrote.
    #[test]
    fn granting_the_same_bucket_again_leaves_the_same_two_statements() {
        let first = merged_delivery_grants(None, "aws", "pool-logs", REGION, ACCOUNT);
        let again =
            merged_delivery_grants(Some(first.clone()), "aws", "pool-logs", REGION, ACCOUNT);
        assert_eq!(
            again, first,
            "granting the same bucket twice must leave the same document, got {again}"
        );

        // And a second pool of the same account exporting to the same bucket is the same
        // grant: the statements are of the bucket and the account, not of the pool.
        let third = merged_delivery_grants(Some(again.clone()), "aws", "pool-logs", REGION, ACCOUNT);
        let statements = third["Statement"].as_array().map(Vec::len).unwrap_or_default();
        assert_eq!(
            statements, 2,
            "a bucket two pools export to must carry two delivery statements, got {third}"
        );
    }
}
