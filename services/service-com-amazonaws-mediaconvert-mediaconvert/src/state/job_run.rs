// SPDX-License-Identifier: CC0-1.0

//! What a job's RUN actually DOES: it produces every output of every output group the job's
//! settings describe — reading the media the job names out of Amazon S3 and writing the
//! output file back into the destination the group names — and records what it produced, so
//! the job's COMPLETE projection reports results the run really established rather than
//! synthesised ones.
//!
//! # The rule this module implements
//!
//! `JobStatus` is "SUBMITTED, PROGRESSING, COMPLETE, CANCELED, or ERROR" and the documented
//! MediaConvert workflow is `CreateJob` followed by polling `GetJob` until the job is
//! COMPLETE; `Job$OutputGroupDetails` then "contains details about the output groups
//! specified in the job settings", one entry per output group, each `OutputDetail` carrying
//! that output's `DurationInMs` and its `VideoDetails` frame size. So a job whose outputs
//! this service can work out RUNS, and what it reports is read off that run; a job whose run
//! could not establish its outputs at all reaches ERROR with the reason it failed.
//!
//! What the run is, exactly, depends on what the job's input IS — "You can specify Video
//! generator, or you can specify an Input file, but you cannot specify both":
//!
//! * an input FILE is read out of the object store and DEMUXED (`crate::ops::probe`'s reader
//!   is the same one `Probe` reports a file with), and its media is written into the
//!   container the output asks for, sample for sample. The essence is carried over rather
//!   than re-encoded — this emulator has no video encoder — so the engine writes an output
//!   in an ISO base media container (MP4, MOV) and nothing else; an output asking for a
//!   container it cannot write, or an input it cannot demux, fails the job rather than
//!   reporting a result for work that did not happen.
//! * an input the job has this service GENERATE (`Input$VideoGenerator`, "MediaConvert
//!   creates a video input with black frames") has its frames CODED here, by the maintained
//!   H.264 encoder this workspace already renders video with, and framed into the file
//!   through the workspace's one H.264/MP4 seam (`service_common::h264_mp4`): the duration,
//!   frame size and frame rate the job states decide what is coded, and the work one job can
//!   ask for is bounded before any of it starts.
//!
//! Every value a completed job reports is read off the media the output was produced FROM —
//! how long it runs and the frame size it is written at — so no value is a result synthesised
//! for work nothing did, and a status of COMPLETE means the output files are in the
//! destination the job named.
//!
//! # What drives it, and exactly once
//!
//! A run is owed to a job that has BEGUN — the clock moved it into `PROGRESSING`
//! ([`super::ComAmazonawsMediaconvertMediaconvertState::settle`]) — and is made by whichever
//! of these gets there first, through the one claim below, so it happens once:
//!
//! * the store's clock pump ([`super::drive_job_clock`]), because a client that reacts to a
//!   job on an EventBridge rule makes no further MediaConvert call and its job still has to
//!   run;
//! * the job read a polling client makes (`GetJob`), which materialises what the clock has
//!   ALREADY made due — the same materialisation that read already makes for the job's
//!   status — so a job completes for a client that polls even where no pump runs.
//!
//! Nothing here decides WHEN a job moves: the stored due instants do. The run stores what it
//! produced; the status change itself is applied by the settle that finds the finish due.
//!
//! Both hops are the in-process callback into this same emulator
//! ([`::service_common::s3_client`]), addressed by the REGION the job was submitted in and
//! by nothing else: the bucket and key a job's settings carry select objects of this
//! emulator's own object store and are never an address. No store guard is held across
//! either of them.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::{Duration, Instant};

use smithy_server_runtime::EmbeddedEmulator;

use super::{
    ComAmazonawsMediaconvertMediaconvertState, JobRecord, PresetRecord, JOB_STATUS_PROGRESSING,
    JOB_STATUS_SUBMITTED,
};
use crate::ops::types::{Input, Output, OutputGroup, VideoDescription};

/// How many job runs one pass makes.
///
/// CHOSEN, and a bound on WORK rather than on jobs: a pass can find many jobs running at
/// once, and each run reads and writes objects. What is left over is run by the next pass —
/// the pump's next wake-up, or the next read that finds it due — because a claim leaves a
/// job it did not take exactly as it found it.
const MAX_RUNS_PER_PASS: usize = 4;

/// How long a CLAIMED run may be outstanding before another driver may claim it again.
///
/// CHOSEN, and a liveness bound rather than a timeout on the work: the claim is taken under
/// the store lock and reported back by the claimant, so a claimant that never reports back —
/// the request it was made on was dropped — would otherwise leave the job claimed and never
/// terminal. It is measured on the MONOTONIC clock the claim is stamped with, so a reclaim
/// cannot race a claimant that is still working, not even across a wall-clock jump.
const RUN_RECLAIM_AFTER: Duration = Duration::from_secs(60);

/// How many bytes of one media file this engine reads.
///
/// A CHOSEN ceiling, the same one `Probe` reads a file to
/// ([`crate::ops::probe::MAX_PROBED_FILE_BYTES`]): the file is read whole — a container's
/// index may sit anywhere in it — so what a job can make this process hold in memory is
/// bounded here rather than by whatever the object store happens to hold.
const MAX_INPUT_BYTES: usize = crate::ops::probe::MAX_PROBED_FILE_BYTES;

/// How many outputs one run produces.
///
/// CHOSEN: `Settings.OutputGroups[].Outputs` is caller-driven and each entry costs one
/// object write, so the work one job can ask for has a ceiling. A job past it fails rather
/// than being written out in part, because a job that reports COMPLETE has produced every
/// output its settings describe.
const MAX_PRODUCED_OUTPUTS: usize = 20;

/// How many bytes one produced output file holds.
///
/// A CHOSEN ceiling, the write-side counterpart of [`MAX_INPUT_BYTES`]: what a produced file
/// holds is driven by the input's own sample table, which a caller writes, so the output is
/// abandoned once it passes this rather than growing with whatever that table indexes.
const MAX_OUTPUT_BYTES: usize = MAX_INPUT_BYTES;

/// How many frames one GENERATED input's video holds.
///
/// A CHOSEN ceiling: `InputVideoGenerator$Duration` and its frame rate are the caller's, and
/// every frame is one the encoder codes, so the work one job can ask for is bounded before
/// any of it starts. A job past it produces nothing and says so.
const MAX_GENERATED_FRAMES: u32 = 900;

/// How many pixels one frame of a GENERATED input holds (1920x1080).
///
/// A CHOSEN ceiling, for the same reason and applied to the geometry BEFORE any buffer is
/// sized from it: every buffer of the rendering is sized from the coded frame.
const MAX_GENERATED_FRAME_PIXELS: u64 = 1_920 * 1_080;

/// How many pixels one GENERATED input's whole video holds.
///
/// A CHOSEN ceiling on the PRODUCT of the frame count and the frame size, rather than on
/// each of them: that product is the work the encoder does.
const MAX_GENERATED_PIXELS: u64 = 250_000_000;

/// How long the frames of one GENERATED input may take to code before the run gives up.
///
/// A CHOSEN wall-clock budget, beside the ceilings above: the work is bounded by its input,
/// and this bounds how long a machine under load spends on it, so a job that cannot be coded
/// inside it fails rather than holding a thread of the blocking pool indefinitely.
const GENERATED_RENDER_BUDGET: Duration = Duration::from_secs(20);

/// What one job's run produced, as the job's own projection then reports it.
#[derive(Debug, Clone)]
pub struct JobRunRecord {
    /// One entry per output group of the job's settings, in settings order — what
    /// `Job$OutputGroupDetails` reports.
    pub output_groups: Vec<OutputGroupRun>,
    /// The warning classes this run raised, each with the number of times it occurred, as
    /// `Job$Warnings` reports them. Empty for a run that raised none.
    pub warnings: Vec<JobWarning>,
    /// Why the run produced nothing, for a run that failed; `None` for one that produced
    /// every output the job's settings describe.
    pub failure: Option<JobRunFailure>,
}

/// What one output group of a run produced: one entry per output of that group.
#[derive(Debug, Clone, Default)]
pub struct OutputGroupRun {
    pub outputs: Vec<OutputRun>,
}

/// What one output of a run produced, measured on the media it was produced FROM.
#[derive(Debug, Clone, Default)]
pub struct OutputRun {
    /// `OutputDetail$DurationInMs`, "Duration in milliseconds", of the produced output.
    pub duration_ms: Option<i32>,
    /// `VideoDetail$WidthInPx` and `$HeightInPx` of the produced output's video stream;
    /// both absent for an output that carries no video at all.
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// One warning class a run raised: the published warning code, and how many times it
/// occurred in the job (`WarningGroup$Code` and `$Count`, both required).
#[derive(Debug, Clone, Copy)]
pub struct JobWarning {
    pub code: i32,
    pub count: i32,
}

/// Why a run produced no outputs.
///
/// Each variant is something the run ESTABLISHED, never assumed: the object store answered
/// that an input is not there or that reading it is not permitted, the demuxer could not
/// read a container out of the bytes it fetched, or the output the job describes is one this
/// engine cannot write. A run that established NOTHING — nothing answered for the input at
/// all — is [`Self::EngineDidNotRun`], which reports no failure of the caller's.
#[derive(Debug, Clone)]
pub enum JobRunFailure {
    /// The object store answered that it holds nothing at this input's address.
    InputUnreadable(String),
    /// The object store answered that reading this input is not permitted.
    InputDenied(String),
    /// The input at this URI was read, and its bytes are not a container this service can
    /// demux, or they carry no track it can carry over.
    InputNotDemuxable(String),
    /// The job's outputs are not ones this engine can produce; the detail says which part.
    NotProducible(String),
    /// An output's file could not be written into the destination its group names.
    OutputNotWritten(String),
    /// Nothing ran this job's transcode at all, so nothing was established about it:
    /// there is no object store in this emulator to read the input from, or the input is
    /// not an address this emulator can read — no connection is made outside this process.
    EngineDidNotRun,
}

impl JobRunFailure {
    /// `Job$ErrorCode` for this failure, when MediaConvert publishes a code for it.
    ///
    /// Only the failure to OPEN an input has one here —
    /// [`super::INPUT_PROBE_FAILURE_CODE`], the published code for an input the service
    /// could not open or probe, which is what the object store answering absent or denied
    /// establishes. The others are this emulator's own limits, which no MediaConvert error
    /// code classifies, so none is reported rather than one invented.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::InputUnreadable(_) | Self::InputDenied(_) => {
                Some(super::INPUT_PROBE_FAILURE_CODE)
            }
            _ => None,
        }
    }

    /// `Job$ErrorMessage` for this failure: what failed, naming the job's own input or
    /// output as the request stated it.
    pub fn message(&self) -> String {
        match self {
            Self::InputUnreadable(uri) => super::input_probe_failure_message(uri),
            Self::InputDenied(uri) => format!(
                "Unable to open input file [{uri}]. The read of that location was denied."
            ),
            Self::InputNotDemuxable(uri) => format!(
                "Unable to transcode input file [{uri}]. The file is not in a container \
                 format that this service can read."
            ),
            Self::NotProducible(detail) => format!(
                "This emulator's transcoding engine could not produce this job's outputs: \
                 {detail}"
            ),
            Self::OutputNotWritten(destination) => {
                format!("Unable to write this job's output to [{destination}].")
            }
            Self::EngineDidNotRun => super::NO_TRANSCODER_MESSAGE.to_string(),
        }
    }
}

/// One claimed run: the job it belongs to, and everything the work needs, copied out from
/// under the store guard so the work itself holds no lock.
#[derive(Debug)]
struct ClaimedRun {
    job_id: String,
    /// Every input of the job, in settings order: the file it names, or the video it has
    /// this service generate.
    inputs: Vec<PlannedInput>,
    /// One entry per output group of the job's settings, in settings order.
    groups: Vec<PlannedGroup>,
}

/// What one input of a job IS.
///
/// `Input$VideoGenerator`: "When you include Video generator, MediaConvert creates a video
/// input with black frames. Use this setting if you do not have a video input... You can
/// specify Video generator, or you can specify an Input file, but you cannot specify both" —
/// so an input is one or the other, and the file is what the input names when it names one.
#[derive(Debug)]
enum PlannedInput {
    /// `Input$FileInput`, "the source file for your transcoding job".
    File(String),
    /// `Input$VideoGenerator`, the video this service CREATES instead of reading one: black
    /// frames of the duration, frame size and frame rate the generator states.
    Generated(GeneratedInput),
}

/// The video an `InputVideoGenerator` states: black frames of this long, at this frame size
/// and this frame rate.
#[derive(Debug, Clone, Copy)]
struct GeneratedInput {
    /// `InputVideoGenerator$Duration`: "Specify the duration, in milliseconds, for your
    /// video generator input."
    duration_ms: Option<i32>,
    /// `InputVideoGenerator$Width` / `$Height`: "To use the default resolution 540x360:
    /// Leave both width and height blank."
    width: Option<i32>,
    height: Option<i32>,
    /// `InputVideoGenerator$FramerateNumerator` / `$FramerateDenominator`, the fraction the
    /// generated video's frame rate is.
    framerate_numerator: Option<i32>,
    framerate_denominator: Option<i32>,
}

/// The frame size a video generator that states none produces, from the member's own
/// documentation: "To use the default resolution 540x360: Leave both width and height
/// blank."
const GENERATED_FRAME: (i32, i32) = (540, 360);

/// The frame rate a video generator that states none produces, as the fraction
/// `numerator / denominator`, from the member's own documentation: "MediaConvert uses a
/// default frame rate of 29.97 when you leave Frame rate numerator and Frame rate
/// denominator blank."
const GENERATED_FRAMERATE: (i32, i32) = (30_000, 1_001);

impl GeneratedInput {
    /// The frame size this generated input carries: the one it states, or the default the
    /// member documents.
    fn frame(&self) -> (i32, i32) {
        (
            self.width.unwrap_or(GENERATED_FRAME.0),
            self.height.unwrap_or(GENERATED_FRAME.1),
        )
    }

    /// The frame rate this generated input runs at, as the fraction the members state, or
    /// the default the members document. A rate is a fraction of two positive numbers, and
    /// the members are stated together ("When you do, you must also specify a value for
    /// Frame rate denominator"), so a pair that is not both stated is no rate at all.
    fn framerate(&self) -> (i32, i32) {
        match (self.framerate_numerator, self.framerate_denominator) {
            (Some(numerator), Some(denominator)) if numerator > 0 && denominator > 0 => {
                (numerator, denominator)
            }
            _ => GENERATED_FRAMERATE,
        }
    }

    /// How many FRAMES the video this generator states holds, when it is written at `rate`
    /// (the fraction `numerator / denominator`).
    ///
    /// Video is whole frames, and the generated video covers the duration the generator
    /// states: the frame shown at the end of that duration is written too, so the count is
    /// the frames that cover the stated duration inclusively. A generator that states no
    /// duration states no video at all.
    ///
    /// Every step is checked, because all three values come from the request: a count that
    /// cannot be expressed is no count, and the caller is answered with the ceiling below
    /// rather than with a wrapped number.
    fn frames(&self, rate: (i32, i32)) -> Option<u32> {
        let duration = i64::from(self.duration_ms.filter(|duration| *duration > 0)?);
        let (numerator, denominator) = rate;
        let per_second = i64::from(numerator.max(1));
        let seconds = i64::from(denominator.max(1)).checked_mul(1_000)?;
        let frames = duration
            .checked_mul(per_second)?
            .checked_div(seconds)?
            .checked_add(1)?;
        u32::try_from(frames).ok()
    }
}

/// What one output group's run writes: where it goes, and what each of its outputs is.
#[derive(Debug)]
struct PlannedGroup {
    /// The group's destination, as its `OutputGroupSettings` states it.
    destination: Option<String>,
    outputs: Vec<PlannedOutput>,
}

/// What one output of a group is: the container it is written in, the name it is written
/// under, and the frame size the job asked for.
#[derive(Debug)]
struct PlannedOutput {
    /// `ContainerSettings$Container` of the output or of the preset it names.
    container: Option<String>,
    /// `Output$Extension`, "Use Extension to specify the file extension for outputs in File
    /// output groups", when the output states one.
    extension: Option<String>,
    /// `Output$NameModifier`, "Use Name modifier to have the service add a string to the end
    /// of each output filename".
    name_modifier: Option<String>,
    /// `VideoDescription$Width` / `$Height` of the output or of its preset: "Use Width to
    /// define the video resolution width, in pixels, for this output. To use the same
    /// resolution as your input: Leave both Width and Height blank. To evenly scale from
    /// your input resolution: Leave Width blank and enter a value for Height."
    width: Option<i32>,
    height: Option<i32>,
    /// The frame rate this output's video is written at, as the fraction its video codec
    /// settings state ([`output_framerate`]), or `None` for an output that states none.
    framerate: Option<(i32, i32)>,
}

/// Make the runs the clock has already made due, at most [`MAX_RUNS_PER_PASS`] of them.
///
/// `asked_about` is the job the caller's own request is ABOUT, when it is about one: its run
/// is made first, so a read of one job reports that job's own run rather than spending the
/// pass on another's.
///
/// Call it with NO store guard held — it awaits.
pub async fn run_due_job_runs<Eng: Send + Sync + 'static>(
    callback: &EmbeddedEmulator,
    engine: &Eng,
    region: &str,
    asked_about: Option<&str>,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = ::std::time::SystemTime::now();
    let due: Vec<String> = {
        let guard = state.lock();
        let mut due: Vec<String> = guard
            .jobs
            .values()
            .filter(|job| run_is_due(job, Instant::now(), now))
            .map(|job| job.id.clone())
            .collect();
        due.sort_by_key(|id| asked_about != Some(id.as_str()));
        due.truncate(MAX_RUNS_PER_PASS);
        due
    };
    for job_id in due {
        // The guard is taken and RELEASED around the claim, so this service's store is never
        // locked across the object reads and writes the run makes. A job another driver
        // claimed first costs this pass nothing.
        let claimed = {
            let mut guard = state.lock();
            claim_run(&mut guard, &job_id, Instant::now(), now)
        };
        let Some(claim) = claimed else {
            continue;
        };
        let produced = produce(callback, region, &claim).await;
        let mut guard = state.lock();
        if let Some(job) = guard.jobs.get_mut(&claim.job_id) {
            job.run_claimed_at = None;
            // The run is recorded only while the job is still on its way through: one that
            // was cancelled while its run was in flight has reached a terminal status of its
            // own, and a run's result is not a reason to move it off that.
            if job.status == JOB_STATUS_SUBMITTED || job.status == JOB_STATUS_PROGRESSING {
                job.run = Some(produced);
            }
        }
    }
}

/// Whether the clock owes `job` its run: the job has BEGUN, or its exit from `SUBMITTED`
/// was already due at `now` — a run is owed from the instant the job leaves its queue's
/// backlog, whether or not anybody has materialised that exit yet — nothing has recorded
/// what its run produced, and no driver is making that run, or the one that claimed it has
/// been outstanding past [`RUN_RECLAIM_AFTER`].
fn run_is_due(job: &JobRecord, now: Instant, wall_clock: ::std::time::SystemTime) -> bool {
    let begun = job.status == JOB_STATUS_PROGRESSING
        || (job.status == JOB_STATUS_SUBMITTED
            && job.settles_at.is_some_and(|due| due <= wall_clock));
    begun
        && job.run.is_none()
        && job
            .run_claimed_at
            .is_none_or(|claimed| now.duration_since(claimed) >= RUN_RECLAIM_AFTER)
}

/// Claim the run of the job `job_id` names, planning it off the job's own settings.
///
/// The probe and the claim happen under the one `&mut` borrow the caller holds the store
/// lock for, so two drivers cannot both claim one run.
fn claim_run(
    store: &mut ComAmazonawsMediaconvertMediaconvertState,
    job_id: &str,
    now: Instant,
    wall_clock: ::std::time::SystemTime,
) -> Option<ClaimedRun> {
    let presets = store.presets.clone();
    let job = store.jobs.get_mut(job_id)?;
    if !run_is_due(job, now, wall_clock) {
        return None;
    }
    job.run_claimed_at = Some(now);
    Some(ClaimedRun {
        job_id: job.id.clone(),
        inputs: job
            .settings
            .inputs
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(planned_input)
            .collect(),
        groups: job
            .settings
            .output_groups
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|group| planned_group(group, &presets))
            .collect(),
    })
}

/// What one input of a job IS: the file it names, or the video it has this service generate
/// ([`PlannedInput`]). `None` for an input that states neither, which is nothing to
/// transcode.
fn planned_input(input: &Input) -> Option<PlannedInput> {
    if let Some(file) = input
        .file_input
        .as_deref()
        .filter(|uri| !super::carries_no_characters(uri))
    {
        return Some(PlannedInput::File(file.to_string()));
    }
    input.video_generator.as_ref().map(|generator| {
        PlannedInput::Generated(GeneratedInput {
            duration_ms: generator.duration,
            width: generator.width.filter(|width| *width > 0),
            height: generator.height.filter(|height| *height > 0),
            framerate_numerator: generator.framerate_numerator,
            framerate_denominator: generator.framerate_denominator,
        })
    })
}

/// What one output group's run will write, read off the group's own settings.
fn planned_group(group: &OutputGroup, presets: &BTreeMap<String, PresetRecord>) -> PlannedGroup {
    PlannedGroup {
        destination: super::output_group_destination(group.output_group_settings.as_ref())
            .map(str::to_string),
        outputs: group
            .outputs
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|output| planned_output(output, presets))
            .collect(),
    }
}

/// What one output is, resolved through the preset it names when it names one: an output
/// states its encoder settings EXACTLY once, inline or by naming a preset, so what it is
/// written as is read the way every other reader of that graph reads it
/// ([`super::output_container_settings`], [`super::output_video_description`]).
fn planned_output(output: &Output, presets: &BTreeMap<String, PresetRecord>) -> PlannedOutput {
    let video = super::output_video_description(output, presets);
    PlannedOutput {
        container: super::output_container_settings(output, presets)
            .and_then(|settings| settings.container.clone()),
        extension: output.extension.clone(),
        name_modifier: output.name_modifier.clone(),
        width: video.and_then(|video| video.width).filter(|width| *width > 0),
        height: video
            .and_then(|video| video.height)
            .filter(|height| *height > 0),
        framerate: video.and_then(output_framerate),
    }
}

/// The frame rate one output's video is written at, as the fraction its video codec settings
/// state.
///
/// Every codec a `VideoCodecSettings` can name states its frame rate the same way — a
/// `FramerateNumerator` and a `FramerateDenominator`, stated together — so the rate is read
/// off whichever codec the output names, and an output that states no rate states none here.
/// It is the clock the output's own frames are counted in.
fn output_framerate(video: &VideoDescription) -> Option<(i32, i32)> {
    let settings = video.codec_settings.as_ref()?;
    let rate = |numerator: Option<i32>, denominator: Option<i32>| match (numerator, denominator) {
        (Some(numerator), Some(denominator)) if numerator > 0 && denominator > 0 => {
            Some((numerator, denominator))
        }
        _ => None,
    };
    [
        settings
            .h264_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .h265_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .av1_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .avc_intra_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .mpeg2_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .prores_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .uncompressed_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .vc3_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .vp8_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
        settings
            .vp9_settings
            .as_ref()
            .and_then(|codec| rate(codec.framerate_numerator, codec.framerate_denominator)),
    ]
    .into_iter()
    .flatten()
    .next()
}

/// Make one claimed run: produce each output of each output group, and report what was
/// produced.
///
/// What a run DOES depends on what its input is ([`PlannedInput`]): an input FILE is read out
/// of the object store, demuxed, and written into the container each output asks for; an
/// input the job has this service GENERATE is CODED here — black frames of the duration,
/// frame size and frame rate the job states — and written the same way.
async fn produce(callback: &EmbeddedEmulator, region: &str, claim: &ClaimedRun) -> JobRunRecord {
    let outputs: usize = claim.groups.iter().map(|group| group.outputs.len()).sum();
    if outputs > MAX_PRODUCED_OUTPUTS {
        return failed(JobRunFailure::NotProducible(format!(
            "this job describes {outputs} outputs, and one job's run writes at most \
             {MAX_PRODUCED_OUTPUTS}"
        )));
    }
    // This engine carries the input's media into the output rather than re-encoding it, so
    // it produces an output from ONE input: stitching several inputs into one output
    // ("The service concatenates these inputs, in the order that you specify them") is work
    // it cannot do, and a job that asks for it fails rather than being answered with an
    // output built from part of what it stated.
    let [input] = claim.inputs.as_slice() else {
        return failed(JobRunFailure::NotProducible(format!(
            "this job states {} inputs, and this engine produces an output from a single \
             input",
            claim.inputs.len()
        )));
    };
    match input {
        PlannedInput::File(uri) => produce_from_file(callback, region, claim, uri).await,
        PlannedInput::Generated(generated) => {
            produce_from_generator(callback, region, claim, *generated).await
        }
    }
}

/// A run that produced nothing, and why.
fn failed(failure: JobRunFailure) -> JobRunRecord {
    JobRunRecord {
        output_groups: Vec::new(),
        warnings: Vec::new(),
        failure: Some(failure),
    }
}

/// A run whose outputs were produced: one entry per output group, in settings order.
fn produced(groups: Vec<OutputGroupRun>) -> JobRunRecord {
    JobRunRecord {
        output_groups: groups,
        // Nothing this engine's run does raises one of MediaConvert's published warning
        // classes, so a job that ran reports no warning rather than a code invented for it.
        warnings: Vec::new(),
        failure: None,
    }
}

/// Produce every output from the input FILE at `uri`: the object is read, demuxed, and
/// written into the container each output asks for, at the frame size that output resolves
/// to.
async fn produce_from_file(
    callback: &EmbeddedEmulator,
    region: &str,
    claim: &ClaimedRun,
    uri: &str,
) -> JobRunRecord {
    let Some((bucket, key)) = ::service_common::s3_uri::url_object_address(uri) else {
        // Not an address this emulator can read — and nothing outside this process is
        // reachable, so no file is fetched from anywhere else — which establishes nothing
        // about the input.
        return failed(JobRunFailure::EngineDidNotRun);
    };
    let media = match read_object(callback, region, &bucket, &key).await {
        ::service_common::s3_object_read::ObjectRead::Served(media) => {
            ::std::sync::Arc::new(media)
        }
        // The store ANSWERED about the address: an input the service could not open is
        // what the job failed on.
        ::service_common::s3_object_read::ObjectRead::Absent => {
            return failed(JobRunFailure::InputUnreadable(uri.to_string()))
        }
        ::service_common::s3_object_read::ObjectRead::Denied => {
            return failed(JobRunFailure::InputDenied(uri.to_string()))
        }
        // An object no read can buffer is one this engine cannot transcode, and a read
        // nothing answered establishes nothing at all.
        ::service_common::s3_object_read::ObjectRead::Unreadable => {
            return failed(JobRunFailure::NotProducible(format!(
                "this job's input is larger than the {MAX_INPUT_BYTES} bytes this engine \
                 reads"
            )))
        }
        ::service_common::s3_object_read::ObjectRead::Unanswered => {
            return failed(JobRunFailure::EngineDidNotRun)
        }
    };
    // What the input IS, read off its own bytes: how long it runs and the frame size of its
    // video, which is what every output of this run is measured against. Reading a
    // caller-supplied container is CPU-bound work with no await of its own, so it runs on
    // the BLOCKING pool and not on the async worker: a worker busy inside a demuxer serves
    // no other request and lands no timeout, cancellation or shutdown signal.
    let measured = {
        let media = media.clone();
        ::tokio::task::spawn_blocking(move || measure_in_barrier(&media)).await
    };
    let Ok(Some(measured)) = measured else {
        return failed(JobRunFailure::InputNotDemuxable(uri.to_string()));
    };

    let mut groups: Vec<OutputGroupRun> = Vec::with_capacity(claim.groups.len());
    for group in &claim.groups {
        let mut produced: Vec<OutputRun> = Vec::with_capacity(group.outputs.len());
        for output in &group.outputs {
            let Some(container) = output.container.as_deref() else {
                return failed(JobRunFailure::NotProducible(
                    "an output names no container to write it in".to_string(),
                ));
            };
            if !is_iso_bmff_container(container) {
                return failed(JobRunFailure::NotProducible(format!(
                    "an output asks for the {container} container, and this engine writes \
                     ISO base media files only"
                )));
            }
            let frame = resolved_frame((output.width, output.height), measured.frame);
            // The transcode itself: reading a container and writing another is CPU-bound
            // work with no await of its own, so it runs on the BLOCKING pool — a worker
            // inside a demuxer serves no other request — and this handler waits for the task
            // it started.
            let written = {
                let media = media.clone();
                ::tokio::task::spawn_blocking(move || remux_in_barrier(&media, frame)).await
            };
            let Ok(Some(bytes)) = written else {
                return failed(JobRunFailure::InputNotDemuxable(uri.to_string()));
            };
            let Some(destination) = group.destination.as_deref() else {
                return failed(JobRunFailure::NotProducible(
                    "an output group names no destination to write its outputs to".to_string(),
                ));
            };
            let name = output_file_name(destination, &key, output, container);
            let Some(address) = object_address(destination, &name) else {
                return failed(JobRunFailure::OutputNotWritten(destination.to_string()));
            };
            if !write_object(
                callback,
                region,
                &address.0,
                &address.1,
                &claim.job_id,
                container,
                &bytes,
            )
            .await
            {
                return failed(JobRunFailure::OutputNotWritten(format!(
                    "s3://{}/{}",
                    address.0, address.1
                )));
            }
            produced.push(OutputRun {
                duration_ms: measured.duration_ms,
                width: frame.map(|(width, _)| width),
                height: frame.map(|(_, height)| height),
            });
        }
        groups.push(OutputGroupRun { outputs: produced });
    }
    produced(groups)
}

/// Produce every output from a GENERATED input: the black frames `Input$VideoGenerator`
/// states are CODED here, written into the container each output asks for, and PUT into the
/// destination its group names.
///
/// What the video IS comes from the job alone — the generator's duration, frame size and
/// frame rate, and the output's own `VideoDescription` where it states a frame size or a
/// rate — and what the output REPORTS is measured on the file that was written, so the
/// detail describes the produced bytes rather than the request.
async fn produce_from_generator(
    callback: &EmbeddedEmulator,
    region: &str,
    claim: &ClaimedRun,
    generated: GeneratedInput,
) -> JobRunRecord {
    let mut groups: Vec<OutputGroupRun> = Vec::with_capacity(claim.groups.len());
    for group in &claim.groups {
        let mut produced: Vec<OutputRun> = Vec::with_capacity(group.outputs.len());
        for output in &group.outputs {
            let Some(container) = output.container.as_deref() else {
                return failed(JobRunFailure::NotProducible(
                    "an output names no container to write it in".to_string(),
                ));
            };
            if !is_iso_bmff_container(container) {
                return failed(JobRunFailure::NotProducible(format!(
                    "an output asks for the {container} container, and this engine writes \
                     ISO base media files only"
                )));
            }
            // The output's own frame size and frame rate where it states them, and the
            // generated video's own where it does not.
            let Some(frame) =
                resolved_frame((output.width, output.height), Some(generated.frame()))
            else {
                return failed(JobRunFailure::NotProducible(
                    "a generated input states no frame size to write".to_string(),
                ));
            };
            let rate = output.framerate.unwrap_or_else(|| generated.framerate());
            let Some(frames) = generated.frames(rate) else {
                return failed(JobRunFailure::NotProducible(
                    "a generated input states no duration to write".to_string(),
                ));
            };
            // Every ceiling is applied BEFORE a single frame is coded or a buffer sized:
            // the frame count, the pixels in one frame, and the product of the two, which
            // is the work the encoder is asked for.
            if let Some(past) = past_generated_budget(frame, frames) {
                return failed(JobRunFailure::NotProducible(past));
            }
            // Coding frames is CPU-bound work with no await of its own, so it runs on the
            // BLOCKING pool and not on the async worker, under its own wall-clock budget;
            // this handler waits for the task it started.
            let written =
                ::tokio::task::spawn_blocking(move || render_black_video(frame, rate, frames))
                    .await;
            let bytes = match written {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(refused)) => return failed(JobRunFailure::NotProducible(refused)),
                // The task did not run to completion: the runtime is shutting down, or it
                // was cancelled, so nothing was produced and nothing is reported.
                Err(error) => {
                    return failed(JobRunFailure::NotProducible(format!(
                        "the video could not be coded: {error}"
                    )))
                }
            };
            let Some(destination) = group.destination.as_deref() else {
                return failed(JobRunFailure::NotProducible(
                    "an output group names no destination to write its outputs to".to_string(),
                ));
            };
            // A generated input names no file, so the output is named after the base name
            // its destination states (an empty one where the destination names a place).
            let name = output_file_name(destination, "", output, container);
            let Some(address) = object_address(destination, &name) else {
                return failed(JobRunFailure::OutputNotWritten(destination.to_string()));
            };
            if !write_object(
                callback,
                region,
                &address.0,
                &address.1,
                &claim.job_id,
                container,
                &bytes,
            )
            .await
            {
                return failed(JobRunFailure::OutputNotWritten(format!(
                    "s3://{}/{}",
                    address.0, address.1
                )));
            }
            // Measured on the file that was WRITTEN, so what the job reports is what the
            // produced output holds.
            let measured = ::tokio::task::spawn_blocking(move || measure_in_barrier(&bytes)).await;
            let Ok(Some(measured)) = measured else {
                return failed(JobRunFailure::NotProducible(
                    "the produced output could not be read back".to_string(),
                ));
            };
            produced.push(OutputRun {
                duration_ms: measured.duration_ms,
                width: measured.frame.map(|(width, _)| width),
                height: measured.frame.map(|(_, height)| height),
            });
        }
        groups.push(OutputGroupRun { outputs: produced });
    }
    produced(groups)
}

/// Why the video a generated input asks for is past what this engine codes, or `None` when
/// it fits: more frames than [`MAX_GENERATED_FRAMES`], more pixels in one frame than
/// [`MAX_GENERATED_FRAME_PIXELS`], or more pixels in the whole video than
/// [`MAX_GENERATED_PIXELS`].
fn past_generated_budget(frame: (i32, i32), frames: u32) -> Option<String> {
    let (width, height) = frame;
    if width <= 0 || height <= 0 {
        return Some(format!(
            "a generated input asks for a frame of {width}x{height}, which is no frame size"
        ));
    }
    if frames > MAX_GENERATED_FRAMES {
        return Some(format!(
            "this engine codes at most {MAX_GENERATED_FRAMES} frames for one output, and \
             this one asks for {frames}"
        ));
    }
    let pixels = u64::from(width.unsigned_abs()).saturating_mul(u64::from(height.unsigned_abs()));
    if pixels > MAX_GENERATED_FRAME_PIXELS {
        return Some(format!(
            "this engine codes at most {MAX_GENERATED_FRAME_PIXELS} pixels in one frame, \
             and {width}x{height} holds {pixels}"
        ));
    }
    let total = pixels.saturating_mul(u64::from(frames));
    (total > MAX_GENERATED_PIXELS).then(|| {
        format!(
            "this engine codes at most {MAX_GENERATED_PIXELS} pixels for one output, and \
             {frames} frames of {width}x{height} hold {total}"
        )
    })
}

/// Code `frames` black frames of `frame` pixels at `rate` frames a second, and write them
/// into an MP4 file carrying that one video track.
///
/// The frames are REAL coded video: each is handed to the maintained H.264 encoder this
/// workspace already renders video with, and the coded stream is framed into the file
/// through the workspace's one H.264/MP4 seam
/// ([`::service_common::h264_mp4::mux_avc`]). Time is counted in the rate's own numerator,
/// so each frame lasts exactly one frame period of the rate the job asked for and the file's
/// length is the frame count at that rate.
///
/// The CODED geometry is the asked one rounded up to an even number of pixels, which is what
/// a macroblock codec takes; the file still STATES the asked geometry, so the output is the
/// size the job asked for.
///
/// `Err` carries what refused, for the job to report: a budget the work did not finish
/// inside, an encoder that could not be started, or a frame it refused.
fn render_black_video(
    frame: (i32, i32),
    rate: (i32, i32),
    frames: u32,
) -> Result<Vec<u8>, String> {
    // Inside a panic barrier, as every other engine this crate drives over caller-chosen
    // sizes is: a codec that trips an assertion on a geometry nobody encodes must leave the
    // job failing with the reason rather than taking the process down.
    ::std::panic::catch_unwind(|| coded_black_video(frame, rate, frames)).unwrap_or_else(
        |panic| {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|message| (*message).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "the coder stopped".to_string());
            Err(format!("the video could not be coded: {detail}"))
        },
    )
}

/// [`render_black_video`]'s own work, inside that barrier.
fn coded_black_video(
    frame: (i32, i32),
    rate: (i32, i32),
    frames: u32,
) -> Result<Vec<u8>, String> {
    let (Ok(width), Ok(height)) = (u32::try_from(frame.0), u32::try_from(frame.1)) else {
        return Err("a generated input asks for a frame size this platform cannot hold".to_string());
    };
    let (coded_width, coded_height) = (width + width % 2, height + height % 2);
    let (Ok(coded_width_pixels), Ok(coded_height_pixels)) =
        (usize::try_from(coded_width), usize::try_from(coded_height))
    else {
        return Err("a generated input asks for a frame size this platform cannot hold".to_string());
    };
    let hz = f64::from(rate.0.max(1)) / f64::from(rate.1.max(1));
    let mut encoder = ::openh264::encoder::Encoder::with_api_config(
        ::openh264::OpenH264API::from_source(),
        ::openh264::encoder::EncoderConfig::new()
            // Constant quality rather than a bitrate budget: what the output carries is the
            // frames the job asked for, whatever they cost.
            .rate_control_mode(::openh264::encoder::RateControlMode::Quality)
            .max_frame_rate(::openh264::encoder::FrameRate::from_hz(hz as f32)),
    )
    .map_err(|error| format!("the H.264 encoder could not be started: {error}"))?;

    // One black frame, coded as many times as the video holds: "MediaConvert creates a video
    // input with black frames".
    let black = ::std::vec![0u8; coded_width_pixels.saturating_mul(coded_height_pixels) * 3];
    let yuv = ::openh264::formats::YUVBuffer::from_rgb8_source(
        ::openh264::formats::RgbSliceU8::new(&black, (coded_width_pixels, coded_height_pixels)),
    );
    let deadline = Instant::now() + GENERATED_RENDER_BUDGET;
    let mut stream: Vec<u8> = Vec::new();
    for _ in 0..frames {
        if Instant::now() >= deadline {
            return Err(
                "the generated video did not finish coding inside this emulator's budget"
                    .to_string(),
            );
        }
        let coded = encoder
            .encode(&yuv)
            .map_err(|error| format!("a frame could not be coded: {error}"))?;
        stream.extend_from_slice(&coded.to_vec());
        if stream.len() > MAX_OUTPUT_BYTES {
            return Err(format!(
                "the generated video is larger than the {MAX_OUTPUT_BYTES} bytes one output \
                 holds"
            ));
        }
    }

    let units = ::service_common::h264_mp4::annex_b_units(&stream);
    let (Some(seq_param_set), Some(pic_param_set)) = (
        ::service_common::h264_mp4::parameter_set(
            &units,
            ::service_common::h264_mp4::SEQUENCE_PARAMETER_SET,
        ),
        ::service_common::h264_mp4::parameter_set(
            &units,
            ::service_common::h264_mp4::PICTURE_PARAMETER_SET,
        ),
    ) else {
        return Err("the coded video carries no parameter sets".to_string());
    };
    // Time is counted in the rate's numerator, so one frame period is its denominator and
    // the file's length is exactly the frame count at the asked rate. A coder that emitted
    // fewer pictures than frames — it may hold or merge one — leaves the last picture on
    // screen for the periods it did not emit, so the file still runs the length the job
    // asked for rather than a length the coder's own buffering decided.
    let period = rate.1.max(1).unsigned_abs();
    let coded = ::service_common::h264_mp4::avcc_samples(&units);
    let Some(last) = coded.len().checked_sub(1) else {
        return Err("the coded video carries no picture".to_string());
    };
    let whole = u64::from(frames).saturating_mul(u64::from(period));
    let before = (last as u64).saturating_mul(u64::from(period));
    let remaining = u32::try_from(whole.saturating_sub(before)).unwrap_or(period);
    let timed: Vec<::service_common::h264_mp4::TimedSample> = coded
        .into_iter()
        .enumerate()
        .map(|(position, sample)| ::service_common::h264_mp4::TimedSample {
            sample,
            duration: if position == last {
                remaining.max(period)
            } else {
                period
            },
        })
        .collect();
    let track = ::service_common::h264_mp4::VideoTrack {
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
        timescale: rate.0.max(1).unsigned_abs(),
        seq_param_set,
        pic_param_set,
    };
    ::service_common::h264_mp4::mux_avc(&track, &timed)
}

/// The frame size one output is written at: `asked` is what the output's own video
/// description states, and `input` the frame size of the video it is produced from.
///
/// `VideoDescription$Width`'s own documentation is the rule: "To use the same resolution as
/// your input: Leave both Width and Height blank. To evenly scale from your input
/// resolution: Leave Width blank and enter a value for Height. For example, if your input is
/// 1920x1080 and you set Height to 720, your output will be 1280x720." The scaled dimension
/// is an even number of pixels, as the members' own ranges are stated in ("Enter an even
/// integer").
///
/// `None` when the video it is produced from has no frame size at all — media carrying no
/// video — because then there is no video stream in the output to state one for.
fn resolved_frame(
    asked: (Option<i32>, Option<i32>),
    input: Option<(i32, i32)>,
) -> Option<(i32, i32)> {
    let (input_width, input_height) = input?;
    let scaled = |from: i32, numerator: i32, denominator: i32| {
        let scaled = i64::from(from) * i64::from(numerator) / i64::from(denominator.max(1));
        let scaled = i32::try_from(scaled).unwrap_or(from).max(2);
        scaled - scaled % 2
    };
    Some(match asked {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, scaled(width, input_height, input_width)),
        (None, Some(height)) => (scaled(height, input_width, input_height), height),
        (None, None) => (input_width, input_height),
    })
}

/// What one media file IS, as its own container states it.
#[derive(Debug, Clone, Copy, Default)]
struct MeasuredMedia {
    /// How long it runs, in milliseconds.
    duration_ms: Option<i32>,
    /// The frame size of its video, or `None` for media carrying no video track.
    frame: Option<(i32, i32)>,
}

/// Whether `container` names an ISO base media container, which is what this engine writes
/// (`ContainerSettings$Container`'s `MP4` and `MOV` values).
fn is_iso_bmff_container(container: &str) -> bool {
    matches!(container.trim().to_ascii_uppercase().as_str(), "MP4" | "MOV")
}

/// [`remux`] inside a panic barrier, as every read of a caller's container in this crate is:
/// a demuxer that trips an assertion on a file nobody wrote by the book must leave the job
/// failing rather than take the process down.
fn remux_in_barrier(media: &[u8], frame: Option<(i32, i32)>) -> Option<Vec<u8>> {
    ::std::panic::catch_unwind(|| remux(media, frame))
        .ok()
        .flatten()
}

/// [`measure`] inside the same barrier, for the same reason.
fn measure_in_barrier(media: &[u8]) -> Option<MeasuredMedia> {
    ::std::panic::catch_unwind(|| measure(media))
        .ok()
        .flatten()
}

/// Write the tracks of the ISO base media file `media` into a new one, carrying every sample
/// over in order.
///
/// What is carried over is the tracks the output CONTAINS: a MediaConvert output is made of
/// the video and audio the job describes, so a track this engine cannot write a sample entry
/// for — a timecode or metadata track an authoring tool left in the input among them — is
/// left out of the output rather than stopping it. A file no track at all could be carried
/// from is unproducible: the essence is copied rather than re-encoded, so there would be
/// nothing in the output.
fn remux(media: &[u8], frame: Option<(i32, i32)>) -> Option<Vec<u8>> {
    let size = u64::try_from(media.len()).ok()?;
    let mut reader = ::mp4::Mp4Reader::read_header(Cursor::new(media), size).ok()?;
    let config = ::mp4::Mp4Config {
        major_brand: *reader.major_brand(),
        minor_version: reader.minor_version(),
        compatible_brands: reader.compatible_brands().to_vec(),
        timescale: reader.timescale(),
    };
    let mut ids: Vec<u32> = reader.tracks().keys().copied().collect();
    ids.sort_unstable();
    let tracks: Vec<(u32, ::mp4::TrackConfig)> = ids
        .iter()
        .filter_map(|id| {
            let track = reader.tracks().get(id)?;
            Some((*id, track_config(track, frame)?))
        })
        .collect();
    if tracks.is_empty() {
        return None;
    }
    let mut writer = ::mp4::Mp4Writer::write_start(Cursor::new(Vec::new()), &config).ok()?;
    for (_, config) in &tracks {
        writer.add_track(config).ok()?;
    }
    for (position, (id, _)) in tracks.iter().enumerate() {
        // The writer numbers the tracks it was given in the order it was given them, which
        // is not the input's numbering once a track has been left out: the samples of the
        // input track `id` are written onto the track that was added for it.
        let written = u32::try_from(position + 1).ok()?;
        let samples = reader.sample_count(*id).ok()?;
        let mut carried = 0usize;
        for index in 1..=samples {
            match reader.read_sample(*id, index) {
                Ok(Some(sample)) => {
                    // How much the written file holds is driven by the input's own sample
                    // TABLE, which may index the same bytes over and over, so the bytes
                    // carried are counted and the file is abandoned at the ceiling rather
                    // than grown to whatever that table asks for.
                    carried = carried.saturating_add(sample.bytes.len());
                    if carried > MAX_OUTPUT_BYTES {
                        return None;
                    }
                    writer.write_sample(written, &sample).ok()?;
                }
                // The input's essence ends here — a sample its container indexes but its
                // bytes do not carry — so the written track carries what the file really
                // holds rather than the run failing over the part that is not there.
                Ok(None) | Err(_) => break,
            }
        }
    }
    writer.write_end().ok()?;
    Some(writer.into_writer().into_inner())
}

/// The written track that carries `track`'s samples: the same kind of track, in the same
/// timescale, under the same language, with the sample entry its codec needs.
///
/// A video track is written at `frame`, the frame size the output RESOLVED to
/// ([`resolved_frame`]), so the file the run writes declares the geometry the output states.
///
/// `None` for a track whose codec this engine cannot write a sample entry for.
fn track_config(
    track: &::mp4::Mp4Track,
    frame: Option<(i32, i32)>,
) -> Option<::mp4::TrackConfig> {
    let pixels = |asked: Option<i32>, own: u16| {
        asked
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(own)
    };
    let width = pixels(frame.map(|(width, _)| width), track.width());
    let height = pixels(frame.map(|(_, height)| height), track.height());
    let media_conf = match track.media_type().ok()? {
        ::mp4::MediaType::H264 => ::mp4::MediaConfig::AvcConfig(::mp4::AvcConfig {
            width,
            height,
            seq_param_set: track.sequence_parameter_set().ok()?.to_vec(),
            pic_param_set: track.picture_parameter_set().ok()?.to_vec(),
        }),
        ::mp4::MediaType::H265 => {
            ::mp4::MediaConfig::HevcConfig(::mp4::HevcConfig { width, height })
        }
        ::mp4::MediaType::VP9 => {
            ::mp4::MediaConfig::Vp9Config(::mp4::Vp9Config { width, height })
        }
        ::mp4::MediaType::AAC => ::mp4::MediaConfig::AacConfig(::mp4::AacConfig {
            bitrate: track.bitrate(),
            profile: track.audio_profile().ok()?,
            freq_index: track.sample_freq_index().ok()?,
            chan_conf: track.channel_config().ok()?,
        }),
        _ => return None,
    };
    Some(::mp4::TrackConfig {
        track_type: track.track_type().ok()?,
        timescale: track.timescale(),
        language: track.language().to_string(),
        media_conf,
    })
}

/// What one media file IS: how long it runs, and the frame size of its video.
///
/// Read off the file's own bytes, with the same demuxer `Probe` reports a file with, so what
/// an output states is measured on the media it was produced from: a MediaConvert output runs
/// for as long as the media it was made from. Media with no video track measures no frame
/// size at all, and an output produced from it carries no video stream to state one for.
///
/// `None` for bytes this demuxer cannot read a container out of: nothing is established about
/// such a file, so nothing is reported for an output made from it.
fn measure(media: &[u8]) -> Option<MeasuredMedia> {
    let size = u64::try_from(media.len()).ok()?;
    let read = ::mp4::Mp4Reader::read_header(Cursor::new(media), size).ok()?;
    let video = read
        .tracks()
        .values()
        .find(|track| matches!(track.track_type(), Ok(::mp4::TrackType::Video)));
    Some(MeasuredMedia {
        duration_ms: i32::try_from(read.duration().as_millis()).ok(),
        frame: video
            .map(|track| (i32::from(track.width()), i32::from(track.height())))
            .filter(|(width, height)| *width > 0 && *height > 0),
    })
}

/// The user-metadata key MediaConvert records the producing job's id under on each output
/// object it writes, as the object's own `x-amz-meta-` header carries it.
const JOB_ID_METADATA: &str = "mediaconvert-jobid";

/// The media type of the container one output is written in, from the IANA media type
/// registry (`video/mp4`) and, for the container IANA registers no type for, the type its
/// vendor publishes (`video/quicktime`) — the same reading `Probe` reports a file's own
/// media type by.
fn output_media_type(container: &str) -> Option<&'static str> {
    match container.trim().to_ascii_uppercase().as_str() {
        "MP4" => Some("video/mp4"),
        "MOV" => Some("video/quicktime"),
        _ => None,
    }
}

/// The file name one output is written under, as MediaConvert builds it: the base name the
/// group's destination states, the output's `NameModifier`, and the extension.
///
/// `FileGroupSettings$Destination` is "the S3 bucket and path where you want MediaConvert to
/// write your outputs", and a destination that ends in `/` names a PLACE rather than a base
/// name — in which case the base name is the input file's own, without its extension. The
/// extension is the output's own `Extension` when it states one, and the container's
/// otherwise.
fn output_file_name(
    destination: &str,
    input_key: &str,
    output: &PlannedOutput,
    container: &str,
) -> String {
    let base = match destination.rsplit('/').next().unwrap_or_default() {
        // The destination names a place, so the output is named after the input file.
        "" => input_key
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .rsplit_once('.')
            .map_or_else(
                || input_key.rsplit('/').next().unwrap_or_default().to_string(),
                |(stem, _)| stem.to_string(),
            ),
        stated => stated.to_string(),
    };
    let modifier = output.name_modifier.as_deref().unwrap_or_default();
    let extension = output
        .extension
        .as_deref()
        .map(|extension| extension.trim_start_matches('.').to_string())
        .unwrap_or_else(|| container.trim().to_ascii_lowercase());
    format!("{base}{modifier}.{extension}")
}

/// The bucket and key one produced file is written at: the group's destination with `name`
/// appended to the prefix it states.
fn object_address(destination: &str, name: &str) -> Option<(String, String)> {
    let (bucket, prefix) = match ::service_common::s3_uri::object_address(destination) {
        Some((bucket, key)) => (bucket, key),
        // A destination that names a bucket and nothing under it writes at its root.
        None => (
            ::service_common::s3_uri::bucket_name(destination)?,
            String::new(),
        ),
    };
    // A destination naming a base name contributes its own directory, not its base name,
    // because the base name is already part of `name`.
    let directory = match prefix.rsplit_once('/') {
        Some((directory, _)) => format!("{directory}/"),
        None => String::new(),
    };
    Some((bucket, format!("{directory}{name}")))
}

/// Read the object at `bucket`/`key`, through the ONE in-process read of an Amazon S3
/// object every emulated service makes ([`::service_common::s3_object_read::read_object`]),
/// buffering at most [`MAX_INPUT_BYTES`] of it.
///
/// That shared read is also the one classification of what the store's answer ESTABLISHED:
/// the store saying it holds nothing at that address, the store saying the read is not
/// permitted, an object no read can buffer, or nothing having answered at all — which is
/// never the job's fault. Nothing is re-stated here; the result is the shared enum.
async fn read_object(
    callback: &EmbeddedEmulator,
    region: &str,
    bucket: &str,
    key: &str,
) -> ::service_common::s3_object_read::ObjectRead {
    ::service_common::s3_object_read::read_object(
        callback,
        region,
        bucket,
        key,
        None,
        MAX_INPUT_BYTES,
    )
    .await
}

/// Write one produced output file into the destination its group names.
///
/// The object carries what MediaConvert states about an output it wrote: the media type of
/// the container it is in, and the id of the JOB that produced it, which the service records
/// as the object's own user metadata ([`JOB_ID_METADATA`]) so a consumer reading the
/// destination can tell which job each file came from.
///
/// `false` when the object store did not take it, which is a job with no output rather than
/// a completed one.
async fn write_object(
    callback: &EmbeddedEmulator,
    region: &str,
    bucket: &str,
    key: &str,
    job_id: &str,
    container: &str,
    bytes: &[u8],
) -> bool {
    let mut written = ::service_common::s3_client::s3_client(callback, region)
        .put_object()
        .bucket(bucket)
        .key(key)
        .metadata(JOB_ID_METADATA, job_id)
        .body(::aws_sdk_s3::primitives::ByteStream::from(bytes.to_vec()));
    if let Some(media_type) = output_media_type(container) {
        written = written.content_type(media_type);
    }
    match written.send().await {
        Ok(_) => true,
        Err(error) => {
            ::tracing::warn!(
                bucket = %bucket,
                key = %key,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a job's output file could not be written to its destination"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a destination ending in `/` names a place, so the output is named after the
    /// input file; one that ends in a base name uses that base name. The `NameModifier` is
    /// appended to the base name, and the extension is the output's own or the container's.
    #[test]
    fn an_output_is_named_from_its_destination_modifier_and_container() {
        let output = |modifier: Option<&str>, extension: Option<&str>| PlannedOutput {
            container: Some("MP4".to_string()),
            extension: extension.map(str::to_string),
            name_modifier: modifier.map(str::to_string),
            width: None,
            height: None,
            framerate: None,
        };
        let cases = [
            ("s3://b/out/", "in/clip.mp4", None, None, "clip.mp4"),
            (
                "s3://b/out/",
                "in/clip.mp4",
                Some("_720"),
                None,
                "clip_720.mp4",
            ),
            ("s3://b/out/movie", "in/clip.mp4", None, None, "movie.mp4"),
            (
                "s3://b/out/movie",
                "clip.mov",
                Some("-hd"),
                Some(".m4v"),
                "movie-hd.m4v",
            ),
            ("s3://b/out/", "clip", None, Some("mp4"), "clip.mp4"),
        ];
        for (destination, key, modifier, extension, expected) in cases {
            let planned = output(modifier, extension);
            let name = output_file_name(destination, key, &planned, "MP4");
            assert_eq!(
                name, expected,
                "case {destination} / {key} / {modifier:?} / {extension:?}: expected \
                 {expected:?}, got {name:?}"
            );
        }
    }

    /// RULE: a produced file is written under the destination's own prefix, whether that
    /// destination names a place or a base name.
    #[test]
    fn a_produced_file_is_addressed_under_its_destination_prefix() {
        let cases = [
            ("s3://bucket/out/", "clip.mp4", Some(("bucket", "out/clip.mp4"))),
            (
                "s3://bucket/out/movie",
                "movie.mp4",
                Some(("bucket", "out/movie.mp4")),
            ),
            ("s3://bucket/movie", "movie.mp4", Some(("bucket", "movie.mp4"))),
            ("s3://bucket/", "clip.mp4", Some(("bucket", "clip.mp4"))),
            ("not-a-uri", "clip.mp4", None),
        ];
        for (destination, name, expected) in cases {
            let address = object_address(destination, name);
            let reported = address
                .as_ref()
                .map(|(bucket, key)| (bucket.as_str(), key.as_str()));
            assert_eq!(
                reported, expected,
                "case {destination} / {name}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: this engine writes ISO base media containers, so those are the
    /// `ContainerSettings$Container` values a run can produce.
    #[test]
    fn only_the_iso_base_media_containers_are_producible() {
        for container in ["MP4", "mp4", "MOV", " mov "] {
            assert!(
                is_iso_bmff_container(container),
                "case {container:?}: an ISO base media container is producible"
            );
        }
        for container in ["M3U8", "MXF", "WEBM", "RAW", ""] {
            assert!(
                !is_iso_bmff_container(container),
                "case {container:?}: this engine writes no such container"
            );
        }
    }

    /// RULE (`VideoDescription$Width`): an output that states both dimensions is written at
    /// them; one that states neither keeps the frame size of the video it is produced from;
    /// one that states a single dimension scales the other evenly from that video's own
    /// aspect ratio — the member's own example is an input of 1920x1080 with Height 720
    /// becoming 1280x720 — and the scaled dimension is an even number of pixels. Video with
    /// no frame size at all resolves to none, because such an output has no video stream.
    #[test]
    fn an_output_frame_size_is_resolved_against_the_video_it_is_produced_from() {
        /// One row: what the output states, the frame size of the video it is produced
        /// from, and the frame size the output is written at.
        type Case = ((Option<i32>, Option<i32>), Option<(i32, i32)>, Option<(i32, i32)>);
        let cases: [Case; 8] = [
            ((None, None), Some((1920, 1080)), Some((1920, 1080))),
            ((Some(1280), Some(720)), Some((1920, 1080)), Some((1280, 720))),
            // The member's own documented example.
            ((None, Some(720)), Some((1920, 1080)), Some((1280, 720))),
            ((Some(960), None), Some((1920, 1080)), Some((960, 540))),
            // A ratio that does not divide evenly is held to an even number of pixels.
            ((None, Some(240)), Some((640, 482)), Some((318, 240))),
            ((None, None), Some((540, 360)), Some((540, 360))),
            ((Some(1280), Some(720)), None, None),
            ((None, None), None, None),
        ];
        for (asked, input, expected) in cases {
            let resolved = resolved_frame(asked, input);
            assert_eq!(
                resolved, expected,
                "case asked={asked:?} from {input:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// RULE: an input states a file or the video the service generates, and never both
    /// ("You can specify Video generator, or you can specify an Input file, but you cannot
    /// specify both"); an input that states neither is nothing to transcode.
    #[test]
    fn an_input_is_the_file_it_names_or_the_video_it_asks_the_service_to_generate() {
        let generator = || {
            Some(crate::ops::types::InputVideoGenerator {
                duration: Some(2_000),
                ..crate::ops::types::InputVideoGenerator::default()
            })
        };
        let cases: [(&str, Option<String>, bool, Option<&str>); 5] = [
            ("a file input", Some("s3://b/in.mp4".to_string()), false, Some("file")),
            ("a generated input", None, true, Some("generated")),
            ("neither", None, false, None),
            ("a file input that carries no characters", Some("  ".to_string()), false, None),
            // Both stated is a request the model forbids; the file is what the input NAMES,
            // so that is what is read.
            (
                "both",
                Some("s3://b/in.mp4".to_string()),
                true,
                Some("file"),
            ),
        ];
        for (case, file_input, generated, expected) in cases {
            let input = Input {
                file_input,
                video_generator: generated.then(generator).flatten(),
                ..Input::default()
            };
            let planned = planned_input(&input).map(|planned| match planned {
                PlannedInput::File(_) => "file",
                PlannedInput::Generated(_) => "generated",
            });
            assert_eq!(
                planned, expected,
                "case {case:?}: expected {expected:?}, got {planned:?}"
            );
        }
    }
}
