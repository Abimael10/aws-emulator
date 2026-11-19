Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f667faacbc6ecad8870388d49cf0b2c06890cb64

# CreateMeetingWithAttendees accepts an inadmissible Capabilities set on a repeated ExternalUserId

_Filed: 2026-11-19T23:36:12Z_
_Operation: CreateMeetingWithAttendees | Class: validation_

> _Note: this issue was identified against the code base at commit `f667faacbc6ecad8870388d49cf0b2c06890cb64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateMeetingWithAttendees` resolves the `ExternalUserId` idempotency shortcut before judging the item's `Capabilities` against the meeting's `MeetingFeatures`. When one `ExternalUserId` appears more than once in the request, every occurrence after the first is answered with the already-minted attendee and its own `Capabilities` are never judged — so a set the meeting does not admit is reported in `Attendees` as a success instead of in `Errors`, and the attendee silently keeps the first item's capabilities. `CreateAttendee` in the same crate judges the set first and refuses it with HTTP 400 for the identical member values.

## Evidence

Smithy — `AttendeeCapabilities` documentation (reached from `CreateAttendeeRequestItem$Capabilities`):

  "If you specify MeetingFeatures:Content:MaxResolution:None when you create a meeting, all API requests that include SendReceive, Send, or Receive for AttendeeCapabilities:Content will be rejected with ValidationError 400."

Repo — src/ops/create_meeting_with_attendees.rs:336-367, shortcut before the judgement:
```
336            // `ExternalUserId` is an idempotency token: an identity named twice in the
337            // same request gets ONE attendee, reported for both items.
338            let existing = store
...
343            if let Some(existing) = existing {
344                created.push(existing);
345                continue;
346            }
348            let capabilities = match item.capabilities {
349                Some(requested) => {
350                    match crate::state::capability_problem(&requested, features.as_ref()) {
```
Sibling with the opposite order — src/ops/create_attendee.rs:209-232: `capability_problem` is evaluated at :211-218 and the idempotency shortcut only at :228-232.

Test coverage: tests/all/capability_refusal_message.rs:448-508 drives `CreateMeetingWithAttendees` with DISTINCT `ExternalUserId`s (`format!("item-{index}")`, line 482), and tests/all/external_user_id_rules.rs:365-405 repeats one identity but with no `Capabilities` at all — so the combination is unpinned and a fix keeps both green.

## Steps to Reproduce

1. POST /meetings?operation=create-attendees with `MeetingFeatures: {"Content": {"MaxResolution": "None"}}` and `Attendees: [{"ExternalUserId":"dup"}, {"ExternalUserId":"dup","Capabilities":{"Audio":"SendReceive","Video":"SendReceive","Content":"SendReceive"}}]`
2. Observe 200 with two entries in `Attendees` (the same attendee twice) and an empty `Errors` — the second item's `Content: SendReceive` against `Content:MaxResolution:None` was never judged (create_meeting_with_attendees.rs:343-346)
3. Send the same `Capabilities` through `CreateAttendee` on that meeting: 400 BadRequestException "Attendee's content capability must be None if MeetingFeatures is configured with Content:MaxResolution:None" (create_attendee.rs:211-218)
