// SPDX-License-Identifier: CC0-1.0

//! WHICH logging destinations a web ACL's logging configuration may name, end to
//! end over the wire.
//!
//! The rule under test: a logging destination is named, not merely addressed — the
//! name a destination carries in its ARN must begin with `aws-waf-logs-`
//! (`smithy_query(query="doc",shape="LoggingConfiguration")`: "the name that you
//! give the destination must start with `aws-waf-logs-`"), and the value that name is
//! read off has to be a well-formed ARN of the service that owns the destination, so
//! WAF refuses a `PutLoggingConfiguration` whose `LogDestinationConfigs` carries a
//! destination named otherwise, or one addressed by an ARN that leaves out a segment
//! its own form requires, with the parameter failure, writing nothing — and accepts one
//! whose destinations are all named that way.
//!
//! Each half is asserted over a TABLE spanning the class rather than the one
//! request that exposed it. The refused rows differ in the KIND of destination (a
//! CloudWatch Logs log group, an S3 bucket, a Firehose delivery stream), in HOW the
//! name fails the prefix (missing, present but not at the start, in another case,
//! carried by a segment that is not the name), in whether the value is an ARN at
//! all, and in WHERE in the list the bad destination sits. The accepted rows span
//! the same kinds, the wildcard form of a log-group ARN, and a list of several
//! destinations.
//!
//! Every call goes through the same front door a real client uses (the `Waf`
//! harness in [`crate::web_acl_lifecycle`]), so what these tests observe is what an
//! SDK observes.

use serde_json::{json, Value};

use crate::parameter_validation::{error_code, error_message};
use crate::web_acl_lifecycle::{create_web_acl, Waf};

/// The `ErrorReason` WAF carries for an ARN it will not accept, as recorded.
/// Spelled out here independently of the service's own constant, so this suite pins
/// the text and not merely its reuse.
const ARN_NOT_VALID: &str = "The ARN isn't valid. A valid ARN begins with arn: and includes \
     other information separated by colons or slashes.";

/// The `ParameterExceptionField` value WAF reports a bad destination against
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists
/// `LOG_DESTINATION` among its values).
const FIELD_LOG_DESTINATION: &str = "LOG_DESTINATION";

/// The `LoggingConfiguration` naming `destinations` in front of `resource_arn`.
fn configuration(resource_arn: &str, destinations: &[&str]) -> Value {
    json!({
        "LoggingConfiguration": {
            "ResourceArn": resource_arn,
            "LogDestinationConfigs": destinations,
        }
    })
}

/// A `GetLoggingConfiguration` request for `resource_arn`.
fn read_request(resource_arn: &str) -> Value {
    json!({ "ResourceArn": resource_arn })
}

/// One destination list a put may carry, and the destination WAF is expected to
/// report when the list is refused.
struct DestinationCase {
    case: &'static str,
    destinations: &'static [&'static str],
    /// The entry WAF refuses — the first one whose name is not acceptable.
    refused: &'static str,
}

/// The refused rows: every destination kind, every way a name can fail the prefix,
/// a value that is no ARN at all, and a bad entry behind a good one.
const REFUSED: &[DestinationCase] = &[
    DestinationCase {
        case: "a CloudWatch Logs log group whose name lacks the prefix",
        destinations: &["arn:aws:logs:us-west-2:000000000000:log-group:probe-waf-delivery"],
        refused: "arn:aws:logs:us-west-2:000000000000:log-group:probe-waf-delivery",
    },
    DestinationCase {
        case: "the same log group written with its wildcard tail",
        destinations: &["arn:aws:logs:us-west-2:000000000000:log-group:probe-waf-delivery:*"],
        refused: "arn:aws:logs:us-west-2:000000000000:log-group:probe-waf-delivery:*",
    },
    DestinationCase {
        case: "an S3 bucket whose name lacks the prefix",
        destinations: &["arn:aws:s3:::probe-waf-delivery"],
        refused: "arn:aws:s3:::probe-waf-delivery",
    },
    DestinationCase {
        case: "a Firehose delivery stream whose name lacks the prefix",
        destinations: &["arn:aws:firehose:us-west-2:000000000000:deliverystream/probe-waf-delivery"],
        refused: "arn:aws:firehose:us-west-2:000000000000:deliverystream/probe-waf-delivery",
    },
    DestinationCase {
        case: "a name that carries the prefix, but not at its start",
        destinations: &["arn:aws:s3:::probe-aws-waf-logs-delivery"],
        refused: "arn:aws:s3:::probe-aws-waf-logs-delivery",
    },
    DestinationCase {
        case: "a name that carries the prefix in another case",
        destinations: &["arn:aws:s3:::AWS-WAF-LOGS-delivery"],
        refused: "arn:aws:s3:::AWS-WAF-LOGS-delivery",
    },
    DestinationCase {
        case: "the prefix on a resource segment that is not the name",
        destinations: &["arn:aws:firehose:us-west-2:000000000000:aws-waf-logs-stream/probe"],
        refused: "arn:aws:firehose:us-west-2:000000000000:aws-waf-logs-stream/probe",
    },
    DestinationCase {
        case: "a destination that is not an ARN at all",
        destinations: &["probe-waf-delivery-destination"],
        refused: "probe-waf-delivery-destination",
    },
    DestinationCase {
        case: "an acceptable destination followed by one that is not",
        destinations: &[
            "arn:aws:s3:::aws-waf-logs-first-destination",
            "arn:aws:s3:::probe-second-destination",
        ],
        refused: "arn:aws:s3:::probe-second-destination",
    },
];

/// The accepted rows: every destination kind, the wildcard form of a log-group ARN,
/// the prefix boundary (a name that is the prefix and nothing more, and the shortest
/// name past it), and a list of several destinations.
///
/// Only the destination-NAME prefix rule is under test here, so a row's name is not
/// also judged against the rules the owning service has for its own names; and a
/// list may carry several destinations because `LogDestinationConfigs` is
/// `@length(1..100)` (`smithy_query(query="shape",shape="LogDestinationConfigs")`),
/// whose entries are each judged by the prefix rule — what a web ACL does with more
/// than one destination is the sibling [`crate::logging_configuration`] suite's
/// subject, not this one's.
const ACCEPTED: &[&[&str]] = &[
    &["arn:aws:logs:us-west-2:000000000000:log-group:aws-waf-logs-probe"],
    &["arn:aws:logs:us-west-2:000000000000:log-group:aws-waf-logs-probe:*"],
    &["arn:aws:s3:::aws-waf-logs-probe"],
    &["arn:aws:firehose:us-west-2:000000000000:deliverystream/aws-waf-logs-probe"],
    &["arn:aws:s3:::aws-waf-logs-"],
    &["arn:aws:s3:::aws-waf-logs-a"],
    &[
        "arn:aws:s3:::aws-waf-logs-one",
        "arn:aws:firehose:us-west-2:000000000000:deliverystream/aws-waf-logs-two",
    ],
];

/// A destination whose name does not begin with the prefix is refused with the
/// parameter failure, and nothing is stored.
#[tokio::test]
async fn a_destination_named_without_the_waf_logs_prefix_is_refused() {
    let waf = Waf::new();
    let (_acl_id, acl_arn, _acl_lock) =
        create_web_acl(&waf, "REGIONAL", "destination-named-acl", json!([])).await;

    for row in REFUSED {
        let (status, body) = waf
            .call(
                "PutLoggingConfiguration",
                configuration(&acl_arn, row.destinations),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 for destinations {:?}, got status {status} and body \
             {body}",
            row.case, row.destinations
        );
        assert_eq!(
            error_code(&body),
            "WAFInvalidParameterException",
            "case {:?}: expected WAFInvalidParameterException, got {body}",
            row.case
        );
        let field = body["Field"].as_str().unwrap_or_default().to_string();
        let parameter = body["Parameter"].as_str().unwrap_or_default().to_string();
        let reason = body["Reason"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            field, FIELD_LOG_DESTINATION,
            "case {:?}: expected Field {FIELD_LOG_DESTINATION}, got {field:?} in body {body}",
            row.case
        );
        assert_eq!(
            parameter, row.refused,
            "case {:?}: Parameter must echo the refused destination {:?}, got {parameter:?}",
            row.case, row.refused
        );
        assert_eq!(
            reason, ARN_NOT_VALID,
            "case {:?}: expected the ARN reason, got {reason:?} in body {body}",
            row.case
        );
        assert_eq!(
            error_message(&body),
            format!("Error reason: {reason}, field: {field}, parameter: {parameter}"),
            "case {:?}: the message must be composed from Reason, Field and Parameter, got \
             {body}",
            row.case
        );

        // A refused put wrote nothing: the web ACL still carries no configuration,
        // which is the absent case a client branches on.
        let (read_status, read_body) = waf
            .call("GetLoggingConfiguration", read_request(&acl_arn))
            .await;
        assert_eq!(
            read_status, 400,
            "case {:?}: a refused put must store nothing, got status {read_status} and body \
             {read_body}",
            row.case
        );
        assert!(
            error_code(&read_body).ends_with("WAFNonexistentItemException"),
            "case {:?}: a refused put must leave the web ACL without a configuration, got \
             {read_body}",
            row.case
        );
    }
}

/// A destination ARN that leaves out a segment its own form requires is refused as the
/// malformed ARN it is, with the field member left out — and the same ARN with that
/// segment present is accepted.
///
/// An ARN is `arn:<partition>:<service>:<region>:<account-id>:<resource>`, and which of
/// those segments may be empty belongs to the service that owns the resource: an Amazon
/// S3 bucket is addressed in a global namespace and carries neither a Region nor an
/// account id, while a CloudWatch Logs log group and a Firehose delivery stream are each
/// addressed in one account and one Region. The rows span WHICH segment is missing (the
/// account id, the Region, both) or present where that form carries none, WHICH
/// destination kind the ARN is of, and the position of the bad entry in the list; each
/// refused row is paired with the same ARN made whole, so what the row changes is the
/// segment alone.
///
/// A value WAF can read no destination NAME off at all is the sibling refusal above,
/// which names the `LOG_DESTINATION` settings: the name is the requirement the API's own
/// documentation states, so it is reported first.
///
/// The refusal carries no `Field`: what is wrong is the value rather than the settings
/// it was written in, so its message renders the field as `null` (recorded behaviour).
#[tokio::test]
async fn a_destination_arn_missing_a_required_segment_is_refused() {
    // `(case, the destination a put names, the same destination made whole)`.
    let rows: &[(&str, &str, &str)] = &[
        (
            "a delivery stream with no account id",
            "arn:aws:firehose:us-west-2::deliverystream/aws-waf-logs-probe",
            "arn:aws:firehose:us-west-2:000000000000:deliverystream/aws-waf-logs-probe",
        ),
        (
            "a delivery stream with no Region",
            "arn:aws:firehose::000000000000:deliverystream/aws-waf-logs-probe",
            "arn:aws:firehose:us-west-2:000000000000:deliverystream/aws-waf-logs-probe",
        ),
        (
            "a log group with no account id",
            "arn:aws:logs:us-west-2::log-group:aws-waf-logs-probe",
            "arn:aws:logs:us-west-2:000000000000:log-group:aws-waf-logs-probe",
        ),
        (
            "a log group with neither Region nor account id",
            "arn:aws:logs:::log-group:aws-waf-logs-probe",
            "arn:aws:logs:us-west-2:000000000000:log-group:aws-waf-logs-probe",
        ),
        (
            "a bucket with a Region an S3 bucket ARN does not carry",
            "arn:aws:s3:us-west-2::aws-waf-logs-probe",
            "arn:aws:s3:::aws-waf-logs-probe",
        ),
        (
            "an acceptable destination followed by a malformed one",
            "arn:aws:firehose:us-west-2::deliverystream/aws-waf-logs-second",
            "arn:aws:firehose:us-west-2:000000000000:deliverystream/aws-waf-logs-second",
        ),
    ];

    let waf = Waf::new();
    let (_acl_id, acl_arn, _acl_lock) =
        create_web_acl(&waf, "REGIONAL", "destination-arn-acl", json!([])).await;

    for (case, malformed, whole) in rows {
        // The last row puts the malformed entry behind an acceptable one, so the list
        // the put names depends on the row.
        let named: Vec<&str> = if case.starts_with("an acceptable destination") {
            vec!["arn:aws:s3:::aws-waf-logs-first", malformed]
        } else {
            vec![malformed]
        };
        let (status, body) = waf
            .call(
                "PutLoggingConfiguration",
                configuration(&acl_arn, &named),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for destinations {named:?}, got status {status} \
             and body {body}"
        );
        assert_eq!(
            error_code(&body),
            "WAFInvalidParameterException",
            "case {case:?}: expected WAFInvalidParameterException, got {body}"
        );
        assert!(
            body["Field"].is_null(),
            "case {case:?}: a malformed ARN carries no field, got {body}"
        );
        assert_eq!(
            body["Parameter"].as_str(),
            Some(*malformed),
            "case {case:?}: Parameter must echo the rejected destination, got {body}"
        );
        assert_eq!(
            body["Reason"].as_str(),
            Some(ARN_NOT_VALID),
            "case {case:?}: expected the ARN reason, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Error reason: {ARN_NOT_VALID}, field: null, parameter: {malformed}"),
            "case {case:?}: the message must render the absent field as null, got {body}"
        );

        // The same ARN with the missing segment present is a destination WAF delivers
        // to, so the row's refusal is about that segment and nothing else.
        let put = waf
            .ok("PutLoggingConfiguration", configuration(&acl_arn, &[whole]))
            .await;
        assert_eq!(
            put["LoggingConfiguration"]["LogDestinationConfigs"],
            json!([whole]),
            "case {case:?}: the whole ARN must be stored, got {put}"
        );
        waf.ok("DeleteLoggingConfiguration", read_request(&acl_arn))
            .await;
    }
}

/// A destination whose name begins with the prefix is accepted and read back, for
/// every destination kind WAF delivers to.
#[tokio::test]
async fn a_destination_named_with_the_waf_logs_prefix_is_stored() {
    let waf = Waf::new();
    let (_acl_id, acl_arn, _acl_lock) =
        create_web_acl(&waf, "REGIONAL", "destination-prefixed-acl", json!([])).await;

    for destinations in ACCEPTED {
        let put = waf
            .ok(
                "PutLoggingConfiguration",
                configuration(&acl_arn, destinations),
            )
            .await;
        assert_eq!(
            put["LoggingConfiguration"]["LogDestinationConfigs"],
            json!(destinations),
            "case {destinations:?}: the put must answer the destinations as given, got {put}"
        );
        let got = waf
            .ok("GetLoggingConfiguration", read_request(&acl_arn))
            .await;
        assert_eq!(
            got["LoggingConfiguration"]["LogDestinationConfigs"],
            json!(destinations),
            "case {destinations:?}: the read must answer the stored destinations, got {got}"
        );
        waf.ok("DeleteLoggingConfiguration", read_request(&acl_arn))
            .await;
    }
}

/// A put refused for its destinations leaves the configuration the web ACL already
/// carried exactly as it was: the refusal replaces nothing.
#[tokio::test]
async fn a_refused_put_leaves_the_stored_configuration_alone() {
    let waf = Waf::new();
    let (_acl_id, acl_arn, _acl_lock) =
        create_web_acl(&waf, "REGIONAL", "destination-replaced-acl", json!([])).await;
    let stored: &[&str] = ACCEPTED[0];
    waf.ok("PutLoggingConfiguration", configuration(&acl_arn, stored))
        .await;

    for row in REFUSED {
        let (status, body) = waf
            .call(
                "PutLoggingConfiguration",
                configuration(&acl_arn, row.destinations),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got status {status} and body {body}",
            row.case
        );
        let got = waf
            .ok("GetLoggingConfiguration", read_request(&acl_arn))
            .await;
        assert_eq!(
            got["LoggingConfiguration"]["LogDestinationConfigs"],
            json!(stored),
            "case {:?}: a refused put must leave the stored destinations in place, got {got}",
            row.case
        );
    }
}
