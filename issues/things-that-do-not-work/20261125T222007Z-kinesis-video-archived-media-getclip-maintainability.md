Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 8beda664eef2534686c4de16bb7887f35213d53c

# The sample-duration rule ("each sample lasts until the next one is shown, the last until the media ends, never less than one tick") is implemented twice

_Filed: 2026-11-25T22:20:07Z_
_Operation: GetClip | Class: maintainability_

> _Note: this issue was identified against the code base at commit `8beda664eef2534686c4de16bb7887f35213d53c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate writes MP4 samples from archived frames in two places — the clip `GetClip` muxes (`src/ops/get_clip.rs`) and the MP4 media fragment the streaming-session playback plane serves (`src/service/playback_media.rs`) — and each one carries its OWN copy of the same timing rule: walk the placed samples in order, let each last until the next one is shown, let the last one last until the media's recorded end, convert the span to u32 ticks with `unwrap_or(u32::MAX)`, and floor it at one tick. Each copy also declares its own `MINIMUM_SAMPLE_TICKS: u32 = 1` constant with a near-identical doc comment. The two copies differ only in where the "end of the media" comes from (the clip's `media_end_millis` across fragments vs. the fragment's own recorded `duration_millis`) and in where the sample bytes are produced, so the rule itself — the branch, the saturating subtraction, the fallback and the floor — is one computation written twice and free to drift in either file.

This crate explicitly holds the READ half of the same container work in exactly one place for this reason (`src/service/archived_container.rs`), and the WRITE half is the copy that was left behind. The repair is to hoist one helper (taking the placed samples and the instant the media ends) next to that shared reader and have both call sites use it, together with one `MINIMUM_SAMPLE_TICKS`; every byte of every response and every existing test stays unchanged, since the computation is identical today.

## Evidence

Copy 1 — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_clip.rs:547-560:
```
    // Each sample lasts until the next one is shown, and the last one until the
    // media of the fragment carrying it ends — never less than one tick, so no
    // sample is written with a duration a player would read as none.
    let mut samples: Vec<TimedSample> = Vec::with_capacity(placed.len());
    for (position, (at_millis, sample)) in placed.iter().enumerate() {
        let until = placed
            .get(position + 1)
            .map_or(media_end_millis, |(next, _)| *next);
        let ticks = u32::try_from(until.saturating_sub(*at_millis)).unwrap_or(u32::MAX);
        samples.push(TimedSample {
            sample: sample.clone(),
            duration: ticks.max(MINIMUM_SAMPLE_TICKS),
        });
    }
```

Copy 2 — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/service/playback_media.rs:196-222:
```
/// One sample per frame of the fragment's video track.
///
/// Each sample lasts until the next one is shown and the last until the fragment's
/// recorded duration ends — never less than one tick, so no sample is written with
/// a duration a player would read as none.
fn timed_samples(
    read: &ArchivedFragment,
    record: &AvcRecord,
    duration_millis: u64,
) -> Vec<TimedSample> {
    let ends_at = i64::try_from(duration_millis).unwrap_or(i64::MAX);
    read.frames
        .iter()
        .enumerate()
        .map(|(position, frame)| {
            let until = read
                .frames
                .get(position + 1)
                .map_or(ends_at, |next| next.offset_millis);
            let ticks = u32::try_from(until.saturating_sub(frame.offset_millis)).unwrap_or(u32::MAX);
            TimedSample {
                sample: avcc_sample(frame, record.length_size),
                duration: ticks.max(MINIMUM_SAMPLE_TICKS),
            }
        })
        .collect()
}
```

The constant is declared twice, with the same meaning and nearly the same words:
- src/ops/get_clip.rs:179-183: "How long the LAST sample of the clip lasts when the fragment carrying it declares no duration past that sample's own instant: one tick of the written timescale ... the shortest span a sample can be shown for." / `const MINIMUM_SAMPLE_TICKS: u32 = 1;`
- src/service/playback_media.rs:43-47: "How long the LAST sample of a fragment lasts when the fragment's own duration ends at or before that sample's instant: one tick of the written timescale ... the shortest span a sample can be shown for." / `const MINIMUM_SAMPLE_TICKS: u32 = 1;`

The crate's own precedent for holding this container work exactly once — src/service/archived_container.rs:14-19: "That read lives HERE, beside neither of them, because two readers of one container are two chances to disagree about what a producer ingested." and src/service.rs:52-55: "It lives beside the operations rather than inside one of them because TWO readers of this plane look inside a fragment — the clip `GetClip` muxes and the media a streaming session's playback plane serves — and two readers of one container are two chances to disagree about what a producer ingested."

Tests that pin both copies and must keep passing unchanged: tests/all/archived_media_read_path.rs:1336 ("RULE: `GetClip` answers `video/mp4` and an MP4 container muxed from exactly ..."), tests/all/archived_media_read_path.rs:2125 ("RULE: whatever shape the archived Matroska fragments have, the clip is an MP4 ...") and tests/all/archived_media_read_path.rs:3141 ("RULE: a session's media fragment carries the frames the archive holds for that ...").

## Steps to Reproduce

1. Open services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_clip.rs:547-560 and services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/service/playback_media.rs:201-222 side by side: the per-sample `until` lookup, the `u32::try_from(until.saturating_sub(..)).unwrap_or(u32::MAX)` conversion and the `.max(MINIMUM_SAMPLE_TICKS)` floor are the same computation written twice.
2. Compare the two `MINIMUM_SAMPLE_TICKS` declarations (get_clip.rs:179-183 and playback_media.rs:43-47): one constant, one meaning, two definitions.
3. Note that the READ side of the same work is deliberately held once in src/service/archived_container.rs (see its module doc at lines 14-19), so the write side is the remaining copy; hoisting one sample-timing helper there and calling it from both sites leaves every response and the tests named in the evidence unchanged.
