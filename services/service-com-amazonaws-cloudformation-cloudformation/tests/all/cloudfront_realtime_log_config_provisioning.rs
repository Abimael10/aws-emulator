// SPDX-License-Identifier: CC0-1.0

//! A stack's declared `AWS::CloudFront::RealtimeLogConfig` is a real-time log configuration
//! Amazon CloudFront really holds, addressable by the ARN the stack publishes for it.
//!
//! The rule under test: **a resource type CloudFormation publishes is one a template may
//! declare — the `Template format error: Unrecognized resource types: [<types>]` refusal is
//! reserved for a type CloudFormation does not know — and a declared type this emulator
//! provisions is created by calling the OWNING service's own create, so the physical resource
//! id the stack publishes is the identifier that service minted, its `Fn::GetAtt` values are
//! the ones that service reports, a declaration its type's SCHEMA does not admit fails the
//! RESOURCE before anything is asked of that service (and the stack rolls back), and the
//! stack's delete takes the resource away there.** For a real-time log configuration the
//! create is `CreateRealtimeLogConfig` (`POST /2020-05-31/realtime-log-config`), the read is
//! `GetRealtimeLogConfig` (`POST /2020-05-31/get-realtime-log-config`), which answers 200 with
//! the stored `RealtimeLogConfig`, and `NoSuchRealtimeLogConfig` (HTTP 404) is modelled ONLY
//! for an identifier that names no configuration — so a 404 for an ARN CloudFormation itself
//! reported is a defect.
//!
//! The rows differ along what the rule generalises over: the sampling rate at each published
//! bound and between them, one endpoint and several, one field and many in an order that is not
//! the sorted one, a field the service's own catalog carries and one it does not, and names of
//! several forms. Every expected value is computed here from what the row declares, never read
//! back out of the answer being checked.
//!
//! The Kinesis data stream each endpoint sends to and the role CloudFront writes to it with are
//! declared by the SAME template and named by `Fn::GetAtt`, so the configuration CloudFront
//! stores is bound to a stream Amazon Kinesis Data Streams really holds and a role IAM really
//! holds rather than to hand-written ARNs.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation, awsJson for
//! Amazon Kinesis Data Streams, restXml for Amazon CloudFront — through the one router
//! `harness.rs` builds, which mounts them all on one engine.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    failed_resource_reason, physical_resource_id, quoted_and_class, CLOUDFRONT_API_VERSION,
    CLOUDFRONT_HOST,
};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router, settled_stack, xml_blocks, xml_text,
    xml_texts, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The resource segment a real-time log configuration's ARN carries, as the type's own "Return
/// values" section writes one
/// (`arn:aws:cloudfront::111122223333:realtime-log-config/ExampleNameForRealtimeLogConfig`).
/// CloudFront is a GLOBAL service, so the Region segment of the ARN is empty.
const REALTIME_LOG_CONFIG_ARN_TYPE: &str = "realtime-log-config";

/// The only `StreamType` an endpoint admits: "The type of data stream where you are sending
/// real-time log data. The only valid value is `Kinesis`"
/// (`smithy_query(query="shape",shape="EndPoint")`).
const KINESIS_STREAM_TYPE: &str = "Kinesis";

/// The two ends of the range CloudFront publishes for the sampling rate: "You can specify a
/// whole number between 1 and 100 (inclusive) to determine the percentage of viewer requests
/// that are represented in the real-time log data"
/// (`smithy_query(query="doc",shape="CreateRealtimeLogConfigRequest",member="SamplingRate")`).
const MIN_SAMPLING_RATE: i64 = 1;
const MAX_SAMPLING_RATE: i64 = 100;

/// The `HandlerErrorCode` class a provider reports a declaration its type's schema does not
/// admit under — a value of the model's own `HandlerErrorCode` enum: what the provider refused
/// is the REQUEST it was handed.
const MODEL_VALIDATION_CLASS: &str = "InvalidRequest";

/// The resource status a refused create leaves the resource in.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The error code CloudFront answers for an identifier no configuration answers to.
const NO_SUCH_REALTIME_LOG_CONFIG: &str = "NoSuchRealtimeLogConfig";

// ---------------------------------------------------------------------------
// The declarations under test
// ---------------------------------------------------------------------------

/// One endpoint a row's configuration sends its records to: the logical id of the
/// `AWS::Kinesis::Stream` the same template declares, and the name that stream is created
/// under.
#[derive(Debug, Clone, Copy)]
struct Stream {
    logical_id: &'static str,
    name: &'static str,
}

/// What a row's `AWS::CloudFront::RealtimeLogConfig` declares.
#[derive(Debug, Clone)]
struct Declared {
    case: &'static str,
    stack: &'static str,
    /// `RealtimeLogConfig$Name`: the unique name the account knows the configuration by, which
    /// is also what its ARN is built around.
    name: &'static str,
    /// `RealtimeLogConfig$SamplingRate`.
    sampling_rate: i64,
    /// The fields each record carries, in the order the declaration writes them.
    fields: &'static [&'static str],
    /// The streams the records go to, one endpoint each.
    streams: &'static [Stream],
}

impl Declared {
    /// The ARN CloudFront mints for a configuration of this name, which is this resource's
    /// identity: its physical resource id, what `Ref` returns and what `Fn::GetAtt Arn` reads.
    fn arn(&self) -> String {
        format!(
            "arn:aws:cloudfront::{ACCOUNT_ID}:{REALTIME_LOG_CONFIG_ARN_TYPE}/{}",
            self.name
        )
    }

    /// The whole template this row deploys: the role CloudFront writes with, the data streams
    /// the records go to, the configuration itself, and the outputs a client reads the
    /// deployment's identifiers out of.
    fn template(&self) -> String {
        let mut resources = ::serde_json::Map::new();
        resources.insert(
            "LogRole".to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {"AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"Service": "cloudfront.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }],
                }},
            }),
        );
        for stream in self.streams {
            resources.insert(
                stream.logical_id.to_string(),
                json!({
                    "Type": "AWS::Kinesis::Stream",
                    "Properties": {"Name": stream.name, "ShardCount": 1},
                }),
            );
        }
        resources.insert(
            LOGICAL_ID.to_string(),
            json!({"Type": REALTIME_LOG_CONFIG_TYPE, "Properties": self.properties()}),
        );
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": Value::Object(resources),
            "Outputs": {
                "RoleArn": {"Value": {"Fn::GetAtt": ["LogRole", "Arn"]}},
                "ConfigRef": {"Value": {"Ref": LOGICAL_ID}},
                "ConfigArn": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Arn"]}},
            },
        })
        .to_string()
    }

    /// The declared properties of the configuration: the four members the type publishes,
    /// with each endpoint naming the stream and the role the same template declares.
    fn properties(&self) -> Value {
        let end_points: Vec<Value> = self
            .streams
            .iter()
            .map(|stream| {
                json!({
                    "StreamType": KINESIS_STREAM_TYPE,
                    // The property spellings the resource reference publishes for a
                    // `KinesisStreamConfig` (`RoleArn`, `StreamArn`), which the API takes as
                    // `RoleARN` and `StreamARN`.
                    "KinesisStreamConfig": {
                        "RoleArn": {"Fn::GetAtt": ["LogRole", "Arn"]},
                        "StreamArn": {"Fn::GetAtt": [stream.logical_id, "Arn"]},
                    },
                })
            })
            .collect();
        json!({
            "Name": self.name,
            "SamplingRate": self.sampling_rate,
            "Fields": self.fields,
            "EndPoints": end_points,
        })
    }
}

/// The logical id every row declares its configuration under, and the type it declares.
const LOGICAL_ID: &str = "EdgeLogs";
const REALTIME_LOG_CONFIG_TYPE: &str = "AWS::CloudFront::RealtimeLogConfig";

/// The declarations the rule above is asserted over.
fn declarations() -> Vec<Declared> {
    vec![
        Declared {
            case: "the published minimum sampling rate, one endpoint, one field",
            stack: "cfn-rtl-minimum",
            name: "cfn-rtl-minimum",
            sampling_rate: MIN_SAMPLING_RATE,
            fields: &["timestamp"],
            streams: &[Stream {
                logical_id: "PrimaryStream",
                name: "cfn-rtl-minimum-primary",
            }],
        },
        Declared {
            case: "the published maximum sampling rate, two endpoints, many unsorted fields",
            stack: "cfn-rtl-maximum",
            name: "cfn_rtl.maximum-2",
            sampling_rate: MAX_SAMPLING_RATE,
            // Field names the service's own catalog carries, written in an order that is
            // neither the sorted one nor the catalog's.
            fields: &[
                "x-edge-result-type",
                "timestamp",
                "cs-uri-stem",
                "sc-status",
                "c-ip",
            ],
            streams: &[
                Stream {
                    logical_id: "PrimaryStream",
                    name: "cfn-rtl-maximum-primary",
                },
                Stream {
                    logical_id: "SecondaryStream",
                    name: "cfn-rtl-maximum-secondary",
                },
            ],
        },
        Declared {
            case: "a rate between the bounds, a field name the catalog does not carry",
            stack: "cfn-rtl-middle",
            name: "cfn-rtl-middle",
            sampling_rate: 37,
            fields: &["timestamp", "x-sim2real-field"],
            streams: &[Stream {
                logical_id: "PrimaryStream",
                name: "cfn-rtl-middle-primary",
            }],
        },
    ]
}

// ---------------------------------------------------------------------------
// Calling Amazon CloudFront
// ---------------------------------------------------------------------------

/// One CloudFront request carrying a request DOCUMENT, over the restXml wire that service
/// speaks: the real-time log configuration surface binds no member to the URI, so every call on
/// one is a `POST` whose body names the configuration.
async fn cloudfront_post(router: &MultiServiceRouter, path: &str, body: &str) -> (u16, String) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/cloudfront/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{CLOUDFRONT_HOST}{path}"))
        .header("host", CLOUDFRONT_HOST)
        .header("content-type", "application/xml")
        .header("authorization", authorization)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed CloudFront request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}

/// `POST /2020-05-31/get-realtime-log-config`, addressing the configuration by the ARN the
/// stack published for it.
async fn get_realtime_log_config(router: &MultiServiceRouter, arn: &str) -> (u16, String) {
    let body = format!(
        "<GetRealtimeLogConfigRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{CLOUDFRONT_API_VERSION}/\">\
         <ARN>{arn}</ARN></GetRealtimeLogConfigRequest>"
    );
    cloudfront_post(
        router,
        &format!("/{CLOUDFRONT_API_VERSION}/get-realtime-log-config"),
        &body,
    )
    .await
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Every declared configuration is accepted, EXISTS in Amazon CloudFront under the physical
/// resource id the stack reports for it, carries what the template declared bound to the stream
/// and the role the same stack created, and goes away with the stack.
#[tokio::test]
async fn declared_realtime_log_configs_are_created_read_and_deleted_in_cloudfront() {
    let router = router();

    for declared in declarations() {
        let case = declared.case;
        let (code, body) = create_stack(
            &router,
            declared.stack,
            &declared.template(),
            // The declared role is an IAM resource whose name CloudFormation generates, which
            // `CAPABILITY_IAM` alone acknowledges.
            &[member("Capabilities.member.1", "CAPABILITY_IAM")],
        )
        .await;
        // The issue's own observable: a template declaring the type must not be refused as a
        // template whose resource types CloudFormation does not recognise.
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted, got {code} {body}"
        );
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: CreateStack must answer the stack's id, got {body}"
        );

        let described = settled_stack(&router, case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {described}"
        );

        let expected_arn = declared.arn();
        let published =
            physical_resource_id(&router, case, declared.stack, LOGICAL_ID).await;
        assert_eq!(
            published, expected_arn,
            "case {case:?}: expected the ARN CloudFront mints for the declared name as the \
             physical resource id, got {published:?}"
        );
        for (key, expected) in [
            ("ConfigRef", expected_arn.as_str()),
            ("ConfigArn", expected_arn.as_str()),
        ] {
            let value = output_value(&described, key).unwrap_or_else(|| {
                panic!("case {case:?}: no {key:?} output, got {described}")
            });
            assert_eq!(
                value, expected,
                "case {case:?}: the {key:?} output must be the configuration's ARN, got {value}"
            );
        }

        // The read CloudFormation's physical id is FOR.
        let (code, reported) = get_realtime_log_config(&router, &published).await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetRealtimeLogConfig({published}) must answer 200, got {code} \
             {reported}"
        );
        assert_declared_configuration(case, &declared, &described, &reported);

        // The teardown: the stack's delete takes the configuration away in CloudFront.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", &stack_id)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack must answer 200, got {code} {body}"
        );
        let described = settled_stack(&router, case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {case:?}: the teardown must reach DELETE_COMPLETE, got {described}"
        );

        let (code, answered) = get_realtime_log_config(&router, &published).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected the configuration to be gone with the stack, got {code} \
             {answered}"
        );
        assert_eq!(
            xml_text(&answered, "Code").as_deref(),
            Some(NO_SUCH_REALTIME_LOG_CONFIG),
            "case {case:?}: expected {NO_SUCH_REALTIME_LOG_CONFIG} once the stack deleted it, \
             got {answered}"
        );
    }
}

/// Assert the `RealtimeLogConfig` document reports what `declared` states, with each endpoint
/// bound to the stream the same stack created and to the role it published.
fn assert_declared_configuration(
    case: &str,
    declared: &Declared,
    described: &str,
    reported: &str,
) {
    let config = xml_blocks(reported, "RealtimeLogConfig")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RealtimeLogConfig, got {reported}"));
    assert_eq!(
        xml_text(&config, "Name").as_deref(),
        Some(declared.name),
        "case {case:?}: the configuration must carry the declared Name, got {config}"
    );
    assert_eq!(
        xml_text(&config, "ARN").as_deref(),
        Some(declared.arn().as_str()),
        "case {case:?}: the configuration must report the ARN the stack published, got {config}"
    );
    assert_eq!(
        xml_text(&config, "SamplingRate").as_deref(),
        Some(declared.sampling_rate.to_string().as_str()),
        "case {case:?}: the configuration must carry the declared SamplingRate, got {config}"
    );
    // The field SET, not its order: "In an API response, the fields are provided in the same
    // order in which they are sent to the Amazon Kinesis data stream"
    // (`smithy_query(query="doc",shape="RealtimeLogConfig",member="Fields")`), so the ORDER a
    // read reports is the owning service's own rather than the declaration's.
    let held: BTreeSet<String> = xml_texts(&config, "Field").into_iter().collect();
    let asked: BTreeSet<String> = declared
        .fields
        .iter()
        .map(|field| (*field).to_string())
        .collect();
    assert_eq!(
        held, asked,
        "case {case:?}: the configuration must include exactly the declared fields, got {config}"
    );

    // Each endpoint names the stream the stack created there and the role it published: the
    // ARNs come from the stack's own outputs and the streams' physical ids, so nothing here is
    // read out of the answer being checked.
    let role_arn = output_value(described, "RoleArn")
        .unwrap_or_else(|| panic!("case {case:?}: no RoleArn output, got {described}"));
    let endpoints = xml_blocks(&config, "member");
    assert_eq!(
        endpoints.len(),
        declared.streams.len(),
        "case {case:?}: expected one endpoint per declared stream, got {config}"
    );
    for (endpoint, stream) in endpoints.iter().zip(declared.streams.iter()) {
        assert_eq!(
            xml_text(endpoint, "StreamType").as_deref(),
            Some(KINESIS_STREAM_TYPE),
            "case {case:?}: the endpoint must carry the declared StreamType, got {endpoint}"
        );
        assert_eq!(
            xml_text(endpoint, "RoleARN").as_deref(),
            Some(role_arn.as_str()),
            "case {case:?}: the endpoint must name the role the stack created, got {endpoint}"
        );
        let expected_stream = format!(
            "arn:aws:kinesis:{REGION}:{ACCOUNT_ID}:stream/{}",
            stream.name
        );
        assert_eq!(
            xml_text(endpoint, "StreamARN").as_deref(),
            Some(expected_stream.as_str()),
            "case {case:?}: the endpoint must name the stream the stack created, got {endpoint}"
        );
    }
}

/// One declaration whose sampling rate is outside the published range, the stack it is deployed
/// as, and the bound keyword the provider's validator reports for it.
#[derive(Debug, Clone, Copy)]
struct OutOfRange {
    case: &'static str,
    stack: &'static str,
    name: &'static str,
    sampling_rate: i64,
    /// The JSON-schema keyword the declared value broke, as the type's schema states the range
    /// with (`minimum`, `maximum`).
    keyword: &'static str,
}

/// A sampling rate past either published bound is refused where the RESOURCE is provisioned —
/// its own provider's model validation against the type's schema — rather than by template
/// parsing: `CreateStack` is accepted, the resource reaches `CREATE_FAILED` carrying the
/// validator's own sentence under the request class, the stack rolls back, and nothing is left
/// in CloudFront under the ARN the declaration asked for.
#[tokio::test]
async fn a_sampling_rate_past_a_published_bound_fails_the_resource_and_rolls_the_stack_back() {
    // One row past each end of the range the type's schema publishes for the property.
    let rows = [
        OutOfRange {
            case: "one above the published maximum",
            stack: "cfn-rtl-over-maximum",
            name: "cfn-rtl-over-maximum",
            sampling_rate: MAX_SAMPLING_RATE + 1,
            keyword: "maximum",
        },
        OutOfRange {
            case: "one below the published minimum",
            stack: "cfn-rtl-under-minimum",
            name: "cfn-rtl-under-minimum",
            sampling_rate: MIN_SAMPLING_RATE - 1,
            keyword: "minimum",
        },
    ];

    for row in rows {
        let router = router();
        let declared = Declared {
            case: row.case,
            stack: row.stack,
            name: row.name,
            sampling_rate: row.sampling_rate,
            fields: &["timestamp"],
            streams: &[Stream {
                logical_id: "PrimaryStream",
                name: "cfn-rtl-refused-primary",
            }],
        };
        let case = row.case;
        // The sentence the provider's schema validator words a broken bound in: the location of
        // the property in the document it validated, and the name of the constraint that
        // failed — nothing of the value that was sent.
        let sentence = format!(
            "Model validation failed (#/SamplingRate: failed validation constraint for keyword \
             [{}])",
            row.keyword
        );

        let (code, body) = create_stack(
            &router,
            row.stack,
            &declared.template(),
            &[member("Capabilities.member.1", "CAPABILITY_IAM")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the template is well formed, so CreateStack must be accepted — the \
             rate is the resource's to refuse — got {code} {body}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused create to be rolled back, got {described}"
        );

        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, LOGICAL_ID, CREATE_FAILED)
                .await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_eq!(
            quoted, sentence,
            "case {case:?}: expected the validator's sentence, got {quoted:?}"
        );
        assert_eq!(
            class, MODEL_VALIDATION_CLASS,
            "case {case:?}: expected the {MODEL_VALIDATION_CLASS} class for a declaration the \
             provider refused, got {class:?} in {reason:?}"
        );

        // Nothing is left behind: the rollback took away what the failed create had made, and
        // the configuration the declaration asked for was never created.
        let (code, answered) = get_realtime_log_config(&router, &declared.arn()).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected CloudFront to hold no configuration under the declared \
             name, got {code} {answered}"
        );
    }
}

/// The complement, so the refusal the issue reported is still the refusal for a type
/// CloudFormation really does not know: a template declaring one is answered `ValidationError`
/// with `Template format error: Unrecognized resource types: [<types>]` and creates nothing.
#[tokio::test]
async fn a_cloudfront_type_cloudformation_does_not_publish_is_still_unrecognized() {
    let router = router();
    // Rows: a misspelling of the type this suite deploys, and a CloudFront resource
    // CloudFormation publishes no type for at all.
    for (case, spelled) in [
        (
            "a misspelling of the published type",
            "AWS::CloudFront::RealTimeLogConfig",
        ),
        (
            "a CloudFront resource with no type",
            "AWS::CloudFront::RealtimeLogConfigNothing",
        ),
    ] {
        let template = json!({
            "Resources": {LOGICAL_ID: {"Type": spelled, "Properties": {}}}
        })
        .to_string();
        let (code, body) = create_stack(&router, "cfn-rtl-unknown", &template, &[]).await;
        assert_eq!(
            code, 400,
            "case {case:?}: an unrecognized type must be refused, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {case:?}: the refusal must be a ValidationError, got {body}"
        );
        assert_eq!(
            xml_text(&body, "Message").as_deref(),
            Some(
                format!("Template format error: Unrecognized resource types: [{spelled}]")
                    .as_str()
            ),
            "case {case:?}: the refusal must name the type that is not recognised, got {body}"
        );
    }
}
