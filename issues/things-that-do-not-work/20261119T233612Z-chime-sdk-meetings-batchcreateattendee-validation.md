Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f667faacbc6ecad8870388d49cf0b2c06890cb64

# BatchCreateAttendee skips the documented capability rejection for an ExternalUserId the meeting already holds

_Filed: 2026-11-19T23:36:12Z_
_Operation: BatchCreateAttendee | Class: validation_

> _Note: this issue was identified against the code base at commit `f667faacbc6ecad8870388d49cf0b2c06890cb64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

In `BatchCreateAttendee` the `ExternalUserId` idempotency shortcut runs BEFORE the capability rules are judged, so an item whose `Capabilities` the meeting does not admit is reported as a SUCCESS (the stored attendee is pushed onto `Attendees`) instead of being refused in `Errors` — provided that identity already has an attendee in the meeting. `CreateAttendee`, in the same crate, orders the two the other way round: it judges `capability_problem` first and only then takes the idempotency shortcut, so the very same item is refused with HTTP 400 `BadRequestException`.

Concretely, for a meeting created with `MeetingFeatures.Video.MaxResolution: None`:
- `CreateAttendee{ExternalUserId: "u1", Capabilities: {Audio: SendReceive, Video: SendReceive, Content: None}}` on an identity that already has an attendee → 400 BadRequestException ("Attendee's video capability must be None if MeetingFeatures is configured with Video:MaxResolution:None").
- `BatchCreateAttendee` with the byte-identical item → 200 with that item listed in `Attendees` and an EMPTY `Errors`, i.e. a request that includes `SendReceive` for `AttendeeCapabilities:Video` is not rejected at all.

The batch handler's own comment claims the two resolve identically, which is the inconsistency. The same ordering exists in `CreateMeetingWithAttendees` (filed separately), where it bites when one `ExternalUserId` appears twice in one request.

## Evidence

Smithy (`smithy_query(query="shape",shape="CreateAttendeeRequestItem")` → `Capabilities` targets `AttendeeCapabilities`, whose own documentation states the rule):

  "If you specify MeetingFeatures:Video:MaxResolution:None when you create a meeting, all API requests that include SendReceive, Send, or Receive for AttendeeCapabilities:Video will be rejected with ValidationError 400."
  (same sentence for `AttendeeCapabilities:Content`)

Repo side 1 — src/ops/create_attendee.rs:209-232 judges the set BEFORE the idempotency shortcut:
```
209        let capabilities = match input.capabilities {
210            Some(requested) => {
211                if let Some(problem) =
212                    crate::state::capability_problem(&requested, features.as_ref())
213                {
214                    return Err(CreateAttendeeErrorModeled::BadRequestException(
...
228        if let Some(existing) = existing {
229            return Ok(CreateAttendeeOutput {
230                attendee: Some(existing),
```

Repo side 2 — src/ops/batch_create_attendee.rs:212-244 takes the shortcut FIRST, so the set is never judged:
```
212            // `ExternalUserId` is an idempotency token, so an identity that already has an
213            // attendee in this meeting is reported with THAT attendee and nothing is
214            // minted — the same resolution a single `CreateAttendee` makes.
215            let existing = store
...
220            if let Some(existing) = existing {
221                created.push(existing);
222                continue;
223            }
225            let capabilities = match item.capabilities {
226                Some(requested) => {
227                    match crate::state::capability_problem(&requested, features.as_ref()) {
```
The comment at :214 ("the same resolution a single `CreateAttendee` makes") is contradicted by the opposite ordering at create_attendee.rs:209-232.

Test coverage: tests/all/capability_refusal_message.rs:328-399 pins the 400 for `CreateAttendee`/`UpdateAttendeeCapabilities`/`BatchUpdateAttendeeCapabilitiesExcept` but only with a FRESH `ExternalUserId` (`format!("user-{index}-{audio}-{video}-{content}")`, line 379); tests/all/external_user_id_rules.rs:294-327 exercises the batch idempotency shortcut only with items that carry NO `Capabilities`. So no existing test pins the combination, and either ordering fix leaves both tests green.

## Steps to Reproduce

1. CreateMeeting with `MeetingFeatures: {"Video": {"MaxResolution": "None"}}`
2. CreateAttendee with `ExternalUserId: "u1"` and no `Capabilities` → 200, attendee stored
3. CreateAttendee again with `ExternalUserId: "u1"` and `Capabilities: {"Audio":"SendReceive","Video":"SendReceive","Content":"None"}` → 400 BadRequestException (create_attendee.rs:211-218)
4. POST /meetings/{MeetingId}/attendees?operation=batch-create with the single item `{"ExternalUserId":"u1","Capabilities":{"Audio":"SendReceive","Video":"SendReceive","Content":"None"}}` → 200, the item appears in `Attendees` and `Errors` is empty (batch_create_attendee.rs:215-223 returns before the check at :227)
