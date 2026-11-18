// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY PROBE a load balancer performs when a write leaves its access logs switched
//! on, and what the bucket holds after several such writes.
//!
//! The rule under test: **every `ModifyLoadBalancerAttributes` write that leaves a load
//! balancer's `access_logs.s3.*` destination switched on probes that destination by putting
//! one non-empty plain-text marker object — not the gzip stream a log file is — at the
//! destination's own root, `<prefix>/AWSLogs/<account-id>/ELBAccessLogTestFile`
//! (`AWSLogs/<account-id>/ELBAccessLogTestFile` where the attributes name no prefix); the
//! probe is keyed by the destination the write configured, so a write under a prefix the
//! destination has not been probed under yet ADDS an object beside the ones already there,
//! a write under a prefix already probed re-puts the same key, and a write that leaves the
//! destination switched off probes nothing and removes nothing.**
//!
//! Grounded in the model and the catalog it documents: `ModifyLoadBalancerAttributes`
//! "Modifies the specified attributes of the specified Application Load Balancer…", and
//! `LoadBalancerAttribute$Key` carries the destination — "`access_logs.s3.enabled` -
//! Indicates whether access logs are enabled. The value is true or false",
//! "`access_logs.s3.bucket` - The name of the S3 bucket for the access logs" and
//! "`access_logs.s3.prefix` - The prefix for the location in the S3 bucket for the access
//! logs" — read with
//! `smithy_query(query="doc",shape="LoadBalancerAttribute",member="Key")`. The marker's own
//! name and the `AWSLogs/<account-id>/` segments the delivery writes under a caller's prefix
//! are Elastic Load Balancing's recorded layout, which the handler's one keying of a
//! destination renders.
//!
//! The legs differ along what the rule generalises over: the FORM of the prefix (a single
//! segment, none at all, a nested one, one merely carrying the layout's own segment later
//! on), whether that prefix has been probed before, and whether the write leaves the
//! destination on at all — and each leg is judged on the WHOLE key set of the bucket, which
//! is what tells adding a probe from replacing one.
//!
//! The bucket and its policy are Amazon S3's, so both are made through Amazon S3's own API,
//! with that service mounted beside this one on one engine, and every object is read back
//! where a client reads it: out of the bucket.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::access_log_destination::grant_log_delivery;
use crate::load_balancer_attributes::{create_load_balancer, write_attributes};
use crate::provisioning_loop::{account, REGION};

/// The three attributes that say whether access logs are delivered and where, from
/// `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED: &str = "access_logs.s3.enabled";
const BUCKET: &str = "access_logs.s3.bucket";
const PREFIX: &str = "access_logs.s3.prefix";

/// The segment the delivery itself writes into every key under a caller's prefix, and the
/// name Elastic Load Balancing gives the object it probes a destination with.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";
const TEST_FILE_NAME: &str = "ELBAccessLogTestFile";

/// The first two bytes of a gzip stream, which is what an access-log FILE is and what the
/// probe object therefore may not be.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// This service and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the bucket the attributes name is the bucket this suite makes and reads.
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

/// The Amazon S3 client this suite makes the bucket and reads the probe objects with, built
/// by the one shared constructor so it reaches the store the handler reaches.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    service_common::s3_client::s3_client(&EmbeddedEmulator::from_router(router.clone()), REGION)
}

/// Make the log bucket and write the grant that admits the delivery into it — the setup a
/// client does before it can turn logging on at all, through the crate's one spelling of
/// that policy.
async fn log_bucket(router: &MultiServiceRouter, bucket: &str) {
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
        .unwrap_or_else(|error| panic!("setup: creating {bucket} must succeed, got {error:?}"));
    grant_log_delivery(router, bucket).await;
}

/// Every key the bucket holds, sorted, which is what a leg is judged on.
async fn keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let listed = s3(router)
        .list_objects_v2()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: listing {bucket} must succeed, got {error:?}")
        });
    let mut held: Vec<String> = listed
        .contents()
        .iter()
        .map(|object| object.key().unwrap_or_default().to_string())
        .collect();
    held.sort();
    held
}

/// The bytes of one object, read where a client reads them.
async fn body(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) -> Vec<u8> {
    let fetched = s3(router)
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: fetching {key} must succeed, got {error:?}"));
    fetched
        .body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: reading {key} must succeed, got {error:?}"))
        .into_bytes()
        .to_vec()
}

/// The key the probe of a destination under `prefix` takes, as the recorded layout spells it:
/// the caller's prefix, the segments the delivery writes, and the marker's own name.
fn probe_key(prefix: &str) -> String {
    let opening = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    format!(
        "{opening}{DELIVERED_LOGS_SEGMENT}/{}/{TEST_FILE_NAME}",
        account()
    )
}

/// One write of the destination's attributes, and the whole key set the bucket must hold
/// afterwards.
struct Leg {
    case: &'static str,
    /// What `access_logs.s3.enabled` is written as.
    enabled: &'static str,
    /// What `access_logs.s3.prefix` is written as, or `None` for a write that names no
    /// prefix at all and so leaves the one already stored in place.
    prefix: Option<&'static str>,
    /// The prefixes whose probe objects the bucket must hold after this write — the whole
    /// set, so a probe that REPLACED an earlier one instead of being added fails the leg.
    probed: &'static [&'static str],
}

/// RULE: every write that leaves the access-log destination switched on probes it with one
/// plain-text marker at that destination's own root, so the probes of the prefixes a load
/// balancer has been enabled under ACCUMULATE in the bucket.
#[tokio::test]
async fn enabling_access_logs_probes_the_destination_and_the_probes_accumulate_per_prefix() {
    let router = emulator();
    let bucket = "elbv2-probe-accumulation";
    log_bucket(&router, bucket).await;
    let arn = create_load_balancer(&router, "probe-accumulation", &[]).await;
    let before = keys(&router, "before any enable", bucket).await;
    assert!(
        before.is_empty(),
        "a bucket nothing has been delivered into must start empty, got {before:?}"
    );

    let legs: &[Leg] = &[
        Leg {
            case: "the first enable, under a single-segment prefix",
            enabled: "true",
            prefix: Some("edge"),
            probed: &["edge"],
        },
        Leg {
            case: "a re-enable naming no prefix at all, which probes the bucket's root",
            enabled: "true",
            prefix: Some(""),
            probed: &["edge", ""],
        },
        Leg {
            case: "a re-enable under a nested prefix",
            enabled: "true",
            prefix: Some("edge/inner"),
            probed: &["edge", "", "edge/inner"],
        },
        Leg {
            case: "a re-enable under a prefix carrying the layout's segment later on",
            enabled: "true",
            prefix: Some("archive/AWSLogs-old"),
            probed: &["edge", "", "edge/inner", "archive/AWSLogs-old"],
        },
        Leg {
            case: "a re-enable under a prefix already probed, which re-puts the same key",
            enabled: "true",
            prefix: Some("edge"),
            probed: &["edge", "", "edge/inner", "archive/AWSLogs-old"],
        },
        Leg {
            case: "a write switching the destination off, which probes and removes nothing",
            enabled: "false",
            prefix: None,
            probed: &["edge", "", "edge/inner", "archive/AWSLogs-old"],
        },
        Leg {
            case: "a write switching it back on under a fresh prefix",
            enabled: "true",
            prefix: Some("rim"),
            probed: &["edge", "", "edge/inner", "archive/AWSLogs-old", "rim"],
        },
    ];

    for leg in legs {
        let mut writes: Vec<(&str, &str)> = vec![(ENABLED, leg.enabled), (BUCKET, bucket)];
        if let Some(prefix) = leg.prefix {
            writes.push((PREFIX, prefix));
        }
        let reply = write_attributes(&router, &arn, &writes).await;
        assert_eq!(
            reply.status,
            200,
            "case {:?}: expected the write to be accepted, got {} body {}",
            leg.case,
            reply.status,
            reply.body
        );
        let mut expected: Vec<String> = leg.probed.iter().copied().map(probe_key).collect();
        expected.sort();
        let held = keys(&router, leg.case, bucket).await;
        assert_eq!(
            held, expected,
            "case {:?}: expected the bucket to hold {expected:?}, got {held:?}",
            leg.case
        );
        for key in &expected {
            let bytes = body(&router, leg.case, bucket, key).await;
            assert!(
                !bytes.is_empty(),
                "case {:?}: the probe at {key} must be non-empty",
                leg.case
            );
            assert!(
                !bytes.starts_with(&GZIP_MAGIC),
                "case {:?}: the probe at {key} is a plain-text marker and no gzip log file, \
                 got {bytes:?}",
                leg.case
            );
            assert!(
                std::str::from_utf8(&bytes).is_ok(),
                "case {:?}: the probe at {key} must be text, got {bytes:?}",
                leg.case
            );
        }
    }
}
