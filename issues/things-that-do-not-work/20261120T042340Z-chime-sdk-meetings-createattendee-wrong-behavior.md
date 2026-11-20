Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 2dffbaf0282f4917a3251b89aa2df32b6656f627

# CreateAttendee judges Capabilities before resolving the ExternalUserId replay, so a retry the batch paths serve is refused 400

_Filed: 2026-11-20T04:23:40Z_
_Operation: CreateAttendee | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `2dffbaf0282f4917a3251b89aa2df32b6656f627`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateAttendee` resolves the documented `ExternalUserId` idempotency replay (returning the attendee the first call minted and explicitly NOT applying the capabilities this call carried) but it runs the `capability_problem` judgement BEFORE that replay branch. So a repeated create for an identity the meeting already holds is refused with HTTP 400 whenever the request carries a capability set the meeting's `MeetingFeatures` do not admit — even though the handler would have discarded that set anyway (the computed `capabilities` value is unused on the `existing` path, so the refusal protects nothing).

The crate's other two creation paths order the same two steps the other way: `BatchCreateAttendee` and `CreateMeetingWithAttendees` resolve the existing attendee first and `continue`, so the identical item answers 200 with the stored attendee and produces no entry in `Errors`. `BatchCreateAttendee`'s own comment claims the two agree. One request therefore gets two different answers depending on which of the three creation operations it is sent through.

## Evidence

services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_attendee.rs:205-232 — the replay is resolved, then the capability check runs BEFORE it is used:
```
        let existing = meeting
            .attendee_with_external_user_id(&input.external_user_id)
            .cloned();

        let capabilities = match input.capabilities {
            Some(requested) => {
                if let Some(problem) =
                    crate::state::capability_problem(&requested, features.as_ref())
                {
                    return Err(CreateAttendeeErrorModeled::BadRequestException(
                        crate::state::bad_request(problem),
                    )
                    .into());
                }
...
        // The repeat of a create this meeting already served: the stored attendee is
        // answered as it stands, nothing is minted, and the capabilities this call carried
        // do not replace the ones the attendee holds — changing them is
        // `UpdateAttendeeCapabilities`.
        if let Some(existing) = existing {
            return Ok(CreateAttendeeOutput {
                attendee: Some(existing),
            });
        }
```

services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/batch_create_attendee.rs:212-227 — the opposite order, under a comment asserting parity with the single call:
```
            // `ExternalUserId` is an idempotency token, so an identity that already has an
            // attendee in this meeting is reported with THAT attendee and nothing is
            // minted — the same resolution a single `CreateAttendee` makes.
            let existing = store
                .meetings
                .get(&input.meeting_id)
                .and_then(|stored| stored.attendee_with_external_user_id(&item.external_user_id))
                .cloned();
            if let Some(existing) = existing {
                created.push(existing);
                continue;
            }

            let capabilities = match item.capabilities {
                Some(requested) => {
                    match crate::state::capability_problem(&requested, features.as_ref()) {
```
services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/create_meeting_with_attendees.rs:336-350 carries the same replay-first order as the batch.

Test coverage: tests/all/external_user_id_rules.rs:258 (`repeating_a_create_for_one_external_identity_answers_the_same_attendee`) pins the replay for a request carrying NO `Capabilities`, and tests/all/attendee_capabilities.rs covers capability refusals on `UpdateAttendeeCapabilities`; no test covers a replay that carries an inadmissible set, which is why the divergence is unnoticed.

## Steps to Reproduce

1. CreateMeeting with `MeetingFeatures: {"Video": {"MaxResolution": "None"}}` and note the `MeetingId`.
2. POST /meetings/{MeetingId}/attendees with `{"ExternalUserId": "user-a"}` → 200, attendee minted with the documented default capabilities.
3. Repeat POST /meetings/{MeetingId}/attendees with `{"ExternalUserId": "user-a", "Capabilities": {"Audio": "SendReceive", "Video": "SendReceive", "Content": "None"}}` → the handler answers 400 BadRequestException (capability check at create_attendee.rs:209-222) instead of the stored attendee.
4. Send the same item through POST /meetings/{MeetingId}/attendees?operation=batch-create with `{"Attendees": [{"ExternalUserId": "user-a", "Capabilities": {"Audio": "SendReceive", "Video": "SendReceive", "Content": "None"}}]}` → 200, the stored attendee in `Attendees` and an empty `Errors`.
5. Compare: one request, two answers, for the operation pair whose comment claims one resolution.
