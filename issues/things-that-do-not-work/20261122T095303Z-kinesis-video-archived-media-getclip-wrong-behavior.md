Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# GetClip writes a 4,294,967,295-tick sample duration whenever two placed samples are not in ascending order (or are far apart), breaking the rule its own comment states

_Filed: 2026-11-22T09:53:03Z_
_Operation: GetClip | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`muxed_clip` places samples in ARCHIVE order (fragment-number order) using each fragment's producer timestamp, then computes each sample's duration as "until the next one is shown". Nothing establishes that those instants ascend: the archive is ordered by fragment NUMBER only (`append_fragment` inserts by `numeric_order` of the fragment number), and the timestamps placed on the clip timeline are always the PRODUCER timestamps even when the selector selected fragments on `SERVER_TIMESTAMP`.

When the next sample's instant precedes the current one, `until.saturating_sub(*at_millis)` is negative and `u32::try_from(...).unwrap_or(u32::MAX)` silently yields `u32::MAX` ticks — at the clip's 1,000-tick-per-second timescale, a single sample lasting about 49.7 days. The same arm is reached for two samples legitimately more than `u32::MAX` ms apart, which the missing span bound on the clip range makes easy to request. The comment directly above states the rule the code then breaks: each sample is to last "until the next one is shown" and "never less than one tick" — it has no provision for an impossible gap, and the fallback turns a malformed timeline into a successful but nonsensical MP4 instead of the modelled refusal the file already has for media it cannot mux (`ClipFault::Unwritable`/`InvalidMediaFrameException`).

## Evidence

The rule, and the code that breaks it — src/ops/get_clip.rs:643-656:
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
Nothing sorts `placed`; it is filled in archive order — src/ops/get_clip.rs:615-632 (`for source in sources { ... placed.push((source.start_millis.saturating_add(frame.offset_millis), ...))`).

The instant used is always the producer timestamp, even for a SERVER_TIMESTAMP selection — src/ops/get_clip.rs:549-557:
```
impl ClipSource {
    /// What the mux reads of one archived fragment.
    fn of(fragment: &StoredFragment) -> Self {
        Self {
            media: fragment.media.clone(),
            start_millis: epoch_millis(fragment.producer_timestamp),
```
while the selection is on whichever origin the request named — src/ops/get_clip.rs:387 (`TimestampOrigin::parse(&selector.fragment_selector_type)`) and 402 (`stream.fragments_in_range(origin, start_millis, end_millis)`).

And the archive's own order is by fragment NUMBER, not by timestamp — src/state.rs:796-802:
```
        let at = stream
            .fragments
            .binary_search_by(|held| {
                numeric_order(&held.fragment_number, &fragment.fragment_number)
            })
            .unwrap_or_else(|insertion_point| insertion_point);
        stream.fragments.insert(at, fragment);
```
No test in tests/all/archived_media_read_path.rs covers a clip whose fragments carry non-ascending producer timestamps (`get_clip_muxes_the_fragments_the_archive_holds_in_range`, line 1142, and `get_clip_truncates_at_the_documented_fragment_ceiling`, line 1213, both seed ascending producer timestamps via `producer_secs`, line 483).

## Steps to Reproduce

1. Archive two fragments on one stream whose fragment numbers ascend but whose producer timestamps do NOT (e.g. fragment "1001" stamped at t+10s and fragment "1002" stamped at t), each carrying a readable H.264 Matroska frame.
2. POST /getClip with a `ClipFragmentSelector` range covering both fragments.
3. The clip is returned HTTP 200; the sample placed for the later-numbered/earlier-stamped pair is written with `duration = u32::MAX` ticks (≈49.7 days at the 1,000-tick timescale) because `u32::try_from(negative).unwrap_or(u32::MAX)` is taken.
4. Same arm is reached with ascending timestamps more than `u32::MAX` milliseconds apart, which the unbounded clip range admits.
