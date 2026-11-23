Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 92fcf93b5493dfb9953528bbe45e4bdccdf435f0

# GetDASHStreamingSessionURL never reports the three codec/media-type refusals its model declares, although the sibling GetClip detects exactly those conditions on the same archive

_Filed: 2026-11-23T19:59:16Z_
_Operation: GetDASHStreamingSessionURL | Class: error-response_

> _Note: this issue was identified against the code base at commit `92fcf93b5493dfb9953528bbe45e4bdccdf435f0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model declares `UnsupportedStreamMediaTypeException`, `MissingCodecPrivateDataException` and `InvalidCodecPrivateDataException` for `GetDASHStreamingSessionURL`, and the operation documentation states the media requirements that raise them. The handler declares all three variants in its error enum but constructs NONE of them: it validates stream addressing, retention, the playback-mode/selector rules and that the manifest snapshot is non-empty, then mints a session over whatever bytes the archive holds. A stream whose archived fragments declare no H.264 video track, or whose video track carries no (or an unreadable) codec private data, is answered HTTP 200 with a session URL.

This crate's own sibling handler `get_clip.rs` performs exactly those three detections over exactly the same `StoredFragment.media` bytes of the same archive and maps them to exactly those three modelled errors, so the MPEG-DASH session path skips a classification the crate already implements, leaving three modelled variants of this operation unreachable.

## Evidence

Model (smithy_query doc, shape `GetDASHStreamingSessionURL` mirrors the HLS text): the stream "must contain h.264/h.265 encoded video" and "The video track of each fragment must contain codec private data in the Advanced Video Coding (AVC) for H.264 format"; the handler's own rendered contract repeats the triggers — src/ops/get_dash_streaming_session_url.rs:176 "`MissingCodecPrivateDataException` (HTTP 400) — No codec private data was found in at least one of tracks of the video stream" and :188 "`UnsupportedStreamMediaTypeException` (HTTP 400) — The type of the media ... could not be determined from the codec IDs of the tracks in the first fragment for a playback session."

Model errors for this operation — OPERATIONS.md:38: "`InvalidCodecPrivateDataException (HTTP 400)`, `MissingCodecPrivateDataException (HTTP 400)` ... `UnsupportedStreamMediaTypeException (HTTP 400)`".

Declared but never built — src/ops/get_dash_streaming_session_url.rs:227 and :231:
```
    MissingCodecPrivateDataException(MissingCodecPrivateDataException),
...
    UnsupportedStreamMediaTypeException(UnsupportedStreamMediaTypeException),
```
The whole `handle` body (src/ops/get_dash_streaming_session_url.rs:280-511) constructs only `InvalidArgumentException`, `ResourceNotFoundException`, `NoDataRetentionException` and `ClientLimitExceededException`; a repo-wide grep for `UnsupportedStreamMediaTypeException|MissingCodecPrivateDataException` finds construction sites ONLY in get_clip.rs:501 and get_clip.rs:515-516.

The sibling that does the check — src/ops/get_clip.rs:706-711 (`read_codec_private` → `ClipFault::MissingCodecPrivateData` / `ClipFault::InvalidCodecPrivateData`) and src/ops/get_clip.rs:892-895 (`.find(TrackDeclaration::is_avc).ok_or(ClipFault::UnsupportedMedia)?`), both reading the same `StoredFragment.media` the DASH handler selects by fragment number.

Test coverage: GetClip's three refusals are pinned by `get_clip_refuses_archived_media_it_cannot_mux` (tests/all/archived_media_read_path.rs:2119). The DASH suite (tests/all/get_dash_streaming_session_url.rs:89, 228, 339, 392, 520, 631, 684, 738) covers selection, defaults, `Expires` bounds, selector rules, no-fragments, retention and minting — none pins a 200 for media that cannot be parsed, and its archive is the valid-H.264 `seeded()` harness (tests/all/get_dash_streaming_session_url.rs:5-8, tests/all/archived_media_read_path.rs:546-604).

## Steps to Reproduce

1. Create a Kinesis Video stream with `DataRetentionInHours` > 0 through the control plane.
2. Ingest one archived fragment whose Matroska `TrackEntry` carries a non-AVC `CodecID` (or no `CodecPrivate`), as the GetClip test rows at tests/all/archived_media_read_path.rs:2122-2150 do.
3. POST /getClip over a range covering that fragment: HTTP 400 `MissingCodecPrivateDataException` / `UnsupportedStreamMediaTypeException`.
4. POST /getDASHStreamingSessionURL with `PlaybackMode: ON_DEMAND` and a `DASHFragmentSelector.TimestampRange` covering the SAME fragment: HTTP 200 with a `DASHStreamingSessionURL`, instead of one of the three codec/media-type refusals the model declares for this operation.
