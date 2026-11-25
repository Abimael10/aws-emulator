Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 3198778d4a31e10d6693ef53b9d76ae067021adb

# ListFragments accepts a GetImages NextToken as a continuation, serving an offset computed over a differently shaped result set

_Filed: 2026-11-25T09:18:28Z_
_Operation: ListFragments | Class: pagination_

> _Note: this issue was identified against the code base at commit `3198778d4a31e10d6693ef53b9d76ae067021adb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both paginated operations mint and decode the SAME cursor type (crate::state::FragmentCursor), and the cursor records nothing about which query minted it. GetImages guards the one member that distinguishes a sampled-instant page from a fragment listing — it rejects a cursor whose interval_millis is below the documented sampling floor — but ListFragments never looks at cursor.interval_millis at all. So a NextToken minted by GetImages (which indexes into the list of SAMPLED INSTANTS of a window) decodes cleanly in ListFragments and is used as an offset into the list of MATCHING FRAGMENTS: a continuation that carries only StreamName and that token (legal, since FragmentSelector "is only required when the NextToken isn't passed in the API") silently skips N fragments and reports the listing truncated or complete at the wrong point. The reverse also succeeds: a ListFragments token (interval_millis = None) replayed to GetImages is read as "the query named no interval", collapsing the instant list to the window's start alone, so an offset past it is answered 200 with an empty Images list and no NextToken. The state module states the rule both copies are supposed to uphold and this breaks.

## Evidence

The rule, src/state.rs:927-938 (FragmentCursor doc):
```
/// Self-describing, so a replayed token continues where the previous page
/// stopped without the service holding per-token server state: it carries
/// EVERY request member that determines the result set its offset indexes into
... A token "specifies where to start paginating the next set"
/// of ONE query's results, so a continuation is served against the set the
/// token names, never against a differently shaped one.
```
and src/ops/list_fragments.rs:139-146: "The refusal a `NextToken` replayed against another query carries. The token is issued for ONE listing...".

The sibling check GetImages applies, src/ops/get_images.rs:514-520:
```
                    let minted = FragmentCursor::decode(token).filter(|cursor| {
                        cursor
                            .interval_millis
                            .is_none_or(|millis| millis >= MIN_SAMPLING_INTERVAL_MILLIS)
                    });
```
The ListFragments decode path, src/ops/list_fragments.rs:277-296, which never reads `interval_millis`:
```
            Some(token) => {
                let Some(cursor) = FragmentCursor::decode(token) else {
                    return Err(invalid("NextToken is not a valid pagination token".to_string()));
                };
                if cursor.stream_name != stream.name {
                    return Err(invalid(TOKEN_FOR_ANOTHER_QUERY_MESSAGE.to_string()));
                }
                if let Some(selector) = input.fragment_selector.as_ref() {
```
and the offset it then uses, src/ops/list_fragments.rs:328-338 (`.skip(cursor.offset).take(page_size)`).

The interval is the only member that distinguishes the two sets, and it is OPTIONAL on the wire for GetImages too (src/state.rs:949-956: "[`None`] for a fragment listing ... and for an image page whose query named no interval"), so a GetImages token minted with no SamplingInterval is indistinguishable from a listing token by construction.

Test coverage: cross-operation replay is untested — tests/all/get_images_generation.rs:1300-1420 only replays GetImages tokens against GetImages (`a_continuation_that_names_another_set_than_its_token_is_refused`), and the ListFragments token tests never construct a cursor carrying an interval.

## Steps to Reproduce

1. Seed a stream with several archived fragments (state::put_stream / append_fragment) and a window that holds them.
2. POST /getImages with that stream, SamplingInterval=1000, MaxResults=1 over the window; keep the returned NextToken (a cursor whose offset counts SAMPLED INSTANTS and whose interval_millis is 1000).
3. POST /listFragments with only StreamName and that NextToken (no FragmentSelector — legal per the member documentation).
4. Observe a 200 whose Fragments list starts `offset` fragments into the listing (src/ops/list_fragments.rs:277-338) instead of the refusal src/ops/list_fragments.rs:139-146 and src/state.rs:927-938 describe for a token issued for another query.
5. Reverse direction: take a ListFragments NextToken and POST /getImages with the same stream, selector and window — the cursor's interval_millis of None collapses the instant list to the window's start (src/ops/get_images.rs:535, 553) and the call is answered 200 with an empty Images list.
