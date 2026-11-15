// SPDX-License-Identifier: CC0-1.0

//! What a Completed asynchronous invocation leaves in the caller's bucket, end to end over
//! HTTP.
//!
//! Two rules, each asserted over a SET of inputs rather than over the one request that
//! showed them:
//!
//! * **The generated media is there.** An asynchronous invocation's result is an artifact
//!   written to the caller's own Amazon S3 location, so an invocation that reports the
//!   modelled `Completed` status has left the generated media object of the requested
//!   modality under its own segment of the submitted prefix, beside the manifest that
//!   indexes it — and that object is a real video file of the geometry and the length the
//!   submitted `modelInput` configured, which the maintained MP4 reader decodes.
//! * **It stays there.** A terminal invocation is a point read: repeated `GetAsyncInvoke`
//!   calls keep reporting the same record with an UNCHANGED
//!   `outputDataConfig.s3OutputDataConfig.s3Uri`, and the artifact stays present and
//!   byte-identical, however many times a client comes back for it.
//!
//! The rows span the two providers' published request members — `dimension` and
//! `durationSeconds` for Amazon Nova Reel, `resolution`, `aspect_ratio` and `duration` for
//! Luma AI Ray v2, plus the defaults a request that configures none of them generates at —
//! and the SHAPE of the configured location, because the invocation's own segment is cut
//! into whichever prefix was submitted.
//!
//! Amazon S3 is mounted beside this service on one engine, the way `core/router` assembles
//! them, so every object below is read back through Amazon S3's own operations: what is
//! asserted is what the store that OWNS the object answers for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state;

use crate::bedrock_runtime_inference::{encode_path, get, Call};
use crate::inference_input_limits::{
    create_bucket, emulator_with_object_store, poll_to_terminal, s3_call, submit,
};

/// The bucket every invocation below writes into.
const BUCKET: &str = "bedrock-video-out";

/// One invocation: the model it names, the `modelInput` that model's own published schema
/// takes, the location it configures, and the video that `modelInput` then describes.
struct Row {
    case: &'static str,
    model_id: &'static str,
    model_input: Value,
    /// The configured `s3Uri`, which is the bucket and the prefix the output goes under.
    s3_uri: String,
    /// The prefix the written keys start with, empty for a location addressing the bucket
    /// root.
    prefix: &'static str,
    /// The frame geometry the submitted members describe, in pixels.
    geometry: (u16, u16),
    /// The length they describe, in seconds.
    duration_seconds: u64,
}

/// The rows: they differ along the invoked model, along WHICH published geometry and
/// length members the payload carries (and whether it carries them at all), and along the
/// shape of the configured location.
///
/// Members and values from the request tables in the Amazon Bedrock User Guide: "Amazon
/// Nova Reel" (`taskType`, `textToVideoParams.text`, `multiShotAutomatedParams.text`,
/// `videoGenerationConfig.dimension` / `durationSeconds` / `fps`) and "Luma AI Ray v2"
/// (`prompt`, `aspect_ratio`, `duration`, `resolution`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Nova Reel single shot, at the bucket root",
            model_id: "amazon.nova-reel-v1:0",
            model_input: json!({
                "taskType": "TEXT_VIDEO",
                "textToVideoParams": {"text": "A kite over a lake at dawn."},
                "videoGenerationConfig": {
                    "durationSeconds": 6,
                    "fps": 24,
                    "dimension": "1280x720",
                },
            }),
            s3_uri: format!("s3://{BUCKET}"),
            prefix: "",
            geometry: (1280, 720),
            duration_seconds: 6,
        },
        Row {
            case: "a Nova Reel multi-shot generation, under one prefix segment",
            model_id: "amazon.nova-reel-v1:1",
            model_input: json!({
                "taskType": "MULTI_SHOT_AUTOMATED",
                "multiShotAutomatedParams": {"text": "Three shots of a harbour at night."},
                "videoGenerationConfig": {"durationSeconds": 12, "fps": 24},
            }),
            s3_uri: format!("s3://{BUCKET}/video-out"),
            prefix: "video-out",
            geometry: (1280, 720),
            duration_seconds: 12,
        },
        Row {
            case: "a Luma landscape generation, under a prefix written with a separator",
            model_id: "luma.ray-v2:0",
            model_input: json!({
                "prompt": "A cyclist crossing a bridge.",
                "aspect_ratio": "16:9",
                "loop": false,
                "duration": "5s",
                "resolution": "540p",
            }),
            s3_uri: format!("s3://{BUCKET}/renders/"),
            prefix: "renders",
            geometry: (960, 540),
            duration_seconds: 5,
        },
        Row {
            case: "a Luma portrait generation at the larger resolution, under a deep prefix",
            model_id: "luma.ray-v2:0",
            model_input: json!({
                "prompt": "A lantern rising over a field.",
                "aspect_ratio": "9:16",
                "duration": "9s",
                "resolution": "720p",
            }),
            s3_uri: format!("s3://{BUCKET}/renders/2026/january"),
            prefix: "renders/2026/january",
            geometry: (720, 1280),
            duration_seconds: 9,
        },
        Row {
            case: "a Luma square generation",
            model_id: "luma.ray-v2:0",
            model_input: json!({
                "prompt": "A spinning top on a table.",
                "aspect_ratio": "1:1",
                "resolution": "540p",
            }),
            s3_uri: format!("s3://{BUCKET}/square"),
            prefix: "square",
            geometry: (540, 540),
            duration_seconds: 5,
        },
        Row {
            case: "a Luma generation that configures nothing but the prompt",
            model_id: "luma.ray-v2:0",
            model_input: json!({"prompt": "A paper boat in a puddle."}),
            s3_uri: format!("s3://{BUCKET}/defaults"),
            prefix: "defaults",
            geometry: (960, 540),
            duration_seconds: 5,
        },
    ]
}

/// RULE: a `Completed` asynchronous invocation has WRITTEN the generated media object of
/// the requested modality under its own segment of the configured prefix, beside the
/// manifest that indexes it — and that object is a real, decodable video of the geometry and
/// the length the submitted `modelInput` configured.
#[tokio::test]
async fn a_completed_video_invocation_leaves_its_generated_media_beside_the_manifest() {
    let router = emulator_with_object_store();
    create_bucket(&router, BUCKET).await;

    // Every invocation names its own submitting call, so the ids the manifests report are
    // collected and compared across the rows.
    let mut request_ids: Vec<String> = Vec::new();
    for row in rows() {
        let case = row.case;
        let arn = submit(&router, case, row.model_id, &row.model_input, &row.s3_uri).await;
        let invocation_id = invocation_id_of(&arn);
        let record = poll_to_terminal(&router, case, &arn).await;
        assert_eq!(
            record["status"].as_str(),
            Some("Completed"),
            "case {case:?}: expected Completed, got {record}"
        );

        // What the prefix the client configured actually holds, listed through Amazon S3's
        // own ListObjectsV2 — the call a client collecting the output makes.
        let segment = if row.prefix.is_empty() {
            format!("{invocation_id}/")
        } else {
            format!("{}/{invocation_id}/", row.prefix)
        };
        let keys = listed_keys(&router, &segment).await;
        let media = format!("{segment}{}", state::async_invoke_video::MEDIA_OBJECT);
        let manifest = format!("{segment}{}", state::async_invoke_output::MANIFEST_OBJECT);
        assert!(
            keys.contains(&media),
            "case {case:?}: expected the generated media at {media:?} under the configured \
             prefix, got {keys:?}"
        );
        assert!(
            keys.contains(&manifest),
            "case {case:?}: expected the manifest at {manifest:?} beside it, got {keys:?}"
        );

        // The media object is there to READ, and what is read is a video of the geometry
        // and the length the request configured.
        let object = s3_call(&router, "GET", &format!("/{BUCKET}/{media}"), &[]).await;
        assert_eq!(
            object.status,
            200,
            "case {case:?}: expected {media:?} to be readable, got {} / {}",
            object.status,
            String::from_utf8_lossy(&object.body)
        );
        assert!(
            !object.body.is_empty(),
            "case {case:?}: expected non-empty media at {media:?}"
        );
        assert_eq!(
            content_type_of(&object).as_deref(),
            Some(state::async_invoke_video::MEDIA_MIME),
            "case {case:?}: expected the video media type, got {:?}",
            content_type_of(&object)
        );
        let (geometry, duration) = video_of(case, &object.body);
        assert_eq!(
            geometry, row.geometry,
            "case {case:?}: expected a {}x{} frame, got {}x{}",
            row.geometry.0, row.geometry.1, geometry.0, geometry.1
        );
        assert_eq!(
            duration, row.duration_seconds,
            "case {case:?}: expected {}s of video, got {duration}s",
            row.duration_seconds
        );

        // The manifest beside it indexes that output: which invocation it is, which call
        // submitted it, which model was invoked and where its segment is.
        let indexed = s3_call(&router, "GET", &format!("/{BUCKET}/{manifest}"), &[]).await;
        let document: Value = serde_json::from_slice(&indexed.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the manifest must be JSON ({error}): {}",
                String::from_utf8_lossy(&indexed.body)
            )
        });
        assert_eq!(
            document["jobId"].as_str(),
            Some(invocation_id.as_str()),
            "case {case:?}: expected the manifest to name this invocation, got {document}"
        );
        assert_eq!(
            document["modelId"].as_str(),
            Some(row.model_id),
            "case {case:?}: expected the manifest to name the invoked model, got {document}"
        );
        let request_id = document["requestId"].as_str().unwrap_or_default().to_string();
        assert!(
            service_common::query_error_envelope::is_request_id(&request_id),
            "case {case:?}: expected the manifest to name its submitting call by a request \
             id, got {request_id:?}"
        );
        assert!(
            !request_ids.contains(&request_id),
            "case {case:?}: expected an id of its own, got {request_id:?} again in \
             {request_ids:?}"
        );
        request_ids.push(request_id);
    }
}

/// RULE: a terminal invocation is a cheap point read that keeps answering. Coming back for
/// the record reports the identical record with an unchanged configured location, and
/// coming back for the artifact finds the identical bytes — reading it changes nothing.
#[tokio::test]
async fn a_completed_invocation_and_its_artifact_stay_readable_and_unchanged() {
    let router = emulator_with_object_store();
    create_bucket(&router, BUCKET).await;

    // Two rows, so the rule is not asserted on one provider's geometry alone.
    for row in rows().into_iter().take(2) {
        let case = row.case;
        let arn = submit(&router, case, row.model_id, &row.model_input, &row.s3_uri).await;
        let invocation_id = invocation_id_of(&arn);
        let settled = poll_to_terminal(&router, case, &arn).await;
        let segment = if row.prefix.is_empty() {
            format!("{invocation_id}/")
        } else {
            format!("{}/{invocation_id}/", row.prefix)
        };
        let media = format!("{segment}{}", state::async_invoke_video::MEDIA_OBJECT);
        let first = s3_call(&router, "GET", &format!("/{BUCKET}/{media}"), &[]).await;

        // Every look again: the record, the location it reports, the listing of the prefix
        // and the bytes under it. Nothing here waits on the clock — a terminal invocation
        // owes no further transition — so the loop simply asks again.
        for look in 1..=5 {
            let again = get(&router, &format!("/async-invoke/{}", encode_path(&arn))).await;
            assert_eq!(
                again.status,
                200,
                "case {case:?} look {look}: expected 200 from GetAsyncInvoke, got {} / {}",
                again.status,
                String::from_utf8_lossy(&again.body)
            );
            let record = again.json();
            assert_eq!(
                record, settled,
                "case {case:?} look {look}: expected the identical record, got {record}"
            );
            assert_eq!(
                record["outputDataConfig"]["s3OutputDataConfig"]["s3Uri"].as_str(),
                Some(row.s3_uri.as_str()),
                "case {case:?} look {look}: expected the configured location, got {record}"
            );
            let keys = listed_keys(&router, &segment).await;
            assert!(
                keys.contains(&media),
                "case {case:?} look {look}: expected {media:?} to still be listed, got {keys:?}"
            );
            let reread = s3_call(&router, "GET", &format!("/{BUCKET}/{media}"), &[]).await;
            assert_eq!(
                (reread.status, reread.body.len()),
                (first.status, first.body.len()),
                "case {case:?} look {look}: expected the identical object, got {} / {} bytes",
                reread.status,
                reread.body.len()
            );
            assert_eq!(
                reread.body, first.body,
                "case {case:?} look {look}: expected the identical bytes of {media:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The invocation id inside an `invocationArn`: its last path segment, which is the
/// segment the invocation's output lives under.
fn invocation_id_of(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or_default().to_string()
}

/// The keys Amazon S3 reports under `prefix`, read through its own `ListObjectsV2`.
async fn listed_keys(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let call = s3_call(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    assert_eq!(
        call.status,
        200,
        "listing {prefix:?} must answer 200, got {} / {}",
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    let body = String::from_utf8_lossy(&call.body).to_string();
    body.split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The `content-type` a response states.
fn content_type_of(call: &Call) -> Option<String> {
    call.headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The frame geometry and the whole-second duration of `bytes`, decoded with the
/// maintained MP4 reader: an object a reader cannot decode is not a video file.
fn video_of(case: &str, bytes: &[u8]) -> ((u16, u16), u64) {
    let size = bytes.len() as u64;
    let file = mp4::Mp4Reader::read_header(std::io::Cursor::new(bytes), size)
        .unwrap_or_else(|error| panic!("case {case:?}: the media must decode as MP4: {error}"));
    let track = file
        .tracks()
        .values()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the media carries no track"));
    assert!(
        track.sample_count() > 0,
        "case {case:?}: expected the video track to carry frames, got none"
    );
    ((track.width(), track.height()), file.duration().as_secs())
}
