Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 847e0aaa570f6ecce3061bfca704956533b10c93

# GetMedia's chunk framing implements only five of the seven AWS_KINESISVIDEO_* MKV tags its Payload documentation declares: the documented AWS_KINESISVIDEO_ERROR_CODE / AWS_KINESISVIDEO_ERROR_ID pair is never emitted, and the one documented error condition the handler does detect ("Requested fragment is not found") is reported with none of it

_Filed: 2026-11-24T16:40:55Z_
_Operation: GetMedia | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `847e0aaa570f6ecce3061bfca704956533b10c93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetMediaOutput$Payload` documents TWO groups of Matroska tags the returned chunks carry: the five metadata tags (CONTINUATION_TOKEN, MILLIS_BEHIND_NOW, FRAGMENT_NUMBER, SERVER_TIMESTAMP, PRODUCER_TIMESTAMP) and, separately, "The following tags will be present if an error occurs" — AWS_KINESISVIDEO_ERROR_CODE and AWS_KINESISVIDEO_ERROR_ID — over an enumerated list of codes that includes "4000 - Requested fragment is not found".

This crate implements the first group and none of the second. `kvs_tags_element` (src/ops/get_media.rs:422-462) writes exactly five `SimpleTag`s, and its own doc restates the model as if the documented set were five tags rather than seven: "carrying the five `AWS_KINESISVIDEO_*` values `GetMediaOutput$Payload` documents". The handler's framing comment asserts the tag framing IS part of this operation's contract (src/ops/get_media.rs:338-343), and the handler's module doc enumerates the places this emulator deliberately "stops short" — throttling and the stream-level/expired-token `NotAuthorizedException` (src/ops/get_media.rs:28-34) — without naming the error-tag half, so the omission is not an admitted gap but an unnoticed one. A grep of the whole crate finds no occurrence of `AWS_KINESISVIDEO_ERROR_CODE`, `AWS_KINESISVIDEO_ERROR_ID` or the code 4000 in src/ or tests/.

The omission is reachable, not hypothetical: the one documented error condition this emulator actually detects is the fragment the start selector requested not being on the stream, which the handler answers from src/ops/get_media.rs:659-674 as a bare `InvalidArgumentException` whose message is emulator-authored ("Fragment number {after} is not on stream {name}."), carrying neither the documented error-code string nor the documented integer id the model assigns that condition (4000). A client written against the documented payload contract — one that reads AWS_KINESISVIDEO_ERROR_CODE / AWS_KINESISVIDEO_ERROR_ID to learn why a GetMedia session stopped — can never observe either tag from this emulator under any input.

This is a different gap from the two already filed against this handler: the fixed wrong-behavior issue (20261123T224652Z) was about which fragment a shared-timestamp selector starts at, and the open error-response issue (20261124T155739Z-...--r2) is about the WORDING of the fragment-number message leaking the caller's value and the stream's registered name. Neither touches the documented error-tag half of `GetMediaOutput$Payload`, which no issue, comment or test in this crate mentions.

## Evidence

Model text (smithy_query query="doc" shape="GetMediaOutput" member="Payload"), the second half of the payload contract:

  "The following tags will be present if an error occurs:"
  "AWS_KINESISVIDEO_ERROR_CODE - String description of an error that caused GetMedia to stop."
  "AWS_KINESISVIDEO_ERROR_ID: Integer code of the error."
  "The error codes are as follows: ... 4000 - Requested fragment is not found ..."

The crate's writer, which emits five tags and nothing else — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:419-451:

    /// The `Tags` element Kinesis Video Streams appends to one returned chunk, carrying
    /// the five `AWS_KINESISVIDEO_*` values `GetMediaOutput$Payload` documents. Both
    /// timestamps are epoch milliseconds, the unit the service reports them in.
    fn kvs_tags_element(
    ...
        for (name, value) in [
            ("AWS_KINESISVIDEO_FRAGMENT_NUMBER", fragment_number.to_string()),
            ("AWS_KINESISVIDEO_SERVER_TIMESTAMP", server_timestamp_millis.to_string()),
            ("AWS_KINESISVIDEO_PRODUCER_TIMESTAMP", producer_timestamp_millis.to_string()),
            ("AWS_KINESISVIDEO_CONTINUATION_TOKEN", continuation_token.to_string()),
            ("AWS_KINESISVIDEO_MILLIS_BEHIND_NOW", millis_behind_now.to_string()),
        ] {

The crate's own claim that this framing is the contract — src/ops/get_media.rs:338-343:

    // `GetMediaOutput$Payload` is documented as "a sequence of chunks from the specified
    // stream" whose chunks "also include the following additional Matroska (MKV) tags:
    // AWS_KINESISVIDEO_CONTINUATION_TOKEN, AWS_KINESISVIDEO_MILLIS_BEHIND_NOW,
    // AWS_KINESISVIDEO_FRAGMENT_NUMBER, AWS_KINESISVIDEO_SERVER_TIMESTAMP,
    // AWS_KINESISVIDEO_PRODUCER_TIMESTAMP". That is the only place the continuation token
    // reaches the client, so the framing is part of this operation's contract.

The handler's scaffolded copy of the model doc is clipped at exactly the point the error tags begin — src/ops/get_media.rs:113-119: "...AWS_KINESISVIDEO_MILLIS_BEHIND_NOW (UTF-8 string) - Client applications can use this tag value to determine how far behind the chunk returned in the response is from the latest chunk on the stream. …" — which is why the second group was never implemented.

The list of gaps this handler DOES admit, which does not include this one — src/ops/get_media.rs:28-34:

    //! Where this emulator stops short of the real service, it stops honestly: a real
    //! `GetMedia` holds the connection open and keeps streaming new fragments, while this
    //! one ends the session at the newest fragment persisted when the call arrived and
    //! hands back the token that resumes there. The throttling
    //! (`ClientLimitExceededException`, `ConnectionLimitExceededException`) behaviours, and the
    //! stream-level/expired-token refusal the modelled `NotAuthorizedException` (401) carries,
    //! are not modelled here and are filed as their own gap issues.

The documented condition the handler detects and answers without any error tag — src/ops/get_media.rs:659-674:

                "FRAGMENT_NUMBER" => {
                    let after = selector.after_fragment_number.as_deref().unwrap_or_default();
                    fragments
                        .iter()
                        .position(|fragment| fragment.fragment_number == after)
                        .map(|index| index + 1)
                        .ok_or_else(|| {
                            invalid_argument(format!(
                                "Fragment number {after} is not on stream {}.",
                                stream.name
                            ))
                        })?

Test coverage: the inverse reader in tests/all/get_media.rs reads only the five metadata tags (`chunk.tag("AWS_KINESISVIDEO_FRAGMENT_NUMBER")`, tests/all/get_media.rs:1076-1079; `chunk.tag("AWS_KINESISVIDEO_CONTINUATION_TOKEN")`, :1086-1089) and `split_payload` asserts the payload ends immediately after the last chunk's single Tags element (tests/all/get_media.rs:308-314), so nothing exercises or pins an error-tag element. The not-found row asserts only status and code — tests/all/get_media.rs:817-828: `("a fragment number that is not on the stream", ... 400, "InvalidArgumentException")`. A repo-wide grep for `AWS_KINESISVIDEO_ERROR_CODE`, `AWS_KINESISVIDEO_ERROR_ID` and the documented code 4000 in this crate returns no match outside an unrelated timecode literal (tests/all/put_media_ingest.rs:825).

## Steps to Reproduce

1. Read the full Payload contract: smithy_query(query="doc", shape="GetMediaOutput", member="Payload") — it documents five metadata tags AND, under "The following tags will be present if an error occurs", AWS_KINESISVIDEO_ERROR_CODE and AWS_KINESISVIDEO_ERROR_ID over a code list that includes "4000 - Requested fragment is not found".
2. Open services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:419-462 and note `kvs_tags_element` writes exactly five SimpleTags, its doc describing them as "the five `AWS_KINESISVIDEO_*` values `GetMediaOutput$Payload` documents".
3. Grep the crate for AWS_KINESISVIDEO_ERROR_CODE / AWS_KINESISVIDEO_ERROR_ID: no src/ or tests/ occurrence — no code path can ever emit either tag.
4. Create a stream through the control plane and ingest fragments 1..4 through POST /putMedia at the announced write endpoint, then POST /getMedia at the announced read endpoint with {"StreamName":"probe","StartSelector":{"StartSelectorType":"FRAGMENT_NUMBER","AfterFragmentNumber":"987654321"}} — the documented "Requested fragment is not found" condition.
5. Observe the answer is a 400 InvalidArgumentException whose Message is "Fragment number 987654321 is not on stream probe.", carrying neither the documented error-code string nor its documented integer id (4000), and that no response of this handler — success or failure — ever carries the AWS_KINESISVIDEO_ERROR_CODE / AWS_KINESISVIDEO_ERROR_ID tags the model declares for an error during GetMedia.
