Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# GetClip ignores the documented 24-hour bound on ClipTimestampRange$EndTimestamp (only the second half of the same sentence is enforced)

_Filed: 2026-11-22T09:53:03Z_
_Operation: GetClip | Class: validation_

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ClipTimestampRange$EndTimestamp` documents two rules in one sentence: the end must be within 24 hours of `StartTimestamp`, and it must be later than `StartTimestamp`. The handler enforces only the second clause — it quotes that clause in a comment and checks `end_millis <= start_millis` — and never compares the span against 24 hours, so a clip request whose range covers months or years is accepted and served.

The crate enforces the equivalent documented window bound for the sibling operation: `GetImages` reads the same kind of sentence off `EndTimestamp` and refuses a wider window with `InvalidArgumentException` (`MAX_WINDOW_MILLIS`). No comment or test in this crate justifies skipping the clip range's own documented span.

## Evidence

Model (smithy_query doc, shape=ClipTimestampRange, member=EndTimestamp): "This value must be within 24 hours of the specified StartTimestamp, and it must be later than the StartTimestamp value. If FragmentSelectorType for the request is SERVER_TIMESTAMP, this value must be in the past."

The handler takes only the second clause — src/ops/get_clip.rs:393-400:
```
            let start_millis = date_time_millis(&selector.timestamp_range.start_timestamp);
            let end_millis = date_time_millis(&selector.timestamp_range.end_timestamp);
            // `ClipTimestampRange$EndTimestamp` "must be later than the
            // StartTimestamp value", so a range that ends where it starts is as
            // much a refusal as one that ends before it.
            if end_millis <= start_millis {
                return Err(invalid(TIMESTAMP_RANGE_ORDER_MESSAGE.to_string()));
            }
```
Nothing else in the file reads the span: grep for `24 hour|86_400|86400` in the crate matches only tests/all/get_images_window_bound.rs:112 (a GetImages row) and a doc comment in tests/all/archived_media_read_path.rs:30.

The sibling operation DOES enforce its documented window — src/ops/get_images.rs:167-170 and 468-473:
```
/// The widest window one request may span, in milliseconds: `EndTimestamp` is
/// documented as "If the time range between StartTimestamp and EndTimestamp is more
/// than 300 seconds above StartTimestamp, you will receive an IllegalArgumentException."
const MAX_WINDOW_MILLIS: i64 = 300_000;
...
            if end_millis.saturating_sub(start_millis) > MAX_WINDOW_MILLIS {
                return Err(invalid(format!(
```

## Steps to Reproduce

1. Create and activate a stream with non-zero retention and archive at least one fragment in it.
2. POST /getClip with `ClipFragmentSelector.TimestampRange.StartTimestamp` = the fragment's timestamp and `EndTimestamp` = StartTimestamp + 30 days.
3. Expected per the modelled sentence: `InvalidArgumentException` (400), because the end is not within 24 hours of the start.
4. Actual: the range is accepted and a clip is built from the selected fragments.
