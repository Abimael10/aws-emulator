// SPDX-License-Identifier: CC0-1.0

//! A WebRTC storage session belongs to the signaling channel the CONTROL plane holds.
//!
//! The rule these cases assert, one level above the recorded case (one channel, one viewer
//! join refused as if the channel did not exist):
//!
//! > A signaling channel exists exactly while `kinesisvideo:CreateSignalingChannel` holds
//! > it, so EVERY plane that takes a `channelArn` resolves it against that one registry:
//! > `JoinStorageSession` and `JoinStorageSessionAsViewer` serve a channel the control
//! > plane holds and answer `ResourceNotFoundException` for an ARN that names none, each
//! > after authorizing the caller's `kinesisvideo:<Operation>` on that ARN. Either
//! > participant may open the session ("Both a video producing device and viewers can join
//! > a session first and wait for other participants"), the session admits the viewers of
//! > its quota and refuses one past it with `ClientLimitExceededException`, and the channel
//! > cannot be deleted while its `MediaStorageConfiguration` is `ENABLED`.
//!
//! Five crates have to agree for that to hold — the Kinesis Video control plane mints the
//! channel and the stream it ingests into, the signaling crate keeps the registry both
//! planes read, the WebRTC storage crate serves the joins, and IAM and STS hold the
//! identity a join is authorized against — so the suite lives here.
//!
//! The rows differ along what each rule generalises over: the Region, the channel name's
//! spelling and the stream's retention; which participant joins first; the policy a caller
//! holds; and which member of the request is malformed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use service_com_amazonaws_kinesisvideowebrtcstorage_awsacuityroutingservicelambda as webrtc_storage;
use smithy_server_runtime::MultiServiceRouter;

/// The emulator's own published long-term access key: the account credential, which holds
/// no IAM policies and is the position AWS account root credentials are in.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// How long a case keeps looking for a resource to reach a status before failing. Generous
/// for a loaded machine, and bounded so a case that will never settle fails instead of
/// hanging the suite.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits between looks; the CONDITION is what it waits for.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The status both resources have to reach before their media-storage configuration can be
/// updated, from `smithy_query(query="shape",shape="Status")` via the control plane's own
/// constant.
const ACTIVE: &str = "ACTIVE";

/// One answer from the router: the HTTP status and the parsed JSON body, with the error
/// shape the envelope names folded into the body so one accessor reads it either way.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code of a failure, with any Smithy namespace stripped.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The text of a failure: the `Message` member this model's error shapes declare, or
    /// the `message` the front door's own refusal carries.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn ok(&self) -> bool {
        self.status == 200 && self.error_code().is_empty()
    }
}

/// The emulator: the five services this suite exercises on ONE engine, as the router mounts
/// them, with request authentication off so each call carries a credential scope — which is
/// what names the Region, the service and the caller — without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(webrtc_storage::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// POST one restJson1 call to `authority`, signed for `kinesisvideo` in `region` with
/// `access_key_id`.
async fn signed_call(
    router: &MultiServiceRouter,
    region: &str,
    authority: &str,
    path: &str,
    body: &Value,
    access_key_id: &str,
) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{region}/kinesisvideo/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let mut body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    if let (Some(error_type), Some(object)) = (error_type, body.as_object_mut()) {
        object
            .entry("__type".to_string())
            .or_insert(Value::String(error_type));
    }
    Answer { status, body }
}

/// The control-plane authority of `region`: the endpoint a client of this emulator addresses
/// the Kinesis Video Streams API at.
fn control_authority(region: &str) -> String {
    format!("kinesisvideo.{region}.amazonaws.com")
}

/// POST one call to the control plane of `region` as the account's own credential.
async fn call(router: &MultiServiceRouter, region: &str, path: &str, body: &Value) -> Answer {
    signed_call(
        router,
        region,
        &control_authority(region),
        path,
        body,
        ACCOUNT_KEY,
    )
    .await
}

/// One awsQuery call to IAM or STS: `signing_name` picks the service, `version` its API
/// version. Answers the response body, having asserted the 200.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encoded(value));
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={ACCOUNT_KEY}/20260101/us-east-1/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: setup call {action} must answer 200, got {status} {answered}"
    );
    answered
}

/// One form field, percent-encoded the way an awsQuery body carries it.
fn form_encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The text of the first `<name>` element of an awsQuery XML response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// An identity-based policy with one statement.
fn policy(effect: &str, actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// Create an IAM role carrying `document` as its inline policy (when one is given), assume
/// it through STS, and answer the access key id of the session — the credential a caller
/// then signs its joins with.
async fn assumed_role_key(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    document: Option<&str>,
) -> String {
    let account = engine::EMULATOR_ACCOUNT_ID;
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    if let Some(document) = document {
        query(
            router,
            case,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", document.to_string()),
            ],
        )
        .await;
    }
    let assumed = query(
        router,
        case,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            (
                "RoleArn",
                format!("arn:aws:iam::{account}:role/{role_name}"),
            ),
            ("RoleSessionName", "probe".to_string()),
        ],
    )
    .await;
    xml_element(&assumed, "AccessKeyId")
        .unwrap_or_else(|| panic!("case {case:?}: AssumeRole must answer an AccessKeyId"))
}

/// Create a stream with `retention` hours of data retention and answer its `StreamARN`.
async fn create_stream(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    retention: i32,
) -> String {
    let created = call(
        router,
        region,
        "/createStream",
        &json!({ "StreamName": name, "DataRetentionInHours": retention }),
    )
    .await;
    assert!(
        created.ok(),
        "case {case:?}: CreateStream must answer 200, got {created:?}"
    );
    created.body["StreamARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no StreamARN in {created:?}"))
        .to_string()
}

/// Create a signaling channel and answer its `ChannelARN`.
async fn create_channel(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
) -> String {
    let created = call(
        router,
        region,
        "/createSignalingChannel",
        &json!({ "ChannelName": name, "ChannelType": "SINGLE_MASTER" }),
    )
    .await;
    assert!(
        created.ok(),
        "case {case:?}: CreateSignalingChannel must answer 200, got {created:?}"
    );
    created.body["ChannelARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ChannelARN in {created:?}"))
        .to_string()
}

/// Poll a reader until the resource it reads reports [`ACTIVE`], as a client waiting on an
/// asynchronous create does: the CONDITION is what is waited for, under a deadline far
/// longer than the transition needs.
async fn await_active(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    path: &str,
    body: &Value,
    status_at: &[&str],
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let answer = call(router, region, path, body).await;
        let mut reported = &answer.body;
        for member in status_at {
            reported = &reported[*member];
        }
        last = reported.clone();
        if last == ACTIVE {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {path} must report {ACTIVE} before the deadline, got {last:?}");
}

/// Wait for the channel `arn` to report [`ACTIVE`].
async fn await_channel_active(router: &MultiServiceRouter, case: &str, region: &str, arn: &str) {
    await_active(
        router,
        case,
        region,
        "/describeSignalingChannel",
        &json!({ "ChannelARN": arn }),
        &["ChannelInfo", "ChannelStatus"],
    )
    .await;
}

/// Wait for the stream `name` to report [`ACTIVE`].
async fn await_stream_active(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    await_active(
        router,
        case,
        region,
        "/describeStream",
        &json!({ "StreamName": name }),
        &["StreamInfo", "Status"],
    )
    .await;
}

/// Set the channel's media-storage configuration to `status`, naming `stream_arn` when the
/// call enables it.
async fn update_media_storage(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    stream_arn: Option<&str>,
    status: &str,
) -> Answer {
    let mut configuration = json!({ "Status": status });
    if let (Some(object), Some(stream_arn)) = (configuration.as_object_mut(), stream_arn) {
        object.insert("StreamARN".to_string(), json!(stream_arn));
    }
    call(
        router,
        region,
        "/updateMediaStorageConfiguration",
        &json!({ "ChannelARN": channel_arn, "MediaStorageConfiguration": configuration }),
    )
    .await
}

/// The authority `GetSignalingChannelEndpoint` hands back for the `WEBRTC` protocol — the
/// endpoint the two join operations are addressed at ("Before using this API, you must call
/// the GetSignalingChannelEndpoint API to request the WEBRTC endpoint").
async fn webrtc_authority(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    channel_arn: &str,
    role: &str,
) -> String {
    let resolved = call(
        router,
        region,
        "/getSignalingChannelEndpoint",
        &json!({
            "ChannelARN": channel_arn,
            "SingleMasterChannelEndpointConfiguration": {
                "Protocols": ["WEBRTC"],
                "Role": role,
            },
        }),
    )
    .await;
    assert!(
        resolved.ok(),
        "case {case:?}: GetSignalingChannelEndpoint({role}) must answer 200, got {resolved:?}"
    );
    let endpoint = resolved.body["ResourceEndpointList"][0]["ResourceEndpoint"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ResourceEndpoint in {resolved:?}"))
        .to_string();
    endpoint
        .split_once("://")
        .map(|(_scheme, authority)| authority.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected an origin, got {endpoint:?}"))
}

/// A channel linked to a stream of its own, ready for ingestion: the whole setup every
/// case below starts from. Answers the channel's and the stream's ARNs.
async fn ingestion_ready(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    retention: i32,
) -> (String, String) {
    let stream_arn = create_stream(router, case, region, &format!("{name}-stream"), retention).await;
    let channel_arn = create_channel(router, case, region, name).await;
    await_stream_active(router, case, region, &format!("{name}-stream")).await;
    await_channel_active(router, case, region, &channel_arn).await;
    let linked = update_media_storage(
        router,
        region,
        &channel_arn,
        Some(&stream_arn),
        "ENABLED",
    )
    .await;
    assert!(
        linked.ok(),
        "case {case:?}: UpdateMediaStorageConfiguration must answer 200, got {linked:?}"
    );
    (channel_arn, stream_arn)
}

/// Join the channel's storage session as a playback VIEWER under `client_id`, addressed to
/// the `WEBRTC` endpoint the control plane handed back.
async fn join_as_viewer(
    router: &MultiServiceRouter,
    region: &str,
    authority: &str,
    channel_arn: &str,
    client_id: &str,
) -> Answer {
    signed_call(
        router,
        region,
        authority,
        "/joinStorageSessionAsViewer",
        &json!({ "channelArn": channel_arn, "clientId": client_id }),
        ACCOUNT_KEY,
    )
    .await
}

/// Join the channel's storage session as the video producing device.
async fn join_as_producer(
    router: &MultiServiceRouter,
    region: &str,
    authority: &str,
    channel_arn: &str,
) -> Answer {
    signed_call(
        router,
        region,
        authority,
        "/joinStorageSession",
        &json!({ "channelArn": channel_arn }),
        ACCOUNT_KEY,
    )
    .await
}

/// One row: a channel to make ingestion-ready, and which participant opens its session.
struct Row {
    case: &'static str,
    region: &'static str,
    channel_name: &'static str,
    /// The stream's `DataRetentionInHours`; any positive value admits ingestion.
    retention: i32,
    /// Whether the VIEWER joins before any video producing device has.
    viewer_first: bool,
}

/// The rows differ along the Region, the channel name's spelling, the stream's retention and
/// which participant opens the session.
const ROWS: &[Row] = &[
    Row {
        case: "the recorded case: a viewer joins a freshly linked channel",
        region: "us-west-2",
        channel_name: "sim2real-kvsrtc-ch",
        retention: 1,
        viewer_first: true,
    },
    Row {
        case: "another Region, a dotted name and a longer retention",
        region: "eu-central-1",
        channel_name: "edge.cam_01-A",
        retention: 24,
        viewer_first: true,
    },
    Row {
        case: "the video producing device joins first",
        region: "ap-northeast-1",
        channel_name: "m",
        retention: 2,
        viewer_first: false,
    },
];

/// Either participant opens the storage session of a channel the control plane holds, the
/// session admits the viewers of its quota, and one past it is refused.
#[tokio::test]
async fn either_participant_joins_the_storage_session_of_a_held_channel() {
    let router = emulator();

    for row in ROWS {
        let case = row.case;
        let (channel_arn, _stream_arn) =
            ingestion_ready(&router, case, row.region, row.channel_name, row.retention).await;
        let viewer_endpoint =
            webrtc_authority(&router, case, row.region, &channel_arn, "VIEWER").await;
        let master_endpoint =
            webrtc_authority(&router, case, row.region, &channel_arn, "MASTER").await;

        let join_endpoints = (&viewer_endpoint, &master_endpoint);

        if !row.viewer_first {
            let producer =
                join_as_producer(&router, row.region, join_endpoints.1, &channel_arn).await;
            assert!(
                producer.ok(),
                "case {case:?}: JoinStorageSession must answer 200, got {producer:?}"
            );
        }

        // Every viewer slot the session carries is admitted, and the body is empty.
        let slots: &[&str] = &["viewer-1", "viewer-2", "viewer-3"];
        assert_eq!(
            slots.len(),
            webrtc_storage::state::MAX_VIEWERS_PER_SESSION,
            "case {case:?}: the rows drive one join per viewer slot"
        );
        for client_id in slots {
            let joined =
                join_as_viewer(&router, row.region, join_endpoints.0, &channel_arn, client_id)
                    .await;
            assert!(
                joined.ok(),
                "case {case:?} viewer {client_id:?}: JoinStorageSessionAsViewer must answer \
                 200, got {joined:?}"
            );
            assert_eq!(
                joined.body,
                Value::Null,
                "case {case:?} viewer {client_id:?}: the modelled output is Unit, so the body \
                 is empty, got {:?}",
                joined.body
            );
        }

        // One more DISTINCT viewer is past the session's quota.
        let refused = join_as_viewer(
            &router,
            row.region,
            join_endpoints.0,
            &channel_arn,
            "viewer-one-too-many",
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "ClientLimitExceededException"),
            "case {case:?}: a viewer past the quota must be refused, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            webrtc_storage::state::VIEWER_LIMIT_MESSAGE,
            "case {case:?}: expected the quota refusal's own sentence, got {:?}",
            refused.message()
        );

        // A viewer already on the session consumes no new slot, so its rejoin is served
        // even at the quota: "If there's an existing session with the same clientId that's
        // found in the join session request, the new request takes precedence."
        let rejoined =
            join_as_viewer(&router, row.region, join_endpoints.0, &channel_arn, "viewer-1").await;
        assert!(
            rejoined.ok(),
            "case {case:?}: a rejoin of a viewer already on the session must be served, got \
             {rejoined:?}"
        );

        // And the producing device joins the same session, whichever participant opened it.
        let producer = join_as_producer(&router, row.region, join_endpoints.1, &channel_arn).await;
        assert!(
            producer.ok(),
            "case {case:?}: JoinStorageSession must answer 200, got {producer:?}"
        );
    }
}

/// A `channelArn` that names no signaling channel is `ResourceNotFoundException` on both
/// joins — a channel never created, and one the control plane has deleted.
#[tokio::test]
async fn a_join_of_a_channel_no_control_plane_record_names_is_not_found() {
    let region = "us-west-2";
    let router = emulator();
    let case = "a channel the control plane holds, then deletes";
    let (channel_arn, _stream_arn) = ingestion_ready(&router, case, region, "to-be-deleted", 1).await;
    let endpoint = webrtc_authority(&router, case, region, &channel_arn, "VIEWER").await;

    // While the control plane holds it, both joins are served.
    let viewer = join_as_viewer(&router, region, &endpoint, &channel_arn, "viewer-1").await;
    let producer = join_as_producer(&router, region, &endpoint, &channel_arn).await;
    assert!(
        viewer.ok() && producer.ok(),
        "case {case:?}: both joins of a held channel must be served, got {viewer:?} and \
         {producer:?}"
    );

    // An ARN of the same shape that no channel carries, and the held channel once it is
    // deleted: both name no channel, and both joins say so.
    let never_created =
        "arn:aws:kinesisvideo:us-west-2:000000000000:channel/never-created/1700000000000"
            .to_string();
    let disabled = update_media_storage(&router, region, &channel_arn, None, "DISABLED").await;
    assert!(
        disabled.ok(),
        "case {case:?}: disabling the configuration must answer 200, got {disabled:?}"
    );
    let deleted = call(
        &router,
        region,
        "/deleteSignalingChannel",
        &json!({ "ChannelARN": channel_arn }),
    )
    .await;
    assert!(
        deleted.ok(),
        "case {case:?}: the delete of an unlinked channel must answer 200, got {deleted:?}"
    );

    for (case, arn) in [
        ("a channel nothing ever created", never_created),
        ("a channel the control plane deleted", channel_arn.clone()),
    ] {
        let viewer = join_as_viewer(&router, region, &endpoint, &arn, "viewer-1").await;
        let producer = join_as_producer(&router, region, &endpoint, &arn).await;
        for (operation, answer) in [
            ("JoinStorageSessionAsViewer", &viewer),
            ("JoinStorageSession", &producer),
        ] {
            assert_eq!(
                (answer.status, answer.error_code().as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} on {operation}: expected 404 ResourceNotFoundException, got \
                 {answer:?}"
            );
        }
    }
}

/// One row of the authorization table: the policy a caller holds, and the status each join
/// owes it.
struct Grant {
    case: &'static str,
    role: &'static str,
    /// The inline policy the role carries; `None` for a principal the account grants
    /// nothing, which is IAM's default deny.
    document: Option<Which>,
    /// The status `JoinStorageSession` owes this caller.
    producer_status: u16,
    /// The status `JoinStorageSessionAsViewer` owes it.
    viewer_status: u16,
    /// Whether a document of this principal's EXPLICITLY denies the call, which is the rule
    /// a refusal names — as against IAM's default deny, where no document refused it and
    /// none allowed it.
    explicitly_denied: bool,
}

/// Which policy document a [`Grant`] carries, built per run because the resources it names
/// are minted per run.
enum Which {
    /// Every `kinesisvideo` action on every resource.
    WildcardAllow,
    /// Every action on the channel under test, with `JoinStorageSession` explicitly denied.
    DenyTheProducerJoin,
    /// Every action, scoped to a channel that is not the one under test.
    AllowAnotherChannel,
}

/// A join is served only to a caller IAM permits, and a refusal names the principal, the
/// action, the resource and which of IAM's rules refused it.
#[tokio::test]
async fn a_join_is_served_only_to_a_caller_iam_permits() {
    let region = "us-west-2";
    let router = emulator();
    let case = "the channel every grant addresses";
    let (channel_arn, _stream_arn) = ingestion_ready(&router, case, region, "iam-probe", 1).await;
    let (other_arn, _other_stream) = ingestion_ready(&router, case, region, "elsewhere", 1).await;
    let endpoint = webrtc_authority(&router, case, region, &channel_arn, "MASTER").await;

    let grants: &[Grant] = &[
        Grant {
            case: "a wildcard allow of every action on every resource",
            role: "wildcard",
            document: Some(Which::WildcardAllow),
            producer_status: 200,
            viewer_status: 200,
            explicitly_denied: false,
        },
        Grant {
            case: "an explicit deny of the producer join beside an allow of every action",
            role: "deny-the-producer-join",
            document: Some(Which::DenyTheProducerJoin),
            producer_status: 403,
            viewer_status: 200,
            explicitly_denied: true,
        },
        Grant {
            case: "an allow scoped to another channel",
            role: "other-channel",
            document: Some(Which::AllowAnotherChannel),
            producer_status: 403,
            viewer_status: 403,
            explicitly_denied: false,
        },
        Grant {
            case: "no policy at all, which is IAM's default deny",
            role: "no-policy",
            document: None,
            producer_status: 403,
            viewer_status: 403,
            explicitly_denied: false,
        },
    ];

    for grant in grants {
        let case = grant.case;
        let document = grant.document.as_ref().map(|which| match which {
            Which::WildcardAllow => policy("Allow", &["kinesisvideo:*"], &["*"]),
            Which::DenyTheProducerJoin => json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "kinesisvideo:*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": "kinesisvideo:JoinStorageSession",
                        "Resource": channel_arn.clone(),
                    },
                ],
            })
            .to_string(),
            Which::AllowAnotherChannel => {
                policy("Allow", &["kinesisvideo:*"], &[other_arn.as_str()])
            }
        });
        let key = assumed_role_key(&router, case, grant.role, document.as_deref()).await;

        for (operation, path, body, expected) in [
            (
                "JoinStorageSession",
                "/joinStorageSession",
                json!({ "channelArn": channel_arn }),
                grant.producer_status,
            ),
            (
                "JoinStorageSessionAsViewer",
                "/joinStorageSessionAsViewer",
                json!({ "channelArn": channel_arn, "clientId": "viewer-1" }),
                grant.viewer_status,
            ),
        ] {
            let answer = signed_call(&router, region, &endpoint, path, &body, &key).await;
            assert_eq!(
                answer.status, expected,
                "case {case:?} on {operation}: expected HTTP {expected}, got {answer:?}"
            );
            if expected != 403 {
                continue;
            }
            assert_eq!(
                answer.error_code(),
                "AccessDeniedException",
                "case {case:?} on {operation}: expected AccessDeniedException, got {answer:?}"
            );
            // The sentence AWS words this refusal in: the principal that signed the call,
            // the action it performs, the resource it names, and which of IAM's rules
            // refused it — an explicit deny is NAMED, and IAM's default deny states the
            // action no policy allows.
            let message = answer.message();
            let tail = match grant.explicitly_denied {
                true => "with an explicit deny in an identity-based policy".to_string(),
                false => format!(
                    "because no identity-based policy allows the kinesisvideo:{operation} action"
                ),
            };
            assert!(
                message.starts_with("User: ")
                    && message.contains(&format!(
                        " is not authorized to perform: kinesisvideo:{operation} on resource: \
                         {channel_arn}"
                    ))
                    && message.ends_with(&tail),
                "case {case:?} on {operation}: expected the refusal to name the principal, the \
                 action and the resource and to end with {tail:?}, got {message:?}"
            );
        }
    }
}

/// A body this service cannot accept is refused by its front door with the one code it
/// emits for that class, and a sentence that states nothing of the request.
#[tokio::test]
async fn a_body_outside_the_modelled_constraints_is_a_bad_request() {
    let region = "us-west-2";
    let router = emulator();
    let case = "the channel the accepted rows address";
    let (channel_arn, _stream_arn) = ingestion_ready(&router, case, region, "bad-body", 1).await;
    let endpoint = webrtc_authority(&router, case, region, &channel_arn, "MASTER").await;

    // (case, path, body) — one row per member and constraint a request can break: the
    // modelled `@pattern` on `channelArn`, the `@pattern` and both `@length` bounds on
    // `clientId`, and a required member left out.
    let too_long = "v".repeat(257);
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "channelArn is not an ARN at all",
            "/joinStorageSession",
            json!({ "channelArn": "not-a-valid-kinesisvideo-channel-arn" }),
        ),
        (
            "channelArn carries no creation time",
            "/joinStorageSession",
            json!({ "channelArn": "arn:aws:kinesisvideo:us-west-2:000000000000:channel/c" }),
        ),
        (
            "channelArn is absent",
            "/joinStorageSession",
            json!({}),
        ),
        (
            "channelArn is not an ARN at all, on the viewer route",
            "/joinStorageSessionAsViewer",
            json!({ "channelArn": "not-a-valid-kinesisvideo-channel-arn", "clientId": "v" }),
        ),
        (
            "clientId carries a character the pattern rejects",
            "/joinStorageSessionAsViewer",
            json!({ "channelArn": channel_arn.clone(), "clientId": "bad id!" }),
        ),
        (
            "clientId is empty",
            "/joinStorageSessionAsViewer",
            json!({ "channelArn": channel_arn.clone(), "clientId": "" }),
        ),
        (
            "clientId is one character past its maximum",
            "/joinStorageSessionAsViewer",
            json!({ "channelArn": channel_arn.clone(), "clientId": too_long.clone() }),
        ),
        (
            "clientId is absent",
            "/joinStorageSessionAsViewer",
            json!({ "channelArn": channel_arn.clone() }),
        ),
    ];

    for (case, path, body) in &rows {
        let answer = signed_call(&router, region, &endpoint, path, body, ACCOUNT_KEY).await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (400, webrtc_storage::service::BAD_REQUEST_EXCEPTION),
            "case {case:?}: expected HTTP 400 {}, got {answer:?}",
            webrtc_storage::service::BAD_REQUEST_EXCEPTION
        );
        let message = answer.message();
        assert_eq!(
            message,
            webrtc_storage::service::INVALID_REQUEST_BODY,
            "case {case:?}: expected the front door's own sentence, got {message:?}"
        );
    }

    // The accepted side of the same members: a value ON each published bound is served.
    for client_id in ["v".to_string(), "v".repeat(256)] {
        let answer = signed_call(
            &router,
            region,
            &endpoint,
            "/joinStorageSessionAsViewer",
            &json!({ "channelArn": channel_arn.clone(), "clientId": client_id.clone() }),
            ACCOUNT_KEY,
        )
        .await;
        assert!(
            answer.ok(),
            "case clientId of {} characters: a value on the published bound must be served, \
             got {answer:?}",
            client_id.len()
        );
    }
}

/// A channel still ingesting into a stream cannot be deleted, and the interlock lifts as
/// soon as its configuration is disabled.
#[tokio::test]
async fn a_channel_linked_to_a_stream_cannot_be_deleted() {
    let region = "us-west-2";
    let router = emulator();
    let case = "a channel linked to its stream";
    let (channel_arn, stream_arn) = ingestion_ready(&router, case, region, "linked", 1).await;

    let refused = call(
        &router,
        region,
        "/deleteSignalingChannel",
        &json!({ "ChannelARN": channel_arn }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "ResourceInUseException"),
        "case {case:?}: the delete of a linked channel must be refused, got {refused:?}"
    );
    assert_eq!(
        refused.message(),
        kvs::state::MEDIA_STORAGE_ENABLED_DELETE_MESSAGE,
        "case {case:?}: expected this refusal's own sentence, got {:?}",
        refused.message()
    );

    // The refusal left the channel where it was: still ACTIVE, and still linked.
    let described = call(
        &router,
        region,
        "/describeSignalingChannel",
        &json!({ "ChannelARN": channel_arn }),
    )
    .await;
    assert_eq!(
        described.body["ChannelInfo"]["ChannelStatus"].as_str(),
        Some(ACTIVE),
        "case {case:?}: a refused delete must leave the channel ACTIVE, got {described:?}"
    );
    let configuration = call(
        &router,
        region,
        "/describeMediaStorageConfiguration",
        &json!({ "ChannelARN": channel_arn }),
    )
    .await;
    assert_eq!(
        configuration.body["MediaStorageConfiguration"],
        json!({ "Status": "ENABLED", "StreamARN": stream_arn }),
        "case {case:?}: a refused delete must leave the mapping, got {configuration:?}"
    );

    // Disabling the configuration releases the stream, and the identical delete succeeds.
    let disabled = update_media_storage(&router, region, &channel_arn, None, "DISABLED").await;
    assert!(
        disabled.ok(),
        "case {case:?}: disabling the configuration must answer 200, got {disabled:?}"
    );
    let deleted = call(
        &router,
        region,
        "/deleteSignalingChannel",
        &json!({ "ChannelARN": channel_arn }),
    )
    .await;
    assert!(
        deleted.ok(),
        "case {case:?}: the delete of an unlinked channel must answer 200, got {deleted:?}"
    );
}

/// Each media-storage refusal carries its own error class's sentence, with nothing of the
/// request or of the store in it.
#[tokio::test]
async fn a_media_storage_refusal_carries_its_error_classes_own_sentence() {
    let region = "us-west-2";
    let router = emulator();
    let case = "the channel holding the mapped stream";
    let (holder_arn, mapped_stream) = ingestion_ready(&router, case, region, "holder", 1).await;

    // A channel of its own, ACTIVE and unlinked, which each row below tries to link.
    let claimant = create_channel(&router, case, region, "claimant").await;
    await_channel_active(&router, case, region, &claimant).await;
    // A stream that persists nothing: "In order for WebRTC Ingestion to work, the stream
    // must have data retention enabled."
    let transient = create_stream(&router, case, region, "transient", 0).await;
    await_stream_active(&router, case, region, "transient").await;

    // (case, the channel to link, the stream to link it to, the code, the sentence)
    let rows: Vec<(&str, &str, &str, &str, &str)> = vec![
        (
            "a stream already mapped to another channel",
            claimant.as_str(),
            mapped_stream.as_str(),
            "ResourceInUseException",
            kvs::state::MEDIA_STORAGE_IN_USE_MESSAGE,
        ),
        (
            "a stream that persists nothing",
            claimant.as_str(),
            transient.as_str(),
            "NoDataRetentionException",
            kvs::state::ZERO_DATA_RETENTION_MESSAGE,
        ),
    ];

    for (case, channel_arn, stream_arn, code, sentence) in &rows {
        let answer =
            update_media_storage(&router, region, channel_arn, Some(stream_arn), "ENABLED").await;
        assert_eq!(
            (answer.status, answer.error_code().as_str()),
            (400, *code),
            "case {case:?}: expected HTTP 400 {code}, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            *sentence,
            "case {case:?}: expected {sentence:?}, got {:?}",
            answer.message()
        );
        // Nothing was mapped: the refusal left the claimant unlinked.
        let configuration = call(
            &router,
            region,
            "/describeMediaStorageConfiguration",
            &json!({ "ChannelARN": channel_arn }),
        )
        .await;
        assert_eq!(
            configuration.body["MediaStorageConfiguration"]["Status"].as_str(),
            Some("DISABLED"),
            "case {case:?}: a refused update must leave the configuration DISABLED, got \
             {configuration:?}"
        );
    }

    // The holder releases the stream, and the identical call that was refused is served.
    let released = update_media_storage(&router, region, &holder_arn, None, "DISABLED").await;
    assert!(
        released.ok(),
        "the holder must release the stream, got {released:?}"
    );
    let linked = update_media_storage(
        &router,
        region,
        &claimant,
        Some(&mapped_stream),
        "ENABLED",
    )
    .await;
    assert!(
        linked.ok(),
        "the released stream must be linkable by the claimant, got {linked:?}"
    );
}
