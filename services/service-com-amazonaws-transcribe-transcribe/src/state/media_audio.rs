// SPDX-License-Identifier: CC0-1.0

//! What the AUDIO a job was pointed at says about itself: the sample rate its own
//! container and codec headers declare.
//!
//! THE RULE: `MediaSampleRateHertz` is "The sample rate, in hertz, of the audio track in
//! your input media file" — the same sentence on every job shape that reports one
//! (`smithy_query(query="doc", shape="TranscriptionJob",
//! member="MediaSampleRateHertz")`, and the same on `MedicalTranscriptionJob` and
//! `CallAnalyticsJob`) — so it is a property of the FILE rather than an echo of what a
//! request declared: a job that has finished processing its media reports the rate the
//! service read out of that media, whether or not the request stated one. Its start
//! request carries the member as a HINT, which is why a declared rate is kept as the
//! start reported it and the determination fills the member in only where the request
//! left it blank, exactly as the format of the same file is determined
//! ([`determined_media_format`]).
//!
//! WHERE the figure comes from: the stored object itself. A job's audio is an object in
//! this emulator's own Amazon S3, uploaded before the job was started, and every
//! container the model's `MediaFormat` enumerates declares its sampling frequency in a
//! header at the front of the file — so the first bytes of that object are read when the
//! job is started ([`MEDIA_HEADER_BYTES`]) and the rate is parsed out of them by
//! `lofty`, an audio-metadata library, rather than guessed from the request or the file
//! name. Nothing decodes the audio and no sample is examined: what is read is the
//! header's own declaration.
//!
//! WHAT IS WITHHELD: a rate the stored media declares none of, one the header is too
//! damaged or too truncated to state, a container `lofty` reads no properties for, and
//! one outside the range the member itself models ([`MediaSampleRateHertz`]'s
//! `@range(min: 8000, max: 48000)`) — none of those is a value of that member, so the
//! job reports none rather than a guessed figure. The read is bounded: at most
//! [`MEDIA_HEADER_BYTES`] of one object per start, however large the stored media is.

use super::*;

/// How many bytes at the FRONT of a stored media object are read to determine what the
/// audio in it is.
///
/// Every container the model's `MediaFormat` enumerates declares its sampling frequency
/// in a header at the start of the file — the MPEG frame header, the RIFF `fmt ` chunk,
/// the FLAC `STREAMINFO` block, the Ogg identification page, the MP4 `moov` atom — so a
/// bounded prefix is enough to read it, and the work one start does is fixed however
/// large the media is. CHOSEN at 1 MiB: large enough for an MP4 whose metadata atom sits
/// behind the leading `ftyp`, small enough that a start holds no media file in memory.
pub const MEDIA_HEADER_BYTES: u64 = 1024 * 1024;

/// The sample rate the audio in `bytes` declares for itself, or `None` for media that
/// declares none this member can carry (see this module's header).
///
/// `bytes` is the leading [`MEDIA_HEADER_BYTES`] of the stored object, so the parse runs
/// in the relaxed mode that reads what a truncated stream does carry instead of refusing
/// the stream for the part that was not read.
pub fn media_sample_rate_of(bytes: &[u8]) -> Option<i32> {
    use ::lofty::config::{ParseOptions, ParsingMode};
    use ::lofty::file::AudioFile;
    use ::lofty::probe::Probe;

    let options = ParseOptions::new()
        .read_tags(false)
        .read_properties(true)
        .parsing_mode(ParsingMode::Relaxed);
    let probed = Probe::new(::std::io::Cursor::new(bytes))
        .options(options)
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    let rate = i32::try_from(probed.properties().sample_rate()?).ok()?;
    // A figure outside the range the member models is not a value of it, so the member
    // stays absent rather than carrying one the shape forbids.
    (MIN_MEDIA_SAMPLE_RATE_HERTZ..=MAX_MEDIA_SAMPLE_RATE_HERTZ)
        .contains(&rate)
        .then_some(rate)
}

/// The lowest sample rate `MediaSampleRateHertz` models (`@range(min: 8000, max: 48000)`,
/// `smithy_query(query="shape", shape="MediaSampleRateHertz")`).
pub const MIN_MEDIA_SAMPLE_RATE_HERTZ: i32 = 8_000;

/// The highest sample rate `MediaSampleRateHertz` models (see
/// [`MIN_MEDIA_SAMPLE_RATE_HERTZ`]).
pub const MAX_MEDIA_SAMPLE_RATE_HERTZ: i32 = 48_000;

/// The `MediaSampleRateHertz` a job that has just FINISHED reports for the media it
/// processed: the rate the request declared, or — where it declared none — the one the
/// service read out of the stored media ([`media_sample_rate_of`]).
///
/// One determination for every family that reports the member, so the transcription, the
/// medical transcription and the Call Analytics job cannot disagree about what the rate
/// of one stored file is; the Call Analytics family's start carries no member to declare
/// one at all, so it reaches this with nothing declared.
pub fn determined_media_sample_rate(declared: Option<i32>, evidence: Option<i32>) -> Option<i32> {
    declared.or(evidence)
}

/// The EFFECTIVE `Settings` a finished transcription job ran with.
///
/// `TranscriptionJob$Settings` "Provides information on any additional settings that were
/// included in your request. Settings include: VocabularyName, VocabularyFilterName,
/// ShowSpeakerLabels, MaxAlternatives, and ChannelIdentification"
/// (`smithy_query(query="doc", shape="TranscriptionJob", member="Settings")`), so a job
/// that has finished reports the settings it actually RAN with rather than only the ones
/// the request spelled: the two options the service switches on or off for every job —
/// channel identification in multi-channel audio and alternative transcriptions — are
/// reported at the value they ran at, which is OFF for a request that asked for neither,
/// so a client reading the settings back learns which options the job ran with instead of
/// having to assume the defaults. The members that name a resource or a ceiling
/// (`VocabularyName`, `MaxSpeakerLabels`, `MaxAlternatives`, the vocabulary filter pair)
/// have no default to report and stay exactly as the request left them.
pub fn effective_settings(requested: Option<Settings>) -> Settings {
    let mut settings = requested.unwrap_or_default();
    settings.channel_identification = Some(settings.channel_identification.unwrap_or(false));
    settings.show_alternatives = Some(settings.show_alternatives.unwrap_or(false));
    settings
}

/// The EFFECTIVE `Settings` a finished MEDICAL transcription job ran with.
///
/// `MedicalTranscriptionSetting` carries the same two switched options as the
/// non-medical `Settings` shape and documents them in the same words ("Enables channel
/// identification in multi-channel audio", "To include alternative transcriptions within
/// your transcription output"), so the same rule answers for it
/// ([`effective_settings`]); the shapes are distinct, so the projection is spelled per
/// shape rather than shared through a conversion.
pub fn effective_medical_settings(
    requested: Option<MedicalTranscriptionSetting>,
) -> MedicalTranscriptionSetting {
    let mut settings = requested.unwrap_or_default();
    settings.channel_identification = Some(settings.channel_identification.unwrap_or(false));
    settings.show_alternatives = Some(settings.show_alternatives.unwrap_or(false));
    settings
}
