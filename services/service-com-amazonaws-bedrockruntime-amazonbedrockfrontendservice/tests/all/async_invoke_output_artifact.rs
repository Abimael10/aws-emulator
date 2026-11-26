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
//!   calls keep reporting the same record with the same per-invocation
//!   `outputDataConfig.s3OutputDataConfig.s3Uri`, and the artifact stays present and
//!   byte-identical, however many times a client comes back for it.
//! * **The location it reports is the one the artifact is under.** The output location an
//!   invocation is reported at is DERIVED from the location its submission configured,
//!   never echoed: that prefix extended by the invocation id the service minted, as one
//!   further key segment, with every other member of the configured destination carried
//!   over as submitted. `GetAsyncInvoke` and `ListAsyncInvokes` report that one value, and
//!   a client that reads it and then lists under it finds this invocation's own output and
//!   nothing else.
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

use crate::bedrock_runtime_inference::{encode_path, get, is_invocation_arn, post_json, Call};
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
                Some(format!("{}/{invocation_id}", row.s3_uri.trim_end_matches('/')).as_str()),
                "case {case:?} look {look}: expected this invocation's own location, got \
                 {record}"
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
// Where the output location an invocation reports comes from
// ---------------------------------------------------------------------------

/// One submission of the location rule below: the location it configures, and the rest of
/// the destination it carries.
struct LocationRow {
    case: &'static str,
    s3_uri: String,
    /// `bucketOwner` as submitted, when the row carries it: a member the derivation does
    /// not touch.
    bucket_owner: Option<&'static str>,
    /// `kmsKeyId` as submitted, when the row carries it.
    kms_key_id: Option<&'static str>,
}

/// The location rows: every SHAPE of a configured location — the bucket root, one prefix
/// segment, a deep prefix, each written with and without a trailing key separator — plus
/// the destination members beside the URI, because the derivation is about the prefix and
/// leaves the rest alone.
fn location_rows() -> Vec<LocationRow> {
    let plain = |case, s3_uri| LocationRow {
        case,
        s3_uri,
        bucket_owner: None,
        kms_key_id: None,
    };
    vec![
        plain("the bucket root", format!("s3://{BUCKET}")),
        plain(
            "the bucket root written with a trailing key separator",
            format!("s3://{BUCKET}/"),
        ),
        plain("one prefix segment", format!("s3://{BUCKET}/video")),
        plain(
            "one prefix segment written with a trailing key separator",
            format!("s3://{BUCKET}/video/"),
        ),
        plain("a deep prefix", format!("s3://{BUCKET}/renders/2026/january")),
        plain(
            "a deep prefix written with a trailing key separator",
            format!("s3://{BUCKET}/renders/2026/january/"),
        ),
        LocationRow {
            case: "a prefix beside the other destination members",
            s3_uri: format!("s3://{BUCKET}/owned.and-encrypted/out"),
            bucket_owner: Some("000000000000"),
            kms_key_id: Some(
                "arn:aws:kms:us-east-1:000000000000:key/\
                 11111111-2222-3333-4444-555555555555",
            ),
        },
    ]
}

/// RULE: the output location an invocation is reported at is the location its submission
/// configured extended by the minted invocation id, as one further key segment — so two
/// invocations submitted into one prefix report two distinct locations — and both readers
/// report that same value from the moment the invocation is accepted.
#[tokio::test]
async fn the_reported_output_location_is_the_submitted_prefix_extended_by_the_invocation_id() {
    let router = emulator_with_object_store();
    create_bucket(&router, BUCKET).await;

    let mut reported: Vec<String> = Vec::new();
    for row in location_rows() {
        let case = row.case;
        let arn = submit_destination(&router, &row).await;
        assert!(
            is_invocation_arn(&arn),
            "case {case:?}: expected an invocationArn matching the modelled pattern, got {arn:?}"
        );
        let invocation_id = invocation_id_of(&arn);

        // The rule: the configured prefix, then this invocation's id as one further key
        // segment — one separator between them whether or not the submitted value ended in
        // one, and none after it.
        let expected = format!("{}/{invocation_id}", row.s3_uri.trim_end_matches('/'));

        let answer = get(&router, &format!("/async-invoke/{}", encode_path(&arn))).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected 200 from GetAsyncInvoke, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let config = answer.json()["outputDataConfig"]["s3OutputDataConfig"].clone();
        let stated = config["s3Uri"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
        // Read as the derivation rather than as one formatted string: the submitted prefix
        // is still there, and the value's last segment is the minted id.
        assert!(
            stated.starts_with(row.s3_uri.trim_end_matches('/')),
            "case {case:?}: expected the submitted prefix to be kept, got {stated:?}"
        );
        assert_eq!(
            stated.rsplit('/').next(),
            Some(invocation_id.as_str()),
            "case {case:?}: expected the minted invocation id as the last segment, got \
             {stated:?}"
        );

        // The derivation touches the URI alone: every other member of the destination is
        // reported as submitted.
        assert_eq!(
            config["bucketOwner"].as_str(),
            row.bucket_owner,
            "case {case:?}: expected the submitted bucketOwner, got {config}"
        );
        assert_eq!(
            config["kmsKeyId"].as_str(),
            row.kms_key_id,
            "case {case:?}: expected the submitted kmsKeyId, got {config}"
        );

        // The listing agrees with the point read: one location per invocation, reported
        // the same way by both readers.
        let summarised = listed_location(&router, case, &arn).await;
        assert_eq!(
            summarised, stated,
            "case {case:?}: expected ListAsyncInvokes to report the same location as \
             GetAsyncInvoke ({stated:?}), got {summarised:?}"
        );

        assert!(
            !reported.contains(&stated),
            "case {case:?}: expected a location of its own, got {stated:?} again in \
             {reported:?}"
        );
        reported.push(stated);
    }
}

/// RULE: the reported location is where the artifacts ARE. Two invocations submitted
/// against one configured prefix each deliver under their own reported location, so the
/// per-invocation output can be addressed from the record the service returns and neither
/// invocation writes over the other.
#[tokio::test]
async fn each_invocations_reported_location_holds_its_own_artifacts_and_no_others() {
    let router = emulator_with_object_store();
    create_bucket(&router, BUCKET).await;

    // Two submissions against ONE configured prefix, so what distinguishes their locations
    // can only be the minted id, plus one at the bucket root.
    let shared = format!("s3://{BUCKET}/shared/prefix/");
    let cases: [(&str, String); 3] = [
        ("the first invocation under a shared prefix", shared.clone()),
        ("the second invocation under the same prefix", shared),
        ("an invocation at the bucket root", format!("s3://{BUCKET}")),
    ];

    let mut locations: Vec<String> = Vec::new();
    for (case, s3_uri) in cases {
        let model_input = json!({"prompt": "A cube under a shared prefix.", "duration": "5s"});
        let arn = submit(&router, case, "luma.ray-v2:0", &model_input, &s3_uri).await;
        let invocation_id = invocation_id_of(&arn);
        let settled = poll_to_terminal(&router, case, &arn).await;
        assert_eq!(
            settled["status"].as_str(),
            Some("Completed"),
            "case {case:?}: expected Completed, got {settled}"
        );
        let location = settled["outputDataConfig"]["s3OutputDataConfig"]["s3Uri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            location,
            format!("{}/{invocation_id}", s3_uri.trim_end_matches('/')),
            "case {case:?}: expected this invocation's own location, got {settled}"
        );
        assert!(
            !locations.contains(&location),
            "case {case:?}: expected a location of its own, got {location:?} again in \
             {locations:?}"
        );

        // What a client does with the record it was handed: read `s3Uri`, then list under
        // it. What it finds is this invocation's output and nothing else.
        let (bucket, prefix) = address_of(case, &location);
        assert_eq!(
            bucket, BUCKET,
            "case {case:?}: expected the configured bucket, got {bucket:?}"
        );
        let mut keys = listed_keys(&router, &format!("{prefix}/")).await;
        keys.sort();
        let media = format!("{prefix}/{}", state::async_invoke_video::MEDIA_OBJECT);
        let manifest = format!("{prefix}/{}", state::async_invoke_output::MANIFEST_OBJECT);
        let mut expected = vec![manifest.clone(), media.clone()];
        expected.sort();
        assert_eq!(
            keys, expected,
            "case {case:?}: expected exactly this invocation's output under {location:?}, \
             got {keys:?}"
        );

        // The media under it is a real ISO base media file, read back through Amazon S3's
        // own GetObject.
        let object = s3_call(&router, "GET", &format!("/{BUCKET}/{media}"), &[]).await;
        assert_eq!(
            object.status,
            200,
            "case {case:?}: expected {media:?} to be readable, got {} / {}",
            object.status,
            String::from_utf8_lossy(&object.body)
        );
        assert_eq!(
            object.body.get(4..8),
            Some(&b"ftyp"[..]),
            "case {case:?}: expected an ISO base media file at {media:?}, got {} bytes \
             starting {:?}",
            object.body.len(),
            object.body.get(..8)
        );

        // The manifest beside it indexes that output at the same location the record
        // reports, and names the call that submitted the invocation.
        let indexed = s3_call(&router, "GET", &format!("/{BUCKET}/{manifest}"), &[]).await;
        let document: Value = serde_json::from_slice(&indexed.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the manifest must be JSON ({error}): {}",
                String::from_utf8_lossy(&indexed.body)
            )
        });
        assert_eq!(
            document["s3Uri"].as_str(),
            Some(location.as_str()),
            "case {case:?}: expected the manifest to state the reported location, got \
             {document}"
        );
        assert_eq!(
            document["jobId"].as_str(),
            Some(invocation_id.as_str()),
            "case {case:?}: expected the manifest to name this invocation, got {document}"
        );
        assert!(
            !document["requestId"].as_str().unwrap_or_default().is_empty(),
            "case {case:?}: expected the manifest to name its submitting call, got {document}"
        );

        locations.push(location);
    }

    // The shared prefix holds both of its invocations' outputs, each under its own
    // segment: nothing was written over.
    let under_shared = listed_keys(&router, "shared/prefix/").await;
    assert_eq!(
        under_shared.len(),
        4,
        "case \"the shared prefix\": expected both invocations' output to survive beside \
         each other, got {under_shared:?}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Submit one invocation against `row`'s destination and return its `invocationArn`.
///
/// The shared `submit` helper configures the URI alone, so the rows carrying the other
/// destination members post their own `outputDataConfig`.
async fn submit_destination(router: &MultiServiceRouter, row: &LocationRow) -> String {
    let mut destination = json!({"s3Uri": row.s3_uri});
    if let Some(owner) = row.bucket_owner {
        destination["bucketOwner"] = Value::String(owner.to_string());
    }
    if let Some(key) = row.kms_key_id {
        destination["kmsKeyId"] = Value::String(key.to_string());
    }
    let case = row.case;
    let call = post_json(
        router,
        "/async-invoke",
        &json!({
            "modelId": "luma.ray-v2:0",
            "modelInput": {"prompt": case, "resolution": "540p", "duration": "5s"},
            "outputDataConfig": {"s3OutputDataConfig": destination},
        }),
    )
    .await;
    assert_eq!(
        call.status,
        200,
        "case {case:?}: expected StartAsyncInvoke to accept the submission, got {} / {}",
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    call.json()["invocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", call.json()))
        .to_string()
}

/// The bucket and the key prefix an `s3://` location addresses — what a client lists under
/// once it has read `s3Uri` off the record.
fn address_of(case: &str, uri: &str) -> (String, String) {
    let rest = uri
        .strip_prefix("s3://")
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} is not an s3 location"));
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    (bucket.to_string(), key.to_string())
}

/// The location `ListAsyncInvokes` reports for the invocation `arn` names.
async fn listed_location(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let call = get(router, "/async-invoke?maxResults=100").await;
    assert_eq!(
        call.status,
        200,
        "case {case:?}: expected 200 from ListAsyncInvokes, got {} / {}",
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    let body = call.json();
    let summaries = body["asyncInvokeSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let summary = summaries
        .iter()
        .find(|summary| summary["invocationArn"].as_str() == Some(arn))
        .unwrap_or_else(|| panic!("case {case:?}: {arn:?} is not among {body}"));
    summary["outputDataConfig"]["s3OutputDataConfig"]["s3Uri"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

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
