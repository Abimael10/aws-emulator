// SPDX-License-Identifier: CC0-1.0

//! What an IVS create proves about a resource of ANOTHER service it references, before
//! it mints anything — driven over the wire against a router that mounts IVS beside this
//! emulator's own Amazon S3 and AWS Elemental MediaTailor on ONE engine, so the
//! references a create resolves are real resources of those services, created and
//! removed through their own APIs.
//!
//! The rule under test, stated once and then exercised over a SET of inputs: a create
//! that takes the ARN or name of a resource another service owns RESOLVES it there
//! first, and when that service answers it holds no such resource the create is refused
//! — persisting nothing — with the error this operation's own model declares for an
//! unresolvable reference: `ResourceNotFoundException` (404) for
//! `CreateAdConfiguration`, whose model declares it, and the service-common
//! `ValidationException` (400) naming the member path for
//! `CreateRecordingConfiguration`, whose model does not
//! (`smithy_query(query="operation",
//! operation="CreateAdConfiguration,CreateRecordingConfiguration")`).
//!
//! The rows differ along what the rule generalises over: how many references the request
//! carries, which of them resolves, how the reference fails to resolve (never created, or
//! created and then deleted), whether it names a resource at all, and the size of the
//! name it carries.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_ivs_amazoninteractivevideoservice::EmulatedService;

/// The Region every request in this suite is signed for, and the one its SDK clients
/// address.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header is how a request names the service and the region it
/// is addressed to, so every call carries one. The signature itself is never checked
/// here: request authentication is off unless a router turns it on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTIVS/20260101/us-east-1/ivs/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// The member path `CreateRecordingConfiguration`'s refusal names, and the sentence it
/// is stated in — the form IVS states a member whose value it cannot satisfy.
const S3_DESTINATION_PATH: &str = "recordingConfiguration.destinationConfiguration.s3";

/// How IVS names a MediaTailor playback configuration in the not-found it answers for a
/// reference that resolves to nothing.
const PLAYBACK_CONFIGURATION_KIND: &str = "mediaTailorPlaybackConfiguration";

/// IVS, Amazon S3 and MediaTailor on one shared engine.
struct Aws {
    router: MultiServiceRouter,
}

/// One answer from the emulator: the status, the wire error code (the restJson1 `__type`
/// discriminator), the `x-amzn-errortype` header a client reads the code off, the body's
/// `message` member and the parsed body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: String,
    error_type_header: Option<String>,
    message: String,
    body: Value,
}

impl Aws {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // The bucket a recording configuration records into is a bucket of THIS object
        // store, and the playback configuration an ad configuration references is a
        // configuration of THIS MediaTailor.
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_mediatailor_mediatailor::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    fn callback(&self) -> EmbeddedEmulator {
        EmbeddedEmulator::from_router(self.router.clone())
    }

    fn s3(&self) -> ::aws_sdk_s3::Client {
        ::service_common::s3_client::s3_client(&self.callback(), REGION)
    }

    /// One IVS call, bound to `POST /<Operation>`.
    async fn call(&self, operation: &str, body: &Value) -> Reply {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://ivs.{REGION}.amazonaws.com/{operation}"))
            .header("content-type", "application/json")
            .header("authorization", AUTHORIZATION)
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type_header = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code: text(&parsed, "__type"),
            error_type_header,
            message: text(&parsed, "message"),
            body: parsed,
        }
    }

    /// An IVS call that must succeed, returning the body.
    async fn ok(&self, operation: &str, body: Value) -> Value {
        let reply = self.call(operation, &body).await;
        assert_eq!(
            reply.status, 200,
            "case {operation} {body}: expected HTTP 200, got {} {}",
            reply.status, reply.body
        );
        reply.body
    }

    /// Create a bucket in this emulator's own object store and return its name.
    async fn create_bucket(&self, bucket: &str) -> String {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case bucket {bucket}: the create must succeed: {error}"));
        bucket.to_string()
    }

    /// Remove a bucket from the object store again, so a reference to it stops resolving.
    async fn delete_bucket(&self, bucket: &str) {
        self.s3()
            .delete_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case bucket {bucket}: the delete must succeed: {error}"));
    }

    /// Create a MediaTailor playback configuration through MediaTailor's own API and
    /// return the ARN it minted — the value an IVS request references it by.
    async fn create_playback_configuration(&self, name: &str) -> String {
        let answer = self
            .mediatailor()
            .put_playback_configuration()
            .name(name)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case playback configuration {name}: the create must succeed: {error}")
            });
        answer
            .playback_configuration_arn()
            .unwrap_or_else(|| {
                panic!("case playback configuration {name}: MediaTailor must answer its ARN")
            })
            .to_string()
    }

    /// Remove it again, so a reference to it stops resolving.
    async fn delete_playback_configuration(&self, name: &str) {
        self.mediatailor()
            .delete_playback_configuration()
            .name(name)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case playback configuration {name}: the delete must succeed: {error}")
            });
    }

    fn mediatailor(&self) -> ::aws_sdk_mediatailor::Client {
        let callback = self.callback();
        let config = ::aws_sdk_mediatailor::Config::builder()
            .behavior_version(::aws_sdk_mediatailor::config::BehaviorVersion::latest())
            .http_client(callback.http_client())
            .credentials_provider(callback.aws_credentials_provider())
            .region(::aws_sdk_mediatailor::config::Region::new(
                REGION.to_string(),
            ))
            .endpoint_url(format!("https://api.mediatailor.{REGION}.amazonaws.com"))
            .retry_config(::aws_sdk_mediatailor::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_mediatailor::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_mediatailor::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_mediatailor::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        ::aws_sdk_mediatailor::Client::from_conf(config)
    }

    /// Every ad configuration this account holds, by name.
    async fn ad_configuration_names(&self) -> Vec<String> {
        let listed = self.ok("ListAdConfigurations", json!({})).await;
        listed["adConfigurations"]
            .as_array()
            .map(|entries| entries.iter().map(|entry| text(entry, "name")).collect())
            .unwrap_or_default()
    }

    /// Every recording configuration this account holds, by name.
    async fn recording_configuration_names(&self) -> Vec<String> {
        let listed = self.ok("ListRecordingConfigurations", json!({})).await;
        listed["recordingConfigurations"]
            .as_array()
            .map(|entries| entries.iter().map(|entry| text(entry, "name")).collect())
            .unwrap_or_default()
    }
}

/// A string member, read as the empty string when absent — which is what a client sees
/// for a member the wire omits.
fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An ARN of the form the modelled `MediaTailorPlaybackConfigurationArn` pattern
/// `^arn:aws:mediatailor:[a-z0-9-]+:[0-9]+:playbackConfiguration/[a-zA-Z0-9-]+$` pins,
/// for a configuration that was never created — the only reference this suite writes by
/// hand, because no create can hand one back.
fn absent_playback_configuration_arn(name: &str) -> String {
    format!("arn:aws:mediatailor:{REGION}:000000000000:playbackConfiguration/{name}")
}

/// The name a MediaTailor playback-configuration ARN names: the segment after the
/// resource type, which is the identity MediaTailor addresses one by and the name the
/// refusal quotes.
fn playback_configuration_name(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or_default().to_string()
}

/// One row: what the ad configuration's request references, and what the service must
/// answer.
struct AdCase {
    case: &'static str,
    name: &'static str,
    references: Vec<Value>,
    /// The name the refusal must quote, or `None` for a row the service admits.
    refused_on: Option<String>,
}

/// Rule, on `CreateAdConfiguration`: every MediaTailor playback configuration the
/// request references is resolved in MediaTailor before anything is minted, and a
/// reference MediaTailor answers it holds nothing for is refused 404
/// `ResourceNotFoundException` — leaving no ad configuration behind.
#[tokio::test]
async fn an_ad_configuration_is_created_only_when_every_reference_resolves() {
    let aws = Aws::new();
    let held = aws.create_playback_configuration("held-config").await;
    let second = aws.create_playback_configuration("second-config").await;
    let third = aws.create_playback_configuration("third-config").await;
    let removed = aws.create_playback_configuration("removed-config").await;
    aws.delete_playback_configuration("removed-config").await;
    let never = absent_playback_configuration_arn("absent-config");

    let reference = |arn: &str| json!({ "playbackConfigurationArn": arn });
    let cases = vec![
        AdCase {
            case: "one reference, to a configuration MediaTailor holds",
            name: "one-held",
            references: vec![reference(&held)],
            refused_on: None,
        },
        AdCase {
            case: "the maximum of three references, all held",
            name: "three-held",
            references: vec![reference(&held), reference(&second), reference(&third)],
            refused_on: None,
        },
        AdCase {
            case: "an entry carrying no playbackConfigurationArn at all, which \
                   references nothing",
            name: "no-reference",
            references: vec![json!({})],
            refused_on: None,
        },
        AdCase {
            case: "one reference, to a configuration that was never created",
            name: "one-absent",
            references: vec![reference(&never)],
            refused_on: Some(playback_configuration_name(&never)),
        },
        AdCase {
            case: "one reference, to a configuration that was created and then deleted",
            name: "one-removed",
            references: vec![reference(&removed)],
            refused_on: Some(playback_configuration_name(&removed)),
        },
        AdCase {
            case: "three references, the LAST of them unresolvable",
            name: "last-absent",
            references: vec![reference(&held), reference(&second), reference(&never)],
            refused_on: Some(playback_configuration_name(&never)),
        },
        AdCase {
            case: "two references, the FIRST of them unresolvable",
            name: "first-absent",
            references: vec![reference(&never), reference(&held)],
            refused_on: Some(playback_configuration_name(&never)),
        },
    ];

    for AdCase {
        case,
        name,
        references,
        refused_on,
    } in cases
    {
        let request = json!({ "name": name, "mediaTailorPlaybackConfigurations": references });
        let reply = aws.call("CreateAdConfiguration", &request).await;
        match refused_on {
            None => {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: every reference resolves, so the create is admitted, \
                     got {} {}",
                    reply.status, reply.body
                );
                assert!(
                    aws.ad_configuration_names()
                        .await
                        .contains(&name.to_string()),
                    "case {case:?}: the admitted configuration must be listed, got {:?}",
                    aws.ad_configuration_names().await
                );
            }
            Some(referenced) => {
                let expected = format!("Resource: {PLAYBACK_CONFIGURATION_KIND} '{referenced}' not found");
                assert_eq!(
                    (
                        reply.status,
                        reply.error_code.as_str(),
                        reply.message.as_str()
                    ),
                    (404, "ResourceNotFoundException", expected.as_str()),
                    "case {case:?}: expected 404 ResourceNotFoundException {expected:?}, \
                     got {} {} {}",
                    reply.status,
                    reply.error_code,
                    reply.body
                );
                assert_eq!(
                    reply.error_type_header.as_deref(),
                    Some("ResourceNotFoundException"),
                    "case {case:?}: the refusal names its code in the header a client reads \
                     it off, got {:?}",
                    reply.error_type_header
                );
                let held_names = aws.ad_configuration_names().await;
                assert!(
                    !held_names.contains(&name.to_string()),
                    "case {case:?}: a refused create must leave nothing behind, got {held_names:?}"
                );
            }
        }
    }
}

/// One row: the bucket the recording configuration's destination names, and whether the
/// object store holds it.
struct RecordingCase {
    case: &'static str,
    name: &'static str,
    bucket: String,
    held: bool,
}

/// The same rule on `CreateRecordingConfiguration`: the bucket its destination names is
/// resolved in the object store before anything is minted, and a bucket the store
/// answers this account does not hold is refused 400 `ValidationException` naming the
/// member — leaving no recording configuration behind. An admitted create is read back
/// through `GetRecordingConfiguration`.
#[tokio::test]
async fn a_recording_configuration_is_created_only_when_its_bucket_exists() {
    let aws = Aws::new();
    let held = aws.create_bucket("ivs-recordings-held").await;
    // The modelled `S3DestinationBucketName` is `@length(3..63)` over `^[a-z0-9-.]+$`,
    // so the rows span both bounds of the name the rule is asked about.
    let shortest = aws.create_bucket("abc").await;
    let longest = aws.create_bucket(&format!("a{}", "b".repeat(62))).await;
    let removed = aws.create_bucket("ivs-recordings-removed").await;
    aws.delete_bucket(&removed).await;

    let cases = vec![
        RecordingCase {
            case: "a bucket the object store holds",
            name: "held",
            bucket: held,
            held: true,
        },
        RecordingCase {
            case: "a bucket whose name is the modelled minimum length",
            name: "shortest",
            bucket: shortest,
            held: true,
        },
        RecordingCase {
            case: "a bucket whose name is the modelled maximum length",
            name: "longest",
            bucket: longest,
            held: true,
        },
        RecordingCase {
            case: "a bucket name nothing was ever created under",
            name: "absent",
            bucket: "ivs-recordings-absent".to_string(),
            held: false,
        },
        RecordingCase {
            case: "a bucket that was created and then removed",
            name: "removed",
            bucket: removed,
            held: false,
        },
    ];

    for RecordingCase {
        case,
        name,
        bucket,
        held,
    } in cases
    {
        let request = json!({ "name": name,
                              "destinationConfiguration": { "s3": { "bucketName": bucket } } });
        let reply = aws.call("CreateRecordingConfiguration", &request).await;
        if held {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: the bucket resolves, so the create is admitted, got {} {}",
                reply.status, reply.body
            );
            // The round trip: what the create persisted is what a read answers.
            let arn = text(&reply.body["recordingConfiguration"], "arn");
            let read = aws
                .ok("GetRecordingConfiguration", json!({ "arn": arn }))
                .await;
            assert_eq!(
                text(&read["recordingConfiguration"]["destinationConfiguration"]["s3"],
                     "bucketName"),
                bucket,
                "case {case:?}: the read must answer the destination the create stored, \
                 got {read}"
            );
        } else {
            let expected = format!(
                "Request member: {S3_DESTINATION_PATH} failed to satisfy the constraints: \
                 {S3_DESTINATION_PATH} is invalid"
            );
            assert_eq!(
                (
                    reply.status,
                    reply.error_code.as_str(),
                    reply.message.as_str()
                ),
                (400, "ValidationException", expected.as_str()),
                "case {case:?}: expected 400 ValidationException {expected:?}, got {} {} {}",
                reply.status,
                reply.error_code,
                reply.body
            );
            assert_eq!(
                reply.error_type_header.as_deref(),
                Some("ValidationException"),
                "case {case:?}: the refusal names its code in the header a client reads it \
                 off, got {:?}",
                reply.error_type_header
            );
            let held_names = aws.recording_configuration_names().await;
            assert!(
                !held_names.contains(&name.to_string()),
                "case {case:?}: a refused create must leave nothing behind, got {held_names:?}"
            );
        }
    }
}
