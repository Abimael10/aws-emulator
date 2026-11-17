// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage for the WebRTC signaling-channel slice of Kinesis Video
//! Streams: `CreateSignalingChannel`, `DescribeSignalingChannel`,
//! `ListSignalingChannels`, `GetSignalingChannelEndpoint`,
//! `DescribeMediaStorageConfiguration`, `ListTagsForResource` and
//! `DeleteSignalingChannel`.
//!
//! Every case drives the real request walk (route -> deserialize -> validate ->
//! handle -> serialize) through [`MultiServiceRouter::dispatch`], so a response here
//! is the same bytes a client on the socket would see — including the `restJson1`
//! error envelope (`Content-Type: application/json` plus `x-amzn-errortype`) every
//! failure has to carry.
//!
//! Where a modeled `@pattern` has to be proved, the test feeds the generated value
//! BACK IN through an input member carrying that same pattern (`ChannelARN`,
//! `CurrentVersion`, `NextToken`). The boundary validator is then the oracle for the
//! pattern, instead of this file re-implementing one.
//!
//! The suites at the end of this file assert the SENTENCES this surface refuses with:
//! the version-conflict sentence of the two channel mutations guarded by
//! `CurrentVersion`, the not-found sentence of the three operations keyed on a bare
//! `ResourceARN` (which name whichever resource kind that ARN's resource segment names,
//! so a stream ARN is covered here too), the duplicate-name sentence of the two creates,
//! the sentence a `WEBRTC` endpoint request on a channel with no media-storage
//! configuration carries, and the sentence an endpoint request missing the configuration,
//! its protocols or its role carries.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use smithy_server_runtime::MultiServiceRouter;

/// A router carrying only this service, over its own engine, with request
/// authentication off so a case can send an unsigned-but-scoped request.
fn harness() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(engine));
    router
}

/// The emulator endpoint every case addresses. Also the authority
/// `GetSignalingChannelEndpoint` has to hand back, so a signaling client lands on this
/// emulator again.
const ENDPOINT_HOST: &str = "kinesisvideo.us-east-1.amazonaws.com";

/// One answer from the router: HTTP status, the parsed JSON body, and the two response
/// headers the `restJson1` error envelope is made of.
struct Answer {
    status: u16,
    body: serde_json::Value,
    content_type: String,
    error_type: String,
}

impl Answer {
    /// The wire error code of a failure body (`__type`), with any Smithy namespace
    /// stripped.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .rsplit('#')
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// The text of a failure, from the `Message` member every error shape of this model
    /// declares. Each suite module of this target owns its own view of an answer (this
    /// one carries the envelope headers the sibling suites do not assert), so this is
    /// that accessor for THIS module's `Answer` rather than a second copy of a shared
    /// helper.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// POST one restJson1 call to `path`.
async fn call(router: &MultiServiceRouter, path: &str, body: serde_json::Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{ENDPOINT_HOST}{path}"))
        .header("host", ENDPOINT_HOST)
        .header("content-type", "application/json")
        // The SigV4 credential scope is what routes the request to this service and
        // names its region; the signature itself is not checked here.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/kinesisvideo/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let content_type = header("content-type");
    let error_type = header("x-amzn-errortype");
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    Answer {
        status,
        body,
        content_type,
        error_type,
    }
}

/// Create `name` with `extra` merged into the request, and return its `ChannelARN`.
async fn create_channel_with(
    router: &MultiServiceRouter,
    name: &str,
    extra: serde_json::Value,
) -> String {
    let mut request = serde_json::json!({ "ChannelName": name });
    if let (Some(target), Some(source)) = (request.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let answer = call(router, "/createSignalingChannel", request).await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: CreateSignalingChannel failed with {:?}",
        answer.body
    );
    answer
        .body
        .get("ChannelARN")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: CreateSignalingChannel returned no ChannelARN: {:?}",
                answer.body
            )
        })
        .to_string()
}

/// Create `name` with nothing but its name, and return its `ChannelARN`.
async fn create_channel(router: &MultiServiceRouter, name: &str) -> String {
    create_channel_with(router, name, serde_json::json!({})).await
}

/// Describe `name` and return its `ChannelInfo`.
async fn describe_channel(router: &MultiServiceRouter, name: &str) -> serde_json::Value {
    let answer = call(
        router,
        "/describeSignalingChannel",
        serde_json::json!({ "ChannelName": name }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: DescribeSignalingChannel failed with {:?}",
        answer.body
    );
    answer
        .body
        .get("ChannelInfo")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: DescribeSignalingChannel returned no ChannelInfo: {:?}",
                answer.body
            )
        })
}

/// Wait out [`kvs::state::CHANNEL_ACTIVATION_SPAN`] so the clock owes every freshly
/// created channel its move to `ACTIVE`.
fn wait_for_activation() {
    std::thread::sleep(kvs::state::CHANNEL_ACTIVATION_SPAN + Duration::from_millis(150));
}

/// RULE: a created signaling channel is persisted, and every reader reports the record
/// that was stored — the whole client script of the issue over one store: create, read
/// back, list, resolve endpoints, read the media-storage configuration and the tags,
/// delete, and read back the absence.
#[tokio::test]
async fn a_signaling_channel_is_created_read_listed_endpointed_and_deleted() {
    let router = harness();

    let answer = call(
        &router,
        "/createSignalingChannel",
        serde_json::json!({
            "ChannelName": "demo-channel",
            "ChannelType": "SINGLE_MASTER",
            "SingleMasterConfiguration": { "MessageTtlSeconds": 60 },
            "Tags": [{ "Key": "stage", "Value": "test" }]
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "CreateSignalingChannel: got {:?}",
        answer.body
    );
    assert_eq!(
        answer.content_type, "application/json",
        "CreateSignalingChannel must answer in the restJson1 envelope, got {:?}",
        answer.content_type
    );
    let arn = answer
        .body
        .get("ChannelARN")
        .and_then(serde_json::Value::as_str)
        .expect("CreateSignalingChannel returns a ChannelARN")
        .to_string();

    // A freshly created channel is reported in the non-terminal status the asynchronous
    // create leaves it in, and the clock owes it ACTIVE.
    let info = describe_channel(&router, "demo-channel").await;
    assert_eq!(
        info["ChannelStatus"], "CREATING",
        "a channel read immediately after its create must still be CREATING: {info:?}"
    );
    wait_for_activation();
    let info = describe_channel(&router, "demo-channel").await;
    assert_eq!(
        info["ChannelStatus"], "ACTIVE",
        "a channel read after the activation span must be ACTIVE: {info:?}"
    );
    assert_eq!(
        info["ChannelARN"], arn,
        "DescribeSignalingChannel must report the ARN the create minted: {info:?}"
    );
    assert_eq!(
        info["ChannelName"], "demo-channel",
        "DescribeSignalingChannel must report the stored name: {info:?}"
    );
    assert_eq!(
        info["ChannelType"], "SINGLE_MASTER",
        "DescribeSignalingChannel must report the stored type: {info:?}"
    );
    assert_eq!(
        info["SingleMasterConfiguration"]["MessageTtlSeconds"], 60,
        "DescribeSignalingChannel must report the stored TTL: {info:?}"
    );
    let version = info["Version"]
        .as_str()
        .expect("ChannelInfo carries a Version")
        .to_string();

    // The listing reports the same record the reader does.
    let answer = call(&router, "/listSignalingChannels", serde_json::json!({})).await;
    assert_eq!(
        answer.status, 200,
        "ListSignalingChannels: got {:?}",
        answer.body
    );
    let listed = answer.body["ChannelInfoList"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "ListSignalingChannels must report the one stored channel, got {listed:?}"
    );
    assert_eq!(
        listed[0], info,
        "the listed entry must be the described record, got {:?} for {info:?}",
        listed[0]
    );

    // The tags the create carried read back through the tag reader.
    let answer = call(
        &router,
        "/ListTagsForResource",
        serde_json::json!({ "ResourceARN": arn.clone() }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "ListTagsForResource: got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["Tags"],
        serde_json::json!({ "stage": "test" }),
        "ListTagsForResource must report the tags the create carried, got {:?}",
        answer.body
    );

    // A channel created without a media-storage configuration has none.
    let answer = call(
        &router,
        "/describeMediaStorageConfiguration",
        serde_json::json!({ "ChannelARN": arn.clone() }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "DescribeMediaStorageConfiguration: got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["MediaStorageConfiguration"]["Status"], "DISABLED",
        "a channel with no media storage reports DISABLED, got {:?}",
        answer.body
    );
    assert!(
        answer.body["MediaStorageConfiguration"]
            .get("StreamARN")
            .is_none_or(serde_json::Value::is_null),
        "a disabled media-storage configuration names no stream, got {:?}",
        answer.body
    );

    // Deleting with the version just read removes the record, and every reader then
    // states its absence.
    let answer = call(
        &router,
        "/deleteSignalingChannel",
        serde_json::json!({ "ChannelARN": arn.clone(), "CurrentVersion": version }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "DeleteSignalingChannel: got {:?}",
        answer.body
    );

    let answer = call(
        &router,
        "/describeSignalingChannel",
        serde_json::json!({ "ChannelName": "demo-channel" }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (404, "ResourceNotFoundException"),
        "DescribeSignalingChannel after delete: got {} {:?}",
        answer.status,
        answer.body
    );

    let answer = call(&router, "/listSignalingChannels", serde_json::json!({})).await;
    assert_eq!(
        answer.body["ChannelInfoList"],
        serde_json::json!([]),
        "ListSignalingChannels after delete must be empty, got {:?}",
        answer.body
    );
}

/// RULE: a reader addressed by `ChannelName` and the same reader addressed by the
/// `ChannelARN` the create minted resolve to the ONE stored record; the choice between the
/// two identifiers is EXCLUSIVE, so a pair naming neither or both — even two that agree —
/// is a bad argument (400), and an identifier that names no stored record is a missing
/// resource (404), stated the same way by every reader that takes the pair.
#[tokio::test]
async fn every_channel_reader_resolves_the_same_record_from_either_identifier() {
    let router = harness();
    let arn = create_channel(&router, "present").await;
    let other_arn = create_channel(&router, "other").await;
    wait_for_activation();

    // The two readers that take a ChannelName/ChannelARN pair, and the member each one
    // answers with.
    let readers = [
        ("/describeSignalingChannel", "ChannelInfo"),
        (
            "/describeMediaStorageConfiguration",
            "MediaStorageConfiguration",
        ),
    ];

    for (path, member) in readers {
        // Rows differ along which identifier is supplied, how many, and whether they
        // agree — the dimensions the resolution rule generalises over.
        let rows: Vec<(&str, serde_json::Value, u16, &str)> = vec![
            (
                "the name alone",
                serde_json::json!({ "ChannelName": "present" }),
                200,
                "",
            ),
            (
                "the ARN alone",
                serde_json::json!({ "ChannelARN": arn.clone() }),
                200,
                "",
            ),
            (
                "the name and the ARN, agreeing",
                serde_json::json!({ "ChannelName": "present", "ChannelARN": arn.clone() }),
                400,
                "InvalidArgumentException",
            ),
            (
                "neither identifier",
                serde_json::json!({}),
                400,
                "InvalidArgumentException",
            ),
            (
                "the name and the ARN, naming different channels",
                serde_json::json!({ "ChannelName": "present", "ChannelARN": other_arn.clone() }),
                400,
                "InvalidArgumentException",
            ),
            (
                "a name that is not in the store",
                serde_json::json!({ "ChannelName": "no-such-channel" }),
                404,
                "ResourceNotFoundException",
            ),
            (
                "an ARN that is not in the store",
                serde_json::json!({
                    "ChannelARN": "arn:aws:kinesisvideo:us-east-1:000000000000:channel/ghost/1"
                }),
                404,
                "ResourceNotFoundException",
            ),
            (
                "a stream's ARN, which names no channel",
                serde_json::json!({
                    "ChannelARN": "arn:aws:kinesisvideo:us-east-1:000000000000:stream/present/1"
                }),
                404,
                "ResourceNotFoundException",
            ),
        ];

        for (case, body, expected_status, expected_code) in rows {
            let answer = call(&router, path, body.clone()).await;
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (expected_status, expected_code),
                "case {case:?} on {path}: expected HTTP {expected_status} {expected_code:?}, \
                 got {} {:?} ({:?})",
                answer.status,
                answer.error_code(),
                answer.body
            );
            if expected_status == 200 {
                assert!(
                    answer.body.get(member).is_some(),
                    "case {case:?} on {path}: a success must carry {member}, got {:?}",
                    answer.body
                );
            }
        }
    }
}

/// RULE: every failure of this service is rendered in the `restJson1` error envelope —
/// `Content-Type: application/json`, an `x-amzn-errortype` naming a MODELED error, and
/// that error's modeled HTTP status.
#[tokio::test]
async fn every_signaling_channel_failure_is_a_modeled_restjson1_error() {
    let router = harness();
    let arn = create_channel(&router, "envelope").await;
    wait_for_activation();

    // The errors the channel surface declares, one row each: the status comes from the
    // shape's `@httpError` (`smithy_query(query="operation", …)` on each operation).
    let rows: Vec<(&str, &str, serde_json::Value, u16, &str)> = vec![
        (
            "a create under a name already in the store",
            "/createSignalingChannel",
            serde_json::json!({ "ChannelName": "envelope" }),
            400,
            "ResourceInUseException",
        ),
        (
            "a create with a name outside the modeled ChannelName pattern",
            "/createSignalingChannel",
            serde_json::json!({ "ChannelName": "not a valid name!" }),
            400,
            "InvalidArgumentException",
        ),
        (
            "a describe of a channel that is not in the store",
            "/describeSignalingChannel",
            serde_json::json!({ "ChannelName": "absent" }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "an endpoint request for a channel that is not in the store",
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent/1",
                "SingleMasterChannelEndpointConfiguration": {
                    "Protocols": ["WSS"], "Role": "MASTER"
                }
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a tag listing for a resource that is not in the store",
            "/ListTagsForResource",
            serde_json::json!({
                "ResourceARN": "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent/1"
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a media-storage read for a channel that is not in the store",
            "/describeMediaStorageConfiguration",
            serde_json::json!({ "ChannelName": "absent" }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a delete of a channel that is not in the store",
            "/deleteSignalingChannel",
            serde_json::json!({
                "ChannelARN": "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent/1"
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a delete whose CurrentVersion is not the stored one",
            "/deleteSignalingChannel",
            serde_json::json!({ "ChannelARN": arn.clone(), "CurrentVersion": "stale1" }),
            400,
            "VersionMismatchException",
        ),
        (
            "a listing resumed from a token this service never minted",
            "/listSignalingChannels",
            serde_json::json!({ "NextToken": "bm90LWEtdG9rZW4=" }),
            400,
            "InvalidArgumentException",
        ),
        (
            "a WEBRTC endpoint on a channel not configured for ingestion",
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": {
                    "Protocols": ["WEBRTC"], "Role": "MASTER"
                }
            }),
            400,
            "InvalidArgumentException",
        ),
        (
            "an endpoint request naming no endpoint configuration",
            "/getSignalingChannelEndpoint",
            serde_json::json!({ "ChannelARN": arn.clone() }),
            400,
            "InvalidArgumentException",
        ),
        (
            "an endpoint request naming protocols but no role",
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": { "Protocols": ["WSS"] }
            }),
            400,
            "InvalidArgumentException",
        ),
        (
            "an endpoint request naming a role but no protocols",
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": { "Role": "MASTER" }
            }),
            400,
            "InvalidArgumentException",
        ),
    ];

    for (case, path, body, expected_status, expected_code) in rows {
        let answer = call(&router, path, body).await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected HTTP {expected_status} {expected_code:?}, got {} {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.content_type, "application/json",
            "case {case:?}: a restJson1 error carries Content-Type application/json, got {:?}",
            answer.content_type
        );
        assert_eq!(
            answer.error_type, expected_code,
            "case {case:?}: a restJson1 error names its shape in x-amzn-errortype, got {:?}",
            answer.error_type
        );
    }

    // The refused create stored nothing.
    let answer = call(
        &router,
        "/describeSignalingChannel",
        serde_json::json!({ "ChannelName": "not" }),
    )
    .await;
    assert_eq!(
        answer.status, 404,
        "a refused create must store no record, got {} with {:?}",
        answer.status, answer.body
    );
}

/// RULE: a create that names no `ChannelType` or `SingleMasterConfiguration` stores the
/// values AWS documents as the defaults (`SINGLE_MASTER`, a 60-second message TTL), and
/// one that names them stores what it was sent — read back through the describe.
///
/// `MessageTtlSeconds` rows walk its modeled `range` (5..120, from
/// `smithy_query(query="shape", shape="MessageTtlSeconds")`): each published bound, a
/// value between them, and one past each bound, which the boundary refuses as this
/// service's `InvalidArgumentException`.
#[tokio::test]
async fn a_create_stores_the_documented_defaults_and_every_value_it_is_sent() {
    let router = harness();

    // (case, name, request extras, expected status, expected ChannelType, expected TTL)
    let rows: Vec<(&str, &str, serde_json::Value, u16, &str, i64)> = vec![
        (
            "neither type nor configuration named",
            "defaults",
            serde_json::json!({}),
            200,
            "SINGLE_MASTER",
            60,
        ),
        (
            "an empty SingleMasterConfiguration",
            "empty-config",
            serde_json::json!({ "SingleMasterConfiguration": {} }),
            200,
            "SINGLE_MASTER",
            60,
        ),
        (
            "the type named explicitly",
            "typed",
            serde_json::json!({ "ChannelType": "SINGLE_MASTER" }),
            200,
            "SINGLE_MASTER",
            60,
        ),
        (
            "the other modeled ChannelType",
            "full-mesh",
            serde_json::json!({ "ChannelType": "FULL_MESH" }),
            200,
            "FULL_MESH",
            60,
        ),
        (
            "the published minimum TTL",
            "ttl-min",
            serde_json::json!({ "SingleMasterConfiguration": { "MessageTtlSeconds": 5 } }),
            200,
            "SINGLE_MASTER",
            5,
        ),
        (
            "a TTL between the bounds",
            "ttl-mid",
            serde_json::json!({ "SingleMasterConfiguration": { "MessageTtlSeconds": 37 } }),
            200,
            "SINGLE_MASTER",
            37,
        ),
        (
            "the published maximum TTL",
            "ttl-max",
            serde_json::json!({ "SingleMasterConfiguration": { "MessageTtlSeconds": 120 } }),
            200,
            "SINGLE_MASTER",
            120,
        ),
        (
            "one below the published minimum TTL",
            "ttl-under",
            serde_json::json!({ "SingleMasterConfiguration": { "MessageTtlSeconds": 4 } }),
            400,
            "",
            0,
        ),
        (
            "one above the published maximum TTL",
            "ttl-over",
            serde_json::json!({ "SingleMasterConfiguration": { "MessageTtlSeconds": 121 } }),
            400,
            "",
            0,
        ),
        (
            "a ChannelType outside the modeled enum",
            "bad-type",
            serde_json::json!({ "ChannelType": "MULTI_MASTER" }),
            400,
            "",
            0,
        ),
    ];

    for (case, name, extra, expected_status, expected_type, expected_ttl) in rows {
        let mut request = serde_json::json!({ "ChannelName": name });
        if let (Some(target), Some(source)) = (request.as_object_mut(), extra.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        let answer = call(&router, "/createSignalingChannel", request).await;
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} with {:?}",
            answer.status, answer.body
        );
        if expected_status != 200 {
            assert_eq!(
                answer.error_code(),
                "InvalidArgumentException",
                "case {case:?}: a value outside a modeled constraint is this service's \
                 InvalidArgumentException, got {:?}",
                answer.body
            );
            continue;
        }
        let info = describe_channel(&router, name).await;
        assert_eq!(
            info["ChannelType"], expected_type,
            "case {case:?}: expected ChannelType {expected_type:?}, got {info:?}"
        );
        assert_eq!(
            info["SingleMasterConfiguration"]["MessageTtlSeconds"], expected_ttl,
            "case {case:?}: expected MessageTtlSeconds {expected_ttl}, got {info:?}"
        );
    }
}

/// RULE: `GetSignalingChannelEndpoint` answers with ONE endpoint per `ChannelProtocol`
/// the request asks for, in the scheme that protocol names, addressed at the emulator
/// the request arrived on — so a client can connect to what it is handed. The entries
/// are listed by `ChannelProtocol` value, which is the service's own order rather than
/// the request's, so the same set of protocols is answered the same way whichever order
/// the request names them in.
///
/// The protocol catalog is the modeled `ChannelProtocol` enum
/// (`smithy_query(query="shape", shape="ChannelProtocol")`, exposed as
/// [`kvs::ops::types::CHANNEL_PROTOCOL_VALUES`]): rows cover each of its values —
/// including `WEBRTC`, which the issue did not cite — in both roles, singly and
/// together.
#[tokio::test]
async fn an_endpoint_is_reported_for_every_protocol_the_request_names() {
    let router = harness();
    let arn = create_channel(&router, "endpoints").await;
    wait_for_activation();

    // Every modeled protocol must be covered by the scheme table below, so a protocol
    // added to the model shows up here rather than silently.
    let covered: BTreeSet<&str> = ["WSS", "HTTPS", "WEBRTC"].into_iter().collect();
    let modeled: BTreeSet<&str> = kvs::ops::types::CHANNEL_PROTOCOL_VALUES
        .iter()
        .copied()
        .collect();
    assert_eq!(
        covered, modeled,
        "the rows below must cover every modeled ChannelProtocol, got {covered:?} for {modeled:?}"
    );

    // One row of the endpoint table: the case name, the protocols the request asks for,
    // the `Role` it names (`None` for a request that names none), and the
    // protocol-and-scheme pairs the answer must carry, in order.
    type EndpointRow<'a> = (
        &'a str,
        Vec<&'a str>,
        Option<&'a str>,
        Vec<(&'a str, &'a str)>,
    );

    let rows: Vec<EndpointRow<'_>> = vec![
        (
            "a websocket endpoint for the master",
            vec!["WSS"],
            Some("MASTER"),
            vec![("WSS", "wss")],
        ),
        (
            "a websocket endpoint for a viewer",
            vec!["WSS"],
            Some("VIEWER"),
            vec![("WSS", "wss")],
        ),
        (
            "an HTTPS endpoint",
            vec!["HTTPS"],
            Some("VIEWER"),
            vec![("HTTPS", "https")],
        ),
        (
            "both signaling protocols at once",
            vec!["WSS", "HTTPS"],
            Some("MASTER"),
            vec![("HTTPS", "https"), ("WSS", "wss")],
        ),
        (
            "the same two named in the other order",
            vec!["HTTPS", "WSS"],
            Some("MASTER"),
            vec![("HTTPS", "https"), ("WSS", "wss")],
        ),
    ];

    for (case, protocols, role, expected) in rows {
        let mut configuration = serde_json::json!({ "Protocols": protocols });
        if let (Some(target), Some(role)) = (configuration.as_object_mut(), role) {
            target.insert("Role".to_string(), serde_json::json!(role));
        }
        let answer = call(
            &router,
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": configuration
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let listed = answer.body["ResourceEndpointList"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            listed.len(),
            expected.len(),
            "case {case:?}: expected {} endpoints, got {listed:?}",
            expected.len()
        );
        for (index, (protocol, scheme)) in expected.iter().enumerate() {
            assert_eq!(
                listed[index]["Protocol"], *protocol,
                "case {case:?}: endpoint {index} must name {protocol}, got {:?}",
                listed[index]
            );
            let reported = listed[index]["ResourceEndpoint"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if *protocol == "WSS" {
                // "If you specify WSS as the protocol, this API produces a secure
                // websocket endpoint": a peer completes an opening handshake against it,
                // which this emulator serves on a socket of its own rather than at its
                // front door, so the announced value is the address that socket landed on
                // — loopback, because that is where the core binds a resource listener.
                let authority = reported
                    .strip_prefix("wss://")
                    .unwrap_or_else(|| panic!("case {case:?}: expected a wss:// URL, got {reported:?}"));
                let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
                    panic!("case {case:?}: expected a host:port, got {authority:?}")
                });
                assert_eq!(
                    host, "127.0.0.1",
                    "case {case:?}: the websocket endpoint must be one this emulator serves, \
                     got {reported:?}"
                );
                assert!(
                    port.parse::<u16>().is_ok_and(|port| port > 0),
                    "case {case:?}: the websocket endpoint must name the port it is served \
                     on, got {reported:?}"
                );
            } else {
                assert_eq!(
                    reported,
                    format!("{scheme}://{ENDPOINT_HOST}"),
                    "case {case:?}: endpoint {index} must address this emulator over \
                     {scheme}, got {:?}",
                    listed[index]
                );
            }
        }
    }

    // A request that leaves the endpoint configuration, its protocols or its role
    // unnamed names no endpoint to resolve, so it is a bad argument rather than an empty
    // success: neither the protocol nor the role can be inferred.
    for (case, body) in [
        (
            "no endpoint configuration at all",
            serde_json::json!({ "ChannelARN": arn.clone() }),
        ),
        (
            "an empty endpoint configuration",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": {}
            }),
        ),
        (
            "a role but no protocols",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": { "Role": "VIEWER" }
            }),
        ),
        (
            "protocols but no role",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": { "Protocols": ["HTTPS"] }
            }),
        ),
    ] {
        let answer = call(&router, "/getSignalingChannelEndpoint", body).await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected HTTP 400 InvalidArgumentException, got {} {:?}",
            answer.status,
            answer.body
        );
    }
}

/// RULE: `ListSignalingChannels` pages over the stored index in name order, filtered by
/// the modeled `ChannelNameCondition` (whose one `ComparisonOperator`, `BEGINS_WITH`,
/// makes it a name prefix), and emits a `NextToken` only while a channel is left over.
#[tokio::test]
async fn the_listing_pages_and_filters_over_the_stored_index() {
    let router = harness();
    // Names chosen so the prefixes partition them differently: two share `alpha`, one
    // shares only `al`, and one shares nothing.
    for name in ["alpha.one", "alpha.two", "al_other", "beta"] {
        create_channel(&router, name).await;
    }
    wait_for_activation();

    // (case, condition, expected names in order)
    let rows: Vec<(&str, serde_json::Value, Vec<&str>)> = vec![
        (
            "no condition at all",
            serde_json::json!({}),
            vec!["al_other", "alpha.one", "alpha.two", "beta"],
        ),
        (
            "a prefix two channels share",
            serde_json::json!({
                "ChannelNameCondition": {
                    "ComparisonOperator": "BEGINS_WITH", "ComparisonValue": "alpha"
                }
            }),
            vec!["alpha.one", "alpha.two"],
        ),
        (
            "a shorter prefix three channels share",
            serde_json::json!({
                "ChannelNameCondition": {
                    "ComparisonOperator": "BEGINS_WITH", "ComparisonValue": "al"
                }
            }),
            vec!["al_other", "alpha.one", "alpha.two"],
        ),
        (
            "a whole name as the prefix",
            serde_json::json!({
                "ChannelNameCondition": {
                    "ComparisonOperator": "BEGINS_WITH", "ComparisonValue": "beta"
                }
            }),
            vec!["beta"],
        ),
        (
            "a prefix no channel shares",
            serde_json::json!({
                "ChannelNameCondition": {
                    "ComparisonOperator": "BEGINS_WITH", "ComparisonValue": "gamma"
                }
            }),
            vec![],
        ),
    ];

    for (case, body, expected) in rows {
        let answer = call(&router, "/listSignalingChannels", body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let names: Vec<String> = answer.body["ChannelInfoList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|entry| entry["ChannelName"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected {expected:?}, got {names:?}"
        );
        assert!(
            answer.body.get("NextToken").is_none_or(
                serde_json::Value::is_null
            ),
            "case {case:?}: a complete page carries no NextToken, got {:?}",
            answer.body
        );
    }

    // Paging with every page size from one to one past the index, walking the token
    // chain: the pages concatenate to the whole index, once each, in name order.
    for page_size in 1..=5 {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..6 {
            let mut body = serde_json::json!({ "MaxResults": page_size });
            if let (Some(target), Some(token)) = (body.as_object_mut(), token.as_deref()) {
                target.insert("NextToken".to_string(), serde_json::json!(token));
            }
            let answer = call(&router, "/listSignalingChannels", body).await;
            assert_eq!(
                answer.status, 200,
                "case page size {page_size}: expected HTTP 200, got {} with {:?}",
                answer.status, answer.body
            );
            let page = answer.body["ChannelInfoList"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case page size {page_size}: a page must hold at most {page_size} entries, \
                 got {page:?}"
            );
            seen.extend(
                page.iter()
                    .map(|entry| entry["ChannelName"].as_str().unwrap_or("").to_string()),
            );
            token = answer.body["NextToken"].as_str().map(ToString::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case page size {page_size}: the token chain must terminate, got {token:?}"
        );
        assert_eq!(
            seen,
            vec!["al_other", "alpha.one", "alpha.two", "beta"],
            "case page size {page_size}: the pages must concatenate to the whole index, got \
             {seen:?}"
        );
    }
}

/// RULE: `ListTagsForResource` reports the tags the addressed resource carries — the map
/// the create stored, and an EMPTY map for a resource that carries none.
#[tokio::test]
async fn the_tag_reader_reports_the_tags_the_resource_carries() {
    let router = harness();

    // (case, Tags on the create, the map the reader must report)
    let rows: Vec<(&str, serde_json::Value, serde_json::Value)> = vec![
        (
            "no Tags member at all",
            serde_json::json!({}),
            serde_json::json!({}),
        ),
        (
            "an empty tag list",
            serde_json::json!({ "Tags": [] }),
            serde_json::json!({}),
        ),
        (
            "one tag",
            serde_json::json!({ "Tags": [{ "Key": "owner", "Value": "team" }] }),
            serde_json::json!({ "owner": "team" }),
        ),
        (
            "a tag whose value is empty, which TagValue admits",
            serde_json::json!({ "Tags": [{ "Key": "owner", "Value": "" }] }),
            serde_json::json!({ "owner": "" }),
        ),
        (
            "several tags",
            serde_json::json!({ "Tags": [
                { "Key": "owner", "Value": "team" },
                { "Key": "stage", "Value": "prod" },
                { "Key": "cost.center", "Value": "cc-1" }
            ] }),
            serde_json::json!({ "owner": "team", "stage": "prod", "cost.center": "cc-1" }),
        ),
        (
            "two entries under one key, the later winning",
            serde_json::json!({ "Tags": [
                { "Key": "owner", "Value": "first" },
                { "Key": "owner", "Value": "second" }
            ] }),
            serde_json::json!({ "owner": "second" }),
        ),
    ];

    for (index, (case, extra, expected)) in rows.into_iter().enumerate() {
        let name = format!("tagged-{index}");
        let arn = create_channel_with(&router, &name, extra).await;
        let answer = call(
            &router,
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": arn }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let reported = answer
            .body
            .get("Tags")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected Tags {expected:?}, got {reported:?}"
        );
        // `Tags` and `NextToken` are the whole body: every tag of a resource fits in
        // one page, so no resume token is minted — and the member still reaches the
        // wire as JSON `null`, the form AWS answers this service's listings with, so a
        // client dereferencing it gets `null` rather than `undefined`.
        assert_eq!(
            answer.body,
            serde_json::json!({ "NextToken": null, "Tags": expected }),
            "case {case:?}: the whole response body must be the tag map and a null NextToken, got {:?}",
            answer.body
        );
    }
}

/// RULE: the three tagging operations keyed on a bare `ResourceARN` address whichever
/// resource the ARN names — the modeled `ResourceARN` pattern leaves the resource type
/// open (`…:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+`) and this service mints two of them, a
/// `channel` and a `stream` — so a tag written through one is read back through the
/// other, and an ARN that names neither is a missing resource (404).
#[tokio::test]
async fn the_tagging_operations_address_whichever_resource_the_arn_names() {
    let router = harness();

    let channel_arn = create_channel(&router, "tagged-channel").await;
    let stream_answer = call(
        &router,
        "/createStream",
        serde_json::json!({ "StreamName": "tagged-stream" }),
    )
    .await;
    assert_eq!(
        stream_answer.status, 200,
        "CreateStream failed with {:?}",
        stream_answer.body
    );
    let stream_arn = stream_answer.body["StreamARN"]
        .as_str()
        .expect("CreateStream returns a StreamARN")
        .to_string();
    wait_for_activation();

    // Both resource types this service mints, so the rule is proved on the type the
    // issue cited AND on the one it did not.
    for (case, arn) in [
        ("a signaling channel's ARN", channel_arn.clone()),
        ("a stream's ARN", stream_arn.clone()),
    ] {
        let answer = call(
            &router,
            "/TagResource",
            serde_json::json!({
                "ResourceARN": arn.clone(),
                "Tags": [
                    { "Key": "owner", "Value": "team" },
                    { "Key": "stage", "Value": "test" }
                ]
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: TagResource expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );

        let answer = call(
            &router,
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": arn.clone() }),
        )
        .await;
        assert_eq!(
            answer.body["Tags"],
            serde_json::json!({ "owner": "team", "stage": "test" }),
            "case {case:?}: the tags written must read back, got {:?}",
            answer.body
        );

        // A second write replaces one key's value and adds another, leaving the rest.
        let answer = call(
            &router,
            "/TagResource",
            serde_json::json!({
                "ResourceARN": arn.clone(),
                "Tags": [
                    { "Key": "stage", "Value": "prod" },
                    { "Key": "cost.center", "Value": "cc-1" }
                ]
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a second TagResource expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let answer = call(
            &router,
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": arn.clone() }),
        )
        .await;
        assert_eq!(
            answer.body["Tags"],
            serde_json::json!({ "owner": "team", "stage": "prod", "cost.center": "cc-1" }),
            "case {case:?}: a second write replaces a key and adds another, got {:?}",
            answer.body
        );

        // Untagging removes exactly the keys named — a key the resource does not carry
        // removes nothing and is not an error.
        let answer = call(
            &router,
            "/UntagResource",
            serde_json::json!({
                "ResourceARN": arn.clone(),
                "TagKeyList": ["stage", "never-set"]
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: UntagResource expected HTTP 200, got {} with {:?}",
            answer.status, answer.body
        );
        let answer = call(
            &router,
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": arn.clone() }),
        )
        .await;
        assert_eq!(
            answer.body["Tags"],
            serde_json::json!({ "owner": "team", "cost.center": "cc-1" }),
            "case {case:?}: untagging removes exactly the keys named, got {:?}",
            answer.body
        );
    }

    // An ARN that names no stored resource is a missing resource for all three, and a
    // write past the per-resource tag ceiling is refused with the modeled error and
    // leaves the tag map as it was.
    let absent = "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent/1";
    let rows: Vec<(&str, &str, serde_json::Value, u16, &str)> = vec![
        (
            "TagResource on an ARN that names nothing",
            "/TagResource",
            serde_json::json!({
                "ResourceARN": absent,
                "Tags": [{ "Key": "k", "Value": "v" }]
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "UntagResource on an ARN that names nothing",
            "/UntagResource",
            serde_json::json!({ "ResourceARN": absent, "TagKeyList": ["k"] }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "ListTagsForResource on an ARN that names nothing",
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": absent }),
            404,
            "ResourceNotFoundException",
        ),
    ];
    for (case, path, body, expected_status, expected_code) in rows {
        let answer = call(&router, path, body).await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected HTTP {expected_status} {expected_code:?}, got {} {:?}",
            answer.status,
            answer.body
        );
    }

    // `MAX_TAGS_PER_RESOURCE` is the modeled `ResourceTags` maximum; the channel already
    // carries two keys, so a write of that many NEW keys crosses the ceiling.
    let over: Vec<serde_json::Value> = (0..kvs::state::MAX_TAGS_PER_RESOURCE)
        .map(|index| serde_json::json!({ "Key": format!("bulk{index}"), "Value": "v" }))
        .collect();
    let answer = call(
        &router,
        "/TagResource",
        serde_json::json!({ "ResourceARN": channel_arn.clone(), "Tags": over }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (400, "TagsPerResourceExceededLimitException"),
        "a write past the per-resource tag ceiling is refused, got {} {:?}",
        answer.status,
        answer.body
    );
    let answer = call(
        &router,
        "/ListTagsForResource",
        serde_json::json!({ "ResourceARN": channel_arn }),
    )
    .await;
    assert_eq!(
        answer.body["Tags"],
        serde_json::json!({ "owner": "team", "cost.center": "cc-1" }),
        "a refused write must leave the tag map as it was, got {:?}",
        answer.body
    );
}

/// RULE: `UpdateSignalingChannel` replaces the channel's `SingleMasterConfiguration`
/// only when `CurrentVersion` is the stored version; it then re-mints the version and
/// leaves the channel in the non-terminal status the asynchronous update owes, with the
/// clock moving it back to `ACTIVE`.
///
/// The `MessageTtlSeconds` rows walk the modeled `range` (5..120) as the create's do:
/// each published bound, a value between them, and one past each bound, which the
/// boundary refuses.
#[tokio::test]
async fn an_update_honours_the_version_guard_and_stores_the_new_configuration() {
    let router = harness();
    let arn = create_channel(&router, "updatable").await;
    wait_for_activation();
    let mut version = describe_channel(&router, "updatable").await["Version"]
        .as_str()
        .expect("ChannelInfo carries a Version")
        .to_string();

    // A stale version is refused and the record, its TTL and its version are untouched.
    let answer = call(
        &router,
        "/updateSignalingChannel",
        serde_json::json!({
            "ChannelARN": arn.clone(),
            "CurrentVersion": "stale1",
            "SingleMasterConfiguration": { "MessageTtlSeconds": 30 }
        }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (400, "VersionMismatchException"),
        "a stale CurrentVersion must be refused, got {} {:?}",
        answer.status,
        answer.body
    );
    let info = describe_channel(&router, "updatable").await;
    assert_eq!(
        (
            info["SingleMasterConfiguration"]["MessageTtlSeconds"].clone(),
            info["Version"].clone()
        ),
        (
            serde_json::json!(kvs::state::DEFAULT_MESSAGE_TTL_SECONDS),
            serde_json::json!(version.clone())
        ),
        "a refused update must change nothing, got {info:?}"
    );

    // (case, the TTL the update names, expected status, the TTL stored afterwards)
    let rows: Vec<(&str, i64, u16, i64)> = vec![
        ("the published minimum TTL", 5, 200, 5),
        ("a TTL between the bounds", 73, 200, 73),
        ("the published maximum TTL", 120, 200, 120),
        ("one below the published minimum TTL", 4, 400, 120),
        ("one above the published maximum TTL", 121, 400, 120),
    ];

    for (case, ttl, expected_status, expected_stored) in rows {
        let answer = call(
            &router,
            "/updateSignalingChannel",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "CurrentVersion": version.clone(),
                "SingleMasterConfiguration": { "MessageTtlSeconds": ttl }
            }),
        )
        .await;
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} with {:?}",
            answer.status, answer.body
        );
        if expected_status == 200 {
            // The update is asynchronous, so the channel is left in the transient status
            // the clock owes ACTIVE.
            let info = describe_channel(&router, "updatable").await;
            assert_eq!(
                info["ChannelStatus"], "UPDATING",
                "case {case:?}: a channel read straight after its update is UPDATING: {info:?}"
            );
            wait_for_activation();
        }
        let info = describe_channel(&router, "updatable").await;
        assert_eq!(
            info["SingleMasterConfiguration"]["MessageTtlSeconds"],
            serde_json::json!(expected_stored),
            "case {case:?}: expected the stored TTL to be {expected_stored}, got {info:?}"
        );
        assert_eq!(
            info["ChannelStatus"], "ACTIVE",
            "case {case:?}: the clock owes the channel ACTIVE: {info:?}"
        );
        let next = info["Version"]
            .as_str()
            .expect("ChannelInfo carries a Version")
            .to_string();
        if expected_status == 200 {
            assert_ne!(
                next, version,
                "case {case:?}: a successful update re-mints the Version, got {next:?} twice"
            );
        } else {
            assert_eq!(
                next, version,
                "case {case:?}: a refused update leaves the Version alone, got {next:?}"
            );
        }
        version = next;
    }

    // An update of a channel that is not in the store is a missing resource.
    let answer = call(
        &router,
        "/updateSignalingChannel",
        serde_json::json!({
            "ChannelARN": "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent/1",
            "CurrentVersion": version
        }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (404, "ResourceNotFoundException"),
        "updating a channel that is not in the store: got {} {:?}",
        answer.status,
        answer.body
    );
}

/// RULE: a delete guarded by `CurrentVersion` removes the channel only when the version
/// it names is the stored one; a stale version leaves the record intact, and an absent
/// one deletes the most recent version.
#[tokio::test]
async fn a_delete_honours_the_version_guard() {
    let router = harness();

    let guarded = create_channel(&router, "guarded").await;
    wait_for_activation();
    let version = describe_channel(&router, "guarded").await["Version"]
        .as_str()
        .expect("ChannelInfo carries a Version")
        .to_string();

    // A stale version is refused and the channel survives.
    let answer = call(
        &router,
        "/deleteSignalingChannel",
        serde_json::json!({ "ChannelARN": guarded.clone(), "CurrentVersion": "0" }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (400, "VersionMismatchException"),
        "a stale CurrentVersion must be refused, got {} {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        describe_channel(&router, "guarded").await["Version"],
        serde_json::json!(version.clone()),
        "a refused delete must leave the record, and its version, intact"
    );

    // The stored version deletes it, and a second delete of the same ARN is a missing
    // resource rather than a second success.
    for (case, expected_status, expected_code) in [
        ("the first delete", 200, ""),
        ("the second delete of the same ARN", 404, "ResourceNotFoundException"),
    ] {
        let answer = call(
            &router,
            "/deleteSignalingChannel",
            serde_json::json!({ "ChannelARN": guarded.clone(), "CurrentVersion": version.clone() }),
        )
        .await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (expected_status, expected_code),
            "case {case:?}: expected HTTP {expected_status} {expected_code:?}, got {} {:?}",
            answer.status,
            answer.body
        );
    }

    // A delete naming no version deletes the most recent one.
    let unguarded = create_channel(&router, "unguarded").await;
    let answer = call(
        &router,
        "/deleteSignalingChannel",
        serde_json::json!({ "ChannelARN": unguarded }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a delete naming no CurrentVersion deletes the most recent version, got {} with {:?}",
        answer.status, answer.body
    );
    let answer = call(
        &router,
        "/describeSignalingChannel",
        serde_json::json!({ "ChannelName": "unguarded" }),
    )
    .await;
    assert_eq!(
        answer.status, 404,
        "a deleted channel must not read back, got {} with {:?}",
        answer.status, answer.body
    );
}

/// RULE: a channel's `ChannelARN` and `Version` satisfy the modeled `@pattern`s their
/// shapes carry — proved by feeding each generated value BACK IN through an input member
/// that carries the same pattern, so the boundary validator is the oracle rather than a
/// regex written here.
#[tokio::test]
async fn the_minted_arn_and_version_satisfy_their_modeled_patterns() {
    let router = harness();

    // Names chosen along what `ChannelName`'s pattern `^[a-zA-Z0-9_.-]+$` admits: the
    // separators, the digits, the case, and its one-character minimum length.
    let names = [
        "a",
        "simple",
        "with-hyphens",
        "with_underscores",
        "with.dots",
        "MiXeDcAsE",
        "0123456789",
        "all_of.the-SEPARATORS.0",
    ];

    for name in names {
        let arn = create_channel(&router, name).await;
        let info = describe_channel(&router, name).await;
        let version = info["Version"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: ChannelInfo carries a Version: {info:?}"))
            .to_string();

        // `DeleteSignalingChannel` takes `ChannelARN` (the ResourceARN pattern) and
        // `CurrentVersion` (the Version pattern), so a value outside either one is
        // refused at the boundary before the handler runs.
        let answer = call(
            &router,
            "/deleteSignalingChannel",
            serde_json::json!({ "ChannelARN": arn.clone(), "CurrentVersion": version.clone() }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {name:?}: the minted ChannelARN {arn:?} and Version {version:?} must satisfy \
             the patterns their shapes carry, got {} with {:?}",
            answer.status, answer.body
        );
    }
}

/// The sentence this service refuses a channel mutation quoting a stale `CurrentVersion`
/// with. Wholly fixed text: it names the two versions it compared without quoting
/// either, which is why every row below expects the very same string.
const CHANNEL_VERSION_MISMATCH: &str =
    "The provided channel version does not match the existing channel version.";

/// The same failure on the STREAM surface, which the channel sentence is the companion
/// of: the resource whose versions were compared is what differs between the two.
const STREAM_VERSION_MISMATCH: &str =
    "The provided stream version does not match the existing stream version.";

/// The sentence a `WEBRTC` endpoint request on a channel with no media-storage
/// configuration is refused with: fixed text naming the configuration that is missing
/// and the protocol that requires it.
const WEBRTC_WITHOUT_MEDIA_STORAGE: &str =
    "MediaStorageConfiguration is required for WEBRTC protocol";

/// The `ResourceNotFoundException` sentence for an ARN naming a resource of `kind` that
/// resolves to nothing. The resource word is the one the ARN's own resource segment
/// names — the resource field the modeled `ResourceARN` pattern leaves open
/// (`…:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+`, from
/// `smithy_query(query="shape", shape="ResourceARN")`), which this service writes as
/// `channel/<name>/<millis>` and `stream/<name>/<millis>`.
fn not_found_sentence(kind: &str) -> String {
    format!("The requested {kind} is not found or not active.")
}

/// Create a stream carrying `retention` hours of data retention, and return its
/// `StreamARN` — the other resource kind a `ResourceARN` addresses.
async fn create_stream(router: &MultiServiceRouter, name: &str, retention: i32) -> String {
    let answer = call(
        router,
        "/createStream",
        serde_json::json!({ "StreamName": name, "DataRetentionInHours": retention }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: CreateStream failed with {:?}",
        answer.body
    );
    answer
        .body
        .get("StreamARN")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: CreateStream returned no StreamARN: {:?}",
                answer.body
            )
        })
        .to_string()
}

/// The ARN of a resource that is NOT stored, built from a stored one by moving its
/// trailing creation-instant segment: same account, region, kind and name, different
/// resource.
fn arn_of_another_instant(arn: &str) -> String {
    let (prefix, _) = arn
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("a minted ARN ends in its creation instant, got {arn:?}"));
    format!("{prefix}/1")
}

/// RULE: a mutation guarded by `CurrentVersion` refuses a version that is not the stored
/// one with `VersionMismatchException` carrying the fixed version-conflict sentence of
/// the resource kind it addresses — the same sentence for every such request, with no
/// name, ARN or version string interpolated — and the stored record is left untouched.
#[tokio::test]
async fn a_guarded_channel_mutation_refuses_a_stale_version_with_the_channel_sentence() {
    let router = harness();

    // Rows differ along the rejected version's shape: one the channel never carried, the
    // shortest and the longest the model's `Version` allows (`@length` 1..64, `@pattern`
    // `^[a-zA-Z0-9]+$`), and a mixed-case one.
    let rows: Vec<(&str, String)> = vec![
        (
            "a version the channel never carried",
            "notthisversion1".to_string(),
        ),
        ("the shortest version the model allows", "7".to_string()),
        ("the longest version the model allows", "v".repeat(64)),
        ("a mixed-case alphanumeric version", "Zz0099Aa".to_string()),
    ];

    for (index, (case, stale)) in rows.into_iter().enumerate() {
        let name = format!("channel-version-guard-{index}");
        let arn = create_channel(&router, &name).await;
        let stored = describe_channel(&router, &name).await["Version"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: ChannelInfo carries a Version"))
            .to_string();

        // Both channel mutations that take a version word the refusal the same way: one
        // sentence per failure class, not one per operation.
        for (operation, path, body) in [
            (
                "UpdateSignalingChannel",
                "/updateSignalingChannel",
                serde_json::json!({
                    "ChannelARN": arn.clone(),
                    "CurrentVersion": stale.clone(),
                    "SingleMasterConfiguration": { "MessageTtlSeconds": 30 }
                }),
            ),
            (
                "DeleteSignalingChannel",
                "/deleteSignalingChannel",
                serde_json::json!({ "ChannelARN": arn.clone(), "CurrentVersion": stale.clone() }),
            ),
        ] {
            let answer = call(&router, path, body).await;
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (400, "VersionMismatchException"),
                "case {case:?} on {operation}: expected HTTP 400 VersionMismatchException, \
                 got {} {:?}",
                answer.status,
                answer.body
            );
            let actual = answer.message();
            assert_eq!(
                actual, CHANNEL_VERSION_MISMATCH,
                "case {case:?} on {operation}: expected Message \
                 {CHANNEL_VERSION_MISMATCH:?}, got {actual:?}"
            );
            // The sentence is fixed, so neither the rejected version, the stored one nor
            // the addressed channel appears in it.
            for (part, value) in [
                ("the rejected version", stale.as_str()),
                ("the stored version", stored.as_str()),
                ("the channel name", name.as_str()),
            ] {
                assert!(
                    !actual.contains(value),
                    "case {case:?} on {operation}: the fixed sentence must not quote {part} \
                     {value:?}, got {actual:?}"
                );
            }
        }

        // Both refusals left the record exactly as it was.
        let info = describe_channel(&router, &name).await;
        assert_eq!(
            (
                info["Version"].clone(),
                info["SingleMasterConfiguration"]["MessageTtlSeconds"].clone()
            ),
            (
                serde_json::json!(stored),
                serde_json::json!(kvs::state::DEFAULT_MESSAGE_TTL_SECONDS)
            ),
            "case {case:?}: a refused mutation must change nothing, got {info:?}"
        );
    }

    // The stream surface words the same failure for ITS resource, so the two sentences
    // differ only in the resource whose versions were compared.
    create_stream(&router, "version-sentence-stream", 24).await;
    let answer = call(
        &router,
        "/updateDataRetention",
        serde_json::json!({
            "StreamName": "version-sentence-stream",
            "CurrentVersion": "notthisversion1",
            "Operation": "INCREASE_DATA_RETENTION",
            "DataRetentionChangeInHours": 1
        }),
    )
    .await;
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (400, "VersionMismatchException"),
        "case \"a stale version on the stream surface\": expected HTTP 400 \
         VersionMismatchException, got {} {:?}",
        answer.status,
        answer.body
    );
    let actual = answer.message();
    assert_eq!(
        actual, STREAM_VERSION_MISMATCH,
        "case \"a stale version on the stream surface\": expected Message \
         {STREAM_VERSION_MISMATCH:?}, got {actual:?}"
    );
}

/// RULE: an endpoint request that leaves a member the resolution needs unnamed is refused
/// with `InvalidArgumentException` naming that member and the TYPE of the channel it
/// addresses, read off the stored record — and the `Role` refusal names the protocol the
/// request asked for as well, since the endpoint a protocol resolves to is the one for
/// the caller's role.
#[tokio::test]
async fn an_endpoint_request_missing_a_member_names_it_and_the_channel_type() {
    let router = harness();

    // Both modeled `ChannelType` values (`smithy_query(query="shape",
    // shape="ChannelType")`), so the sentence's channel type is shown to come from the
    // addressed record rather than being fixed.
    for channel_type in ["SINGLE_MASTER", "FULL_MESH"] {
        let name = format!("endpoint-members-{channel_type}");
        let arn = create_channel_with(
            &router,
            &name,
            serde_json::json!({ "ChannelType": channel_type }),
        )
        .await;

        // (case, the endpoint configuration the request carries, the expected sentence)
        let rows: Vec<(&str, Option<serde_json::Value>, String)> = vec![
            (
                "no endpoint configuration at all",
                None,
                format!(
                    "SingleMasterChannelEndpointConfiguration is required for \
                     {channel_type} channels."
                ),
            ),
            (
                "an empty endpoint configuration",
                Some(serde_json::json!({})),
                format!("Protocols is required for {channel_type} channels."),
            ),
            (
                "a role but no protocols",
                Some(serde_json::json!({ "Role": "MASTER" })),
                format!("Protocols is required for {channel_type} channels."),
            ),
            (
                "the websocket protocol but no role",
                Some(serde_json::json!({ "Protocols": ["WSS"] })),
                format!(
                    "Role is required for {channel_type} channels requests for WSS protocol."
                ),
            ),
            (
                "the HTTPS protocol but no role",
                Some(serde_json::json!({ "Protocols": ["HTTPS"] })),
                format!(
                    "Role is required for {channel_type} channels requests for HTTPS protocol."
                ),
            ),
            (
                "two protocols and no role",
                Some(serde_json::json!({ "Protocols": ["WSS", "HTTPS"] })),
                format!(
                    "Role is required for {channel_type} channels requests for WSS protocol."
                ),
            ),
            (
                "the same two in the other order, and no role",
                Some(serde_json::json!({ "Protocols": ["HTTPS", "WSS"] })),
                format!(
                    "Role is required for {channel_type} channels requests for HTTPS protocol."
                ),
            ),
        ];

        for (case, configuration, expected) in rows {
            let mut request = serde_json::json!({ "ChannelARN": arn.clone() });
            if let (Some(target), Some(configuration)) = (request.as_object_mut(), configuration) {
                target.insert(
                    "SingleMasterChannelEndpointConfiguration".to_string(),
                    configuration,
                );
            }
            let answer = call(&router, "/getSignalingChannelEndpoint", request).await;
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (400, "InvalidArgumentException"),
                "case {case:?} on a {channel_type} channel: expected HTTP 400 \
                 InvalidArgumentException, got {} {:?}",
                answer.status,
                answer.body
            );
            let actual = answer.message();
            assert_eq!(
                actual, expected,
                "case {case:?} on a {channel_type} channel: expected Message {expected:?}, \
                 got {actual:?}"
            );
        }
    }
}

/// RULE: an operation keyed on a bare `ResourceARN` addresses whichever resource kind
/// that ARN's resource segment names, so an ARN that resolves to no stored resource is
/// refused with HTTP 404 `ResourceNotFoundException` whose sentence names THAT kind.
#[tokio::test]
async fn an_arn_keyed_refusal_names_the_resource_kind_the_arn_names() {
    let router = harness();

    // One stored resource of each kind, so the only reason an ARN below resolves to
    // nothing is the ARN itself.
    let channel_arn = create_channel(&router, "tagged-channel").await;
    let stream_arn = create_stream(&router, "tagged-stream", 24).await;

    // (case, the ARN, the resource kind its resource segment names)
    let arns: Vec<(&str, String, &str)> = vec![
        (
            "a channel ARN naming no stored channel",
            "arn:aws:kinesisvideo:us-east-1:000000000000:channel/absent-channel/1700000000000"
                .to_string(),
            "channel",
        ),
        (
            "a stream ARN naming no stored stream",
            "arn:aws:kinesisvideo:us-east-1:000000000000:stream/absent-stream/1700000000000"
                .to_string(),
            "stream",
        ),
        (
            "a channel ARN whose name is stored but whose ARN is not",
            arn_of_another_instant(&channel_arn),
            "channel",
        ),
        (
            "a stream ARN whose name is stored but whose ARN is not",
            arn_of_another_instant(&stream_arn),
            "stream",
        ),
        (
            "a stream ARN of another region",
            "arn:aws:kinesisvideo:eu-west-1:000000000000:stream/tagged-stream/1700000000000"
                .to_string(),
            "stream",
        ),
    ];

    // The three operations the model keys on a bare `ResourceARN`.
    let operations: Vec<(&str, &str, serde_json::Value)> = vec![
        (
            "ListTagsForResource",
            "/ListTagsForResource",
            serde_json::json!({}),
        ),
        (
            "TagResource",
            "/TagResource",
            serde_json::json!({ "Tags": [{ "Key": "k", "Value": "v" }] }),
        ),
        (
            "UntagResource",
            "/UntagResource",
            serde_json::json!({ "TagKeyList": ["k"] }),
        ),
    ];

    for (case, arn, kind) in arns {
        for (operation, path, extra) in &operations {
            let mut request = serde_json::json!({ "ResourceARN": arn.clone() });
            if let (Some(target), Some(source)) = (request.as_object_mut(), extra.as_object()) {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
            let answer = call(&router, path, request).await;
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} on {operation}: expected HTTP 404 ResourceNotFoundException, \
                 got {} {:?}",
                answer.status,
                answer.body
            );
            let expected = not_found_sentence(kind);
            let actual = answer.message();
            assert_eq!(
                actual, expected,
                "case {case:?} on {operation}: expected Message {expected:?}, got {actual:?}"
            );
        }
    }

    // The ARNs that DO resolve are still read: the kind is read off the ARN for the
    // refusal only, and resolution is unchanged.
    for (case, arn) in [
        ("the stored channel's ARN", channel_arn),
        ("the stored stream's ARN", stream_arn),
    ] {
        let answer = call(
            &router,
            "/ListTagsForResource",
            serde_json::json!({ "ResourceARN": arn }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: an ARN that names a stored resource must be read, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["Tags"],
            serde_json::json!({}),
            "case {case:?}: an untagged resource reports an empty map, got {:?}",
            answer.body
        );
    }
}

/// RULE: a create under a name this account and region already holds is refused with
/// `ResourceInUseException` whose sentence names the resource kind and the requested
/// name, and nothing else.
#[tokio::test]
async fn a_duplicate_create_names_the_resource_kind_and_the_requested_name() {
    let router = harness();

    // Names differ along what the `ChannelName`/`StreamName` pattern
    // `^[a-zA-Z0-9_.-]+$` admits: its one-character minimum, the separators, the digits
    // and the case.
    for name in ["a", "dup.name-0", "MiXeD_case", "0123456789"] {
        // (case, the create's path, the name member, the sentence's resource word)
        for (operation, path, member, kind) in [
            (
                "CreateSignalingChannel",
                "/createSignalingChannel",
                "ChannelName",
                "channel",
            ),
            ("CreateStream", "/createStream", "StreamName", "stream"),
        ] {
            let request = serde_json::json!({ member: name });
            let answer = call(&router, path, request.clone()).await;
            assert_eq!(
                answer.status, 200,
                "case {name:?} on {operation}: the first create must be stored, got {} {:?}",
                answer.status, answer.body
            );

            let answer = call(&router, path, request).await;
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (400, "ResourceInUseException"),
                "case {name:?} on {operation}: expected HTTP 400 ResourceInUseException, \
                 got {} {:?}",
                answer.status,
                answer.body
            );
            let expected = format!("The {kind} {name} already exists.");
            let actual = answer.message();
            assert_eq!(
                actual, expected,
                "case {name:?} on {operation}: expected Message {expected:?}, got {actual:?}"
            );
        }
    }
}

/// RULE: a `WEBRTC` endpoint request on a channel that is not configured for ingestion is
/// refused with `InvalidArgumentException` naming the media-storage configuration it
/// requires, wherever `WEBRTC` sits in the requested protocols; the protocols that need
/// no such configuration are served, one endpoint each, listed by protocol value.
#[tokio::test]
async fn a_webrtc_request_without_media_storage_names_the_configuration_it_requires() {
    let router = harness();
    let arn = create_channel(&router, "webrtc-precondition").await;

    // Rows differ along where `WEBRTC` sits among the requested protocols, how many
    // protocols are asked for, and the `Role` the request names — the refusal is the
    // channel's missing configuration, so none of them changes it.
    let rows: Vec<(&str, Vec<&str>, &str)> = vec![
        ("WEBRTC alone, for the master", vec!["WEBRTC"], "MASTER"),
        ("WEBRTC alone, for a viewer", vec!["WEBRTC"], "VIEWER"),
        ("WEBRTC after another protocol", vec!["WSS", "WEBRTC"], "MASTER"),
        ("WEBRTC before another protocol", vec!["WEBRTC", "HTTPS"], "VIEWER"),
        (
            "WEBRTC between both others",
            vec!["HTTPS", "WEBRTC", "WSS"],
            "MASTER",
        ),
    ];

    for (case, protocols, role) in rows {
        let answer = call(
            &router,
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": {
                    "Protocols": protocols, "Role": role
                }
            }),
        )
        .await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (400, "InvalidArgumentException"),
            "case {case:?}: expected HTTP 400 InvalidArgumentException, got {} {:?}",
            answer.status,
            answer.body
        );
        let actual = answer.message();
        assert_eq!(
            actual, WEBRTC_WITHOUT_MEDIA_STORAGE,
            "case {case:?}: expected Message {WEBRTC_WITHOUT_MEDIA_STORAGE:?}, got {actual:?}"
        );
    }

    // The protocols that need no media-storage configuration are served, by protocol
    // value: the same set comes back in the same order whichever order it was asked in.
    for (case, protocols, expected) in [
        (
            "both signaling protocols, HTTPS named first",
            vec!["HTTPS", "WSS"],
            vec!["HTTPS", "WSS"],
        ),
        (
            "both signaling protocols, the websocket named first",
            vec!["WSS", "HTTPS"],
            vec!["HTTPS", "WSS"],
        ),
        ("one protocol only", vec!["HTTPS"], vec!["HTTPS"]),
    ] {
        let answer = call(
            &router,
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": {
                    "Protocols": protocols.clone(), "Role": "MASTER"
                }
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} {:?}",
            answer.status, answer.body
        );
        let reported: Vec<String> = answer.body["ResourceEndpointList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| item["Protocol"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the endpoints {expected:?}, got {:?}",
            answer.body
        );
    }
}
