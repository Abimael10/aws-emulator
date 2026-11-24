Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 2467eda56f564b6aee6366b263dced9add6c2085

# NotificationsConfiguration is persisted but no meeting or attendee event is ever delivered to its SNS/SQS/Lambda target

_Filed: 2026-11-24T06:36:18Z_
_Operation: CreateMeeting | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `2467eda56f564b6aee6366b263dced9add6c2085`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateMeeting` (and `CreateMeetingWithAttendees`) accept the modelled `NotificationsConfiguration` and copy it onto the stored meeting, but the field is WRITE-ONLY in this crate: nothing ever reads `StoredMeeting::notifications_configuration`, and no handler on any meeting or attendee lifecycle path (`CreateAttendee`, `BatchCreateAttendee`, `DeleteAttendee`, `DeleteMeeting`, `StartMeetingTranscription`, `StopMeetingTranscription`) publishes anything to the `SnsTopicArn`, `SqsQueueArn` or `LambdaFunctionArn` the caller supplied.

The model states what the member is FOR: "The configuration for resource targets to receive notifications when meeting and attendee events occur." A client that creates a meeting with an SQS queue ARN, then creates and deletes attendees and finally deletes the meeting, observes an empty queue — the emulator reports 200/204 for every call and the configured targets never receive anything, so a workflow built on meeting/attendee notifications cannot be exercised at all. The crate's own state comment concedes the delivery is AWS's and not this emulator's ("the targets AWS would publish meeting and attendee events to"), and the field is kept only because "it belongs to the meeting".

The workspace already has the cross-service publishing seams this needs (`services/common/src/sns_client.rs`, `services/common/src/sqs_client.rs`), so the targets are reachable from a handler; nothing in this crate calls them.

## Evidence

Smithy, `smithy_query(query="shape",shape="NotificationsConfiguration")` — the shape's purpose and its three targets:
> "doc": "<p>The configuration for resource targets to receive notifications when meeting and attendee events occur.</p>"
> members: `LambdaFunctionArn` — "<p>The ARN of the Amazon Web Services Lambda function in the notifications configuration.</p>", `SnsTopicArn` — "<p>The ARN of the SNS topic.</p>", `SqsQueueArn` — "<p>The ARN of the SQS queue.</p>"

And the input member doc carried into the handler, services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_meeting.rs:77-78:
```
//! * `NotificationsConfiguration` (NotificationsConfiguration) — The configuration for
//!   resource targets to receive notifications when meeting and attendee events occur.
```

The only thing the handler does with it — store it, services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_meeting.rs:269-281:
```
        store.meetings.insert(
            meeting_id.clone(),
            crate::state::StoredMeeting {
                meeting,
                attendees: ::std::collections::BTreeMap::new(),
                notifications_configuration: input.notifications_configuration,
```
(the same single write in services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_meeting_with_attendees.rs:302).

The stored field's own comment, services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/state.rs:428-431 — it says the publishing is AWS's, not this emulator's:
```
    /// `NotificationsConfiguration` as supplied: the targets AWS would publish meeting
    /// and attendee events to. Kept because it belongs to the meeting, and reported by no
    /// operation — the `Meeting` shape has no member for it.
    pub notifications_configuration: Option<NotificationsConfiguration>,
```

Proof it has no reader: `grep -n 'notifications_configuration|NotificationsConfiguration|SnsTopicArn|SqsQueueArn|LambdaFunctionArn'` over the whole crate returns only the shape definition (src/ops/types/shared_types.rs:127-129), the two generated input structs (src/ops/types/create_meeting_types.rs:21, create_meeting_with_attendees_types.rs:22), the two writes above and the state field declaration. No handler, helper or test mentions it.

Seams that exist in the workspace but are never called from this crate: services/common/src/sns_client.rs, services/common/src/sqs_client.rs.

Test coverage: none — no file under tests/all/ sends `NotificationsConfiguration` at all, so nothing pins the current silence (closest lifecycle tests are tests/all/meeting_attendee_lifecycle.rs:261 `create_meeting_persists_a_meeting_derived_from_its_minted_id` and :1049 `deleting_removes_exactly_what_it_names`).

## Steps to Reproduce

1. Create an SQS queue in the emulator and note its ARN.
2. POST /meetings with {"ClientRequestToken":"tok-notify","MediaRegion":"us-east-1","ExternalMeetingId":"ext-notify","NotificationsConfiguration":{"SqsQueueArn":"<that queue ARN>"}} — 200, meeting minted.
3. POST /meetings/{MeetingId}/attendees with {"ExternalUserId":"user-1"} — 200, attendee minted (a 'meeting and attendee event').
4. DELETE /meetings/{MeetingId}/attendees/{AttendeeId} — 204, then DELETE /meetings/{MeetingId} — 204.
5. ReceiveMessage on the configured queue: expected per the member documentation ('resource targets to receive notifications when meeting and attendee events occur') at least one notification for the attendee/meeting lifecycle; actual: the queue is empty for every step, because `notifications_configuration` is written at create_meeting.rs:274 and read by nothing in the crate.
