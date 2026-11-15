// SPDX-License-Identifier: CC0-1.0

//! Rendering a capture source's stored video at the OUTPUT FORMAT a read asked for.
//!
//! # The rule this module implements
//!
//! `GetCaptureData` carries the output format configuration for video processing: a target
//! frame rate, a target width and a target height. They describe the OUTPUT, not the
//! selection — which segments overlap the requested window is the window's business — so a
//! read that names one is answered with the stored media PROCESSED to it: pictures shown
//! at the asked rate, each the asked number of pixels across, in the same container type
//! and over the same span the segment covers. A target the media already meets, and
//! settings naming no target at all, ask for no processing and are answered with the stored
//! media itself.
//!
//! # How it is really done
//!
//! The pictures are really decoded and really re-encoded, in this process, by the
//! maintained H.264 codec this workspace already encodes video with: the stored container
//! is read with the maintained ISO base media reader, each coded picture is decoded,
//! scaled to the asked geometry by the maintained image resampler, and encoded again into
//! the file the read answers with ([`::service_common::h264_mp4`] frames the stream into
//! that file). Nothing is synthesised: every picture in the answer is a picture the
//! producer's own media carried, and the only thing this module decides is which of them a
//! moment of the output shows and how large it is drawn.
//!
//! The output's timeline is the SOURCE's: the picture at a moment of the answer is the
//! picture the stored media displays at that moment, so lowering the rate drops the
//! pictures the asked rate does not sample and raising it shows one picture for the several
//! moments that fall inside its own display interval. The span is therefore unchanged,
//! which is what lets a read report the segment's own start and end for the media it
//! answers with.
//!
//! Media this emulator cannot take pictures out of at all — a container with no coded video
//! track, or one coded in something other than the codec above — is answered as stored:
//! there are no pictures to re-render, and no frame is invented for media that carries none.
//!
//! Media that DOES carry pictures and cannot be served is REFUSED instead, for the same
//! reason an output past the ceilings is: answering it with the stored bytes would report
//! the asked format as applied where nothing applied it. The work one read may do is bounded
//! on both sides — the pictures the stored media declares and their geometry
//! ([`past_decode_budget`]), and the frames the output carries, the pixels of each and the
//! product of the two ([`past_budget`]) — and each ceiling names itself in the refusal.
//!
//! One thing the codec decides rather than the request: a coded picture covers an even
//! number of pixels in each direction and at least one whole macroblock, so a geometry asked
//! outside that is CODED at the one that carries it while the file states the asked geometry
//! as the output's own ([`coded_side`]).

use ::std::io::Cursor;
use ::std::panic::AssertUnwindSafe;

use ::openh264::formats::YUVSource;
use ::service_common::h264_mp4;

/// How many nanoseconds one second holds — what a sample time stated in a media timescale
/// and an output time stated in frames per second are both converted against, so the two
/// timelines are compared in one unit.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// The most frames one read renders.
///
/// A rendering bound, not an AWS quota: the asked rate times the span the stored media
/// covers is what decides how many pictures an answer carries, and the span is the
/// producer's, so without a ceiling one request could ask this process to encode without
/// limit.
pub const MAX_RENDERED_FRAMES: u32 = 600;

/// The most pixels one rendered frame holds: a 3840 by 2160 frame, the largest geometry
/// this emulator draws a picture at.
pub const MAX_RENDERED_FRAME_PIXELS: u64 = 3_840 * 2_160;

/// The largest side one rendered frame has, in pixels.
///
/// A file states each side of its video in sixteen bits (the container's track header and
/// visual sample entry), so a side past this could not be STATED by the answer: the read
/// is refused rather than answered with a geometry nobody asked for.
pub const MAX_RENDERED_SIDE: u32 = u16::MAX as u32;

/// The most pixels one read renders in total — the frames it carries times the pixels each
/// of them holds. The product, and neither factor alone, is what one request can make this
/// process encode.
pub const MAX_RENDERED_PIXELS: u64 = 64_000_000;

/// How long one read may spend rendering. The ceilings above bound the WORK; this bounds
/// the WAIT, which is what a read waiting on a blocking task needs in order to have
/// somewhere to land when it is cancelled or the server is shutting down, and what the
/// render itself stops at so no thread goes on working for an answer nobody will receive.
/// A backstop for a host far slower than any this emulator is developed on, not a budget a
/// request inside the ceilings above is meant to reach.
pub const MAX_RENDER_TIME: ::std::time::Duration = ::std::time::Duration::from_secs(15);

/// How many reads may be RENDERING at once in this process.
///
/// The ceilings above bound what ONE read may do; this bounds how many of them may be doing
/// it together, because the work runs on a blocking thread that a read which stopped
/// waiting cannot take back. Past this a read is refused the rate limit its own model
/// declares rather than queued behind renders nobody is waiting for.
pub const MAX_CONCURRENT_RENDERS: usize = 4;

/// How many renders are running right now.
static RENDERS_IN_FLIGHT: ::std::sync::atomic::AtomicUsize =
    ::std::sync::atomic::AtomicUsize::new(0);

/// The right to render, held for as long as one render runs and given back when it ends.
///
/// MOVE it into the work it permits: the permit is returned when the rendering really
/// finishes, not when whoever was waiting for it stopped waiting, which is what keeps the
/// count honest about the threads that are actually busy.
#[derive(Debug)]
pub struct RenderPermit;

impl RenderPermit {
    /// Take a permit, or `None` when [`MAX_CONCURRENT_RENDERS`] renders are already running.
    pub fn take() -> Option<Self> {
        let mut held = RENDERS_IN_FLIGHT.load(::std::sync::atomic::Ordering::Relaxed);
        loop {
            if held >= MAX_CONCURRENT_RENDERS {
                return None;
            }
            match RENDERS_IN_FLIGHT.compare_exchange_weak(
                held,
                held.saturating_add(1),
                ::std::sync::atomic::Ordering::AcqRel,
                ::std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(current) => held = current,
            }
        }
    }
}

impl Drop for RenderPermit {
    fn drop(&mut self) {
        RENDERS_IN_FLIGHT.fetch_sub(1, ::std::sync::atomic::Ordering::AcqRel);
    }
}

/// The output format one read asked for: each target its format settings named, and
/// nothing for one they left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputFormat {
    /// The target frame rate for the output.
    pub frames_per_second: Option<u32>,
    /// The target width of the output, in pixels.
    pub width_in_pixels: Option<u32>,
    /// The target height of the output, in pixels.
    pub height_in_pixels: Option<u32>,
}

impl OutputFormat {
    /// Whether the settings name no target at all, which asks for no processing.
    pub fn names_no_target(&self) -> bool {
        self.frames_per_second.is_none()
            && self.width_in_pixels.is_none()
            && self.height_in_pixels.is_none()
    }

    /// The refusal message for an ask no stored media could make this emulator render, or
    /// `None` when the ask itself fits.
    ///
    /// Judged on the REQUEST ALONE, so a read past a ceiling is refused before any
    /// rendering is started and before a buffer is sized from one of its numbers: the rate
    /// it asks for, each side it names, and — when it names both — the pixels one frame
    /// holds at the geometry those sides are coded at. The ceilings that need the media
    /// (how many frames the rate asks for over the segment's span, and the pixels that many
    /// frames hold) are applied by [`past_budget`] once the container has been read.
    pub fn past_asked_budget(&self) -> Option<String> {
        if let Some(rate) = self.frames_per_second {
            if rate > MAX_RENDERED_FRAMES {
                return Some(format!(
                    "This emulator renders at most {MAX_RENDERED_FRAMES} frames for each request, and a rate of {rate} frames per second asks for more than that in one second."
                ));
            }
        }
        for side in [self.width_in_pixels, self.height_in_pixels]
            .into_iter()
            .flatten()
        {
            if side > MAX_RENDERED_SIDE {
                return Some(format!(
                    "This emulator renders at most {MAX_RENDERED_SIDE} pixels in each direction, and {side} is past that."
                ));
            }
        }
        // One side alone says nothing about the pixels of a frame: the other is the
        // media's, and is judged with it.
        let (Some(width), Some(height)) = (self.width_in_pixels, self.height_in_pixels) else {
            return None;
        };
        past_frame_budget(width, height)
    }
}

/// What a read answers one stored segment with, once the asked output format has been
/// applied to it.
#[derive(Debug)]
pub enum CaptureRendering {
    /// The media re-rendered at the asked format, as the MP4 file to answer with.
    Rendered(Vec<u8>),
    /// The stored media itself: the asked format names no target, is the format the media
    /// is already in, or the media carries no coded picture this emulator can re-render.
    AsStored,
    /// The output the format asks for is past what one read renders. Carries the refusal's
    /// message.
    PastBudget(String),
    /// The pictures were there and the rendering failed anyway. Carries what failed.
    Failed(String),
}

/// Render `media` — one stored segment's bytes — at the output format `format` asks for.
///
/// The bytes are a PRODUCER's, read out of an object store, so the whole read of the
/// container happens inside a panic barrier: a reader that trips an assertion on a
/// container nobody wrote by the book would otherwise take the request with it, and media
/// no picture can be taken out of is already media this emulator answers as stored.
pub fn render_capture(media: &[u8], format: &OutputFormat) -> CaptureRendering {
    if format.names_no_target() {
        return CaptureRendering::AsStored;
    }
    ::std::panic::catch_unwind(AssertUnwindSafe(|| rendered(media, format)))
        .unwrap_or(CaptureRendering::AsStored)
}

/// The coded video track of a stored container, as everything this module needs off it.
#[derive(Debug, Clone)]
struct SourceTrack {
    /// Which track of the file it is.
    id: u32,
    /// How many units of time one second of its timeline holds.
    timescale: u32,
    /// How many coded pictures it carries.
    sample_count: u32,
    /// How long it runs.
    duration: ::std::time::Duration,
    /// How many pictures it shows per second.
    frames_per_second: u32,
    /// The width the track states, in pixels.
    width: u32,
    /// The height it states.
    height: u32,
    /// The sequence parameter set its pictures are decoded against.
    seq_param_set: Vec<u8>,
    /// Its picture parameter set.
    pic_param_set: Vec<u8>,
}

/// How wide the length field framing each unit of a sample is, in bytes.
///
/// The configuration record states it as `lengthSizeMinusOne`; four bytes is what every
/// writer of this format — the one this module answers with among them — frames a unit
/// with, and the reader this module reads a container through exposes no narrower value.
const AVCC_LENGTH_SIZE: usize = 4;

/// The rendering, inside the panic barrier [`render_capture`] puts around it.
fn rendered(media: &[u8], format: &OutputFormat) -> CaptureRendering {
    let Ok(size) = u64::try_from(media.len()) else {
        return CaptureRendering::AsStored;
    };
    let Ok(mut reader) = ::mp4::Mp4Reader::read_header(Cursor::new(media), size) else {
        return CaptureRendering::AsStored;
    };
    let Some(source) = coded_video_track(&reader) else {
        return CaptureRendering::AsStored;
    };

    // The media CARRIES pictures this emulator decodes, so a read of it that cannot be
    // served is REFUSED and not answered with the media unprocessed: an answer carrying
    // the stored bytes would report the asked format as applied when nothing applied it.
    // The as-stored answer is for media no picture can be taken out of at all.
    if let Some(message) = past_decode_budget(&source) {
        return CaptureRendering::PastBudget(message);
    }

    // Every target the settings leave out is the media's OWN: a read that asks for a
    // smaller picture and says nothing about the rate asks for that picture at the rate the
    // media already shows.
    let frames_per_second = format
        .frames_per_second
        .unwrap_or(source.frames_per_second)
        .max(1);
    let width = format.width_in_pixels.unwrap_or(source.width).max(1);
    let height = format.height_in_pixels.unwrap_or(source.height).max(1);
    if (frames_per_second, width, height) == (source.frames_per_second, source.width, source.height)
    {
        return CaptureRendering::AsStored;
    }

    let frames = output_frames(source.duration, frames_per_second);
    let target = Target::new(frames_per_second, width, height, frames);
    if let Some(message) = past_budget(&target) {
        return CaptureRendering::PastBudget(message);
    }

    // The work is bounded by the ceilings above, so this is a backstop for a host that
    // renders far slower than any this emulator is developed on: the blocking task that is
    // doing it stops at the same instant the read waiting on it stops waiting, rather than
    // going on burning a thread for an answer nobody will receive.
    let deadline = ::std::time::Instant::now().checked_add(MAX_RENDER_TIME);
    let stream = match encode_rendered_frames(&mut reader, &source, &target, deadline) {
        Ok(stream) => stream,
        Err(failure) => return failure,
    };
    mux_rendered(&stream, &target)
}

/// The geometry, the rate and the length of the output one read asked for.
#[derive(Debug, Clone, Copy)]
struct Target {
    frames_per_second: u32,
    /// The width the output STATES, which is the one the read asked for.
    width: u32,
    /// Its height, for the same reason.
    height: u32,
    /// The width the pictures are CODED at: [`Self::width`] rounded up to the whole
    /// pixels a coded picture is made of.
    coded_width: u32,
    /// Their coded height, for the same reason.
    coded_height: u32,
    /// How many pictures the output carries over the span the media covers.
    frames: u32,
}

/// How many pixels wide and tall one coded picture's chroma is sampled in: the codec
/// samples colour in pairs, so a coded picture covers an even number of pixels in each
/// direction and a geometry asked in an odd number is CODED at the even one that carries
/// it while the file states the asked geometry as the output's own.
const CHROMA_SAMPLING: u32 = 2;

/// The side of one coded macroblock, which is the smallest picture the codec codes: a
/// geometry asked smaller than this is coded at one whole macroblock and drawn into the
/// whole of it, with the file stating the asked geometry as the output's own.
const MACROBLOCK_SIDE: u32 = 16;

impl Target {
    /// The target an asked rate, geometry and length make.
    fn new(frames_per_second: u32, width: u32, height: u32, frames: u32) -> Self {
        Self {
            frames_per_second,
            width,
            height,
            coded_width: coded_side(width),
            coded_height: coded_side(height),
            frames,
        }
    }
}

/// Whether a picture of `width` by `height` is one this emulator draws at all:
/// [`MAX_RENDERED_FRAME_PIXELS`] bounds every buffer sized from a geometry, whether the
/// geometry came from the request or from the producer's own media.
fn within_frame_budget(width: u32, height: u32) -> bool {
    u64::from(width).saturating_mul(u64::from(height)) <= MAX_RENDERED_FRAME_PIXELS
}

/// The refusal message for stored media past what one read DECODES, or `None` when it
/// fits.
///
/// The pictures a read decodes are bounded exactly as the ones it draws, and for the same
/// reason: how many a container declares and how large each is are a PRODUCER's, so
/// without these ceilings one read could be made to decode without limit — and the loop
/// that reads the samples is bounded by the declared count, which is why the count is
/// checked here, before a sample of it is read. Media past a ceiling is REFUSED: it does
/// carry pictures this emulator decodes, so answering it with the stored bytes would report
/// the asked format as applied to media nothing processed.
fn past_decode_budget(source: &SourceTrack) -> Option<String> {
    let frames = source.sample_count;
    let width = source.width;
    let height = source.height;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if frames > MAX_RENDERED_FRAMES {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_FRAMES} frames for each request, and the stored media carries {frames} pictures over this span."
        ));
    }
    if pixels > MAX_RENDERED_FRAME_PIXELS {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_FRAME_PIXELS} pixels in each frame, and the stored media's {width}x{height} holds {pixels}."
        ));
    }
    let total = pixels.saturating_mul(u64::from(frames));
    if total > MAX_RENDERED_PIXELS {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_PIXELS} pixels for each request, and the stored media's {frames} pictures of {width}x{height} hold {total}."
        ));
    }
    None
}

/// The side a picture of `asked` pixels is CODED at: the even number of pixels that
/// carries it, and never less than one whole macroblock.
fn coded_side(asked: u32) -> u32 {
    asked
        .div_ceil(CHROMA_SAMPLING)
        .saturating_mul(CHROMA_SAMPLING)
        .max(MACROBLOCK_SIDE)
}

/// The track the pictures come out of: the first video track coded in the codec this
/// module decodes, or `None` for a container carrying none — which is media no picture can
/// be taken out of, and the one thing a read of it can answer with is the media itself.
fn coded_video_track(reader: &::mp4::Mp4Reader<Cursor<&[u8]>>) -> Option<SourceTrack> {
    reader.tracks().values().find_map(|track| {
        if track.track_type().ok()? != ::mp4::TrackType::Video {
            return None;
        }
        if track.media_type().ok()? != ::mp4::MediaType::H264 {
            return None;
        }
        let seq_param_set = track.sequence_parameter_set().ok()?.to_vec();
        let pic_param_set = track.picture_parameter_set().ok()?.to_vec();
        let rate = track.frame_rate();
        if !rate.is_finite() || rate <= 0.0 {
            return None;
        }
        Some(SourceTrack {
            id: track.track_id(),
            timescale: track.timescale(),
            sample_count: track.sample_count(),
            duration: track.duration(),
            // The rate a track SHOWS pictures at is what a read naming no rate asks for,
            // rounded to a whole number of frames per second, which is the unit the
            // requested member states one in.
            frames_per_second: (rate.round() as u64).clamp(1, u64::from(u32::MAX)) as u32,
            width: u32::from(track.width()),
            height: u32::from(track.height()),
            seq_param_set,
            pic_param_set,
        })
    })
}

/// How many pictures an output at `frames_per_second` carries over `duration`: one for
/// every moment of the span the rate samples, so the output covers the same span the
/// stored media does.
fn output_frames(duration: ::std::time::Duration, frames_per_second: u32) -> u32 {
    let nanos = duration.as_nanos();
    let frames = nanos
        .saturating_mul(u128::from(frames_per_second))
        .div_ceil(NANOS_PER_SECOND);
    u32::try_from(frames.max(1)).unwrap_or(u32::MAX)
}

/// The moment output frame `index` shows, in nanoseconds from the start of the span.
fn output_moment(index: u32, frames_per_second: u32) -> u128 {
    u128::from(index)
        .saturating_mul(NANOS_PER_SECOND)
        .checked_div(u128::from(frames_per_second.max(1)))
        .unwrap_or(0)
}

/// That same conversion for a media timeline: `units` of a `timescale`-per-second clock, in
/// nanoseconds.
fn media_moment(units: u64, timescale: u32) -> u128 {
    u128::from(units)
        .saturating_mul(NANOS_PER_SECOND)
        .checked_div(u128::from(timescale.max(1)))
        .unwrap_or(0)
}

/// The refusal message for an output past what one read renders, or `None` when it fits.
///
/// The ceilings are measured on the geometry the pictures are really CODED at, which is
/// what every buffer of the rendering is sized from — a side asked below one macroblock is
/// drawn into a whole one, so measuring the asked side would account for less work than
/// the request actually makes. The message carries the asked geometry, and the coded one
/// beside it where the two differ, so a caller can see what it was measured on. The three
/// ceilings are stated separately so the refusal says WHICH one the request is past.
fn past_budget(target: &Target) -> Option<String> {
    let frames = target.frames;
    let width = target.width;
    let height = target.height;
    let pixels = u64::from(target.coded_width).saturating_mul(u64::from(target.coded_height));
    // What the geometry is DRAWN as, which is the asked one when the codec codes it as
    // asked and names both when it does not.
    let drawn = if (target.coded_width, target.coded_height) == (width, height) {
        format!("{width}x{height}")
    } else {
        format!(
            "{width}x{height}, coded at {}x{}",
            target.coded_width, target.coded_height
        )
    };
    if frames > MAX_RENDERED_FRAMES {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_FRAMES} frames for each request, and the requested frame rate asks for {frames} over this span."
        ));
    }
    if let Some(message) = past_frame_budget(width, height) {
        return Some(message);
    }
    let total = pixels.saturating_mul(u64::from(frames));
    if total > MAX_RENDERED_PIXELS {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_PIXELS} pixels for each request, and {frames} frames of {drawn} hold {total}."
        ));
    }
    None
}

/// The refusal message for ONE frame of `width` by `height` past what this emulator draws,
/// or `None` when it fits: a side the answer could not state, or more pixels in the frame
/// the geometry is CODED at — which is what every buffer of the rendering is sized from —
/// than one frame may hold. The one statement of both ceilings, so the request-only check
/// ([`OutputFormat::past_asked_budget`]) and the one that has the media ([`past_budget`])
/// cannot drift.
fn past_frame_budget(width: u32, height: u32) -> Option<String> {
    if width > MAX_RENDERED_SIDE || height > MAX_RENDERED_SIDE {
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_SIDE} pixels in each direction, and {width}x{height} is past that."
        ));
    }
    let coded = (coded_side(width), coded_side(height));
    let pixels = u64::from(coded.0).saturating_mul(u64::from(coded.1));
    if pixels > MAX_RENDERED_FRAME_PIXELS {
        let drawn = if coded == (width, height) {
            format!("{width}x{height}")
        } else {
            format!("{width}x{height}, coded at {}x{}", coded.0, coded.1)
        };
        return Some(format!(
            "This emulator renders at most {MAX_RENDERED_FRAME_PIXELS} pixels in each frame, and {drawn} holds {pixels}."
        ));
    }
    None
}

/// Decode the track's pictures and encode the output's, answering the H.264 byte stream of
/// the whole output.
///
/// ONE decoder and ONE encoder for the whole segment, so a picture is decoded against the
/// pictures before it and coded against the one before it, exactly as the stored media
/// itself was. `deadline` is the instant the work stops at whatever it has reached, so the
/// thread doing it is not left running for an answer the read waiting on it has stopped
/// waiting for.
fn encode_rendered_frames(
    reader: &mut ::mp4::Mp4Reader<Cursor<&[u8]>>,
    source: &SourceTrack,
    target: &Target,
    deadline: Option<::std::time::Instant>,
) -> Result<Vec<u8>, CaptureRendering> {
    let api = ::openh264::OpenH264API::from_source();
    let mut decoder = ::openh264::decoder::Decoder::with_api_config(
        api,
        ::openh264::decoder::DecoderConfig::new(),
    )
    .map_err(|error| {
        CaptureRendering::Failed(format!("the H.264 decoder could not be started: {error}"))
    })?;
    let mut encoder = ::openh264::encoder::Encoder::with_api_config(
        ::openh264::OpenH264API::from_source(),
        ::openh264::encoder::EncoderConfig::new()
            // Constant quality rather than a bitrate budget: what the output has to carry
            // is the producer's own pictures at the asked geometry, whatever that costs.
            .rate_control_mode(::openh264::encoder::RateControlMode::Quality)
            .max_frame_rate(::openh264::encoder::FrameRate::from_hz(
                target.frames_per_second as f32,
            )),
    )
    .map_err(|error| {
        CaptureRendering::Failed(format!("the H.264 encoder could not be started: {error}"))
    })?;

    // The parameter sets the track states, in the framing a decoder reads: they are in the
    // container's configuration record rather than in a sample, so every picture handed to
    // the decoder carries them.
    let parameter_sets = h264_mp4::annex_b_parameter_sets(&[
        source.seq_param_set.clone(),
        source.pic_param_set.clone(),
    ]);

    let mut stream: Vec<u8> = Vec::new();
    let mut emitted = 0u32;
    let mut last: Option<Vec<u8>> = None;
    for sample_id in 1..=source.sample_count {
        if emitted >= target.frames {
            break;
        }
        if past_deadline(deadline) {
            return Err(CaptureRendering::Failed(
                "the rendering did not finish inside this emulator's budget".to_string(),
            ));
        }
        let Ok(Some(sample)) = reader.read_sample(source.id, sample_id) else {
            continue;
        };
        // When this picture stops being the one on display, which is what decides how many
        // moments of the output show it.
        let shown_until = media_moment(
            sample.start_time.saturating_add(u64::from(sample.duration)),
            source.timescale,
        );
        let mut packet = parameter_sets.clone();
        if !h264_mp4::annex_b_from_avcc(&sample.bytes, AVCC_LENGTH_SIZE, &mut packet) {
            // A sample of a track that otherwise decodes, whose length fields do not frame
            // its own bytes: media this emulator could not read, reported as the failure it
            // is rather than as a read answered with the media unprocessed.
            return Err(CaptureRendering::Failed(
                "a sample of the stored media is not framed by its own lengths".to_string(),
            ));
        }
        let Ok(Some(picture)) = decoder.decode(&packet) else {
            // A sample that yielded no picture leaves the previous one on display: its
            // moments are covered by the next picture's interval.
            continue;
        };
        let Some(drawn) = drawn_frame(&picture, target) else {
            return Err(CaptureRendering::Failed(
                "a picture of the stored media is larger than this emulator draws".to_string(),
            ));
        };
        while emitted < target.frames
            && output_moment(emitted, target.frames_per_second) < shown_until
        {
            encode_frame(&mut encoder, &drawn, target, &mut stream)?;
            emitted = emitted.saturating_add(1);
        }
        last = Some(drawn);
    }
    // The span the stored media covers is the span the answer covers, so a timeline that
    // ran out of pictures before the last moment of it shows the last picture there was.
    if let Some(drawn) = last {
        while emitted < target.frames {
            encode_frame(&mut encoder, &drawn, target, &mut stream)?;
            emitted = emitted.saturating_add(1);
        }
    }
    if emitted == 0 {
        return Err(CaptureRendering::AsStored);
    }
    Ok(stream)
}

/// Whether the instant the rendering stops at has passed.
///
/// `None` is a deadline this platform's clock could not express, which is no reason to
/// abandon the work the ceilings already bound.
fn past_deadline(deadline: Option<::std::time::Instant>) -> bool {
    deadline.is_some_and(|deadline| ::std::time::Instant::now() >= deadline)
}

/// One decoded picture at the asked geometry, as the red-green-blue samples the encoder is
/// handed, or `None` for a picture larger than this emulator draws.
///
/// A picture already that size is taken as it is; otherwise it is RESAMPLED to it by the
/// maintained resampler, with the triangle filter, so the output's pixels are the
/// producer's picture rather than a crop of it. The decoded geometry is the CODEC's and not
/// the container's, so the frame ceiling is applied to it here as well as to what the
/// container declared: every buffer sized from a geometry is bounded before it is
/// allocated.
fn drawn_frame(
    picture: &::openh264::decoder::DecodedYUV<'_>,
    target: &Target,
) -> Option<Vec<u8>> {
    let (width, height) = picture.dimensions();
    let (Ok(source_width), Ok(source_height)) = (u32::try_from(width), u32::try_from(height))
    else {
        return None;
    };
    if !within_frame_budget(source_width, source_height) {
        return None;
    }
    let mut rgb = ::std::vec![0u8; pixel_bytes(source_width, source_height)];
    picture.write_rgb8(&mut rgb);
    if (source_width, source_height) == (target.coded_width, target.coded_height) {
        return Some(rgb);
    }
    let frame = ::image::RgbImage::from_raw(source_width, source_height, rgb)?;
    Some(
        ::image::imageops::resize(
            &frame,
            target.coded_width,
            target.coded_height,
            ::image::imageops::FilterType::Triangle,
        )
        .into_raw(),
    )
}

/// How many bytes the red-green-blue samples of a `width` by `height` frame take.
fn pixel_bytes(width: u32, height: u32) -> usize {
    usize::try_from(
        u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(3),
    )
    .unwrap_or(0)
}

/// Encode one output picture, appending its units to `stream`.
fn encode_frame(
    encoder: &mut ::openh264::encoder::Encoder,
    rgb: &[u8],
    target: &Target,
    stream: &mut Vec<u8>,
) -> Result<(), CaptureRendering> {
    let expected = pixel_bytes(target.coded_width, target.coded_height);
    if rgb.len() != expected {
        return Err(CaptureRendering::Failed(
            "a frame could not be drawn at the requested geometry".to_string(),
        ));
    }
    let (Ok(width), Ok(height)) = (
        usize::try_from(target.coded_width),
        usize::try_from(target.coded_height),
    ) else {
        return Err(CaptureRendering::Failed(
            "the requested geometry is past this platform's frame size".to_string(),
        ));
    };
    let yuv = ::openh264::formats::YUVBuffer::from_rgb8_source(
        ::openh264::formats::RgbSliceU8::new(rgb, (width, height)),
    );
    let encoded = encoder
        .encode(&yuv)
        .map_err(|error| CaptureRendering::Failed(format!("a frame could not be encoded: {error}")))?;
    stream.extend_from_slice(&encoded.to_vec());
    Ok(())
}

/// Write the encoded output into the MP4 file the read answers with: one video track
/// stating the asked geometry, counting time in the asked rate, one sample per picture.
fn mux_rendered(stream: &[u8], target: &Target) -> CaptureRendering {
    let units = h264_mp4::annex_b_units(stream);
    let (Some(seq_param_set), Some(pic_param_set)) = (
        h264_mp4::parameter_set(&units, h264_mp4::SEQUENCE_PARAMETER_SET),
        h264_mp4::parameter_set(&units, h264_mp4::PICTURE_PARAMETER_SET),
    ) else {
        return CaptureRendering::Failed(
            "the encoded stream carries no parameter sets".to_string(),
        );
    };
    // Time is counted in the asked rate itself, so every picture lasts exactly one of its
    // own frame periods and the file's length is its frame count at that rate.
    let timed: Vec<h264_mp4::TimedSample> = h264_mp4::avcc_samples(&units)
        .into_iter()
        .map(|sample| h264_mp4::TimedSample {
            sample,
            duration: 1,
        })
        .collect();
    let track = h264_mp4::VideoTrack {
        width: u16::try_from(target.width).unwrap_or(u16::MAX),
        height: u16::try_from(target.height).unwrap_or(u16::MAX),
        timescale: target.frames_per_second,
        seq_param_set,
        pic_param_set,
    };
    match h264_mp4::mux_avc(&track, &timed) {
        Ok(file) => CaptureRendering::Rendered(file),
        Err(failure) => CaptureRendering::Failed(failure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a rendered file states about itself, read back with the maintained container
    /// reader: the geometry its video track declares, how many pictures it carries, and
    /// the rate it shows them at.
    #[derive(Debug, PartialEq)]
    struct Rendered {
        width: u16,
        height: u16,
        frames: u32,
        frames_per_second: u32,
    }

    /// Encode `frames` pictures of `width` by `height` at `frames_per_second` into an MP4
    /// file: the media a producer's own import would have brought in.
    fn source_file(width: u32, height: u32, frames: u32, frames_per_second: u32) -> Vec<u8> {
        let mut encoder = ::openh264::encoder::Encoder::with_api_config(
            ::openh264::OpenH264API::from_source(),
            ::openh264::encoder::EncoderConfig::new()
                .rate_control_mode(::openh264::encoder::RateControlMode::Quality)
                .max_frame_rate(::openh264::encoder::FrameRate::from_hz(
                    frames_per_second as f32,
                )),
        )
        .expect("the test fixture's encoder starts");
        let mut stream = Vec::new();
        for index in 0..frames {
            let rgb = moving_block(width, height, index);
            let yuv = ::openh264::formats::YUVBuffer::from_rgb8_source(
                ::openh264::formats::RgbSliceU8::new(&rgb, (width as usize, height as usize)),
            );
            stream.extend_from_slice(
                &encoder
                    .encode(&yuv)
                    .expect("the test fixture's frame encodes")
                    .to_vec(),
            );
        }
        let units = h264_mp4::annex_b_units(&stream);
        let track = h264_mp4::VideoTrack {
            width: u16::try_from(width).expect("a short side"),
            height: u16::try_from(height).expect("a short side"),
            timescale: frames_per_second,
            seq_param_set: h264_mp4::parameter_set(&units, h264_mp4::SEQUENCE_PARAMETER_SET)
                .expect("the fixture carries a sequence parameter set"),
            pic_param_set: h264_mp4::parameter_set(&units, h264_mp4::PICTURE_PARAMETER_SET)
                .expect("the fixture carries a picture parameter set"),
        };
        let timed: Vec<h264_mp4::TimedSample> = h264_mp4::avcc_samples(&units)
            .into_iter()
            .map(|sample| h264_mp4::TimedSample {
                sample,
                duration: 1,
            })
            .collect();
        h264_mp4::mux_avc(&track, &timed).expect("the test fixture muxes")
    }

    /// One fixture picture: a bright block whose place in the frame follows `index`, so two
    /// pictures of one clip differ.
    fn moving_block(width: u32, height: u32, index: u32) -> Vec<u8> {
        let mut rgb = ::std::vec![16u8; pixel_bytes(width, height)];
        let column = (index * 4) % width.max(1);
        for row in 0..height {
            for offset in 0..4u32 {
                let x = (column + offset) % width.max(1);
                let at = ((row * width + x) * 3) as usize;
                if let Some(pixel) = rgb.get_mut(at..at + 3) {
                    pixel.copy_from_slice(&[235, 235, 235]);
                }
            }
        }
        rgb
    }

    /// What a rendered answer states about itself.
    fn read_back(file: &[u8]) -> Rendered {
        let size = u64::try_from(file.len()).expect("a short file");
        let reader =
            ::mp4::Mp4Reader::read_header(Cursor::new(file), size).expect("a readable container");
        let track = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(::mp4::TrackType::Video))
            .expect("a video track");
        Rendered {
            width: track.width(),
            height: track.height(),
            frames: track.sample_count(),
            frames_per_second: track.frame_rate().round() as u32,
        }
    }

    /// The geometry the pictures of a rendered answer really DECODE at, which is what says
    /// the pixels were resampled rather than the container relabelled.
    fn decoded_geometry(file: &[u8]) -> (usize, usize) {
        let size = u64::try_from(file.len()).expect("a short file");
        let mut reader =
            ::mp4::Mp4Reader::read_header(Cursor::new(file), size).expect("a readable container");
        let source = coded_video_track(&reader).expect("a coded video track");
        let mut decoder = ::openh264::decoder::Decoder::with_api_config(
            ::openh264::OpenH264API::from_source(),
            ::openh264::decoder::DecoderConfig::new(),
        )
        .expect("the reading decoder starts");
        let sets = h264_mp4::annex_b_parameter_sets(&[
            source.seq_param_set.clone(),
            source.pic_param_set.clone(),
        ]);
        for sample_id in 1..=source.sample_count {
            let Ok(Some(sample)) = reader.read_sample(source.id, sample_id) else {
                continue;
            };
            let mut packet = sets.clone();
            assert!(
                h264_mp4::annex_b_from_avcc(&sample.bytes, AVCC_LENGTH_SIZE, &mut packet),
                "a rendered sample must be framed by its lengths"
            );
            if let Ok(Some(picture)) = decoder.decode(&packet) {
                return picture.dimensions();
            }
        }
        panic!("a rendered answer must carry a decodable picture");
    }

    /// One row of the format table: the settings a read asked for, and the output they ask
    /// of a two-second clip of eight 64 by 48 pictures.
    struct FormatRow {
        case: &'static str,
        format: OutputFormat,
        expected: Rendered,
    }

    /// The rule: the answer's pictures are shown at the asked rate and drawn at the asked
    /// geometry, over the span the stored media covers, with every target the settings
    /// leave out taken from the media itself.
    #[test]
    fn an_asked_output_format_is_what_the_answer_carries() {
        // A two-second clip: eight pictures at four per second.
        let media = source_file(64, 48, 8, 4);
        let rows = [
            FormatRow {
                case: "a lower rate drops the pictures it does not sample",
                format: OutputFormat {
                    frames_per_second: Some(2),
                    ..OutputFormat::default()
                },
                expected: Rendered {
                    width: 64,
                    height: 48,
                    frames: 4,
                    frames_per_second: 2,
                },
            },
            FormatRow {
                case: "a higher rate samples the span more often",
                format: OutputFormat {
                    frames_per_second: Some(10),
                    ..OutputFormat::default()
                },
                expected: Rendered {
                    width: 64,
                    height: 48,
                    frames: 20,
                    frames_per_second: 10,
                },
            },
            FormatRow {
                case: "a smaller geometry keeps the rate the media shows",
                format: OutputFormat {
                    width_in_pixels: Some(32),
                    height_in_pixels: Some(16),
                    ..OutputFormat::default()
                },
                expected: Rendered {
                    width: 32,
                    height: 16,
                    frames: 8,
                    frames_per_second: 4,
                },
            },
            FormatRow {
                case: "a larger geometry and a lower rate together",
                format: OutputFormat {
                    frames_per_second: Some(1),
                    width_in_pixels: Some(96),
                    height_in_pixels: Some(96),
                },
                expected: Rendered {
                    width: 96,
                    height: 96,
                    frames: 2,
                    frames_per_second: 1,
                },
            },
            FormatRow {
                case: "an odd geometry",
                format: OutputFormat {
                    frames_per_second: Some(2),
                    width_in_pixels: Some(33),
                    height_in_pixels: Some(25),
                },
                expected: Rendered {
                    width: 33,
                    height: 25,
                    frames: 4,
                    frames_per_second: 2,
                },
            },
            FormatRow {
                // The smallest the model admits: `PositiveInteger`, range 1.., which
                // `smithy_query(query="shape", shape="FormatSettings")` states for all
                // three members.
                case: "the smallest geometry the model admits",
                format: OutputFormat {
                    frames_per_second: Some(1),
                    width_in_pixels: Some(1),
                    height_in_pixels: Some(1),
                },
                expected: Rendered {
                    width: 1,
                    height: 1,
                    frames: 2,
                    frames_per_second: 1,
                },
            },
            FormatRow {
                // ... and the smallest rate beside a geometry of one whole chroma pair.
                case: "a geometry of one chroma pair",
                format: OutputFormat {
                    frames_per_second: Some(1),
                    width_in_pixels: Some(2),
                    height_in_pixels: Some(2),
                },
                expected: Rendered {
                    width: 2,
                    height: 2,
                    frames: 2,
                    frames_per_second: 1,
                },
            },
        ];
        for row in rows {
            let rendering = render_capture(&media, &row.format);
            let CaptureRendering::Rendered(file) = rendering else {
                panic!(
                    "case {}: expected a rendered answer, got {rendering:?}",
                    row.case
                );
            };
            let actual = read_back(&file);
            assert_eq!(
                actual, row.expected,
                "case {}: expected {:?}, got {actual:?}",
                row.case, row.expected
            );
            assert_ne!(
                file, media,
                "case {}: a rendered answer must not be the stored bytes",
                row.case
            );
        }
    }

    /// The pixels are really resampled: the pictures of the answer DECODE at the asked
    /// geometry, so the container is not merely relabelled.
    #[test]
    fn rendered_pictures_decode_at_the_asked_geometry() {
        let media = source_file(64, 48, 4, 2);
        for (case, width, height) in [
            ("half the width and height", 32u32, 24u32),
            ("twice the width and height", 128, 96),
        ] {
            let rendering = render_capture(&media, &OutputFormat {
                width_in_pixels: Some(width),
                height_in_pixels: Some(height),
                ..OutputFormat::default()
            });
            let CaptureRendering::Rendered(file) = rendering else {
                panic!("case {case}: expected a rendered answer, got {rendering:?}");
            };
            let decoded = decoded_geometry(&file);
            assert_eq!(
                decoded,
                (width as usize, height as usize),
                "case {case}: expected pictures decoding at {width}x{height}, got {decoded:?}"
            );
        }
    }

    /// Settings that ask for no processing — naming no target, or naming the format the
    /// media is already in — and media no picture can be taken out of are answered with
    /// the stored bytes.
    #[test]
    fn media_that_needs_no_processing_is_answered_as_stored() {
        let media = source_file(64, 48, 4, 2);
        let rows: [(&str, Vec<u8>, OutputFormat); 5] = [
            ("no target at all", media.clone(), OutputFormat::default()),
            (
                "the rate the media already shows",
                media.clone(),
                OutputFormat {
                    frames_per_second: Some(2),
                    ..OutputFormat::default()
                },
            ),
            (
                "the whole format the media is already in",
                media.clone(),
                OutputFormat {
                    frames_per_second: Some(2),
                    width_in_pixels: Some(64),
                    height_in_pixels: Some(48),
                },
            ),
            (
                "bytes that are no container at all",
                b"not-a-container".to_vec(),
                OutputFormat {
                    width_in_pixels: Some(16),
                    ..OutputFormat::default()
                },
            ),
            (
                "a container carrying no coded video track",
                header_only_file(),
                OutputFormat {
                    width_in_pixels: Some(16),
                    ..OutputFormat::default()
                },
            ),
        ];
        for (case, media, format) in rows {
            let rendering = render_capture(&media, &format);
            assert!(
                matches!(rendering, CaptureRendering::AsStored),
                "case {case}: expected the stored media, got {rendering:?}"
            );
        }
    }

    /// A container with a movie header and no track at all, which is video this emulator
    /// can state a duration for and take no picture out of.
    fn header_only_file() -> Vec<u8> {
        let mut body = ::std::vec![0u8; 12];
        body.extend_from_slice(&1_000u32.to_be_bytes());
        body.extend_from_slice(&2_000u32.to_be_bytes());
        body.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        body.extend_from_slice(&0x0100u16.to_be_bytes());
        body.extend_from_slice(&[0u8; 10]);
        for value in [
            0x0001_0000u32,
            0,
            0,
            0,
            0x0001_0000,
            0,
            0,
            0,
            0x4000_0000,
        ] {
            body.extend_from_slice(&value.to_be_bytes());
        }
        body.extend_from_slice(&[0u8; 24]);
        body.extend_from_slice(&1u32.to_be_bytes());
        let mut file = Vec::new();
        for (name, content) in [
            (*b"ftyp", [b"isom".as_slice(), &[0u8; 4], b"isom"].concat()),
            (*b"moov", {
                let mut moov = Vec::new();
                moov.extend_from_slice(&u32::try_from(8 + body.len()).expect("a short box").to_be_bytes());
                moov.extend_from_slice(b"mvhd");
                moov.extend_from_slice(&body);
                moov
            }),
        ] {
            file.extend_from_slice(
                &u32::try_from(8 + content.len())
                    .expect("a short box")
                    .to_be_bytes(),
            );
            file.extend_from_slice(&name);
            file.extend_from_slice(&content);
        }
        file
    }

    /// An output past a rendering ceiling is refused, with the ceiling it is past.
    #[test]
    fn an_output_past_the_rendering_budget_is_refused() {
        let media = source_file(64, 48, 4, 2);
        let rows = [
            (
                "more frames than one request renders",
                OutputFormat {
                    frames_per_second: Some(MAX_RENDERED_FRAMES),
                    ..OutputFormat::default()
                },
                format!("{MAX_RENDERED_FRAMES} frames"),
            ),
            (
                "a frame larger than one request renders",
                OutputFormat {
                    width_in_pixels: Some(4_000),
                    height_in_pixels: Some(4_000),
                    ..OutputFormat::default()
                },
                format!("{MAX_RENDERED_FRAME_PIXELS} pixels in each frame"),
            ),
            (
                "a side the answer could not state",
                OutputFormat {
                    width_in_pixels: Some(MAX_RENDERED_SIDE.saturating_add(1)),
                    height_in_pixels: Some(8),
                    ..OutputFormat::default()
                },
                format!("{MAX_RENDERED_SIDE} pixels in each direction"),
            ),
            (
                "more pixels in total than one request renders",
                OutputFormat {
                    frames_per_second: Some(100),
                    width_in_pixels: Some(1_920),
                    height_in_pixels: Some(1_080),
                },
                format!("{MAX_RENDERED_PIXELS} pixels for each request"),
            ),
        ];
        for (case, format, expected) in rows {
            let rendering = render_capture(&media, &format);
            let CaptureRendering::PastBudget(message) = rendering else {
                panic!("case {case}: expected a refusal, got {rendering:?}");
            };
            assert!(
                message.contains(&expected),
                "case {case}: expected a refusal naming {expected:?}, got {message:?}"
            );
        }
    }

    /// One row of the decode-ceiling table: what a container declares about its video, and
    /// the ceiling a read of it is past (or none, where the media is one this emulator
    /// decodes).
    struct DecodeRow {
        case: &'static str,
        frames: u32,
        width: u32,
        height: u32,
        expected: Option<String>,
    }

    /// How many renders may run at once: permits are handed out up to the ceiling, refused
    /// past it, and the one a finished render gives back is handed to the next read.
    #[test]
    fn renders_in_flight_are_bounded_by_their_permits() {
        let held: Vec<RenderPermit> = (0..MAX_CONCURRENT_RENDERS)
            .map(|at| {
                RenderPermit::take()
                    .unwrap_or_else(|| panic!("case permit {at}: expected one to be free"))
            })
            .collect();
        assert!(
            RenderPermit::take().is_none(),
            "case one render too many: expected no permit while {MAX_CONCURRENT_RENDERS} run"
        );
        drop(held);
        assert!(
            RenderPermit::take().is_some(),
            "case after the renders end: expected a permit to be free again"
        );
    }

    /// Which stored media this emulator decodes for one read, and what a read of media past
    /// a ceiling answers: the ceilings are measured on what the CONTAINER declares, before a
    /// sample of it is read, so the loop that reads them cannot be driven past them by a
    /// container that declares more than it carries — and media that carries pictures and is
    /// past a ceiling is REFUSED, naming that ceiling, rather than served unprocessed.
    #[test]
    fn media_past_the_decoding_ceilings_is_refused_by_the_ceiling_it_is_past() {
        let rows = [
            DecodeRow {
                case: "a short clip of small pictures",
                frames: 8,
                width: 64,
                height: 48,
                expected: None,
            },
            DecodeRow {
                case: "every frame the ceiling allows",
                frames: MAX_RENDERED_FRAMES,
                width: 16,
                height: 16,
                expected: None,
            },
            DecodeRow {
                case: "one picture more than that",
                frames: MAX_RENDERED_FRAMES.saturating_add(1),
                width: 16,
                height: 16,
                expected: Some(format!("{MAX_RENDERED_FRAMES} frames")),
            },
            DecodeRow {
                case: "a count no file could carry",
                frames: u32::MAX,
                width: 16,
                height: 16,
                expected: Some(format!("{MAX_RENDERED_FRAMES} frames")),
            },
            DecodeRow {
                case: "one picture larger than it draws",
                frames: 1,
                width: 3_840,
                height: 2_161,
                expected: Some(format!("{MAX_RENDERED_FRAME_PIXELS} pixels in each frame")),
            },
            DecodeRow {
                case: "the largest picture it draws",
                frames: 1,
                width: 3_840,
                height: 2_160,
                expected: None,
            },
            DecodeRow {
                case: "more pixels in total than it decodes",
                frames: 240,
                width: 1_920,
                height: 1_080,
                expected: Some(format!("{MAX_RENDERED_PIXELS} pixels for each request")),
            },
        ];
        for row in rows {
            let source = SourceTrack {
                id: 1,
                timescale: 1_000,
                sample_count: row.frames,
                duration: ::std::time::Duration::from_secs(1),
                frames_per_second: 30,
                width: row.width,
                height: row.height,
                seq_param_set: Vec::new(),
                pic_param_set: Vec::new(),
            };
            let refusal = past_decode_budget(&source);
            match (&refusal, &row.expected) {
                (None, None) => {}
                (Some(message), Some(expected)) => assert!(
                    message.contains(expected),
                    "case {}: expected a refusal naming {expected:?}, got {message:?}",
                    row.case
                ),
                _ => panic!(
                    "case {}: expected {:?}, got {refusal:?}",
                    row.case, row.expected
                ),
            }
        }
    }

    /// How many pictures a span carries at a rate, over the spans and rates the rule
    /// spans: the count is the rate times the span, so the output covers it.
    #[test]
    fn a_span_carries_the_pictures_its_rate_samples() {
        let rows = [
            ("one second at one frame per second", 1_000u64, 1u32, 1u32),
            ("one second at thirty", 1_000, 30, 30),
            ("ten seconds at fifteen", 10_000, 15, 150),
            ("half a second at four", 500, 4, 2),
            ("a span shorter than one frame", 10, 1, 1),
            ("no span at all", 0, 24, 1),
        ];
        for (case, millis, rate, expected) in rows {
            let actual = output_frames(::std::time::Duration::from_millis(millis), rate);
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected} frames, got {actual}"
            );
        }
    }
}

