Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: d83b8dd007c95a50c7612e04a2c8b0990b29e692

# ListFragments honours a NextToken minted by GetImages: the two operations share one untagged cursor, and only the stream name is checked

_Filed: 2026-11-22T19:12:06Z_
_Operation: ListFragments | Class: validation_

> _Note: this issue was identified against the code base at commit `d83b8dd007c95a50c7612e04a2c8b0990b29e692`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both paginated operations of this service mint their `NextToken` with the SAME `FragmentCursor::encode`, producing the same five JSON keys with nothing that says which operation issued it — and the cursor struct's own documentation admits its `offset` means two different things ("matching fragments for a fragment listing, sampled instants for an image page"). `FragmentCursor::decode` therefore cannot tell the two apart.

`ListFragments` then validates only that the token's stream name equals the resolved stream. A `NextToken` a `GetImages` response minted for the same stream is consequently accepted as a fragment cursor: its `offset` (a count of sampled image instants, which with a 200 ms interval reaches the hundreds over one window) and its `start_millis`/`end_millis`/`origin` (the image window) are used as the fragment page's own selector and starting position. The result is a 200 carrying the wrong page — typically an empty `Fragments` list with no `NextToken`, i.e. "this stream has no more fragments" — instead of the `InvalidArgumentException` such a token deserves.

The sibling handler shows the check this one lacks: `GetImages` re-validates the cursor against the request's origin and window before using its offset; `ListFragments` validates the stream name alone.

## Evidence

The model says which token is admissible here — smithy_query doc, ListFragmentsInput$NextToken: "A token to specify where to start paginating. This is the ListFragmentsOutput$NextToken from a previously truncated response." The other operation's member names its own token: GetImagesInput$NextToken — "This is the `GetImages:NextToken` from a previously truncated response."

One cursor type serves both, and its doc admits the dual meaning — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/state.rs:693-695:
```
    /// How many results earlier pages already returned: matching fragments for a
    /// fragment listing, sampled instants for an image page.
    pub offset: usize,
```
with one untagged encoding for both — src/state.rs:705-713:
```
    pub fn encode(&self) -> String {
        let json = ::serde_json::json!({
            "s": self.stream_name,
            "o": self.origin.as_str(),
            "f": self.start_millis,
            "t": self.end_millis,
            "i": self.offset,
        });
```
`GetImages` mints exactly that shape for an image page — src/ops/get_images.rs:555-563:
```
            let next_token = (next_offset < instants.len()).then(|| {
                FragmentCursor {
                    stream_name: stream.name.clone(),
                    origin,
                    start_millis,
                    end_millis,
                    offset: next_offset,
                }
                .encode()
```
and `ListFragments` accepts any decodable token whose stream matches — src/ops/list_fragments.rs:265-276:
```
        let cursor = match input.next_token.as_deref() {
            Some(token) => {
                let Some(cursor) = FragmentCursor::decode(token) else {
                    return Err(invalid("NextToken is not a valid pagination token".to_string()));
                };
                if cursor.stream_name != stream.name {
                    return Err(invalid(
                        "NextToken was issued for a different stream".to_string(),
                    ));
                }
                cursor
```
while the sibling checks three more fields before trusting the offset — src/ops/get_images.rs:500-509:
```
                    if cursor.stream_name != stream.name
                        || cursor.origin != origin
                        || cursor.start_millis != start_millis
                        || cursor.end_millis != end_millis
                    {
                        return Err(invalid(
                            "NextToken was issued for a different stream or timestamp range"
                                .to_string(),
                        ));
```

Test coverage: tests/all/archived_media_read_path.rs drives ListFragments pagination with its own tokens and tests/all/refusal_message_wording.rs pins the addressing refusals; no test presents a token minted by the other paginated operation, so nothing pins the current acceptance.

## Steps to Reproduce

1. Seed a stream with retention and several archived fragments inside one 300-second window.
2. POST /getImages for that stream with `SamplingInterval: 200` and that window; take the `NextToken` from the response (its `offset` counts sampled instants, e.g. 25).
3. POST /listFragments for the SAME stream passing that `NextToken` (and no `FragmentSelector`).
4. Observe HTTP 200: the token is accepted, the image window and sample offset are used as the fragment cursor, and the page is wrong — typically `Fragments: []` with no `NextToken`, reporting the fragment listing as exhausted — instead of `InvalidArgumentException` for a token this operation never issued.
