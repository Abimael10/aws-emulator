Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d83b8dd007c95a50c7612e04a2c8b0990b29e692

# GetMediaForFragmentList returns the stored chunks unaltered, omitting the three AWS_KINESISVIDEO_* MKV tags its own Payload documentation says the returned chunks carry

_Filed: 2026-11-22T19:12:06Z_
_Operation: GetMediaForFragmentList | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d83b8dd007c95a50c7612e04a2c8b0990b29e692`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model's `GetMediaForFragmentListOutput$Payload` documentation states that the chunks this operation returns carry three ADDITIONAL Matroska tags that the stored chunk does not have: the fragment number and the two timestamps. The handler returns `ArchivedStream::concatenate(&requested)` — the archive's stored bytes, byte-for-byte — so none of those tags is ever present, and a client that reads `AWS_KINESISVIDEO_FRAGMENT_NUMBER` out of the payload to map chunks back to fragments finds nothing.

The handler carries a comment that ADMITS the gap and gives a reason: "this emulator does not parse or rewrite media". That reason is false inside this very crate: the sibling `GetClip` handler parses exactly these stored bytes as a Matroska document with `WebmIterable` and writes a whole new MP4 container around their frames, and the store declares the archive to BE Matroska (`ARCHIVE_CONTENT_TYPE = "video/webm"`). So the capability the comment says the emulator lacks is already in the crate and already applied to the same `StoredFragment.media` field.

## Evidence

The model states the rule (smithy_query doc, GetMediaForFragmentListOutput$Payload):
"The payload that Kinesis Video Streams returns is a sequence of chunks from the specified stream. ... The chunks that Kinesis Video Streams returns in the `GetMediaForFragmentList` call also include the following additional Matroska (MKV) tags:" — "AWS_KINESISVIDEO_FRAGMENT_NUMBER - Fragment number returned in the chunk.", "AWS_KINESISVIDEO_SERVER_SIDE_TIMESTAMP - Server-side timestamp of the fragment.", "AWS_KINESISVIDEO_PRODUCER_SIDE_TIMESTAMP - Producer-side timestamp of the fragment."

The handler returns the stored bytes untouched and admits it — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_media_for_fragment_list.rs:260-265:
```
        // The payload is the archive's own stored bytes. The real service also
        // adds per-chunk MKV tags (AWS_KINESISVIDEO_FRAGMENT_NUMBER and the two
        // timestamps) while re-framing the Matroska stream; this emulator does
        // not parse or rewrite media, so it returns the stored chunks unaltered
        // rather than synthesising a container it cannot read.
```
and src/ops/get_media_for_fragment_list.rs:293:
```
        let payload = ArchivedStream::concatenate(&requested).map_err(|total| -> Self::Error {
```

The stated reason is contradicted by this crate's own sibling handler, which DOES parse and rewrite the same media — src/ops/get_clip.rs:820:
```
    for read in WebmIterator::new(Cursor::new(media), &[]) {
```
over bytes the store declares as Matroska — src/state.rs:199-201:
```
/// The content type Kinesis Video Streams archives fragments as: the archive is
/// Matroska, served as `video/webm`.
pub const ARCHIVE_CONTENT_TYPE: &str = "video/webm";
```

Test coverage: tests/all/archived_media_read_path.rs:999-1038 pins the current behaviour byte-for-byte — `/// RULE: GetMediaForFragmentList's payload is the stored media of exactly the requested fragments, concatenated in the order the request lists them.` with `assert_eq!(response.body().as_ref(), expected.as_slice(), ...)` where `expected` is `indexes.iter().copied().flat_map(media).collect()`. That pinned rule is exactly what the quoted model sentence contradicts, so the test's expectation has to grow the tags.

## Steps to Reproduce

1. Seed a stream whose fragments hold Matroska documents (as tests/all/archived_media_read_path.rs builds them for the GetClip suite).
2. POST /getMediaForFragmentList with `StreamName` and `Fragments: ["<fragment number>"]`.
3. Observe HTTP 200 whose body is byte-identical to the stored `StoredFragment.media` chunk.
4. Search the returned payload for the Matroska tag names `AWS_KINESISVIDEO_FRAGMENT_NUMBER`, `AWS_KINESISVIDEO_SERVER_SIDE_TIMESTAMP` and `AWS_KINESISVIDEO_PRODUCER_SIDE_TIMESTAMP`: none is present, though the Payload documentation says the returned chunks include all three.
