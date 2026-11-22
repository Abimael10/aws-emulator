// SPDX-License-Identifier: CC0-1.0

//! What a FINISHED job reports about the MEDIA it processed and the SETTINGS it ran
//! with, end to end over the wire and over audio this emulator's own Amazon S3 holds.
//!
//! THE RULES THESE ROWS ASSERT:
//!
//! * `MediaSampleRateHertz` is "The sample rate, in hertz, of the audio track in your
//!   input media file" (`smithy_query(query="doc", shape="TranscriptionJob",
//!   member="MediaSampleRateHertz")`, and the same sentence on `MedicalTranscriptionJob`
//!   and `CallAnalyticsJob`) — a property of the FILE, which the service determines from
//!   the audio it read, so a job that has finished reports the rate the stored media
//!   declares whether or not the request stated one. A rate the request DID state is kept
//!   as the start reported it, exactly as a declared `MediaFormat` is, and media that
//!   declares no rate the member can carry — none at all, or one outside the range the
//!   member models (`@range(min: 8000, max: 48000)`,
//!   `smithy_query(query="shape", shape="MediaSampleRateHertz")`) — leaves the member
//!   absent rather than guessed.
//!
//! * `Settings` "Provides information on any additional settings that were included in
//!   your request" (`smithy_query(query="doc", shape="TranscriptionJob",
//!   member="Settings")`), so a job that has FINISHED reports the settings it actually ran
//!   with: the two options the service switches on or off for every job — channel
//!   identification and alternative transcriptions — at the value they ran at, which is
//!   off for a request that asked for neither, while the members that name a resource or a
//!   ceiling stay exactly as the request left them. A job the service is still working on
//!   reports only what the request spelled, because what it RAN with is not settled until
//!   it has run.
//!
//! The rows differ along what those rules generalise over: the sample rate the stored
//! audio declares (both published bounds of the member, rates between them, and one
//! outside each bound), whether the request declares a rate of its own, whether the media
//! declares one at all, which family's start named the media (all three that report the
//! member), and which of the switched settings the request asked for.
//!
//! The media is a real RIFF/WAVE file whose header states its sample rate, written with
//! the RIFF/WAVE writer rather than spelled out byte by byte, and uploaded to the Amazon
//! S3 mounted beside transcribe on one shared engine
//! (`crate::media_object_resolution::emulator`, reached through the shared harness) — the
//! upload a caller performs before a start names the location.

use serde_json::{json, Value};

use crate::media_object_resolution::{given_object, Family, MEDIA_BYTES};
use crate::transcription_jobs::Transcribe;

/// The bucket every audio file below is uploaded to, as the caller uploads it before
/// naming it.
const MEDIA_BUCKET: &str = "example-determined-media";

/// A RIFF/WAVE file of one tenth of a second of silence sampled at `sample_rate`.
///
/// Written with the RIFF/WAVE writer, so the `fmt ` chunk really states that rate and the
/// file is one an audio reader accepts — what the rule is about is the service reading the
/// rate out of the media's own header.
fn wave(sample_rate: u32) -> Vec<u8> {
    let spec = ::hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: ::hound::SampleFormat::Int,
    };
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ::hound::WavWriter::new(&mut buffer, spec).expect("a wave writer");
        for _ in 0..(sample_rate / 10) {
            writer.write_sample(0i16).expect("a wave sample");
        }
        writer.finalize().expect("the wave file is finished");
    }
    buffer.into_inner()
}

/// One row of the sample-rate table: the rate the uploaded audio declares, and the rate
/// the finished job must report for it.
struct RateRow {
    /// What this row varies, named in every assertion it fails.
    case: &'static str,
    /// The rate the RIFF/WAVE header of the uploaded audio states.
    declared_by_the_media: u32,
    /// The `MediaSampleRateHertz` the finished job must report, or `None` for a rate the
    /// member cannot carry.
    reported: Option<i64>,
}

/// The rows: the rate the stored audio declares, spanning both published bounds of
/// `MediaSampleRateHertz` (`@range(min: 8000, max: 48000)`), rates between them that no
/// recording of this service cites, and one past each bound.
fn rate_rows() -> Vec<RateRow> {
    vec![
        RateRow {
            case: "the lowest rate the member publishes",
            declared_by_the_media: 8_000,
            reported: Some(8_000),
        },
        RateRow {
            case: "a rate between the published bounds",
            declared_by_the_media: 16_000,
            reported: Some(16_000),
        },
        RateRow {
            case: "another rate between the published bounds",
            declared_by_the_media: 22_050,
            reported: Some(22_050),
        },
        RateRow {
            case: "a third rate between the published bounds",
            declared_by_the_media: 44_100,
            reported: Some(44_100),
        },
        RateRow {
            case: "the highest rate the member publishes",
            declared_by_the_media: 48_000,
            reported: Some(48_000),
        },
        RateRow {
            case: "one hertz below the lowest rate the member publishes",
            declared_by_the_media: 7_999,
            reported: None,
        },
        RateRow {
            case: "one hertz above the highest rate the member publishes",
            declared_by_the_media: 48_001,
            reported: None,
        },
        RateRow {
            case: "far above the highest rate the member publishes",
            declared_by_the_media: 96_000,
            reported: None,
        },
    ]
}

/// The job name and the media key of a rate row, which differ per row so the rows share
/// nothing but the bucket.
fn rate_names(row: &RateRow) -> (String, String) {
    (
        format!("declared-rate-{}", row.declared_by_the_media),
        format!("audio/{}.wav", row.declared_by_the_media),
    )
}

/// The emulator these rows run on: transcribe beside this emulator's own Amazon S3, on
/// one shared engine.
fn transcribe() -> Transcribe {
    Transcribe::with_object_store()
}

#[tokio::test]
async fn a_finished_job_reports_the_sample_rate_its_stored_media_declares() {
    let transcribe = transcribe();
    let rows = rate_rows();

    for row in &rows {
        let (name, key) = rate_names(row);
        given_object(
            &transcribe.router,
            MEDIA_BUCKET,
            &key,
            &wave(row.declared_by_the_media),
        )
        .await;
        // No `MediaSampleRateHertz` of its own: what the job reports is the service's own
        // determination, read out of the audio it was pointed at.
        transcribe
            .call(
                "StartTranscriptionJob",
                json!({
                    "TranscriptionJobName": name,
                    "LanguageCode": "en-US",
                    "Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")},
                }),
            )
            .await
            .ok(row.case);
    }

    for row in &rows {
        let (name, _) = rate_names(row);
        let job = transcribe.transcription_job_settled(&name, None).await;
        assert_eq!(
            job["TranscriptionJobStatus"],
            json!("COMPLETED"),
            "case {:?}: a job over audio the store holds completes, got {job:?}",
            row.case
        );
        let expected = row.reported.map_or(Value::Null, Value::from);
        assert_eq!(
            job["MediaSampleRateHertz"], expected,
            "case {:?}: expected MediaSampleRateHertz {expected:?}, got {:?}",
            row.case, job["MediaSampleRateHertz"]
        );
    }
}

#[tokio::test]
async fn a_rate_the_request_declared_is_kept_and_undeterminable_media_leaves_it_absent() {
    let transcribe = transcribe();
    // The audio every row below is pointed at declares 44100 Hz, so a row that reports
    // anything else reports something other than what the media says.
    let key = "mixed/declared.wav";
    given_object(&transcribe.router, MEDIA_BUCKET, key, &wave(44_100)).await;
    // An object that is NOT an audio container at all: the store answers for it, so the
    // start is accepted, and it declares no rate for anything to read.
    let opaque = "mixed/opaque.wav";
    given_object(&transcribe.router, MEDIA_BUCKET, opaque, MEDIA_BYTES).await;

    // (case, the start's own members beyond the name, the rate the finished job reports)
    let rows: Vec<(&str, Value, Option<i64>)> = vec![
        (
            "a request that declares its own rate keeps it",
            json!({
                "MediaSampleRateHertz": 16_000,
                "Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")},
            }),
            Some(16_000),
        ),
        (
            "a request that declares none reports what the media declares",
            json!({"Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")}}),
            Some(44_100),
        ),
        (
            "media that declares no rate leaves the member absent",
            json!({"Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{opaque}")}}),
            None,
        ),
        (
            "media no object store was asked about leaves the member absent",
            json!({"Media": {"MediaFileUri": "https://example.invalid/media/clip.mp4"}}),
            None,
        ),
    ];

    for (index, (case, members, _)) in rows.iter().enumerate() {
        let mut request = json!({
            "TranscriptionJobName": format!("declared-or-not-{index}"),
            "LanguageCode": "en-US",
        });
        let Value::Object(members) = members.clone() else {
            panic!("case {case:?}: a row's members are an object");
        };
        for (member, value) in members {
            request[member] = value;
        }
        transcribe
            .call("StartTranscriptionJob", request)
            .await
            .ok(case);
    }

    for (index, (case, _, reported)) in rows.iter().enumerate() {
        let job = transcribe
            .transcription_job_settled(&format!("declared-or-not-{index}"), None)
            .await;
        let expected = reported.map_or(Value::Null, Value::from);
        assert_eq!(
            job["MediaSampleRateHertz"], expected,
            "case {case:?}: expected MediaSampleRateHertz {expected:?}, got {:?}",
            job["MediaSampleRateHertz"]
        );
    }
}

#[tokio::test]
async fn every_family_that_reports_the_member_determines_it_from_the_same_media() {
    let transcribe = transcribe();
    let key = "families/shared.wav";
    given_object(&transcribe.router, MEDIA_BUCKET, key, &wave(24_000)).await;
    let media = json!({"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")});

    // Every family whose job shape carries `MediaSampleRateHertz`
    // (`smithy_query(query="shape", shape="TranscriptionJob")` and the same member on
    // `MedicalTranscriptionJob` and `CallAnalyticsJob`); the Medical Scribe family's shape
    // carries none, so it names no row.
    let families = [
        ("a transcription job", Family::Transcription),
        ("a medical transcription job", Family::Medical),
        ("a Call Analytics job", Family::CallAnalytics),
    ];
    for (index, (case, family)) in families.iter().enumerate() {
        let name = format!("family-rate-{index}");
        transcribe
            .call(family.start(), family.start_request(&name, media.clone()))
            .await
            .ok(case);
    }

    for (index, (case, family)) in families.iter().enumerate() {
        let name = format!("family-rate-{index}");
        let job = transcribe
            .job_settled(
                family.get(),
                family.name_member(),
                family.job_member(),
                match family {
                    Family::CallAnalytics => "CallAnalyticsJobStatus",
                    _ => "TranscriptionJobStatus",
                },
                &name,
                None,
            )
            .await;
        assert_eq!(
            job["MediaSampleRateHertz"],
            json!(24_000),
            "case {case:?}: expected the rate the shared media declares (24000), got {:?}",
            job["MediaSampleRateHertz"]
        );
    }
}

/// The two options the service switches on or off for every transcription job, at the
/// value a request that asked for neither ran them at. Read from the shape's own boolean
/// members (`smithy_query(query="shape", shape="Settings")`).
fn switched_off() -> Value {
    json!({"ChannelIdentification": false, "ShowAlternatives": false})
}

#[tokio::test]
async fn a_finished_job_reports_the_effective_settings_it_ran_with() {
    let transcribe = transcribe();
    let key = "settings/audio.wav";
    given_object(&transcribe.router, MEDIA_BUCKET, key, &wave(16_000)).await;

    // (case, the `Settings` the request carries, the `Settings` the finished job reports)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a request carrying no settings at all",
            Value::Null,
            switched_off(),
        ),
        (
            "a request that asked for speaker partitioning",
            json!({"ShowSpeakerLabels": true, "MaxSpeakerLabels": 4}),
            json!({
                "ShowSpeakerLabels": true,
                "MaxSpeakerLabels": 4,
                "ChannelIdentification": false,
                "ShowAlternatives": false
            }),
        ),
        (
            "a request that switched channel identification on",
            json!({"ChannelIdentification": true}),
            json!({"ChannelIdentification": true, "ShowAlternatives": false}),
        ),
        (
            "a request that asked for alternative transcriptions",
            json!({"ShowAlternatives": true, "MaxAlternatives": 3}),
            json!({
                "ShowAlternatives": true,
                "MaxAlternatives": 3,
                "ChannelIdentification": false
            }),
        ),
    ];

    for (index, (case, settings, _)) in rows.iter().enumerate() {
        let mut request = json!({
            "TranscriptionJobName": format!("effective-settings-{index}"),
            "LanguageCode": "en-US",
            "Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")},
        });
        if !settings.is_null() {
            request["Settings"] = settings.clone();
        }
        let started = transcribe.call("StartTranscriptionJob", request).await;
        let job = started.ok(case)["TranscriptionJob"].clone();
        // A job the service is still working on reports only what the request spelled:
        // what it RAN with is not settled until it has run.
        assert_eq!(
            job["Settings"],
            settings.clone(),
            "case {case:?}: a job that has not finished reports the settings the request \
             spelled, got {:?}",
            job["Settings"]
        );
    }

    for (index, (case, _, effective)) in rows.iter().enumerate() {
        let name = format!("effective-settings-{index}");
        let job = transcribe.transcription_job_settled(&name, None).await;
        assert_eq!(
            job["Settings"], *effective,
            "case {case:?}: expected the effective settings {effective:?}, got {:?}",
            job["Settings"]
        );
        // A read does not change what a later read sees.
        let again = transcribe
            .call(
                "GetTranscriptionJob",
                json!({"TranscriptionJobName": name.clone()}),
            )
            .await;
        assert_eq!(
            again.ok(case)["TranscriptionJob"]["Settings"],
            *effective,
            "case {case:?}: the effective settings must be stable across reads, got {:?}",
            again.body["TranscriptionJob"]["Settings"]
        );
    }
}
