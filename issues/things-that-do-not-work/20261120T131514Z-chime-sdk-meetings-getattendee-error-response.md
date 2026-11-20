Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 1e28e5a4dc1c9cfc23bd395bc196ff785ea73c3a

# Answer the modelled NotFoundException, not ForbiddenException, for an AttendeeId that names no attendee

_Filed: 2026-11-20T13:15:14Z_
_Operation: GetAttendee | Class: error-response_

> _Note: this issue was identified against the code base at commit `1e28e5a4dc1c9cfc23bd395bc196ff785ea73c3a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When the meeting exists but the `AttendeeId` the request carries names no attendee of it (never created, or already deleted), the handler answers the modelled `ForbiddenException` (HTTP 403) instead of the modelled `NotFoundException` (HTTP 404).

The model documents what each of those two errors reports. `NotFoundException` is documented as "One or more of the resources in the request does not exist in the system." — which is exactly the situation here: the request carries two resource ids (`MeetingId`, `AttendeeId`) and one of them names nothing. `ForbiddenException` is documented as "The client is permanently forbidden from making the request." — which the handler's own behaviour contradicts: the very same client, with the very same credentials, is served HTTP 200 for `GetAttendee` on another attendee of that meeting and for `ListAttendees` on it, so it is plainly not forbidden from making the request.

The crate already words the "a resource named in the request does not exist" refusal as the modelled 404 for the other id of the SAME request (an absent `MeetingId` in `GetAttendee`/`DeleteAttendee`/`UpdateAttendeeCapabilities` is `NotFoundException`); only the attendee half is turned into a 403. The refusal is minted in one shared helper (`crate::state::attendee_not_in_meeting`), so the same wrong class is served by `GetAttendee`, `DeleteAttendee` and `UpdateAttendeeCapabilities` — fixing the helper fixes all three.

A client branching on the error code sees a permanent authorization failure (403 is not retryable and is commonly surfaced as a credentials problem) where the model says it should see "resource does not exist".

## Evidence

Smithy (`smithy_query(query="shape",shape="NotFoundException,ForbiddenException")`):
- `NotFoundException` — `httpError: 404`, doc: "One or more of the resources in the request does not exist in the system."
- `ForbiddenException` — `httpError: 403`, doc: "The client is permanently forbidden from making the request."
Both are declared on `GetAttendee`, `DeleteAttendee` and `UpdateAttendeeCapabilities` (`smithy_query(query="operation",operation="GetAttendee,DeleteAttendee")`), so the handler is choosing between them.

Handler, `src/ops/get_attendee.rs:168-173`:
```
        let Some(attendee) = meeting.attendees.get(&input.attendee_id) else {
            return Err(GetAttendeeErrorModeled::ForbiddenException(
                crate::state::attendee_not_in_meeting(&input.attendee_id, &input.meeting_id),
            )
            .into());
        };
```
Same class from the two siblings that call the same helper — `src/ops/delete_attendee.rs:188-193` (`if meeting.attendees.remove(&input.attendee_id).is_none() { return Err(DeleteAttendeeErrorModeled::ForbiddenException(...` ) and `src/ops/update_attendee_capabilities.rs:226-231`.

The helper, `src/state.rs:259-262`, states the reasoning the model does not support:
```
/// The refusal is the modelled `ForbiddenException` (HTTP 403, "The client is permanently
/// forbidden from making the request") rather than a 404: the meeting the attendee was
/// looked for in EXISTS, so access to that attendee's record is revoked rather than the
/// request naming an unknown resource
```
The immediately preceding path of the same handler shows the crate's own treatment of "an id in the request names nothing" — `src/ops/get_attendee.rs:158-162`:
```
        let Some(meeting) = store.meetings.get(&input.meeting_id) else {
            return Err(GetAttendeeErrorModeled::NotFoundException(
                store.meeting_absent(&input.meeting_id),
```

Current coverage pins the 403, with an interpretive reason rather than a model citation — `tests/all/not_found_message.rs:352-357`: "When the MEETING exists but the attendee does not, the refusal is the modelled `ForbiddenException` … knows the attendee is gone for good rather than mistyped", and `tests/all/meeting_attendee_lifecycle.rs:996-1023` asserts `(403, "ForbiddenException")` for "GetAttendee on an absent attendee", "DeleteAttendee on an absent attendee" and "GetAttendee through the wrong meeting". Those assertions (and `tests/all/attendee_capabilities.rs:446`) are what must change with the fix.

## Steps to Reproduce

1. POST /meetings with {"ClientRequestToken":"tok-1","MediaRegion":"us-east-1","ExternalMeetingId":"ext-1"} and keep the returned MeetingId.
2. POST /meetings/{MeetingId}/attendees with {"ExternalUserId":"user-1"} and keep the returned AttendeeId.
3. GET /meetings/{MeetingId}/attendees/11111111-2222-3333-4444-555555555555 (a well-formed GuidString that names no attendee) — expected per the model: HTTP 404 `NotFoundException` ("One or more of the resources in the request does not exist in the system"); actual: HTTP 403 `ForbiddenException`.
4. GET /meetings/{MeetingId}/attendees/{AttendeeId} again — HTTP 200, proving the same client is not "permanently forbidden from making the request".
5. DELETE /meetings/{MeetingId}/attendees/{AttendeeId} (204), then repeat the DELETE and the GET — expected HTTP 404 `NotFoundException`; actual HTTP 403 `ForbiddenException` from both (`src/ops/delete_attendee.rs:189`, `src/ops/get_attendee.rs:169`).
6. PUT /meetings/{MeetingId}/attendees/11111111-2222-3333-4444-555555555555/capabilities with a valid Capabilities set — same 403 instead of 404 (`src/ops/update_attendee_capabilities.rs:227`).
