// SPDX-License-Identifier: CC0-1.0

//! A `CreateDeliveryStream` carrying a source configuration of another stream type.
//!
//! The rule under test: a Firehose stream's `DeliveryStreamType` says what feeds it, and
//! each type is configured by exactly ONE of the create's source-configuration members
//! (`DirectPutSourceConfiguration`, `KinesisStreamSourceConfiguration`,
//! `MSKSourceConfiguration`, `DatabaseSourceConfiguration` — independent optional members
//! between which `smithy_query(query="operation",operation="CreateDeliveryStream")` states
//! no exclusion). A source configuration belonging to any OTHER type configures no part of
//! the stream being made, so the create ignores it in full — it is not an argument fault,
//! it contributes no `Source` to the stream's description, and the role it names is not one
//! Firehose resolves — while the stream is still built from the configuration its declared
//! type names, whose absence remains an argument fault.
//!
//! This suite needs IAM beside Firehose on one engine: "ignored in full" includes the
//! `RoleARN` the foreign configuration carries, and whether a role is resolved at all is
//! only observable against the IAM store Firehose asks (`GetRole` over the in-process
//! callback). A single-service router has no IAM to ask, which is why this lives here
//! rather than in the Firehose crate, beside [`crate::firehose_destination_role`].
//!
//! The rows span what the rule generalises over: WHICH source configuration is the foreign
//! one, how many foreign ones a create carries at once, which type is declared (every value
//! of `DeliveryStreamType`, plus the member left absent), whether the declared type's own
//! configuration is present, whether the foreign configuration's role exists in IAM, and
//! two destination members — so no row's outcome can come from its destination.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// A generous deadline for a stream to reach the status the clock owes it, and the pause
/// between two looks. The suite waits for the CONDITION — the status a describe reports —
/// so it does not depend on how fast this host runs.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The Firehose service principal a role Firehose assumes must trust.
const FIREHOSE_PRINCIPAL: &str = "firehose.amazonaws.com";

/// The role every configuration below names, except where a row names the absent one on
/// purpose. Created in IAM with a trust policy that admits Firehose.
const TRUSTED_ROLE: &str = "firehose-source-rule";

/// A role name no `CreateRole` below creates, so IAM reports this account holds no such
/// role.
const ABSENT_ROLE: &str = "never-created-role";

/// An S3 bucket ARN satisfying `BucketARN`'s modelled pattern, naming the bucket the setup
/// creates: Firehose resolves a destination's bucket against Amazon S3 when the stream is
/// written, so it is there for every row and what a row varies is its SOURCE.
const BUCKET: &str = "demo-bucket";
const BUCKET_ARN: &str = "arn:aws:s3:::demo-bucket";

/// The Kinesis data stream every `KinesisStreamSourceConfiguration` below names.
const UPSTREAM_ARN: &str = "arn:aws:kinesis:us-west-2:000000000000:stream/upstream";

/// Every value of `DeliveryStreamType`
/// (`smithy_query(query="shape",shape="DeliveryStreamType")`).
const DIRECT_PUT: &str = "DirectPut";
const KINESIS_STREAM_AS_SOURCE: &str = "KinesisStreamAsSource";
const MSK_AS_SOURCE: &str = "MSKAsSource";
const DATABASE_AS_SOURCE: &str = "DatabaseAsSource";

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The
/// signature is never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// The ARN of a role of this account, under the default path.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// A trust policy admitting the Firehose service principal. Written with no `&`, `=`, `+`
/// or `%`, so it travels in an `awsQuery` form body as written.
fn firehose_trust_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{FIREHOSE_PRINCIPAL}\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// A `KinesisStreamSourceConfiguration`: the Kinesis data stream that feeds the Firehose
/// stream and the role Firehose reads it under.
fn kinesis_source(role: &str) -> Value {
    json!({
        "KinesisStreamARN": UPSTREAM_ARN,
        "RoleARN": role,
    })
}

/// The source read the role of a `KinesisStreamSourceConfiguration` has to be allowed to
/// make: Firehose resolves it when the stream is written, so the trusted role states it and a
/// row's outcome is about its source configuration rather than about that permission. Written
/// with no `&`, `=`, `+` or `%`, so it travels in an `awsQuery` form body as written.
fn source_read_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":[\"kinesis:DescribeStream\",\"kinesis:GetShardIterator\",\
         \"kinesis:GetRecords\"],\"Resource\":\"{UPSTREAM_ARN}\"}}]}}"
    )
}

/// An `MSKSourceConfiguration`: the cluster, the topic and the authentication Firehose
/// reads it with.
fn msk_source(role: &str) -> Value {
    json!({
        "MSKClusterARN": format!("arn:aws:kafka:{REGION}:000000000000:cluster/events/1"),
        "TopicName": "events",
        "AuthenticationConfiguration": { "RoleARN": role, "Connectivity": "PRIVATE" },
    })
}

/// A `DatabaseSourceConfiguration`, every @required member of it
/// (`smithy_query(query="shape",shape="DatabaseSourceConfiguration")`).
fn database_source(role: &str) -> Value {
    json!({
        "Type": "MySQL",
        "Endpoint": "db.example.internal",
        "Port": 3306,
        "SSLMode": "Enabled",
        "Databases": { "Include": ["sales"] },
        "Tables": { "Include": ["orders"] },
        "SnapshotWatermarkTable": "sales.firehose_watermark",
        "DatabaseSourceAuthenticationConfiguration": {
            "SecretsManagerConfiguration": {
                "SecretARN":
                    format!("arn:aws:secretsmanager:{REGION}:000000000000:secret:db-credentials"),
                "RoleARN": role,
                "Enabled": true,
            },
        },
        "DatabaseSourceVPCConfiguration": {
            "VpcEndpointServiceName": "com.amazonaws.vpce.us-west-2.vpce-svc-01234567890abcdef",
        },
    })
}

/// The throughput hint a `DirectPutSourceConfiguration` carries (range 1..100,
/// `smithy_query(query="shape",shape="DirectPutSourceConfiguration")`).
const THROUGHPUT_HINT: i64 = 7;

fn direct_put_source() -> Value {
    json!({ "ThroughputHintInMBs": THROUGHPUT_HINT })
}

/// An `S3DestinationConfiguration` naming the trusted role.
fn s3_destination() -> Value {
    json!({ "RoleARN": role_arn(TRUSTED_ROLE), "BucketARN": BUCKET_ARN, "Prefix": "raw/" })
}

/// An `ExtendedS3DestinationConfiguration` naming the trusted role: a second destination
/// member, so a row's outcome cannot come from its sink.
fn extended_s3_destination() -> Value {
    json!({ "RoleARN": role_arn(TRUSTED_ROLE), "BucketARN": BUCKET_ARN })
}

/// What a row expects of the create.
enum Expected {
    /// The create is served: the stream carries this `DeliveryStreamType`, and its
    /// description carries this `Source` (`Value::Null` for a stream with none).
    Created {
        stored_type: &'static str,
        source: Value,
    },
    /// The create is refused `400 InvalidArgumentException` with this message.
    Refused(String),
}

/// One row: the create's members beside `DeliveryStreamName`, and what it must answer.
struct Case {
    case: &'static str,
    /// The stream's name, so each row writes its own resource.
    stream: &'static str,
    /// The create's type, source and destination members.
    request: Value,
    expected: Expected,
}

/// The expected `Source` of a stream fed by the Kinesis data stream above.
fn kinesis_source_description() -> Value {
    json!({
        "KinesisStreamARN": format!("arn:aws:kinesis:{REGION}:000000000000:stream/upstream"),
        "RoleARN": role_arn(TRUSTED_ROLE),
    })
}

fn cases() -> Vec<Case> {
    let trusted = role_arn(TRUSTED_ROLE);
    vec![
        // --- a DirectPut stream carrying a configuration of another type: served, and the
        // foreign configuration leaves no trace on the stream.
        Case {
            case: "DirectPut declared beside a KinesisStreamSourceConfiguration",
            stream: "direct-put-with-kinesis-source",
            request: json!({
                "DeliveryStreamType": DIRECT_PUT,
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: DIRECT_PUT,
                source: Value::Null,
            },
        },
        Case {
            // The type left out defaults to DirectPut, and three foreign configurations at
            // once are ignored exactly as one is.
            case: "the type absent beside Kinesis, MSK and Database source configurations",
            stream: "default-type-with-every-foreign-source",
            request: json!({
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "MSKSourceConfiguration": msk_source(&trusted),
                "DatabaseSourceConfiguration": database_source(&trusted),
                "ExtendedS3DestinationConfiguration": extended_s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: DIRECT_PUT,
                source: Value::Null,
            },
        },
        Case {
            // The declared type's OWN configuration still configures the stream when a
            // foreign one travels beside it.
            case: "DirectPut with its own source configuration beside a foreign MSK one",
            stream: "direct-put-own-and-foreign-source",
            request: json!({
                "DeliveryStreamType": DIRECT_PUT,
                "DirectPutSourceConfiguration": direct_put_source(),
                "MSKSourceConfiguration": msk_source(&trusted),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: DIRECT_PUT,
                source: json!({
                    "DirectPutSourceDescription": { "ThroughputHintInMBs": THROUGHPUT_HINT },
                }),
            },
        },
        Case {
            // "Ignored in full": the role a foreign configuration names is no role of this
            // stream, so it is never resolved against IAM and an absent one refuses
            // nothing. The paired row below shows the same role IS resolved when the
            // configuration carrying it is the declared type's own.
            case: "DirectPut beside a foreign Kinesis source naming a role IAM does not hold",
            stream: "direct-put-foreign-source-absent-role",
            request: json!({
                "DeliveryStreamType": DIRECT_PUT,
                "KinesisStreamSourceConfiguration": kinesis_source(&role_arn(ABSENT_ROLE)),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: DIRECT_PUT,
                source: Value::Null,
            },
        },
        // --- the rule is symmetric: a stream fed by a service ignores a DirectPut (or any
        // other) source configuration and keeps the one its type names.
        Case {
            case: "KinesisStreamAsSource beside a foreign DirectPutSourceConfiguration",
            stream: "kinesis-source-with-direct-put-config",
            request: json!({
                "DeliveryStreamType": KINESIS_STREAM_AS_SOURCE,
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "DirectPutSourceConfiguration": direct_put_source(),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: KINESIS_STREAM_AS_SOURCE,
                source: json!({ "KinesisStreamSourceDescription": kinesis_source_description() }),
            },
        },
        Case {
            case: "MSKAsSource beside foreign Kinesis and DirectPut configurations",
            stream: "msk-source-with-foreign-configs",
            request: json!({
                "DeliveryStreamType": MSK_AS_SOURCE,
                "MSKSourceConfiguration": msk_source(&trusted),
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "DirectPutSourceConfiguration": direct_put_source(),
                "ExtendedS3DestinationConfiguration": extended_s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: MSK_AS_SOURCE,
                source: json!({
                    "MSKSourceDescription": {
                        "MSKClusterARN":
                            format!("arn:aws:kafka:{REGION}:000000000000:cluster/events/1"),
                        "TopicName": "events",
                        "AuthenticationConfiguration": {
                            "RoleARN": trusted, "Connectivity": "PRIVATE",
                        },
                    },
                }),
            },
        },
        Case {
            case: "DatabaseAsSource beside a foreign Kinesis source configuration",
            stream: "database-source-with-foreign-config",
            request: json!({
                "DeliveryStreamType": DATABASE_AS_SOURCE,
                "DatabaseSourceConfiguration": database_source(&trusted),
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Created {
                stored_type: DATABASE_AS_SOURCE,
                // The description is large, so the members the configuration decided
                // stand for it; every OTHER member of `SourceDescription` is asserted
                // absent by [`assert_source`].
                source: json!({
                    "DatabaseSourceDescription": {
                        "Type": "MySQL",
                        "Endpoint": "db.example.internal",
                        "Port": 3306,
                    },
                }),
            },
        },
        // --- what stays an argument fault: the declared type's OWN configuration is
        // absent, a foreign one being no substitute for it.
        Case {
            case: "KinesisStreamAsSource with only a foreign source configuration",
            stream: "kinesis-source-missing-own-config",
            request: json!({
                "DeliveryStreamType": KINESIS_STREAM_AS_SOURCE,
                "DirectPutSourceConfiguration": direct_put_source(),
                "S3DestinationConfiguration": s3_destination(),
            }),
            // Firehose names this configuration `KinesisSourceStreamConfig` in its own
            // refusal, not after the request member.
            expected: Expected::Refused(mandatory(
                "KinesisSourceStreamConfig",
                KINESIS_STREAM_AS_SOURCE,
            )),
        },
        Case {
            case: "MSKAsSource with only a foreign source configuration",
            stream: "msk-source-missing-own-config",
            request: json!({
                "DeliveryStreamType": MSK_AS_SOURCE,
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Refused(mandatory("MSKSourceConfiguration", MSK_AS_SOURCE)),
        },
        Case {
            case: "DatabaseAsSource with only a foreign source configuration",
            stream: "database-source-missing-own-config",
            request: json!({
                "DeliveryStreamType": DATABASE_AS_SOURCE,
                "KinesisStreamSourceConfiguration": kinesis_source(&trusted),
                "ExtendedS3DestinationConfiguration": extended_s3_destination(),
            }),
            expected: Expected::Refused(mandatory(
                "DatabaseSourceConfiguration",
                DATABASE_AS_SOURCE,
            )),
        },
        Case {
            // The pair of the absent-role row above: the role of the configuration the
            // declared type IS fed by is resolved, so an absent one is still refused.
            case: "KinesisStreamAsSource whose own source names a role IAM does not hold",
            stream: "kinesis-source-own-config-absent-role",
            request: json!({
                "DeliveryStreamType": KINESIS_STREAM_AS_SOURCE,
                "KinesisStreamSourceConfiguration": kinesis_source(&role_arn(ABSENT_ROLE)),
                "S3DestinationConfiguration": s3_destination(),
            }),
            expected: Expected::Refused(unassumable(&role_arn(ABSENT_ROLE))),
        },
    ]
}

/// Every member of `SourceDescription`
/// (`smithy_query(query="shape",shape="SourceDescription")`), so a description a foreign
/// configuration would have written is looked for at each of them.
const SOURCE_DESCRIPTION_MEMBERS: &[&str] = &[
    "DirectPutSourceDescription",
    "KinesisStreamSourceDescription",
    "MSKSourceDescription",
    "DatabaseSourceDescription",
];

/// Assert the `Source` a describe reports is the one the declared type's own configuration
/// decided: every member `expected` names carries the fields it states, and every other
/// member of `SourceDescription` is absent. `Value::Null` expects no `Source` at all.
fn assert_source(case: &str, expected: &Value, reported: &Value, described: &Value) {
    let Some(members) = expected.as_object() else {
        assert!(
            reported.is_null(),
            "case {case:?}: an ignored source configuration leaves no Source, got {described}"
        );
        return;
    };
    for (member, fields) in members {
        for (field, value) in fields.as_object().expect("a description's fields") {
            assert_eq!(
                &reported[member][field],
                value,
                "case {case:?}: expected Source.{member}.{field} {value}, got {described}"
            );
        }
    }
    for member in SOURCE_DESCRIPTION_MEMBERS {
        assert!(
            members.contains_key(*member) || reported[*member].is_null(),
            "case {case:?}: a configuration the declared type is not fed by must leave no \
             Source.{member}, got {described}"
        );
    }
}

/// The sentence Firehose states for a stream type whose own source configuration is absent.
fn mandatory(configuration: &str, stream_type: &str) -> String {
    format!("{configuration} is mandatory for {stream_type} stream type.")
}

/// The sentence Firehose states for a role it cannot assume.
fn unassumable(role_arn: &str) -> String {
    format!("Firehose is unable to assume role {role_arn}. Please check the role provided.")
}

/// The sentence Firehose states for a direct put to a stream another service feeds.
fn not_permitted(stream_type: &str) -> String {
    format!("This operation is not permitted on {stream_type} delivery stream type.")
}

/// Send a request and return the status with the body as text.
fn send(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    content_type: &str,
    target: Option<&str>,
    body: String,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Drive one `awsQuery` IAM action: `POST /` with the parameters in the form body.
fn iam(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push_str(&format!("&{key}={value}"));
    }
    send(
        router,
        runtime,
        "iam",
        "application/x-www-form-urlencoded",
        None,
        body,
    )
}

/// Drive one firehose operation (`awsJson1_1`), returning the status and the parsed body.
fn firehose(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (status, text) = send(
        router,
        runtime,
        "firehose",
        "application/x-amz-json-1.1",
        Some(&format!("Firehose_20150804.{operation}")),
        body.to_string(),
    );
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create the bucket every row's destination names: `PUT /<bucket>` on Amazon S3's own wire.
fn create_bucket(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    bucket: &str,
) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://{host}/{bucket}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
             </CreateBucketConfiguration>"
        )))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: the destination bucket {bucket} must be created, got {}",
        String::from_utf8_lossy(response.body())
    );
}

/// The wire error code, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// The error message, whichever casing the envelope carries it under.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Describe `stream` until it reports `ACTIVE` — the status the clock owes a created
/// stream — or the deadline passes. Returns the last description read.
fn describe_until_active(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    stream: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let (status, described) = firehose(
            router,
            runtime,
            "DescribeDeliveryStream",
            &json!({ "DeliveryStreamName": stream }),
        );
        let active = status == 200
            && described["DeliveryStreamDescription"]["DeliveryStreamStatus"] == json!("ACTIVE");
        if active || started.elapsed() >= ACTIVE_DEADLINE {
            assert_eq!(
                status, 200,
                "case {stream:?}: the created stream must be readable, got {status} {described}"
            );
            return described;
        }
        std::thread::sleep(LOOK_AGAIN_AFTER);
    }
}

#[test]
fn a_create_ignores_every_source_configuration_its_declared_type_is_not_fed_by() {
    std::thread::Builder::new()
        .name("firehose-foreign-source".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The time driver, because the emulator's services run CLOCKS: a service
                // whose state owes a transition spawns a background task that waits on a
                // timer, and a timer on a runtime built without this panics the task.
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let policy = firehose_trust_policy();
            let (status, body) = iam(
                &router,
                &runtime,
                "CreateRole",
                &[
                    ("RoleName", TRUSTED_ROLE),
                    ("AssumeRolePolicyDocument", policy.as_str()),
                ],
            );
            assert_eq!(status, 200, "setup: CreateRole got {status} {body}");

            // The two resources a create resolves beside its source configuration, so that no
            // row below is answered for one of them: the destination bucket Amazon S3 has to
            // hold, and the source read the trusted role has to be allowed to make.
            let read = source_read_policy();
            let (status, body) = iam(
                &router,
                &runtime,
                "PutRolePolicy",
                &[
                    ("RoleName", TRUSTED_ROLE),
                    ("PolicyName", "source-read"),
                    ("PolicyDocument", read.as_str()),
                ],
            );
            assert_eq!(status, 200, "setup: PutRolePolicy got {status} {body}");
            create_bucket(&router, &runtime, BUCKET);

            let cases = cases();

            // Every stream is created first, so the creation spans of the served rows
            // overlap and ONE poll each settles them.
            let mut created: Vec<&Case> = Vec::new();
            for case in &cases {
                let mut request = case.request.clone();
                request["DeliveryStreamName"] = json!(case.stream);
                let (status, body) = firehose(&router, &runtime, "CreateDeliveryStream", &request);
                match &case.expected {
                    Expected::Created { .. } => {
                        assert_eq!(
                            status, 200,
                            "case {:?}: the create must be served, got {status} {body}",
                            case.case
                        );
                        let arn = body["DeliveryStreamARN"].as_str().unwrap_or_default();
                        let expected_arn = format!(
                            "arn:aws:firehose:{REGION}:{}:deliverystream/{}",
                            engine::emulator_account_id(),
                            case.stream
                        );
                        assert_eq!(
                            arn, expected_arn,
                            "case {:?}: expected the minted ARN {expected_arn:?}, got {body}",
                            case.case
                        );
                        created.push(case);
                    }
                    Expected::Refused(message) => {
                        assert_eq!(
                            (status, error_code(&body).as_str()),
                            (400, "InvalidArgumentException"),
                            "case {:?}: expected 400 InvalidArgumentException, got {status} {body}",
                            case.case
                        );
                        assert_eq!(
                            error_message(&body),
                            *message,
                            "case {:?}: expected {message:?}, got {body}",
                            case.case
                        );
                        let (read, described) = firehose(
                            &router,
                            &runtime,
                            "DescribeDeliveryStream",
                            &json!({ "DeliveryStreamName": case.stream }),
                        );
                        assert_eq!(
                            (read, error_code(&described).as_str()),
                            (400, "ResourceNotFoundException"),
                            "case {:?}: a refused create must persist nothing, got {read} \
                             {described}",
                            case.case
                        );
                    }
                }
            }

            for case in created {
                let Expected::Created {
                    stored_type,
                    source,
                } = &case.expected
                else {
                    continue;
                };
                let described = describe_until_active(&router, &runtime, case.stream);
                let description = &described["DeliveryStreamDescription"];
                assert_eq!(
                    description["DeliveryStreamStatus"],
                    json!("ACTIVE"),
                    "case {:?}: the created stream must reach ACTIVE, got {described}",
                    case.case
                );
                assert_eq!(
                    description["DeliveryStreamType"],
                    json!(stored_type),
                    "case {:?}: expected the declared type {stored_type:?}, got {described}",
                    case.case
                );
                assert_eq!(
                    description["VersionId"],
                    json!("1"),
                    "case {:?}: a created stream is at version 1, got {described}",
                    case.case
                );
                assert_eq!(
                    description["HasMoreDestinations"],
                    json!(false),
                    "case {:?}: the create wrote one destination, got {described}",
                    case.case
                );
                assert_eq!(
                    description["Destinations"].as_array().map(Vec::len),
                    Some(1),
                    "case {:?}: the create wrote one destination, got {described}",
                    case.case
                );

                // The `Source` the stream carries: exactly what the declared type's own
                // configuration decided, with no member of a foreign one.
                assert_source(case.case, source, &description["Source"], &described);

                // What the stored type means on the data plane: a stream whose source IS
                // direct puts takes them, and one another service feeds does not — so an
                // ignored `KinesisStreamSourceConfiguration` did not quietly make a
                // DirectPut stream source-fed.
                let (status, put) = firehose(
                    &router,
                    &runtime,
                    "PutRecord",
                    &json!({
                        "DeliveryStreamName": case.stream,
                        "Record": { "Data": aws_smithy_types::base64::encode("a payload") },
                    }),
                );
                if *stored_type == DIRECT_PUT {
                    assert_eq!(
                        status, 200,
                        "case {:?}: a DirectPut stream takes a direct put, got {status} {put}",
                        case.case
                    );
                    assert!(
                        put["RecordId"].as_str().is_some_and(|id| !id.is_empty()),
                        "case {:?}: an accepted put answers a RecordId, got {put}",
                        case.case
                    );
                } else {
                    assert_eq!(
                        (status, error_code(&put).as_str()),
                        (400, "InvalidArgumentException"),
                        "case {:?}: a source-fed stream refuses a direct put, got {status} {put}",
                        case.case
                    );
                    assert_eq!(
                        error_message(&put),
                        not_permitted(stored_type),
                        "case {:?}: expected the not-permitted refusal, got {put}",
                        case.case
                    );
                }

                // And the stream a client created is a stream it can tear down.
                let (status, deleted) = firehose(
                    &router,
                    &runtime,
                    "DeleteDeliveryStream",
                    &json!({ "DeliveryStreamName": case.stream }),
                );
                assert_eq!(
                    status, 200,
                    "case {:?}: the delete must be served, got {status} {deleted}",
                    case.case
                );
            }
        })
        .expect("spawn the foreign-source thread")
        .join()
        .expect("a create ignores the source configurations its type is not fed by");
}
