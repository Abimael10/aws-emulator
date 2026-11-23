Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 15494e7376f3d94ea795b0f7c4ba531dc039674f

# GetImages refuses a window with 404 when no fragment STARTS in it, even when a longer earlier fragment covers every sampled instant

_Filed: 2026-11-23T04:14:49Z_
_Operation: GetImages | Class: error-response_

> _Note: this issue was identified against the code base at commit `15494e7376f3d94ea795b0f7c4ba531dc039674f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `GetImages` handler gates the whole call on `fragments_in_range(origin, start, end).is_empty()`, and `fragments_in_range` selects a fragment by the position of its START instant only (it compares the fragment's selected timestamp against the inclusive bounds). So a window that lies INSIDE one long fragment — the fragment started before `StartTimestamp` and plays past `EndTimestamp` — contains no fragment START, the gate fires, and the operation answers HTTP 404 `ResourceNotFoundException` / "No fragments found in the stream for the GetImages request."

That is exactly the case the handler's own image-reading helper says it exists to serve. `covering_media` deliberately searches the WHOLE archive, and its doc states why: "The whole archive is searched, not only the requested window, because a fragment that started before the window may still be the one covering its first instants." With the gate in front of it, a request whose instants are covered ONLY by an earlier-starting fragment can never reach that code: the handler reports "no fragments" for media it holds and could read.

The model's own description of absence is per-image, not wholesale: `GetImagesOutput$Images` — "If there is no media available for the given timestamp, the `NO_MEDIA` error will be listed in the output." This is NOT a request to drop the wholesale refusal (a stream with nothing in the window at all must keep answering 404 — that is pinned by tests and by the fixed issue 20261029T235218Z); the defect is that the gate's condition is "no fragment starts here" instead of "this window has no media", so a 404 is returned for a window whose every sampled instant is backed by archived bytes.

## Evidence

The gate — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_images.rs:514-526:
```
            // The archive holds nothing in the requested window: the WHOLE call is
            // refused, rather than answered with a list of absence markers.
            if stream
                .fragments_in_range(origin, start_millis, end_millis)
                .is_empty()
            {
                return Err(GetImagesErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: Some(NO_FRAGMENTS_MESSAGE.to_string()),
                    },
                )
                .into());
            }
```
and what that predicate actually tests — src/state.rs:339-357:
```
/// The fragments of `fragments` whose `origin` timestamp falls in the inclusive
/// range `[start_millis, end_millis]`, in ingestion order.
...
        .filter(|f| {
            let at = epoch_millis(origin.of(f));
            at >= start_millis && at <= end_millis
        })
```
(`origin.of(f)` is the fragment's own producer/server timestamp — its START — src/state.rs:121-125.)

The same file states the opposite rule for the bytes an image is read from — src/ops/get_images.rs:678-698:
```
/// The archived media of the fragment that was being recorded at `at_millis`, read on
/// the selector's timestamp field: the fragment whose selected timestamp starts at or
/// before that instant and whose playback duration still covers it.
///
/// `None` when the archive holds no such fragment — the `NO_MEDIA` case. The whole
/// archive is searched, not only the requested window, because a fragment that started
/// before the window may still be the one covering its first instants.
fn covering_media(...) -> Option<Vec<u8>> {
...
            from <= at_millis && (at_millis < until || at_millis == from)
```

Model (smithy_query doc, shape=GetImagesOutput, member=Images): "The list of images generated from the video stream. If there is no media available for the given timestamp, the NO_MEDIA error will be listed in the output. If an error occurs while the image is being generated, the MEDIA_ERROR will be listed in the output as the cause of the missing image."

Test coverage: no test exercises a window spanned by one longer fragment, because the suite's seed makes that impossible by construction — tests/all/get_images_generation.rs:49-53:
```
/// How far apart the seeded fragments start. Wider than their duration, so the
/// archive has GAPS no fragment covers.
const FRAGMENT_SPACING_SECS: i64 = 10;
/// How long each seeded fragment plays, in milliseconds.
const FRAGMENT_DURATION_MILLIS: u64 = 2_000;
```
The closest row, "a window inside a gap between two fragments" (tests/all/get_images_generation.rs:307-314, window `[producer_secs(0)+3, producer_secs(1)-1]`), is a window NO fragment covers (fragment 0 ends 2 s after it starts), so it stays a 404 under the coverage-based condition too; nothing pins the behaviour reported here.

## Steps to Reproduce

1. Create and activate a stream with non-zero retention (control plane `CreateStream`).
2. Archive ONE fragment into it whose producer timestamp is T0 and whose `duration_millis` is 60_000 (one minute of media), carrying bytes an image can be generated from.
3. POST /getImages for that stream with `ImageSelectorType: PRODUCER_TIMESTAMP`, `Format: JPEG`, `StartTimestamp: T0 + 10s`, `EndTimestamp: T0 + 20s`, `SamplingInterval: 1000`.
4. Observe HTTP 404 `ResourceNotFoundException` with "No fragments found in the stream for the GetImages request." — although `covering_media` would have found that very fragment covering every sampled instant of the window (T0 <= instant < T0+60s), so images were available.
5. Contrast: the same request with `StartTimestamp: T0` (so the fragment's start falls inside the window) is answered 200 with generated images from the same fragment.
