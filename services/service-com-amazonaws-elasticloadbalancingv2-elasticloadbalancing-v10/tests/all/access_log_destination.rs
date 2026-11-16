// SPDX-License-Identifier: CC0-1.0

//! The Amazon S3 DESTINATION a load balancer's log attributes name, judged where the bucket
//! lives.
//!
//! The rule under test: **a write that leaves one of a load balancer's
//! `<name>.s3.{enabled,bucket,prefix}` log destinations switched on is accepted only if the
//! delivery could write into it — Amazon S3 holds the bucket the attributes name, that
//! bucket's policy admits the delivery's `s3:PutObject` under the path these attributes send
//! the logs to, and the prefix does not begin with the `AWSLogs` segment the layout itself
//! writes — and a destination the delivery could not write into is refused 400
//! `InvalidConfigurationRequest` with nothing stored; a destination it accepted is verified
//! the way Elastic Load Balancing verifies one, by the test file it writes into the
//! destination's own folder.**
//!
//! `LoadBalancerAttribute$Key` is the catalog of those triples — "`access_logs.s3.enabled` -
//! Indicates whether access logs are enabled", "`access_logs.s3.bucket` - The name of the S3
//! bucket for the access logs", "`access_logs.s3.prefix` - The prefix for the location in the
//! S3 bucket for the access logs", and the same three for the connection and the health-check
//! logs — and `ModifyLoadBalancerAttributes` models
//! `InvalidConfigurationRequestException` (HTTP 400) for a configuration the load balancer
//! cannot hold.
//!
//! The rows differ along what the rule generalises over: WHY the destination cannot receive a
//! log (no such bucket, a bucket with no policy at all, a policy scoped to another prefix of
//! the same bucket, no bucket named at all, a prefix opening with the reserved segment), the
//! destination FAMILY the attributes belong to, whether the write leaves the destination
//! switched on at all, and — on the accepted side — a destination with a prefix, one without,
//! and one whose prefix merely contains the reserved segment later on.
//!
//! The bucket and its policy are Amazon S3's, so both are set up through Amazon S3's own API,
//! with that service mounted beside this one on one engine; and the file the delivery writes
//! is read back where a client reads it, out of the bucket.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::load_balancer_attributes::{
    create_load_balancer, described_attributes, write_attributes, INVALID_CONFIGURATION,
};
use crate::provisioning_loop::{account, REGION};
use crate::target_group_attributes_and_modification::unescaped;

/// The three attributes of the ACCESS log destination, and of the CONNECTION log destination
/// beside it, from `LoadBalancerAttribute$Key`'s own catalog.
const ENABLED: &str = "access_logs.s3.enabled";
const BUCKET: &str = "access_logs.s3.bucket";
const PREFIX: &str = "access_logs.s3.prefix";
const CONNECTION_ENABLED: &str = "connection_logs.s3.enabled";
const CONNECTION_BUCKET: &str = "connection_logs.s3.bucket";
const HEALTH_CHECK_ENABLED: &str = "health_check_logs.s3.enabled";
const HEALTH_CHECK_BUCKET: &str = "health_check_logs.s3.bucket";

/// The segment the delivery itself writes into the key, which a caller's prefix may not begin
/// with, and the name Elastic Load Balancing gives the file it verifies a destination with.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";
const TEST_FILE_NAME: &str = "ELBAccessLogTestFile";

/// The awsQuery code this API answers an input value it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// This service and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the bucket a load balancer's attributes name is the bucket this suite creates, so
/// what the handler asks Amazon S3 is asked of the same store.
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

/// The Amazon S3 client this suite sets the bucket up and reads the delivered file with, built
/// by the one shared constructor so it reaches the store the handler reaches.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    service_common::s3_client::s3_client(&EmbeddedEmulator::from_router(router.clone()), REGION)
}

/// What a row needs in Amazon S3 before the attribute write.
#[derive(Debug, Clone)]
enum Setup {
    /// Amazon S3 holds no bucket under the name the attributes name.
    Absent,
    /// The bucket exists and carries no policy at all.
    NoPolicy,
    /// The bucket exists and its policy grants the log delivery `s3:PutObject` on the keys
    /// this pattern names (the `Resource` of the statement, after the bucket).
    Granting(String),
}

/// Create the bucket `bucket` and put on it whatever policy `setup` asks for.
async fn stage(router: &MultiServiceRouter, case: &str, bucket: &str, setup: &Setup) {
    if matches!(setup, Setup::Absent) {
        return;
    }
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
    let Setup::Granting(keys) = setup else {
        return;
    };
    grant_log_delivery_on(router, bucket, keys).await;
}

/// Write onto `bucket` the grant AWS documents for a log-delivery bucket, covering every key of
/// it: the setup a client does before it can turn a load balancer's logging on at all, since a
/// destination whose policy admits the delivery nothing is refused the attribute write.
///
/// `pub(crate)` because the suites that turn access logging on to assert what is DELIVERED
/// (`all/alb_lambda_target_access_log.rs`, `all/alb_https_access_log_tls.rs`) need the same
/// grant: ONE spelling of the documented policy for the crate, so no suite can drift on it.
pub(crate) async fn grant_log_delivery(router: &MultiServiceRouter, bucket: &str) {
    grant_log_delivery_on(router, bucket, "*").await;
}

/// [`grant_log_delivery`], narrowed to the keys `keys` names — which is what tells a grant
/// scoped to this destination from one scoped to another prefix of the same bucket.
async fn grant_log_delivery_on(router: &MultiServiceRouter, bucket: &str, keys: &str) {
    // The grant AWS documents for a log-delivery bucket: the delivery's service principal,
    // the object write, and the keys of this bucket it may write.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "logdelivery.elasticloadbalancing.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/{keys}"),
        }],
    })
    .to_string();
    s3(router)
        .put_bucket_policy()
        .bucket(bucket)
        .policy(policy)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("setup: granting the delivery on {bucket} must succeed, got {error:?}")
        });
}

/// One row: what Amazon S3 holds, the attributes written, and the code and sentence the write
/// is refused with — `None` for a write the destination admits.
struct Row {
    case: &'static str,
    /// The bucket the row stages, which is also the name its attributes name unless the row
    /// names none at all.
    bucket: &'static str,
    setup: Setup,
    /// The attributes the write carries, in order.
    writes: Vec<(&'static str, &'static str)>,
    refused: Option<(&'static str, String)>,
}

/// Why a bucket Amazon S3 holds nothing under is refused: the request's own value, so the
/// request is what has to be fixed.
fn absent(bucket: &str) -> (&'static str, String) {
    (VALIDATION_ERROR, format!("S3Bucket: {bucket} does not exist"))
}

/// Why a write that switches a destination on without naming a bucket is refused.
fn incomplete(bucket_key: &str, delivery: &str) -> (&'static str, String) {
    (
        VALIDATION_ERROR,
        format!("The key '{bucket_key}' must set in order to enable {delivery}"),
    )
}

/// Why a bucket whose policy admits the delivery's write nowhere under this destination's path
/// is refused: the resources cannot hold the configuration together, which is the modelled
/// refusal.
fn ungranted(bucket: &str) -> (&'static str, String) {
    (
        INVALID_CONFIGURATION,
        format!("Access Denied for bucket: {bucket}. Please check S3bucket permission"),
    )
}

/// Why a prefix opening with the segment the layout writes is refused.
fn reserved(prefix_key: &str) -> (&'static str, String) {
    (
        VALIDATION_ERROR,
        format!("The value of '{prefix_key}' cannot start with '{DELIVERED_LOGS_SEGMENT}'"),
    )
}

/// The keys of this account's own delivery folder under `prefix`, which is what a
/// destination-scoped grant names: `<prefix>/AWSLogs/<account-id>/*`.
fn delivery_keys(prefix: &str) -> String {
    let opening = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    format!("{opening}{DELIVERED_LOGS_SEGMENT}/{}/*", account())
}

/// RULE: a write that leaves a log destination switched on is accepted only if the delivery
/// could write into it; one it could not is refused `InvalidConfigurationRequest` and nothing
/// is stored.
#[tokio::test]
async fn a_log_destination_the_delivery_cannot_write_into_is_refused_before_it_is_stored() {
    let router = emulator();
    let granted_prefix = delivery_keys("edge");
    let rows: Vec<Row> = vec![
        Row {
            case: "a bucket Amazon S3 holds no bucket under",
            bucket: "elbv2-logs-absent",
            setup: Setup::Absent,
            writes: vec![(ENABLED, "true"), (BUCKET, "elbv2-logs-absent")],
            refused: Some(absent("elbv2-logs-absent")),
        },
        Row {
            case: "a bucket carrying no policy at all",
            bucket: "elbv2-logs-ungranted",
            setup: Setup::NoPolicy,
            writes: vec![(ENABLED, "true"), (BUCKET, "elbv2-logs-ungranted")],
            refused: Some(ungranted("elbv2-logs-ungranted")),
        },
        Row {
            case: "a policy granting the delivery only another prefix of the same bucket",
            bucket: "elbv2-logs-other-prefix",
            setup: Setup::Granting("elsewhere/AWSLogs/*".to_string()),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-logs-other-prefix"),
                (PREFIX, "edge"),
            ],
            refused: Some(ungranted("elbv2-logs-other-prefix")),
        },
        Row {
            case: "switched on without naming a bucket at all",
            bucket: "elbv2-logs-unnamed",
            setup: Setup::Absent,
            writes: vec![(ENABLED, "true")],
            refused: Some(incomplete(BUCKET, "access logs")),
        },
        Row {
            case: "a prefix opening with the segment the layout writes",
            bucket: "elbv2-logs-reserved",
            setup: Setup::Granting("*".to_string()),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-logs-reserved"),
                (PREFIX, "AWSLogs/edge"),
            ],
            refused: Some(reserved(PREFIX)),
        },
        Row {
            case: "a prefix that is exactly that segment",
            bucket: "elbv2-logs-reserved-bare",
            setup: Setup::Granting("*".to_string()),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-logs-reserved-bare"),
                (PREFIX, "AWSLogs"),
            ],
            refused: Some(reserved(PREFIX)),
        },
        Row {
            case: "the CONNECTION log destination, judged the same way",
            bucket: "elbv2-connection-logs-absent",
            setup: Setup::Absent,
            writes: vec![
                (CONNECTION_ENABLED, "true"),
                (CONNECTION_BUCKET, "elbv2-connection-logs-absent"),
            ],
            refused: Some(absent("elbv2-connection-logs-absent")),
        },
        Row {
            case: "the CONNECTION log destination switched on with no bucket",
            bucket: "elbv2-connection-logs-unnamed",
            setup: Setup::Absent,
            writes: vec![(CONNECTION_ENABLED, "true")],
            refused: Some(incomplete(CONNECTION_BUCKET, "connection logs")),
        },
        Row {
            case: "the HEALTH CHECK log destination switched on with no bucket",
            bucket: "elbv2-health-check-logs-unnamed",
            setup: Setup::Absent,
            writes: vec![(HEALTH_CHECK_ENABLED, "true")],
            refused: Some(incomplete(HEALTH_CHECK_BUCKET, "health check logs")),
        },
        Row {
            case: "a bucket that does not exist, named with the destination left off",
            bucket: "elbv2-logs-off",
            setup: Setup::Absent,
            writes: vec![(ENABLED, "false"), (BUCKET, "elbv2-logs-off")],
            refused: None,
        },
        Row {
            case: "a granted bucket under the prefix the attributes name",
            bucket: "elbv2-logs-granted",
            setup: Setup::Granting(granted_prefix.clone()),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-logs-granted"),
                (PREFIX, "edge"),
            ],
            refused: None,
        },
        Row {
            case: "a granted bucket with no prefix at all",
            bucket: "elbv2-logs-granted-root",
            setup: Setup::Granting("*".to_string()),
            writes: vec![(ENABLED, "true"), (BUCKET, "elbv2-logs-granted-root")],
            refused: None,
        },
        Row {
            case: "a prefix carrying that segment somewhere other than its opening",
            bucket: "elbv2-logs-granted-deep",
            setup: Setup::Granting("*".to_string()),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-logs-granted-deep"),
                (PREFIX, "edge/AWSLogs"),
            ],
            refused: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            bucket,
            setup,
            writes,
            refused,
        } = row;
        stage(&router, case, bucket, setup).await;
        let arn = create_load_balancer(&router, &format!("destination-{index}"), &[]).await;
        let before = described_attributes(&router, &arn).await;
        let reply = write_attributes(&router, &arn, writes).await;
        let stored = described_attributes(&router, &arn).await;
        match refused {
            Some((code, message)) => {
                let stated = unescaped(&reply.text("Message").unwrap_or_default());
                assert_eq!(
                    (reply.status, reply.error_code(), stated),
                    (400, (*code).to_string(), message.clone()),
                    "case {case:?}: expected 400 {code} {message:?}, got {}",
                    reply.body
                );
                assert_eq!(
                    stored, before,
                    "case {case:?}: a refused write must store nothing, got {stored:?}"
                );
            }
            None => {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: a destination the delivery can write into must be \
                     accepted, got {}",
                    reply.body
                );
                for (key, value) in writes {
                    assert_eq!(
                        stored.get(*key).map(String::as_str),
                        Some(*value),
                        "case {case:?}: {key} must read back as {value:?}, got {:?}",
                        stored.get(*key)
                    );
                }
            }
        }
    }
}

/// One row of the verification table: the bucket, where the file must land — `None` where
/// nothing must land at all — and the attributes written.
struct TestFileRow {
    case: &'static str,
    bucket: &'static str,
    expected: Option<String>,
    writes: Vec<(&'static str, &'static str)>,
}

/// RULE: a destination the write accepted is verified the way Elastic Load Balancing verifies
/// one — the test file lands in the destination's own folder, `<prefix>/AWSLogs/<account>/`,
/// and nothing lands in a bucket whose destination the write left switched off.
#[tokio::test]
async fn enabling_a_destination_writes_the_test_file_under_the_configured_prefix() {
    let router = emulator();

    // The rows differ along the prefix the destination carries, which is what decides where
    // the file lands, and along whether the destination is switched on at all.
    let rows: Vec<TestFileRow> = vec![
        TestFileRow {
            case: "a destination under a prefix",
            bucket: "elbv2-test-file-prefixed",
            expected: Some(format!(
                "edge/{DELIVERED_LOGS_SEGMENT}/{}/{TEST_FILE_NAME}",
                account()
            )),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-test-file-prefixed"),
                (PREFIX, "edge"),
            ],
        },
        TestFileRow {
            case: "a destination with no prefix",
            bucket: "elbv2-test-file-root",
            expected: Some(format!(
                "{DELIVERED_LOGS_SEGMENT}/{}/{TEST_FILE_NAME}",
                account()
            )),
            writes: vec![(ENABLED, "true"), (BUCKET, "elbv2-test-file-root")],
        },
        TestFileRow {
            case: "a destination under a nested prefix",
            bucket: "elbv2-test-file-nested",
            expected: Some(format!(
                "edge/inner/{DELIVERED_LOGS_SEGMENT}/{}/{TEST_FILE_NAME}",
                account()
            )),
            writes: vec![
                (ENABLED, "true"),
                (BUCKET, "elbv2-test-file-nested"),
                (PREFIX, "edge/inner"),
            ],
        },
        TestFileRow {
            case: "a bucket named with the destination left switched off",
            bucket: "elbv2-test-file-none",
            expected: None,
            writes: vec![(ENABLED, "false"), (BUCKET, "elbv2-test-file-none")],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let TestFileRow {
            case,
            bucket,
            expected,
            writes,
        } = row;
        stage(&router, case, bucket, &Setup::Granting("*".to_string())).await;
        let arn = create_load_balancer(&router, &format!("test-file-{index}"), &[]).await;
        let reply = write_attributes(&router, &arn, writes).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected the write to be accepted, got {}",
            reply.body
        );
        let listed = s3(&router)
            .list_objects_v2()
            .bucket(*bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: listing {bucket} must succeed, got {error:?}"));
        let keys: Vec<String> = listed
            .contents()
            .iter()
            .map(|object| object.key().unwrap_or_default().to_string())
            .collect();
        match expected {
            Some(key) => {
                assert_eq!(
                    keys,
                    vec![key.clone()],
                    "case {case:?}: expected exactly {key:?} in {bucket}, got {keys:?}"
                );
                let fetched = s3(&router)
                    .get_object()
                    .bucket(*bucket)
                    .key(key)
                    .send()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: fetching {key} must succeed, got {error:?}")
                    });
                let body = fetched
                    .body
                    .collect()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: reading {key} must succeed, got {error:?}")
                    })
                    .into_bytes();
                assert!(
                    !body.is_empty() && !body.starts_with(&[0x1f, 0x8b]),
                    "case {case:?}: the verification file is a non-empty plain-text marker and \
                     no gzip log, got {body:?}"
                );
            }
            None => assert!(
                keys.is_empty(),
                "case {case:?}: a destination left switched off writes nothing, got {keys:?}"
            ),
        }
    }
}
