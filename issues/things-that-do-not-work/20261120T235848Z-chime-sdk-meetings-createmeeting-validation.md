Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 4f8289a55da0569c35b4094a258c497c237277f1

# CreateMeeting accepts an ExternalMeetingId outside the character pattern its own member documentation states

_Filed: 2026-11-20T23:58:48Z_
_Operation: CreateMeeting | Class: validation_

> _Note: this issue was identified against the code base at commit `4f8289a55da0569c35b4094a258c497c237277f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateMeetingRequest$ExternalMeetingId` carries TWO rules in its member documentation: a character pattern, and the reserved `aws:` prefix. The handler implements only the second one. The `ExternalMeetingId` shape itself carries no `@pattern` trait (only `@length(2..64)` and `@sensitive`), so the framework's boundary validator cannot enforce the pattern either — it exists nowhere in the request path. The result is that a value the documentation forbids (any value containing a character outside `[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]`, e.g. `ext*meeting!`) is accepted, persisted on the meeting record, and read back by `GetMeeting`.

`CreateMeetingWithAttendees` reaches the same gap through the same path (`create_meeting_with_attendees.rs:216-219` calls the same helper and nothing else), so correcting the one helper closes both.

## Evidence

Smithy, `smithy_query(query="doc",shape="CreateMeetingRequest",member="ExternalMeetingId")`:

> "<p>The external meeting ID.</p>\n <p>Pattern: <code>[-_&@+=,(){}\\[\\]\\/«».:|'\"#a-zA-Z0-9À-ÿ\\s]*</code>\n </p>\n <p>Values that begin with <code>aws:</code> are reserved. You can't configure a value that uses this prefix. Case insensitive.</p>"

The pattern is PROSE only — the shape declares no `@pattern` trait, so nothing in the framework applies it; `smithy_query(query="shape",shape="ExternalMeetingId")`:

> `{"shape_id": "com.amazonaws.chimesdkmeetings#ExternalMeetingId", "type": "string", "length": {"min": 2, "max": 64}, "sensitive": true}`

Handler — the ONLY check applied to the member, the second sentence of that same paragraph, with no pattern check anywhere, `services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_meeting.rs:206-213`:

```rust
        // The reserved-prefix rule the model states on `ExternalMeetingId`, applied before
        // anything is resolved, minted or stored.
        if let Some(refusal) = crate::state::reserved_prefix_refusal(
            crate::state::EXTERNAL_MEETING_ID_MEMBER,
            &input.external_meeting_id,
        ) {
            return Err(CreateMeetingErrorModeled::BadRequestException(refusal).into());
        }
```

The value then goes straight into the stored record, `create_meeting.rs:257`:

```rust
            external_meeting_id: Some(input.external_meeting_id),
```

The crate's own helper implements only the reserved-prefix half of the paragraph, `src/state.rs:151-185`:

> "/// From the model: `CreateAttendeeRequest$ExternalUserId` and\n/// `CreateMeetingRequest$ExternalMeetingId` both document \"Values that begin with `aws:`\n/// are reserved. …\""

and `pub fn reserved_prefix_refusal(member: &str, value: &str) -> Option<BadRequestException>` compares only the first 4 bytes.

Test coverage: `tests/all/external_user_id_rules.rs:236-238` names the pattern but only draws IN-pattern rows from it —

> "/// `ExternalUserId`s spanning the member's own constraints\n/// (`smithy_query(query=\"shape\",shape=\"ExternalUserId\"`): `@length(2..64)`, over the\n/// documented pattern `[-_&@+=,(){}\\[\\]\\/«».:|'\"#a-zA-Z0-9À-ÿ\\s]*`)."

— and `external_user_id_rules.rs:617-644` pins only values that are inside the pattern as admitted. No test anywhere in `tests/all/` asserts the outcome for an out-of-pattern value, so nothing pins the current acceptance.

## Steps to Reproduce

1. Start the emulator and point an AWS SDK `chime-sdk-meetings` client at it.
2. CreateMeeting(ClientRequestToken='tok-pattern', MediaRegion='us-east-1', ExternalMeetingId='ext*meeting!') — the value is length 13 (inside `@length(2..64)`) and does not begin with `aws:`, but `*` and `!` are outside the documented pattern `[-_&@+=,(){}\[\]\/«».:|'"#a-zA-Z0-9À-ÿ\s]`.
3. Expected per the member documentation: the value is not configurable, i.e. the modelled `BadRequestException` (HTTP 400).
4. Actual: HTTP 200 with the meeting created and `Meeting.ExternalMeetingId == 'ext*meeting!'`.
5. GetMeeting(MeetingId=<minted id>) reads the out-of-pattern value back, so the value is persisted and not merely echoed.
6. Repeat step 2 against `POST /meetings?operation=create-attendees` (CreateMeetingWithAttendees) for the same outcome through the second create path.
