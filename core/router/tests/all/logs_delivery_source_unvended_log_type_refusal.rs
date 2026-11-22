// SPDX-License-Identifier: CC0-1.0

//! A delivery source's `logType` must be a log type the SENDING resource's service sends, and
//! the refusal of one that is not names the service/log-source mismatch.
//!
//! RULE: `logType` "defines the type of log that the source is sending", so which values are
//! legal is a property of the service whose resource sends them — the `logType` member's own
//! catalogue lists them per service, the same catalogue `DescribeConfigurationTemplates`
//! publishes — and a `PutDeliverySource` naming a type that resource's service does not send is
//! refused 400 `ValidationException` and stores nothing. Which sentence the refusal carries
//! turns on whether the type names a log source of the delivery plane at all: a type SOME other
//! service sends is a log source whose SENDING SERVICE is what is not allowed, so the refusal is
//! the fixed sentence `This service is not allowed for this LogSource.` with no variable part;
//! a value no service sends is no log source, so the refusal states that the provided log type
//! is not valid and lists the types that service does send.
//!
//! This suite exercises the rule over a resource its owning service really HOLDS: the ARN is a
//! distribution CloudFront minted in this emulator, created here through CloudFront's own API,
//! so the put gets past the resolution of the resource against its owner and is judged on its
//! log type alone. That needs CloudWatch Logs and CloudFront on one engine (CloudWatch Logs
//! reaches CloudFront through the in-process callback), which is why it lives here rather than
//! in the CloudWatch Logs crate, where the ARNs are of resources no mounted service holds.
//! Amazon S3 is mounted beside them because the created distribution's origin is a bucket.
//!
//! The rows differ along what the rule generalises over: the type the sending service does send;
//! log types OTHER services in the catalogue send, taken from several of its senders (regional
//! and global, single-type and multi-type rows); values that name no log source at all, at the
//! published bounds of `LogType` (`@length` 1..255, `@pattern ^[\w]*$`) and one past its
//! maximum; and a value differing from the legal one only in case.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_oac_s3_origin_edge::{
    create_bucket, create_distribution, create_origin_access_control,
};
use crate::logs_delivery_source_cloudfront_resource::{error_code, logs};

/// The AWS-managed cache policy `Managed-CachingDisabled`, named on the distribution's default
/// behaviour so the create needs nothing else configured.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The bucket the created distribution's origin names.
const BUCKET: &str = "logs-unvended-log-type-origin";

/// The one log type CloudFront sends ("For CloudFront, the valid value is ACCESS_LOGS",
/// `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`).
const VENDED: &str = "ACCESS_LOGS";

/// The refusals the operations below model for these cases.
const VALIDATION: &str = "ValidationException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole message a log type ANOTHER service sends is refused with: entirely fixed, naming
/// neither the type, nor the resource, nor the service, nor the types that service does send.
const SERVICE_NOT_ALLOWED: &str = "This service is not allowed for this LogSource.";

/// The whole message a value that is no log source at all is refused with: the fixed sentence,
/// and the types the sending service does send as its one variable part.
fn not_a_valid_log_type(supported: &[&str]) -> String {
    format!(
        "Provided LogType is not valid. Supported options are [{}].",
        supported.join(", ")
    )
}

/// Log types other senders in the catalogue send, so each is a log source of this delivery plane
/// that CloudFront does not send. Source: the `logType` member of `PutDeliverySourceRequest`
/// (`smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`) — "For Amazon
/// EventBridge Event Bus (Custom), the valid values are INFO_LOGS and ERROR_LOGS", "For Amazon
/// Web Services Amplify, the valid values are ACCESS_LOGS and WAF_LOGS", "For Application Load
/// Balancer, the valid values are ALB_ACCESS_LOGS, ALB_CONNECTION_LOGS, and
/// ALB_HEALTH_CHECK_LOGS", "For Amazon Web Services Shield Advanced, the valid value is
/// FLOW_LOGS", "For Amazon S3, the valid value is S3_SERVER_ACCESS_LOGS", "For Route 53 Global
/// Resolver, the valid value is GLOBAL_RESOLVER_LOGS".
const SENT_BY_OTHER_SERVICES: [&str; 7] = [
    "ERROR_LOGS",
    "INFO_LOGS",
    "WAF_LOGS",
    "ALB_ACCESS_LOGS",
    "FLOW_LOGS",
    "S3_SERVER_ACCESS_LOGS",
    "GLOBAL_RESOLVER_LOGS",
];

/// What a put naming one of the rows below must answer.
#[derive(Debug)]
enum Expected {
    /// The type the sending service sends: stored, and read back as stored.
    Stored,
    /// Refused with exactly this message.
    Refused(String),
    /// Refused as a `ValidationException` whose wording is the modelled constraint's rather than
    /// this rule's, so only the class and that nothing was stored are asserted.
    RefusedOutsideTheModelledBounds,
}

#[tokio::test]
async fn a_log_type_cloudfront_does_not_send_is_refused_as_the_service_not_being_allowed() {
    let router = emulator();

    let case = "a distribution CloudFront holds";
    create_bucket(&router, case, BUCKET).await;
    let control =
        create_origin_access_control(&router, case, "oac-unvended-log-type", "always").await;
    let (_id, _domain, resource_arn) = create_distribution(
        &router,
        case,
        "unvended-log-type-source",
        BUCKET,
        &control,
        CACHING_DISABLED,
    )
    .await;

    let not_valid = not_a_valid_log_type(&[VENDED]);
    let mut rows: Vec<(String, String, Expected)> = vec![(
        "the log type CloudFront sends".to_string(),
        VENDED.to_string(),
        Expected::Stored,
    )];
    rows.extend(SENT_BY_OTHER_SERVICES.iter().map(|log_type| {
        (
            format!("{log_type}, a log type another service sends"),
            (*log_type).to_string(),
            Expected::Refused(SERVICE_NOT_ALLOWED.to_string()),
        )
    }));
    rows.extend(
        [
            ("a value no service sends", "NOT_A_LOG_TYPE".to_string()),
            // A log type is matched exactly, so a value differing from the legal one only in
            // case names no log source either.
            ("the legal type in lower case", VENDED.to_lowercase()),
            ("the shortest value the member admits", "L".to_string()),
            ("the longest value the member admits", "L".repeat(255)),
        ]
        .into_iter()
        .map(|(case, log_type)| {
            (
                case.to_string(),
                log_type,
                Expected::Refused(not_valid.clone()),
            )
        }),
    );
    rows.push((
        "one character past the member's maximum length".to_string(),
        "L".repeat(256),
        Expected::RefusedOutsideTheModelledBounds,
    ));

    for (index, (case, log_type, expected)) in rows.iter().enumerate() {
        let name = format!("log-type-{index}");
        let (status, answered) = logs(
            &router,
            "PutDeliverySource",
            &json!({
                "name": name,
                "resourceArn": resource_arn,
                "logType": log_type,
            }),
        )
        .await;
        let (read_status, read) = logs(&router, "GetDeliverySource", &json!({"name": name})).await;

        match expected {
            Expected::Stored => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the put to be accepted, got {status} {answered}"
                );
                assert_eq!(
                    read["deliverySource"]["logType"].as_str(),
                    Some(log_type.as_str()),
                    "case {case:?}: expected the stored source to send {log_type:?}, got {read}"
                );
                assert_eq!(
                    read["deliverySource"]["service"].as_str(),
                    Some("cloudfront"),
                    "case {case:?}: expected the stored source's service to be the sending \
                     resource's, got {read}"
                );
                assert_eq!(
                    read["deliverySource"]["resourceArns"],
                    json!([resource_arn]),
                    "case {case:?}: expected the stored source to represent \
                     {resource_arn:?}, got {read}"
                );
            }
            Expected::Refused(message) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the put to be refused, got {status} {answered}"
                );
                let code = error_code(&answered);
                assert_eq!(
                    code, VALIDATION,
                    "case {case:?}: expected {VALIDATION}, got {code} {answered}"
                );
                assert_eq!(
                    answered["message"].as_str(),
                    Some(message.as_str()),
                    "case {case:?}: expected the message {message:?}, got {answered}"
                );
                assert_refused_put_stored_nothing(case, read_status, &read);
            }
            Expected::RefusedOutsideTheModelledBounds => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the put to be refused, got {status} {answered}"
                );
                let code = error_code(&answered);
                assert_eq!(
                    code, VALIDATION,
                    "case {case:?}: expected {VALIDATION}, got {code} {answered}"
                );
                assert_refused_put_stored_nothing(case, read_status, &read);
            }
        }
    }
}

/// A refused put stores nothing, so the name it named holds no delivery source afterwards.
fn assert_refused_put_stored_nothing(case: &str, read_status: u16, read: &Value) {
    assert_eq!(
        read_status, 400,
        "case {case:?}: a refused put stores nothing, got {read_status} {read}"
    );
    let code = error_code(read);
    assert_eq!(
        code, NOT_FOUND,
        "case {case:?}: a refused put stores nothing, so the read expects {NOT_FOUND}, \
         got {code} {read}"
    );
}

/// The catalogue the refusal is grounded in is the one this service publishes: the configuration
/// templates for the sending service name the log type it sends and none of the types other
/// services send.
#[tokio::test]
async fn the_published_catalogue_names_only_the_log_type_cloudfront_sends() {
    let router = emulator();

    let (status, answered) = logs(
        &router,
        "DescribeConfigurationTemplates",
        &json!({"service": "cloudfront"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeConfigurationTemplates must be accepted, got {status} {answered}"
    );
    let templates = answered["configurationTemplates"].as_array().unwrap_or_else(|| {
        panic!("DescribeConfigurationTemplates must report configurationTemplates: {answered}")
    });
    let published: Vec<&str> = templates
        .iter()
        .filter_map(|template| template["logType"].as_str())
        .collect();
    assert!(
        published.contains(&VENDED),
        "case {VENDED:?}: the published catalogue must name the log type the service sends, \
         got {published:?}"
    );
    for log_type in SENT_BY_OTHER_SERVICES {
        assert!(
            !published.contains(&log_type),
            "case {log_type:?}: the published catalogue must not name a log type another \
             service sends, got {published:?}"
        );
    }
}

/// CloudWatch Logs, CloudFront and Amazon S3 on one engine exactly as the router mounts them,
/// with request authentication off: each call carries a credential scope (which is what names
/// the Region) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}
