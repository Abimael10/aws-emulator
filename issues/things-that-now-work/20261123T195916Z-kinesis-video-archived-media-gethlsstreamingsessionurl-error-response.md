Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 92fcf93b5493dfb9953528bbe45e4bdccdf435f0

# GetHLSStreamingSessionURL never reports the three codec/media-type refusals its model declares, although the sibling GetClip detects exactly those conditions on the same archive

_Filed: 2026-11-23T19:59:16Z_
_Operation: GetHLSStreamingSessionURL | Class: error-response_

> _Note: this issue was identified against the code base at commit `92fcf93b5493dfb9953528bbe45e4bdccdf435f0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model declares `UnsupportedStreamMediaTypeException`, `MissingCodecPrivateDataException` and `InvalidCodecPrivateDataException` for `GetHLSStreamingSessionURL`, and the operation documentation states the media requirements that raise them. The handler declares all three variants in its error enum but constructs NONE of them: it checks only stream addressing, retention, the playback-mode/selector rules and that the selected set is non-empty, and then mints a session over whatever bytes the archive holds. A stream whose archived fragments declare no H.264 video track, or whose video track carries no (or an unreadable) codec private data, is answered HTTP 200 with a session URL.

This crate's own sibling handler `get_clip.rs` performs exactly those three detections over exactly the same `StoredFragment.media` bytes of the same archive, and maps them to exactly those three modelled errors. So one read path over the archive classifies the media and two (HLS/DASH) ignore it; the three modelled variants of this operation are dead code.

## Evidence

Model (smithy_query doc, shape `GetHLSStreamingSessionURL`): "For streaming video, the media must contain H.264 or H.265 encoded video and, optionally, AAC encoded audio. Specifically, the codec ID of track 1 should be `V_MPEG/ISO/AVC` (for H.264)..." and "The video track of each fragment must contain codec private data in the Advanced Video Coding (AVC) for H.264 format or HEVC for H.265 format".

Model errors for this operation — OPERATIONS.md:45: "**Errors:** `ClientLimitExceededException (HTTP 400)`, `InvalidArgumentException (HTTP 400)`, `InvalidCodecPrivateDataException (HTTP 400)`, `MissingCodecPrivateDataException (HTTP 400)`, `NoDataRetentionException (HTTP 400)`, `NotAuthorizedException (HTTP 401)`, `ResourceNotFoundException (HTTP 404)`, `UnsupportedStreamMediaTypeException (HTTP 400)`".

Declared but never built — src/ops/get_hls_streaming_session_url.rs:231-236:
```
    InvalidCodecPrivateDataException(InvalidCodecPrivateDataException),
    MissingCodecPrivateDataException(MissingCodecPrivateDataException),
...
    UnsupportedStreamMediaTypeException(UnsupportedStreamMediaTypeException),
```
The whole `handle` body (src/ops/get_hls_streaming_session_url.rs:285-521) constructs only `InvalidArgumentException`, `ResourceNotFoundException`, `NoDataRetentionException` and `ClientLimitExceededException`; a repo-wide grep for `UnsupportedStreamMediaTypeException|MissingCodecPrivateDataException` finds construction sites ONLY in get_clip.rs:501 and get_clip.rs:515-516.

The sibling that does the check — src/ops/get_clip.rs:702-711:
```
fn read_codec_private(declared: &TrackDeclaration) -> Result<AvcRecord, ClipFault> {
    // "The video track of each fragment must contain codec private data in the
    // Advanced Video Coding (AVC) for H.264 format": a track that declares none
    // is the modelled missing-record refusal, naming the track it is missing for.
    let private = declared.codec_private.as_deref().filter(|bytes| !bytes.is_empty())
        .ok_or(ClipFault::MissingCodecPrivateData(declared.number))?;
    AvcRecord::read(private).ok_or(ClipFault::InvalidCodecPrivateData(declared.number))
}
```
and src/ops/get_clip.rs:888-895:
```
    // "the codec ID of track 1 should be V_MPEG/ISO/AVC (for h.264)": the clip is
    // muxed from the track whose codec id names that codec, and media whose
    // declarations name no such track is media whose type this operation cannot
    // determine.
    let track = declared.into_iter().find(TrackDeclaration::is_avc)
        .ok_or(ClipFault::UnsupportedMedia)?;
```

Test coverage: `get_clip_refuses_archived_media_it_cannot_mux` (tests/all/archived_media_read_path.rs:2119, rows at 2123-2150 and 2231-2241) pins all three refusals for GetClip. The HLS tests (tests/all/archived_media_read_path.rs:1402, 1578, 1671, 1755, 1846) cover addressing, defaults, selector rules, retention and the no-fragments 404 only — none pins a 200 for unparseable media, and the seeded archive is real H.264 Matroska (tests/all/archived_media_read_path.rs:55-57 `MkvFragment::of(...)`, 79-82 "The frames are really encoded H.264 and the track's `CodecPrivate` is really that encoding's decoder configuration record"), so the existing HLS expectations stay unchanged.

## Steps to Reproduce

1. Create a Kinesis Video stream with `DataRetentionInHours` > 0 through the control plane.
2. Ingest one archived fragment whose Matroska `TrackEntry` declares a non-AVC `CodecID` (or omits `CodecPrivate`) — e.g. the `MkvFragment { codec_private: None, .. }` media the GetClip test at tests/all/archived_media_read_path.rs:2124 uses.
3. POST /getClip with a `ClipFragmentSelector` covering that fragment: the service answers HTTP 400 `MissingCodecPrivateDataException` / `UnsupportedStreamMediaTypeException`.
4. POST /getHLSStreamingSessionURL with `PlaybackMode: ON_DEMAND` and an `HLSFragmentSelector.TimestampRange` covering the SAME fragment: the service answers HTTP 200 with an `HLSStreamingSessionURL` instead of one of the three codec/media-type refusals its model declares.
