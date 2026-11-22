Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# GetImages can never generate an image from the Matroska archive this crate declares it holds: every instant reports MEDIA_ERROR

_Filed: 2026-11-22T09:53:03Z_
_Operation: GetImages | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Two sibling handlers of this crate read the SAME stored field, `StoredFragment.media`, under mutually exclusive assumptions about its format.

`GetClip` reads it as a Matroska document (and the store declares the archive as Matroska, served as `video/webm`), while `GetImages` only accepts bytes whose magic numbers say PNG or JPEG and treats everything else as an un-generatable image. So for an archive holding what this crate says it holds — Matroska fragments, which is also exactly what the ingest plane writes into this very store — `GetImages` answers HTTP 200 whose every `Image` carries `Error: MEDIA_ERROR` and no `ImageContent`, for the same fragments from which `GetClip` successfully muxes an MP4 video frame-by-frame.

The crate already owns the reader needed: `get_clip.rs::read_fragment` walks a fragment's Matroska tree and extracts the H.264 frames of its video track, and `GetImages` does not call it (or anything like it) — it hands the raw fragment bytes straight to an image decoder. The result is a hollow success: the operation claims to have run and reports the modelled absence marker for media that is demonstrably present in the archive.

## Evidence

This crate declares what the archive holds — state.rs:199-201:
```
/// The content type Kinesis Video Streams archives fragments as: the archive is
/// Matroska, served as `video/webm`.
pub const ARCHIVE_CONTENT_TYPE: &str = "video/webm";
```
and the sibling handler reads `StoredFragment.media` as exactly that — src/ops/get_clip.rs:539-541:
```
struct ClipSource {
    /// The fragment's archived media — a Matroska document of its own.
    media: Vec<u8>,
```
src/ops/get_clip.rs:820: `for read in WebmIterator::new(Cursor::new(media), &[]) {`

But GetImages refuses anything that is not a still PNG/JPEG — src/ops/get_images.rs:756-762:
```
/// The format comes from the bytes' own magic numbers, never from the stream's declared
/// `MediaType`: what the archive holds is what can be decoded.
fn decode_frame(media: &[u8]) -> Option<::image::DynamicImage> {
    let format = match ::image::guess_format(media) {
        Ok(format @ (::image::ImageFormat::Png | ::image::ImageFormat::Jpeg)) => format,
        _ => return None,
```
and the `None` becomes the modelled error, src/ops/get_images.rs:724-728:
```
                Some(None) => Image {
                    time_stamp,
                    error: Some(MEDIA_ERROR.to_string()),
                    image_content: None,
                },
```
The bytes that actually land in this store are Matroska chunks: the ingest plane writes `produced.chunk` straight into `archive::StoredFragment::new(...)` (services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:337-343), with `archive_content_type: archive::ARCHIVE_CONTENT_TYPE` (same file, state.rs:656).

Test coverage shows the divergence: the GetImages suite seeds the archive with raw PNG blobs as fragment media — tests/all/get_images_generation.rs:108-128 (`/// The frame every seeded fragment carries, as PNG bytes.`) used at tests/all/get_images_generation.rs:164 — while the GetClip suite seeds real EBML/Matroska documents (tests/all/archived_media_read_path.rs:359-412, `push_ebml_element` / `clip_track`). No test exercises GetImages against a Matroska fragment.

## Steps to Reproduce

1. Seed (or ingest through PutMedia) a stream whose archived fragments are Matroska documents — the form `ARCHIVE_CONTENT_TYPE` declares, built the way tests/all/archived_media_read_path.rs builds them.
2. POST /getClip with a `ClipFragmentSelector` covering those fragments: HTTP 200 with a muxed `video/mp4` payload, proving the archived bytes carry decodable frames.
3. POST /getImages with `ImageSelectorType`, `StartTimestamp`, `EndTimestamp` covering the same fragments and `Format: JPEG`.
4. Observe HTTP 200 whose `Images` entries all carry `Error: "MEDIA_ERROR"` and no `ImageContent` — no image is ever generated from a realistically archived stream.
