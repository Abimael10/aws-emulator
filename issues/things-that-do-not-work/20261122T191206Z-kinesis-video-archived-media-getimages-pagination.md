Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: d83b8dd007c95a50c7612e04a2c8b0990b29e692

# A GetImages NextToken is not bound to the SamplingInterval that minted it, so a continuation with a different (or absent) interval answers an empty final page and loses the rest of the window

_Filed: 2026-11-22T19:12:06Z_
_Operation: GetImages | Class: pagination_

> _Note: this issue was identified against the code base at commit `d83b8dd007c95a50c7612e04a2c8b0990b29e692`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `GetImages` cursor carries only the stream, the timestamp origin and the window bounds, and the handler checks exactly those four before using the cursor's offset. But the page's content is the list of SAMPLED INSTANTS, which is computed from `SamplingInterval` — an OPTIONAL member. Nothing binds the token to the interval that produced it and nothing re-checks it.

The consequence is a silent loss of results rather than a refusal: page 1 taken with `SamplingInterval: 200` over a 300-second window produces 1,501 instants, returns 25 and mints a token with `offset: 25`. Replaying that token WITHOUT `SamplingInterval` (or with a larger one) makes `sampled_instants` return a single instant, so `skip(25)` yields nothing, `next_offset (25) < instants.len() (1)` is false, and the handler answers HTTP 200 with an empty `Images` list and NO `NextToken` — i.e. it reports the pagination as complete while 1,476 sampled instants of the requested window were never served.

The handler's own comment states the rule the code then fails to enforce: it says the token "must have been issued for this very query", and justifies carrying only the offset by the fact that the re-stated members are REQUIRED ones — a justification that does not extend to `SamplingInterval`, which the model leaves optional.

## Evidence

The handler's comment states the rule and the checks it performs — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_images.rs:488-509:
```
            // Where this page starts. A continuation re-states the window and the
            // selector (the model makes all three required members), so the token
            // carries only how far the previous pages got — and it must have been issued
            // for this very query.
            let offset = match input.next_token.as_deref() {
...
                    if cursor.stream_name != stream.name
                        || cursor.origin != origin
                        || cursor.start_millis != start_millis
                        || cursor.end_millis != end_millis
```
but what the offset indexes into depends on the interval — src/ops/get_images.rs:528:
```
            let instants = sampled_instants(start_millis, end_millis, interval);
```
and src/ops/get_images.rs:660-663:
```
fn sampled_instants(start_millis: i64, end_millis: i64, interval: Option<i64>) -> Vec<i64> {
    let Some(interval) = interval else {
        return vec![start_millis];
    };
```
so an absent interval collapses the instant list to one entry while the token still says `offset: 25`, and the truncation flag is computed against that collapsed list — src/ops/get_images.rs:553-564:
```
            // "This value is null when there are no more results to return."
            let next_offset = offset.saturating_add(sampled.len());
            let next_token = (next_offset < instants.len()).then(|| {
```

The model makes the interval optional, so a continuation is not obliged to re-state it (OPERATIONS.md:51 lists `SamplingInterval` (integer) among the non-required members, while `ImageSelectorType`, `StartTimestamp`, `EndTimestamp` and `Format` are `required`), and its doc describes it as shaping which instants exist: "The time interval in milliseconds (ms) at which the images need to be generated from the stream." (smithy_query doc, GetImagesInput$SamplingInterval).

No test covers a continuation whose `SamplingInterval` differs from the first page: tests/all/get_images_sampling.rs and tests/all/get_images_request_bounds.rs exercise the interval and the page size, but the cursor checks pinned anywhere are only stream/origin/window.

## Steps to Reproduce

1. Seed a stream with retention and fragments covering a 300-second window.
2. POST /getImages with `ImageSelectorType`, `Format`, `StartTimestamp`/`EndTimestamp` 300 seconds apart and `SamplingInterval: 200`; note the 25 images and the returned `NextToken`.
3. POST /getImages again with the same stream, selector type, format and window, the returned `NextToken`, and NO `SamplingInterval` (or `SamplingInterval: 20000`).
4. Observe HTTP 200 with an empty `Images` list and no `NextToken`: the walk is reported as finished although only 25 of the 1,501 sampled instants of that window were ever returned, instead of the token being refused as not issued for this query.
