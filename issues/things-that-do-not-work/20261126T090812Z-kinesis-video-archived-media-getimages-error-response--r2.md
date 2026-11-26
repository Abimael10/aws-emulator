Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: acd81662dbc544138b2d21536303ff01a7fd1f9b

# GetImages refuses a window with no archived media as ResourceNotFoundException instead of listing NO_MEDIA images

_Filed: 2026-11-26T09:08:12Z_
_Operation: GetImages | Class: error-response_

> _Note: this issue was identified against the code base at commit `acd81662dbc544138b2d21536303ff01a7fd1f9b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When the stream resolves fine but its archive holds no media anywhere in the requested window, the handler refuses the WHOLE call with `ResourceNotFoundException` (404) instead of answering 200 with one `Image` per sampled instant carrying the modelled `NO_MEDIA` error. The model says the opposite twice: `GetImagesOutput$Images` documents the absent-media case as an entry IN THE OUTPUT, and `ResourceNotFoundException` documents only ONE trigger for GetImages — the stream cannot be found — attributing the "no fragments within the requested time range" trigger to GetHLSStreamingSessionURL/GetDASHStreamingSessionURL alone. The handler already has the per-instant NO_MEDIA machinery (it uses it whenever at least one instant of the window is covered), so the only input class that gets a 404 is the one where EVERY sampled instant would have reported NO_MEDIA. The handler's own stated reason for the refusal ("the refusal is a pure function of the archive and the requested window, so repeating the identical call — concurrently or not — answers identically") does not distinguish the two answers: a page of NO_MEDIA markers is the same pure function of the archive and the window.

## Evidence

Smithy, `GetImagesOutput$Images` (smithy_query query=doc shape=GetImagesOutput member=Images): "The list of images generated from the video stream. If there is no media available for the given timestamp, the NO_MEDIA error will be listed in the output. If an error occurs while the image is being generated, the MEDIA_ERROR will be listed in the output as the cause of the missing image."

Smithy, `ResourceNotFoundException` (smithy_query query=doc shape=ResourceNotFoundException): "GetImages will throw this error when Kinesis Video Streams can't find the stream that you specified." — the second paragraph names only "GetHLSStreamingSessionURL and GetDASHStreamingSessionURL throw this error if a session with a PlaybackMode of ON_DEMAND or LIVE_REPLAYis requested for a stream that has no fragments within the requested time range".

Handler, services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_images.rs:549-556:
```
            if !stream.holds_media_in_range(origin, start_millis, end_millis) {
                return Err(GetImagesErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: Some(NO_FRAGMENTS_MESSAGE.to_string()),
                    },
                )
                .into());
            }
```
and the constant it sends, get_images.rs:217-222, which asserts the un-modelled second trigger: "`ResourceNotFoundException` (404) is what Kinesis Video Streams raises when it \"can't find the stream that you specified\" AND when the request names no archived media".

The same file already implements the modelled answer for the covered case, get_images.rs:748-752:
```
                None => Image {
                    time_stamp,
                    error: Some(NO_MEDIA.to_string()),
                    image_content: None,
                },
```

Test coverage: tests/all/get_images_generation.rs:406-409 and the `covered: None` rows (e.g. "a window that opens the instant the fragment has finished playing", line 479-487) pin the 404; the test's stated reason is the playback-coverage relation (lines 412-417), not a justification for preferring a 404 over the documented NO_MEDIA output.

## Steps to Reproduce

1. Seed one ACTIVE stream with retention > 0 and a single archived fragment whose producer timestamp is T0 and whose duration is 2 s.
2. POST /getImages with {StreamName, ImageSelectorType: PRODUCER_TIMESTAMP, StartTimestamp: T0+60, EndTimestamp: T0+70, SamplingInterval: 1000, Format: JPEG}.
3. Observe HTTP 404 `ResourceNotFoundException` with "No fragments found in the stream for the GetImages request."
4. Expected per the model: HTTP 200 with `Images` carrying one entry per sampled instant, each with `Error: NO_MEDIA` and no `ImageContent` (the same shape the handler already returns when only SOME instants are uncovered).
