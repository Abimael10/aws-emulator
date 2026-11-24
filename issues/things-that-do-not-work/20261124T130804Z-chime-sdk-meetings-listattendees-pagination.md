Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 07bdfbdeb1a9903f81cb3ce584922eadb4392cb8

# A NextToken this listing never issued is accepted, although the crate states such a token must be refused

_Filed: 2026-11-24T13:08:04Z_
_Operation: ListAttendees | Class: pagination_

> _Note: this issue was identified against the code base at commit `07bdfbdeb1a9903f81cb3ce584922eadb4392cb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListAttendees` validates its `next-token` only by hex-decoding it and checking a fixed literal prefix, so ANY value a client constructs as `hex("attendee:" + <anything>)` is accepted and used as a page boundary. Two consequences follow, both of which the crate's own comments say must not happen:

1. A token the service never issued is SERVED instead of refused. The handler's refusal branch only catches tokens of the wrong shape (non-hex, or hex without the prefix); a token a caller assembled itself from the one hard-coded prefix and an `AttendeeId` it already knows passes `decode_page_token` and silently resumes the listing at that id. `crate::state::encode_page_token` states the opposite requirement in its own doc comment ("a client must pass back what it was handed, not construct one"), and `PAGE_TOKEN_PREFIX` exists expressly "so a token from somewhere else is refused rather than read as an `AttendeeId`" — but a constructed token is exactly "a token from somewhere else" and is read as an `AttendeeId`.

2. Nothing in the token binds it to the meeting whose listing issued it, so a `NextToken` returned by `ListAttendees` on meeting A is accepted by `ListAttendees` on meeting B and silently returns a SUFFIX of meeting B's attendees (every attendee of B whose id sorts at or after A's boundary id) with HTTP 200. The caller is handed an incomplete listing with no indication that anything was skipped — the handler's own stated alternative ("refuse it rather than silently restarting") was chosen precisely to avoid silently wrong listings.

The defect is in the acceptance check, not in the token's encoding: the handler claims to distinguish tokens this service issued from tokens it did not, and it does not make that distinction.

## Evidence

The rule the crate states for this token, `src/ops/list_attendees.rs:167-181`:
```
        let resume_at = match requested_token {
            None => None,
            Some(token) => match crate::state::decode_page_token(token) {
                Some(attendee_id) => Some(attendee_id),
                // A token this service did not issue carries no page boundary, so the
                // request cannot be served: refuse it rather than silently restarting.
                None => {
                    return Err(ListAttendeesErrorModeled::BadRequestException(
                        crate::state::bad_request(
                            "NextToken is not a token this service issued",
                        ),
                    )
                    .into());
                }
```

The same claim in the state module, `src/state.rs:914-916`:
```
/// Prefix carried inside a pagination token, so a token from somewhere else is refused
/// rather than read as an `AttendeeId`.
const PAGE_TOKEN_PREFIX: &str = "attendee:";
```
and `src/state.rs:918-925`:
```
/// Encode the opaque `NextToken` that resumes a listing AT `next_id`.
///
/// Opaque to the caller (hex of a tagged byte string) because the model gives `NextToken`
/// no structure: a client must pass back what it was handed, not construct one. 2 hex
/// characters per byte keeps it inside the modelled length 0..4096 for any `AttendeeId`.
pub fn encode_page_token(next_id: &str) -> String {
    hex::encode(format!("{PAGE_TOKEN_PREFIX}{next_id}"))
}
```

What the acceptance check actually does — `src/state.rs:927-933` — is the contradicting side: it carries no secret, no signature and no meeting id, so every one of its checks is reproducible by the caller:
```
/// The `AttendeeId` a page token resumes at, or `None` when the token was not one this
/// service issued.
pub fn decode_page_token(token: &str) -> Option<String> {
    let bytes = hex::decode(token).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    text.strip_prefix(PAGE_TOKEN_PREFIX).map(str::to_string)
}
```
The token is then applied to whatever meeting the request names — `src/ops/list_attendees.rs:192-196`:
```
        let (attendees, next_token) = crate::state::page(
            meeting.attendees.iter(),
            resume_at.as_deref(),
            crate::state::page_size(input.max_results),
        );
```

Test coverage pins only the wrong-SHAPE tokens, so the forgeable and cross-meeting cases are untested — `tests/all/meeting_attendee_lifecycle.rs:893-903`:
```
/// A `next-token` this service did not issue is refused rather than silently read as the
/// start of the listing.
#[tokio::test]
async fn list_attendees_refuses_a_next_token_it_did_not_issue() {
    // Rows differ along the shape of the bad token: not hex, hex of the wrong payload,
    // and a bare attendee id.
    let cases: &[(&str, &str)] = &[
        ("not hexadecimal at all", "not-a-token"),
        ("hex of an untagged payload", "6162636465"),
        ("a bare AttendeeId", ABSENT_MEETING_ID),
    ];
```
None of the three rows carries the `attendee:` prefix, so all three take the `None` arm; a fourth row shaped `hex("attendee:<guid>")` would be served 200.

## Steps to Reproduce

1. POST /meetings with {"ClientRequestToken":"tok-A","MediaRegion":"us-east-1","ExternalMeetingId":"ext-A"} and keep MeetingId A.
2. POST /meetings/A/attendees three times with ExternalUserId user-1/user-2/user-3; keep the three AttendeeIds.
3. GET /meetings/A/attendees?next-token=<hex of the ASCII string "attendee:" followed by the LAST AttendeeId in sort order> — a token this service never issued, assembled from the one literal prefix in src/state.rs:916. Expected per src/ops/list_attendees.rs:171-172 ("A token this service did not issue … refuse it"): HTTP 400 BadRequestException. Actual: HTTP 200 with a one-attendee page.
4. Repeat the second meeting: POST /meetings with ClientRequestToken tok-B / ExternalMeetingId ext-B, and add three attendees to MeetingId B.
5. GET /meetings/A/attendees?max-results=1 and keep the returned NextToken (a boundary inside A's attendee ids).
6. GET /meetings/B/attendees?next-token=<that token from meeting A> — expected: the token names no boundary in B's listing, so it is not a token for this listing; actual: HTTP 200 with a silently truncated suffix of B's attendees (and no indication that earlier attendees of B were skipped), because the token carries no meeting id (src/state.rs:923-925).
