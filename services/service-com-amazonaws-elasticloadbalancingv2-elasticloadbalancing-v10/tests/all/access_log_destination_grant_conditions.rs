// SPDX-License-Identifier: CC0-1.0

//! The CONDITION a log-delivery bucket policy carries, judged against the write the delivery
//! itself makes.
//!
//! The rule under test: **a load balancer's `<name>.s3.*` destination is admitted when the
//! bucket's policy allows the delivery's `s3:PutObject` under the path the attributes send the
//! logs to FOR THE REQUEST THE DELIVERY ACTUALLY MAKES — so the grant is decided with the
//! condition context that write carries: the `bucket-owner-full-control` canned ACL the
//! delivery names on every object it puts, the account and the load balancer the logs are for,
//! and the TLS transport it writes over. A statement conditioned on one of those facts grants
//! the delivery, a statement conditioned on a different value of one of them grants it
//! nothing, a statement conditioned on a key the delivery's write carries no value for is
//! undecided and so grants nothing, and a `Deny` conditioned on the absence of those facts
//! does not apply to it.**
//!
//! Grounded in the model and in the published log-delivery grant: `ModifyLoadBalancerAttributes`
//! models `InvalidConfigurationRequestException` (HTTP 400) — "the requested configuration is
//! not valid" — for a destination the load balancer cannot hold, and
//! `LoadBalancerAttribute$Key` is the catalog of the destination triples
//! (`smithy_query(query="doc",shape="LoadBalancerAttribute",member="Key")`:
//! "`access_logs.s3.enabled` - Indicates whether access logs are enabled",
//! "`access_logs.s3.bucket` - The name of the S3 bucket for the access logs"). The bucket
//! policy AWS publishes for a log-delivery destination grants the delivery's object write to
//! the log-delivery service principal and conditions it on the canned ACL that hands the
//! object to the bucket's owner, which is the condition this suite's rows are written over.
//!
//! The rows differ along what the rule generalises over: WHICH fact of the delivery's write a
//! statement is conditioned on (the canned ACL, the account, the load balancer, the
//! transport), which OPERATOR states it (`StringEquals`, `ArnLike`, `Bool`, their negations),
//! whether the value stated is the one the write carries or another, whether the statement is
//! an `Allow` or a `Deny`, and a key no delivery carries a value for at all.
//!
//! The bucket and its policy are Amazon S3's, so both are made through Amazon S3's own API,
//! with that service mounted beside this one on one engine; and what the accepted destination
//! then receives is read back where a client reads it — out of the bucket.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::load_balancer_attributes::{
    create_load_balancer, write_attributes, INVALID_CONFIGURATION,
};
use crate::provisioning_loop::{account, REGION};
use crate::target_group_attributes_and_modification::unescaped;

/// The two attributes each row writes, from `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED: &str = "access_logs.s3.enabled";
const BUCKET: &str = "access_logs.s3.bucket";

/// The segment the delivery itself writes into every key, and the name Elastic Load Balancing
/// gives the file it verifies a destination with.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";
const TEST_FILE_NAME: &str = "ELBAccessLogTestFile";

/// The log-delivery identity the published grant names.
const DELIVERY_PRINCIPAL: &str = "delivery.logs.amazonaws.com";

/// The action the delivery writes its objects with.
const DELIVERY_ACTION: &str = "s3:PutObject";

/// The canned ACL the delivery names on every object it writes, which hands the object to the
/// bucket's owner — the fact the published log-delivery grant conditions itself on.
const DELIVERY_ACL: &str = "bucket-owner-full-control";

/// The condition keys the rows are written over: the canned ACL an object write names, the
/// account and the resource a service-made call is for, and the transport it was made over.
const ACL_KEY: &str = "s3:x-amz-acl";
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";
const SOURCE_ARN_KEY: &str = "aws:SourceArn";
const SECURE_TRANSPORT_KEY: &str = "aws:SecureTransport";

/// A key an object write of this emulator carries no value for, so a clause over it can be
/// decided by nothing.
const UNCARRIED_KEY: &str = "s3:x-amz-storage-class";

/// This service and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the bucket a load balancer's attributes name is the bucket this suite makes and reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// The Amazon S3 client this suite makes the bucket and reads the delivered file with, built by
/// the one shared constructor so it reaches the store the handler reaches.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    service_common::s3_client::s3_client(&EmbeddedEmulator::from_router(router.clone()), REGION)
}

/// Make the bucket `bucket` and put `statements` on it as its policy.
async fn staged(router: &MultiServiceRouter, case: &str, bucket: &str, statements: &Value) {
    s3(router)
        .create_bucket()
        .bucket(bucket)
        .create_bucket_configuration(
            aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: setup creating {bucket} must succeed, got {error:?}")
        });
    let policy = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    s3(router)
        .put_bucket_policy()
        .bucket(bucket)
        .policy(policy)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: setup granting the delivery on {bucket} must succeed, got {error:?}")
        });
}

/// The published grant — the delivery's own write on every key of `bucket` — carrying
/// `condition`, or carrying none for `Value::Null`.
fn allow(bucket: &str, condition: Value) -> Value {
    statement("Allow", bucket, condition)
}

/// The same statement as a `Deny`, which is how a published bucket policy forbids a write that
/// does not carry one of those facts.
fn deny(bucket: &str, condition: Value) -> Value {
    statement("Deny", bucket, condition)
}

fn statement(effect: &str, bucket: &str, condition: Value) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": { "Service": DELIVERY_PRINCIPAL },
        "Action": DELIVERY_ACTION,
        "Resource": format!("arn:aws:s3:::{bucket}/*"),
    });
    if !condition.is_null() {
        statement["Condition"] = condition;
    }
    statement
}

/// One row: the bucket, the `Statement` list its policy carries for the load balancer the logs
/// are for, and whether the delivery is admitted into it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    /// Built per row from the bucket and the ARN of the load balancer whose logs these are,
    /// because a grant may be conditioned on that load balancer.
    statements: fn(&str, &str) -> Value,
    admitted: bool,
}

/// Why a bucket whose policy admits the delivery's write nowhere under this destination's path
/// is refused — the wording this API answers that class with.
fn ungranted(bucket: &str) -> String {
    format!("Access Denied for bucket: {bucket}. Please check S3bucket permission")
}

/// The key the file that verifies a destination with no prefix takes.
fn test_file_key() -> String {
    format!("{DELIVERED_LOGS_SEGMENT}/{}/{TEST_FILE_NAME}", account())
}

/// Every key `bucket` holds.
async fn keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let listed = s3(router)
        .list_objects_v2()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: listing {bucket} must succeed, got {error:?}")
        });
    listed
        .contents()
        .iter()
        .map(|object| object.key().unwrap_or_default().to_string())
        .collect()
}

/// RULE: the destination's grant is decided for the request the delivery's own write makes, so
/// a statement conditioned on a fact of that write decides as it would in production.
#[tokio::test]
async fn a_grant_conditioned_on_the_deliverys_own_write_admits_the_destination() {
    let rows: &[Row] = &[
        Row {
            case: "a grant carrying no condition at all",
            bucket: "elbv2-condition-none",
            statements: |bucket, _| json!([allow(bucket, Value::Null)]),
            admitted: true,
        },
        Row {
            case: "a grant conditioned on the canned ACL the delivery names",
            bucket: "elbv2-condition-acl",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "StringEquals": { ACL_KEY: DELIVERY_ACL } })
                )])
            },
            admitted: true,
        },
        Row {
            case: "a grant conditioned on another canned ACL",
            bucket: "elbv2-condition-other-acl",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "StringEquals": { ACL_KEY: "private" } })
                )])
            },
            admitted: false,
        },
        Row {
            case: "a grant conditioned on the account the logs are for",
            bucket: "elbv2-condition-account",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "StringEquals": { SOURCE_ACCOUNT_KEY: account() } })
                )])
            },
            admitted: true,
        },
        Row {
            case: "a grant conditioned on another account",
            bucket: "elbv2-condition-other-account",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "StringEquals": { SOURCE_ACCOUNT_KEY: "210987654321" } })
                )])
            },
            admitted: false,
        },
        Row {
            case: "a grant conditioned on the load balancer the logs are for",
            bucket: "elbv2-condition-source",
            statements: |bucket, arn| {
                json!([allow(
                    bucket,
                    json!({ "ArnLike": { SOURCE_ARN_KEY: arn } })
                )])
            },
            admitted: true,
        },
        Row {
            case: "a grant conditioned on another load balancer of the same account",
            bucket: "elbv2-condition-other-source",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "ArnLike": {
                        SOURCE_ARN_KEY: format!(
                            "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/app/elsewhere/*",
                            account()
                        )
                    } })
                )])
            },
            admitted: false,
        },
        Row {
            case: "a grant conditioned on the transport the delivery writes over",
            bucket: "elbv2-condition-transport",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "Bool": { SECURE_TRANSPORT_KEY: "true" } })
                )])
            },
            admitted: true,
        },
        Row {
            case: "a grant conditioned on a key the delivery's write carries no value for",
            bucket: "elbv2-condition-uncarried",
            statements: |bucket, _| {
                json!([allow(
                    bucket,
                    json!({ "StringEquals": { UNCARRIED_KEY: "STANDARD" } })
                )])
            },
            admitted: false,
        },
        Row {
            case: "a grant beside a deny of any write that keeps the object from the bucket's \
                   owner",
            bucket: "elbv2-condition-acl-deny",
            statements: |bucket, _| {
                json!([
                    allow(bucket, Value::Null),
                    deny(
                        bucket,
                        json!({ "StringNotEquals": { ACL_KEY: DELIVERY_ACL } })
                    )
                ])
            },
            admitted: true,
        },
        Row {
            case: "a grant beside a deny of any write made in the clear",
            bucket: "elbv2-condition-transport-deny",
            statements: |bucket, _| {
                json!([
                    allow(bucket, Value::Null),
                    deny(
                        bucket,
                        json!({ "Bool": { SECURE_TRANSPORT_KEY: "false" } })
                    )
                ])
            },
            admitted: true,
        },
    ];

    let router = emulator();
    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            bucket,
            statements,
            admitted,
        } = row;
        // The load balancer is made BEFORE the policy, because a grant may be conditioned on
        // the load balancer whose logs these are.
        let arn = create_load_balancer(&router, &format!("conditioned-{index}"), &[]).await;
        staged(&router, case, bucket, &statements(bucket, &arn)).await;
        let reply = write_attributes(&router, &arn, &[(ENABLED, "true"), (BUCKET, bucket)]).await;
        let held = keys(&router, case, bucket).await;
        if *admitted {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: a destination the delivery's own write is granted must be \
                 accepted, got {} body {}",
                reply.status, reply.body
            );
            assert_eq!(
                held,
                vec![test_file_key()],
                "case {case:?}: expected the destination to be verified by {:?}, got {held:?}",
                test_file_key()
            );
        } else {
            let stated = unescaped(&reply.text("Message").unwrap_or_default());
            assert_eq!(
                (reply.status, reply.error_code(), stated),
                (400, INVALID_CONFIGURATION.to_string(), ungranted(bucket)),
                "case {case:?}: expected 400 {INVALID_CONFIGURATION} {:?}, got {}",
                ungranted(bucket),
                reply.body
            );
            assert!(
                held.is_empty(),
                "case {case:?}: a refused destination is verified by nothing, got {held:?}"
            );
        }
    }
}
