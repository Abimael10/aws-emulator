Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 553a1d2e685367944e9839a399c7f24f7975bfe6

# The documented character pattern on ExternalUserId/ExternalMeetingId is never enforced, while the reserved-prefix rule from the same doc paragraph is

_Filed: 2026-11-20T11:56:33Z_
_Operation: CreateAttendee | Class: validation_

> _Note: this issue was identified against the code base at commit `553a1d2e685367944e9839a399c7f24f7975bfe6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model documents TWO rules on this service's caller-supplied external identifiers, in one paragraph: a character PATTERN, and the reserved `aws:` prefix. The crate implements the second rule in a shared helper (`crate::state::reserved_prefix_refusal`) that all four creating handlers call, and implements NOTHING of the first: no handler, and no helper in `state.rs`, ever inspects the characters of `ExternalUserId` or `ExternalMeetingId`.

Neither `ExternalUserId` nor `ExternalMeetingId` carries a Smithy `@pattern` trait (both are `string, length 2..64, sensitive` only), so the boundary validator does not enforce the documented pattern either. The result: `CreateAttendee` (and `BatchCreateAttendee`, `CreateMeeting`, `CreateMeetingWithAttendees`) accept and PERSIST an external identifier containing characters the documentation's pattern excludes — e.g. `user$1`, `a;b`, `x*y`, `back\slash` — and a later `GetAttendee`/`ListAttendees`/`GetMeeting` reports them back as a stored identity. The service's own refusal class for exactly this is modelled: `BadRequestException` (HTTP 400) — "The input parameters don't match the service's restrictions" — and the crate already words it (`crate::state::bad_request`) for the neighbouring sentence of the same paragraph.

So one documented constraint on one member is half-enforced: the sentence after the pattern is checked, the pattern itself is not.

## Evidence

Smithy, `smithy_query(query="shape",shape="Attendee")` → member `ExternalUserId` doc (identical text on `CreateAttendeeRequest$ExternalUserId` and `Meeting$ExternalMeetingId`):

  "<p>The Amazon Chime SDK external user ID. An idempotency token. Links the attendee to an identity managed by a builder application.</p>\n <p>Pattern: <code>[-_&@+=,(){}\\[\\]\\/«».:|'\"#a-zA-Z0-9À-ÿ\\s]*</code></p>\n <p>Values that begin with <code>aws:</code> are reserved. You can't configure a value that uses this prefix. Case insensitive.</p>"

And the shape carries NO pattern trait, so nothing enforces it before the handler: `smithy_query(query="shape",shape="ExternalUserId")` → `{"type": "string", "length": {"min": 2, "max": 64}, "sensitive": true}` (same for `ExternalMeetingId`).

The same paragraph is quoted in the crate, and only its SECOND sentence is implemented — services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/state.rs:151-158:
```
/// The prefix the model RESERVES in the two caller-supplied identifiers of this service.
///
/// From the model: `CreateAttendeeRequest$ExternalUserId` and
/// `CreateMeetingRequest$ExternalMeetingId` both document "Values that begin with `aws:`
/// are reserved. You can't configure a value that uses this prefix. Case insensitive."
pub const RESERVED_IDENTIFIER_PREFIX: &str = "aws:";
```
and src/ops/create_attendee.rs:184-191 is the only check the value gets:
```
        // The reserved-prefix rule the model states on this member, applied BEFORE
        // anything is resolved or minted, so a refused request leaves the store as it was.
        if let Some(refusal) = crate::state::reserved_prefix_refusal(
            crate::state::EXTERNAL_USER_ID_MEMBER,
            &input.external_user_id,
        ) {
            return Err(CreateAttendeeErrorModeled::BadRequestException(refusal).into());
        }
```
The handler's own copy of the contract shows both sentences side by side — src/ops/create_attendee.rs:71-74:
```
//! * `ExternalUserId` (ExternalUserId, len 2..64) **required** — The Amazon Chime SDK
//!   external user ID. An idempotency token. Links the attendee to an identity managed by a
//!   builder application. Pattern: [-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]* Values that
//!   begin with aws: are reserved. You can't configure a value that uses this prefix.
```
The value is stored unchecked at src/ops/create_attendee.rs:259 (`external_user_id: Some(input.external_user_id)`), and the meeting's at src/ops/create_meeting.rs:257 (`external_meeting_id: Some(input.external_meeting_id)`).

Test coverage: tests/all/external_user_id_rules.rs:236-253 enumerates the accepted FORMS and every row is INSIDE the documented pattern ("characters from the documented pattern", `"a.b:c/d_e+f@g|h#i'j"`, line 245-246); no test sends an out-of-pattern character, and no comment or test states a reason for admitting one. Enforcing the pattern therefore leaves every existing test green.

## Steps to Reproduce

1. POST /meetings with {"ClientRequestToken":"tok-1","MediaRegion":"us-east-1","ExternalMeetingId":"meet$1"} → 200: the meeting is created and `GetMeeting` reports `ExternalMeetingId: "meet$1"`, although `$` is outside the documented pattern `[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]*`
2. POST /meetings/{MeetingId}/attendees with {"ExternalUserId":"user*;1"} → 200, the attendee is minted and persisted
3. GET /meetings/{MeetingId}/attendees/{AttendeeId} → 200 reporting `ExternalUserId: "user*;1"`
4. Compare with the sibling rule from the same doc paragraph: POST the same route with {"ExternalUserId":"aws:user"} → 400 BadRequestException (create_attendee.rs:186-191), showing one half of the paragraph is enforced and the other is not
