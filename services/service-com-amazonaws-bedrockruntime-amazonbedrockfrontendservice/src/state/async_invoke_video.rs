// SPDX-License-Identifier: CC0-1.0

//! The generated MEDIA object a finished video invocation delivers, rendered and encoded
//! in this process.
//!
//! # The rule this module implements
//!
//! Asynchronous invocation is published for the video generation models, and its result is
//! an artifact written to the caller's own Amazon S3 location rather than a response body.
//! So an invocation reaching the modelled `Completed` status has to leave the generated
//! media object of the requested modality under its own segment of the submitted prefix,
//! beside the manifest that indexes it: a client polls the invocation to a terminal status
//! and then READS the media out of its own bucket, and a `Completed` with nothing to read
//! is a success nothing backs.
//!
//! # What the frames carry, and what they do not claim
//!
//! NO VIDEO MODEL RUNS IN THIS EMULATOR, and nothing here pretends otherwise — exactly as
//! the text generation stage beside it ([`super::generation`]) answers a notice saying so
//! rather than text synthesised to look like inference. The object written is a REAL video:
//! its frames are rendered here, pixel by pixel, and encoded by the maintained H.264
//! encoder and MP4 writer this crate depends on, so any player or probe decodes it. What
//! the frames SAY is that no model ran, beside the facts of the invocation that was
//! actually served — which model was invoked, which job this is, the geometry and length the
//! request configured, and where in that length the frame sits. Nothing renders the scene
//! the prompt describes, no pixel is derived from the prompt's own words, and no frame is
//! dressed up to look generated.
//!
//! Everything AROUND the media is the request's: the frame size follows the geometry the
//! submitted `modelInput` configured (a provider's `dimension`, or its `resolution` and
//! `aspect_ratio`), and the file's duration is the one it asked for.

use ::std::io::Cursor;

use ::font8x8::legacy::BASIC_LEGACY;

/// The object name a finished video invocation writes its generated media to, under the
/// invocation's own segment of the submitted prefix.
pub const MEDIA_OBJECT: &str = "output.mp4";

/// That object's media type, as the ISO base media file format's registered type.
pub const MEDIA_MIME: &str = "video/mp4";

/// How many frames this emulator RENDERS per second of the configured length.
///
/// The length and the geometry are the request's, but the frame rate is this emulator's
/// own: rendering a provider's 24 frames per second of a two-minute generation would make
/// one read spend minutes encoding frames that all say the same thing. One frame per second
/// keeps the artifact a real video of the requested length while bounding both the bytes it
/// takes and the time the read that materialises it spends.
const RENDERED_FRAMES_PER_SECOND: u32 = 1;

/// The fewest frames any rendered video carries, so even the shortest generation is a
/// moving picture rather than a still.
const MIN_RENDERED_FRAMES: u32 = 2;

/// The most frames any rendered video carries, whatever length was configured.
const MAX_RENDERED_FRAMES: u32 = 6;

/// The timescale the written file counts time in: milliseconds, so a configured length in
/// whole seconds is expressed exactly.
const TIMESCALE: u32 = 1_000;

/// The luma value the rendered background carries (studio-swing black).
const BACKGROUND_LUMA: u8 = 16;

/// The luma value the rendered lettering carries (studio-swing white).
const LETTERING_LUMA: u8 = 235;

/// The chroma value both chroma planes carry: the neutral point, so the rendered picture is
/// greyscale. No colour is invented for a scene nothing rendered.
const NEUTRAL_CHROMA: u8 = 128;

/// The geometry and the length one invocation's video is rendered at, taken off the
/// submitted `modelInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPlan {
    /// Frame width in pixels, always even (4:2:0 chroma is sampled in pairs).
    pub width: u32,
    /// Frame height in pixels, always even, for the same reason.
    pub height: u32,
    /// How many seconds of video the request configured.
    pub duration_seconds: u32,
}

/// The default frame geometry of the Amazon Nova Reel request schema
/// (`videoGenerationConfig.dimension`, whose single published value this is).
const NOVA_REEL_DIMENSION: (u32, u32) = (1280, 720);

/// The default length of a Nova Reel generation in seconds
/// (`videoGenerationConfig.durationSeconds`, whose single-shot published value this is).
const NOVA_REEL_DURATION_SECONDS: u32 = 6;

/// The default short side of a Luma AI Ray v2 generation, from its published `resolution`
/// value set.
const LUMA_RESOLUTION: u32 = 540;

/// The default aspect ratio of a Luma AI Ray v2 generation, from its published
/// `aspect_ratio` value set.
const LUMA_ASPECT_RATIO: (u32, u32) = (16, 9);

/// The default length of a Luma AI Ray v2 generation in seconds, from its published
/// `duration` value set.
const LUMA_DURATION_SECONDS: u32 = 5;

/// The longest video this emulator renders, in seconds: the longest a published request
/// schema of this service allows (`videoGenerationConfig.durationSeconds` of the
/// multi-shot Nova Reel task). A configured length past it is rendered at it rather than
/// trusted, so no payload can ask this emulator for an unbounded file.
const MAX_DURATION_SECONDS: u32 = 120;

/// What the invoked model's own request schema says this invocation's video is, or `None`
/// for a model whose generated artifact this module does not render.
///
/// Sources, both in the Amazon Bedrock User Guide: "Amazon Nova Reel" under "Video
/// generation models" (`videoGenerationConfig.dimension` and `durationSeconds`) and "Luma
/// AI Ray v2" (`resolution`, `aspect_ratio` and `duration`). A member the request leaves
/// out is the provider's own default for it, which is what the generation would have run
/// at.
#[must_use]
pub fn plan(model_id: &str, model_input: &::serde_json::Value) -> Option<VideoPlan> {
    let config = model_input.get("videoGenerationConfig");
    match model_id {
        "amazon.nova-reel-v1:0" | "amazon.nova-reel-v1:1" => {
            let (width, height) = config
                .and_then(|config| config.get("dimension"))
                .and_then(::serde_json::Value::as_str)
                .and_then(dimension)
                .unwrap_or(NOVA_REEL_DIMENSION);
            let duration_seconds = config
                .and_then(|config| config.get("durationSeconds"))
                .and_then(::serde_json::Value::as_u64)
                .and_then(|seconds| u32::try_from(seconds).ok())
                .unwrap_or(NOVA_REEL_DURATION_SECONDS);
            Some(VideoPlan {
                width,
                height,
                duration_seconds,
            })
        }
        "luma.ray-v2:0" => {
            let short_side = model_input
                .get("resolution")
                .and_then(::serde_json::Value::as_str)
                .and_then(resolution)
                .unwrap_or(LUMA_RESOLUTION);
            let ratio = model_input
                .get("aspect_ratio")
                .and_then(::serde_json::Value::as_str)
                .and_then(aspect_ratio)
                .unwrap_or(LUMA_ASPECT_RATIO);
            let duration_seconds = model_input
                .get("duration")
                .and_then(::serde_json::Value::as_str)
                .and_then(duration)
                .unwrap_or(LUMA_DURATION_SECONDS);
            let (width, height) = sides(short_side, ratio);
            Some(VideoPlan {
                width,
                height,
                duration_seconds,
            })
        }
        _ => None,
    }
}

impl VideoPlan {
    /// The plan as the renderer uses it: both sides even and at least one macroblock, and
    /// the length inside [`MAX_DURATION_SECONDS`].
    fn normalised(&self) -> Self {
        Self {
            width: even(self.width),
            height: even(self.height),
            duration_seconds: self.duration_seconds.clamp(1, MAX_DURATION_SECONDS),
        }
    }

    /// How many frames are rendered for this plan.
    fn frames(&self) -> u32 {
        self.duration_seconds
            .saturating_mul(RENDERED_FRAMES_PER_SECOND)
            .clamp(MIN_RENDERED_FRAMES, MAX_RENDERED_FRAMES)
    }
}

/// `value` rounded up to an even number of pixels, and never zero.
fn even(value: u32) -> u32 {
    let value = value.clamp(16, 4096);
    value + (value % 2)
}

/// The two sides a `WIDTHxHEIGHT` geometry names, or `None` for a value that is not one.
fn dimension(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

/// The short side a `<N>p` resolution names, or `None` for a value that is not one.
fn resolution(value: &str) -> Option<u32> {
    value.strip_suffix('p')?.parse().ok()
}

/// The two terms a `W:H` aspect ratio names, or `None` for a value that is not one.
fn aspect_ratio(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(':')?;
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// The seconds a `<N>s` duration names, or `None` for a value that is not one.
fn duration(value: &str) -> Option<u32> {
    value.strip_suffix('s')?.parse().ok()
}

/// The frame size a short side and an aspect ratio describe: the SHORTER side is the one
/// the resolution names, so a portrait ratio is that many pixels wide and a landscape one
/// that many pixels tall.
fn sides(short_side: u32, (ratio_width, ratio_height): (u32, u32)) -> (u32, u32) {
    let long_side = short_side
        .saturating_mul(ratio_width.max(ratio_height))
        .checked_div(ratio_width.min(ratio_height))
        .unwrap_or(short_side);
    if ratio_width >= ratio_height {
        (long_side, short_side)
    } else {
        (short_side, long_side)
    }
}

/// What one rendered video states about itself, beside the notice that no model ran.
#[derive(Debug, Clone)]
pub struct VideoSubject {
    /// The model the invocation was submitted against.
    pub model_id: String,
    /// The invocation's own id, which is the segment its output lives under.
    pub invocation_id: String,
}

/// Render and encode the video object of one invocation, as MP4 bytes.
///
/// `Err` carries what stopped the encoder, for the delivery to log: a failure to produce
/// the artifact is reported as the invocation FAILING rather than as a `Completed` with
/// nothing to read.
pub fn render(plan: &VideoPlan, subject: &VideoSubject) -> Result<Vec<u8>, String> {
    let plan = plan.normalised();
    let frames = plan.frames();
    let total_millis = u64::from(plan.duration_seconds).saturating_mul(1_000);
    let annex_b = encode_frames(&plan, subject, frames)?;
    let units = nal_units(&annex_b);
    mux(&plan, &units, frames, total_millis)
}

/// The side of one coded macroblock, in pixels.
///
/// A coded picture is a whole grid of them, so the SAMPLES handed to the encoder cover
/// the grid the picture's geometry rounds up to (a 540-row frame is coded in 34 rows of
/// macroblocks) and the encoder states the visible geometry inside it. The pixels of that
/// margin are never seen.
const MACROBLOCK_SIDE: usize = 16;

/// `value` rounded up to a whole number of macroblocks.
fn whole_macroblocks(value: usize) -> usize {
    value.div_ceil(MACROBLOCK_SIDE) * MACROBLOCK_SIDE
}

/// The H.264 Annex B byte stream of every rendered frame.
///
/// One encoder for the whole file, so the frames after the first are coded against the one
/// before them: a slate that barely changes between frames costs almost nothing to carry,
/// which is what keeps the delivered object small.
fn encode_frames(plan: &VideoPlan, subject: &VideoSubject, frames: u32) -> Result<Vec<u8>, String> {
    let coded_width = whole_macroblocks(plan.width as usize);
    let coded_height = whole_macroblocks(plan.height as usize);
    let chroma = ::std::vec![NEUTRAL_CHROMA; (coded_width / 2) * (coded_height / 2)];
    let mut encoder = ::openh264::encoder::Encoder::with_api_config(
        ::openh264::OpenH264API::from_source(),
        ::openh264::encoder::EncoderConfig::new()
            // The lettering has to stay legible at every geometry a provider publishes, so
            // the encoder is asked for constant quality rather than a bitrate budget: a
            // slate is cheap to carry either way.
            .rate_control_mode(::openh264::encoder::RateControlMode::Quality)
            .max_frame_rate(::openh264::encoder::FrameRate::from_hz(
                RENDERED_FRAMES_PER_SECOND as f32,
            )),
    )
    .map_err(|error| format!("the H.264 encoder could not be started: {error}"))?;
    let mut stream: Vec<u8> = Vec::new();
    for index in 0..frames {
        let frame = EncodedFrame {
            luma: luma_plane(plan, subject, index, frames),
            chroma: &chroma,
            width: plan.width as usize,
            height: plan.height as usize,
            stride: coded_width,
        };
        let encoded = encoder
            .encode(&frame)
            .map_err(|error| format!("a frame could not be encoded: {error}"))?;
        stream.extend_from_slice(&encoded.to_vec());
    }
    Ok(stream)
}

/// One rendered frame as the encoder reads it: the luma plane the lettering was written
/// into, and the neutral chroma planes beside it.
struct EncodedFrame<'a> {
    luma: Vec<u8>,
    chroma: &'a [u8],
    width: usize,
    height: usize,
    /// The stride of the coded grid the visible frame sits in the corner of.
    stride: usize,
}

impl ::openh264::formats::YUVSource for EncodedFrame<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.stride, self.stride / 2, self.stride / 2)
    }

    fn y(&self) -> &[u8] {
        &self.luma
    }

    fn u(&self) -> &[u8] {
        self.chroma
    }

    fn v(&self) -> &[u8] {
        self.chroma
    }
}

/// One frame's luma plane: the background, with the notice and the invocation's own facts
/// written into it.
fn luma_plane(plan: &VideoPlan, subject: &VideoSubject, index: u32, frames: u32) -> Vec<u8> {
    let width = plan.width as usize;
    let height = plan.height as usize;
    let coded_width = whole_macroblocks(width);
    let coded_height = whole_macroblocks(height);
    let mut plane = ::std::vec![BACKGROUND_LUMA; coded_width * coded_height];
    let at_millis = u64::from(index)
        .saturating_mul(u64::from(plan.duration_seconds).saturating_mul(1_000))
        .checked_div(u64::from(frames.max(1)))
        .unwrap_or(0);
    let lines = [
        "No video model runs in this emulator.".to_string(),
        format!(
            "{} generated no video for this request.",
            subject.model_id
        ),
        format!("job {}", subject.invocation_id),
        format!("{}x{} {}s", plan.width, plan.height, plan.duration_seconds),
        format!(
            "frame {}/{frames} at {at_millis}ms",
            index.saturating_add(1)
        ),
    ];
    let longest = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(1)
        .max(1);
    // The lettering is scaled to the frame, so the same notice is legible at every geometry
    // a provider publishes: as large as fits the widest line and the stack of lines.
    let scale = (width / (longest * GLYPH_SIDE + GLYPH_SIDE))
        .min(height / (lines.len() * (GLYPH_SIDE + 2) + GLYPH_SIDE))
        .clamp(1, 8);
    let left = GLYPH_SIDE * scale / 2;
    let top = height
        .saturating_sub(lines.len() * (GLYPH_SIDE + 2) * scale)
        .checked_div(2)
        .unwrap_or(0);
    for (row, line) in lines.iter().enumerate() {
        let baseline = top + row * (GLYPH_SIDE + 2) * scale;
        write_line(
            &mut plane,
            Frame {
                width,
                height,
                stride: coded_width,
            },
            line,
            (left, baseline),
            scale,
        );
    }
    plane
}

/// The frame the lettering is written into: the VISIBLE geometry, and the stride of the
/// coded grid that carries it.
#[derive(Debug, Clone, Copy)]
struct Frame {
    width: usize,
    height: usize,
    stride: usize,
}

/// How wide and how tall one glyph cell of the bitmap font is, in pixels.
const GLYPH_SIDE: usize = 8;

/// Write `line` into `plane` with its top-left corner at `at`, each glyph pixel drawn as a
/// `scale` by `scale` block so the lettering carries to a large frame.
fn write_line(plane: &mut [u8], frame: Frame, line: &str, at: (usize, usize), scale: usize) {
    let Frame {
        width,
        height,
        stride,
    } = frame;
    let (left, top) = at;
    for (column, character) in line.chars().enumerate() {
        // The table holds one cell per ASCII code; a character outside it is written as the
        // glyph of one that is in it rather than indexed for.
        let index = usize::try_from(u32::from(character)).unwrap_or(usize::MAX);
        let Some(glyph) = BASIC_LEGACY
            .get(index)
            .or_else(|| BASIC_LEGACY.get(usize::from(b'?')))
            .copied()
        else {
            continue;
        };
        let origin = left + column * GLYPH_SIDE * scale;
        for (row, bits) in glyph.iter().enumerate() {
            for bit in 0..GLYPH_SIDE {
                // The font's rows are least significant bit first, so bit `bit` is the
                // pixel that many places right of the glyph's left edge.
                if bits & (1u8 << bit) == 0 {
                    continue;
                }
                for inner_y in 0..scale {
                    let y = top + row * scale + inner_y;
                    if y >= height {
                        continue;
                    }
                    for inner_x in 0..scale {
                        let x = origin + bit * scale + inner_x;
                        if x >= width {
                            continue;
                        }
                        if let Some(pixel) = plane.get_mut(y * stride + x) {
                            *pixel = LETTERING_LUMA;
                        }
                    }
                }
            }
        }
    }
}

/// One network abstraction layer unit of an H.264 stream: its type, and its payload with
/// no start code.
#[derive(Debug, Clone)]
struct NalUnit<'a> {
    /// The `nal_unit_type` field, which is what says whether the unit is a parameter set
    /// or a coded picture.
    kind: u8,
    bytes: &'a [u8],
}

/// Split an Annex B byte stream into its units.
///
/// Annex B delimits each unit with a three- or four-byte start code and nothing else, which
/// is the whole framing: the units are handed to the MP4 writer, which frames them by
/// length instead.
fn nal_units(stream: &[u8]) -> Vec<NalUnit<'_>> {
    // Where each unit's own bytes begin, which is just past its start code.
    let mut payloads: Vec<usize> = Vec::new();
    let mut at = 0usize;
    while at + 3 <= stream.len() {
        if stream[at..at + 3] == [0, 0, 1] {
            payloads.push(at + 3);
            at += 3;
        } else {
            at += 1;
        }
    }
    let mut units = Vec::with_capacity(payloads.len());
    for (index, payload) in payloads.iter().enumerate() {
        let end = payloads
            .get(index + 1)
            .map_or(stream.len(), |next| trimmed_end(stream, *next - 3));
        let bytes = &stream[*payload..end.max(*payload)];
        if let Some(header) = bytes.first() {
            units.push(NalUnit {
                kind: header & 0x1F,
                bytes,
            });
        }
    }
    units
}

/// Where a unit ends when the next start code is a four-byte one: the leading zero of
/// `00 00 00 01` belongs to the delimiter, not to the unit before it.
fn trimmed_end(stream: &[u8], next_start: usize) -> usize {
    if next_start > 0 && stream.get(next_start - 1) == Some(&0) {
        next_start - 1
    } else {
        next_start
    }
}

/// Whether a unit is a coded slice of a picture, which is what starts a new sample.
fn is_picture(kind: u8) -> bool {
    (1..=5).contains(&kind)
}

/// The `nal_unit_type` of a coded slice of an instantaneous decoding refresh picture: the
/// unit a player may start decoding at, which is what an MP4 sample reports as a sync
/// sample.
const IDR_PICTURE: u8 = 5;

/// Write the encoded units into an MP4 file with one video track of `frames` samples
/// spanning `total_millis`.
fn mux(
    plan: &VideoPlan,
    units: &[NalUnit<'_>],
    frames: u32,
    total_millis: u64,
) -> Result<Vec<u8>, String> {
    let parameter_set = |kind: u8| {
        units
            .iter()
            .find(|unit| unit.kind == kind)
            .map(|unit| unit.bytes.to_vec())
    };
    let (Some(seq_param_set), Some(pic_param_set)) = (parameter_set(7), parameter_set(8)) else {
        return Err("the encoded stream carries no parameter sets".to_string());
    };
    let samples = samples(units);
    if samples.is_empty() {
        return Err("the encoded stream carries no coded picture".to_string());
    }
    let config = ::mp4::Mp4Config {
        major_brand: ::mp4::FourCC::from(*b"isom"),
        minor_version: 512,
        compatible_brands: ::std::vec![
            ::mp4::FourCC::from(*b"isom"),
            ::mp4::FourCC::from(*b"iso2"),
            ::mp4::FourCC::from(*b"avc1"),
            ::mp4::FourCC::from(*b"mp41"),
        ],
        timescale: TIMESCALE,
    };
    let mut writer = ::mp4::Mp4Writer::write_start(Cursor::new(Vec::new()), &config)
        .map_err(|error| format!("the MP4 writer could not be started: {error}"))?;
    let track = ::mp4::TrackConfig {
        track_type: ::mp4::TrackType::Video,
        timescale: TIMESCALE,
        language: "und".to_string(),
        media_conf: ::mp4::MediaConfig::AvcConfig(::mp4::AvcConfig {
            width: u16::try_from(plan.width).unwrap_or(u16::MAX),
            height: u16::try_from(plan.height).unwrap_or(u16::MAX),
            seq_param_set,
            pic_param_set,
        }),
    };
    writer
        .add_track(&track)
        .map_err(|error| format!("the MP4 writer refused the video track: {error}"))?;
    // The samples span exactly the configured length: each one lasts the same, and the last
    // carries whatever the division left over, so the file's duration is the request's.
    let count = u64::from(frames).max(samples.len() as u64);
    let each = total_millis.checked_div(count).unwrap_or(0).max(1);
    let mut start_time = 0u64;
    for (index, sample) in samples.iter().enumerate() {
        let last = index + 1 == samples.len();
        let duration = if last {
            total_millis.saturating_sub(start_time).max(1)
        } else {
            each
        };
        writer
            .write_sample(
                1,
                &::mp4::Mp4Sample {
                    start_time,
                    duration: u32::try_from(duration).unwrap_or(u32::MAX),
                    rendering_offset: 0,
                    is_sync: sample.sync,
                    bytes: ::bytes::Bytes::from(sample.bytes.clone()),
                },
            )
            .map_err(|error| format!("a sample could not be written: {error}"))?;
        start_time = start_time.saturating_add(duration);
    }
    writer
        .write_end()
        .map_err(|error| format!("the MP4 file could not be finished: {error}"))?;
    Ok(writer.into_writer().into_inner())
}

/// One coded picture as an MP4 track carries it.
struct Sample {
    /// Its units, each framed by its length rather than by a start code.
    bytes: Vec<u8>,
    /// Whether a player may start decoding here, which holds for a refresh picture.
    sync: bool,
}

/// One sample per coded picture, in the length-prefixed framing an MP4 track carries:
/// every unit that is not a parameter set, with the units that precede a picture attached
/// to it.
fn samples(units: &[NalUnit<'_>]) -> Vec<Sample> {
    let mut samples: Vec<Sample> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    for unit in units {
        if unit.kind == 7 || unit.kind == 8 {
            continue;
        }
        let length = u32::try_from(unit.bytes.len()).unwrap_or(u32::MAX);
        pending.extend_from_slice(&length.to_be_bytes());
        pending.extend_from_slice(unit.bytes);
        if is_picture(unit.kind) {
            samples.push(Sample {
                bytes: ::std::mem::take(&mut pending),
                sync: unit.kind == IDR_PICTURE,
            });
        }
    }
    if !pending.is_empty() {
        if let Some(last) = samples.last_mut() {
            last.bytes.extend_from_slice(&pending);
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the plan table: a submitted `modelInput` and the video it describes.
    struct PlanRow {
        case: &'static str,
        model_id: &'static str,
        model_input: ::serde_json::Value,
        expected: VideoPlan,
    }

    /// The geometry and length a plan reads off a submitted `modelInput`, over the value
    /// sets the two providers publish.
    #[test]
    fn a_plan_follows_the_published_request_members() {
        let rows = [
            PlanRow {
                case: "the Nova Reel single-shot geometry",
                model_id: "amazon.nova-reel-v1:0",
                model_input: ::serde_json::json!({
                    "videoGenerationConfig": {"dimension": "1280x720", "durationSeconds": 6},
                }),
                expected: VideoPlan {
                    width: 1280,
                    height: 720,
                    duration_seconds: 6,
                },
            },
            PlanRow {
                case: "Nova Reel defaults, nothing configured",
                model_id: "amazon.nova-reel-v1:1",
                model_input: ::serde_json::json!({"taskType": "TEXT_VIDEO"}),
                expected: VideoPlan {
                    width: 1280,
                    height: 720,
                    duration_seconds: 6,
                },
            },
            PlanRow {
                case: "a Nova Reel multi-shot length",
                model_id: "amazon.nova-reel-v1:1",
                model_input: ::serde_json::json!({
                    "videoGenerationConfig": {"durationSeconds": 24},
                }),
                expected: VideoPlan {
                    width: 1280,
                    height: 720,
                    duration_seconds: 24,
                },
            },
            PlanRow {
                case: "a Luma landscape ratio at 720p",
                model_id: "luma.ray-v2:0",
                model_input: ::serde_json::json!({
                    "resolution": "720p",
                    "aspect_ratio": "16:9",
                    "duration": "9s",
                }),
                expected: VideoPlan {
                    width: 1280,
                    height: 720,
                    duration_seconds: 9,
                },
            },
            PlanRow {
                case: "a Luma portrait ratio at 540p",
                model_id: "luma.ray-v2:0",
                model_input: ::serde_json::json!({"resolution": "540p", "aspect_ratio": "9:16"}),
                expected: VideoPlan {
                    width: 540,
                    height: 960,
                    duration_seconds: 5,
                },
            },
            PlanRow {
                case: "Luma defaults, only the prompt",
                model_id: "luma.ray-v2:0",
                model_input: ::serde_json::json!({"prompt": "a kite"}),
                expected: VideoPlan {
                    width: 960,
                    height: 540,
                    duration_seconds: 5,
                },
            },
        ];
        for row in rows {
            let planned = plan(row.model_id, &row.model_input);
            assert_eq!(
                planned,
                Some(row.expected.clone()),
                "case {:?}: expected {:?}, got {planned:?}",
                row.case,
                row.expected
            );
        }
    }

    /// A model this module renders no artifact for has no plan at all.
    #[test]
    fn a_model_this_module_renders_nothing_for_has_no_plan() {
        for case in ["amazon.nova-lite-v1:0", "", "luma.ray-v9:0"] {
            let planned = plan(case, &::serde_json::json!({"prompt": "a kite"}));
            assert!(
                planned.is_none(),
                "case {case:?}: expected no plan, got {planned:?}"
            );
        }
    }

    /// RULE: what is written is a REAL video file of the geometry and the length the
    /// request configured — so the maintained reader decodes it, its track reports those
    /// dimensions, and its duration is the configured one.
    ///
    /// The rows span the geometries the two providers publish: landscape and portrait, a
    /// non-macroblock-multiple height, the squarest and the widest ratio, and the shortest
    /// and a long length.
    #[test]
    fn the_rendered_object_is_a_video_of_the_configured_geometry_and_length() {
        let rows: [(&str, VideoPlan); 5] = [
            (
                "the Nova Reel geometry",
                VideoPlan {
                    width: 1280,
                    height: 720,
                    duration_seconds: 6,
                },
            ),
            (
                "a 16:9 frame at 540p",
                VideoPlan {
                    width: 960,
                    height: 540,
                    duration_seconds: 5,
                },
            ),
            (
                "a portrait 9:16 frame at 540p",
                VideoPlan {
                    width: 540,
                    height: 960,
                    duration_seconds: 9,
                },
            ),
            (
                "a square frame",
                VideoPlan {
                    width: 540,
                    height: 540,
                    duration_seconds: 1,
                },
            ),
            (
                "the widest published ratio, at the longest published length",
                VideoPlan {
                    width: 1680,
                    height: 720,
                    duration_seconds: 120,
                },
            ),
        ];
        for (case, planned) in rows {
            let subject = VideoSubject {
                model_id: "amazon.nova-reel-v1:1".to_string(),
                invocation_id: "0123456789ab".to_string(),
            };
            let bytes = render(&planned, &subject)
                .unwrap_or_else(|error| panic!("case {case:?}: the render failed: {error}"));
            assert!(
                !bytes.is_empty(),
                "case {case:?}: expected a non-empty object"
            );
            // The artifact a read materialises stays modest: a slate that barely changes
            // between frames is cheap to carry, and the object lives in this emulator's own
            // memory until the caller reads it.
            assert!(
                bytes.len() < 1_048_576,
                "case {case:?}: expected a bounded object, got {} bytes",
                bytes.len()
            );
            let size = bytes.len() as u64;
            let file = ::mp4::Mp4Reader::read_header(Cursor::new(bytes), size)
                .unwrap_or_else(|error| panic!("case {case:?}: not a readable MP4: {error}"));
            let track = file
                .tracks()
                .values()
                .next()
                .unwrap_or_else(|| panic!("case {case:?}: the file carries no track"));
            assert_eq!(
                (track.width(), track.height()),
                (
                    u16::try_from(planned.width).unwrap_or(u16::MAX),
                    u16::try_from(planned.height).unwrap_or(u16::MAX)
                ),
                "case {case:?}: expected the configured geometry, got {}x{}",
                track.width(),
                track.height()
            );
            assert_eq!(
                file.duration().as_secs(),
                u64::from(planned.duration_seconds),
                "case {case:?}: expected {}s of video, got {:?}",
                planned.duration_seconds,
                file.duration()
            );
            assert!(
                track.sample_count() >= MIN_RENDERED_FRAMES,
                "case {case:?}: expected at least {MIN_RENDERED_FRAMES} frames, got {}",
                track.sample_count()
            );
        }
    }
}
