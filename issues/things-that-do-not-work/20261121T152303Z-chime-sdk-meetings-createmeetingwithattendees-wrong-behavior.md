Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 3d06e34e7ae6d921cd5e12708e0a1c71c97a3d75

# CreateMeetingWithAttendees serves a CreateMeeting ClientRequestToken as its own replay, answering 200 with zero attendees and zero errors

_Filed: 2026-11-21T15:23:03Z_
_Operation: CreateMeetingWithAttendees | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `3d06e34e7ae6d921cd5e12708e0a1c71c97a3d75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateMeeting` and `CreateMeetingWithAttendees` write and read ONE idempotency map, `meeting_by_client_request_token`, whose own field documentation scopes it to `CreateMeeting`. So a `CreateMeetingWithAttendees` call whose `ClientRequestToken` was previously used by a plain `CreateMeeting` takes the replay branch: it returns the meeting that `CreateMeeting` made — a meeting the model documents as having *no initial attendees* — with `Attendees: []` and `Errors: []`, and it creates NONE of the 1..20 `@required` attendees the request carried.

The result is a hollow 200: the operation reports success, reports no per-item refusal, and persists nothing of the `Attendees` list it was given. A client that reads `Attendees`/`Errors` to learn which identities joined is told "none asked for, none refused", and no `AttendeeId`/`JoinToken` ever exists for the identities it sent. The handler's own comment justifying the empty `Errors` on a replay rests on a premise that is false in this case — it reasons about "the attendees that call created", but the call that owned the token created none and could not have.

The reverse direction is harmless (a `CreateMeeting` replaying a with-attendees token legitimately gets that meeting back), so the defect is one-sided: it is `CreateMeetingWithAttendees` reading a record that belongs to a different operation.

## Evidence

Smithy — the two operations' own documentation says they produce different things, so one's replay record cannot stand for the other's:
- `smithy_query(query="doc",shape="CreateMeetingWithAttendees")`: "Creates a new Amazon Chime SDK meeting in the specified media Region, **with attendees**."
- `smithy_query(query="doc",shape="CreateMeeting")`: "Creates a new Amazon Chime SDK meeting in the specified media Region **with no initial attendees**."
- `CreateMeetingWithAttendeesRequest$Attendees` is `@required`, `list<CreateAttendeeRequestItem>` len 1..20 (OPERATIONS.md:57), so the request can never legally ask for zero attendees.

Repo side 1 — the store field is documented as `CreateMeeting`'s replay record, src/state.rs:408-412:
```
    /// `ClientRequestToken` -> the `MeetingId` it created, so a replayed `CreateMeeting`
    /// returns the meeting the first call created instead of minting a second one. One
    /// entry per live meeting: `DeleteMeeting` drops the meeting's entry with it, so this
    /// map is bounded by [`MAX_MEETINGS`] too.
    pub meeting_by_client_request_token: HashMap<String, String>,
```

Repo side 2 — `CreateMeeting` is what writes into it, src/ops/create_meeting.rs:266-268:
```
        store
            .meeting_by_client_request_token
            .insert(input.client_request_token.clone(), meeting_id.clone());
```

Repo side 3 — `CreateMeetingWithAttendees` reads that same map as if the entry were its own, and returns before the per-item loop ever runs, src/ops/create_meeting_with_attendees.rs:225-252:
```
        // `ClientRequestToken` is `@idempotencyToken`: a replay returns the meeting the
        // first call created — with the attendees that call created — so a client that
        // retries does not accumulate meetings or duplicate attendees.
...
        // `Errors` is empty on a replay for the same reason. A per-item refusal is a fact
        // about ONE request, not about the meeting
...
        let replayed = store
            .meeting_by_client_request_token
            .get(&input.client_request_token)
            .and_then(|meeting_id| store.meetings.get(meeting_id))
            .map(|stored| {
                (
                    stored.announced_meeting(ctx),
                    stored.attendees.values().cloned().collect::<Vec<Attendee>>(),
                )
            });
        if let Some((meeting, attendees)) = replayed {
            return Ok(CreateMeetingWithAttendeesOutput {
                meeting: Some(meeting),
                attendees: Some(attendees),
                errors: Some(Vec::new()),
            });
        }
```
The comment's premise ("with the attendees that call created") is contradicted by the Smithy sentence for the operation that actually owns the token ("with no initial attendees").

Test coverage: tests/all/create_meeting_with_attendees.rs exercises the replay only with tokens this operation itself issued (the suite's own `"MediaRegion"` rows at :210-312 and the idempotency rows at :469-503); no test uses one token across the two create operations, so no existing assertion pins the current behaviour.

## Steps to Reproduce

1. POST /meetings with {"ClientRequestToken":"tok-1","MediaRegion":"us-east-1","ExternalMeetingId":"ext-1"} → 200, a meeting with no attendees is stored and `tok-1` is recorded in `meeting_by_client_request_token` (src/ops/create_meeting.rs:266-268)
2. POST /meetings?operation=create-attendees with {"ClientRequestToken":"tok-1","MediaRegion":"us-east-1","ExternalMeetingId":"ext-1","Attendees":[{"ExternalUserId":"user-a"},{"ExternalUserId":"user-b"}]}
3. Observe HTTP 200 with the first meeting, `"Attendees": []` and `"Errors": []` — the replay branch at src/ops/create_meeting_with_attendees.rs:236-252 returns before the per-item loop at :322
4. GET /meetings/{MeetingId}/attendees → 200 with an empty `Attendees` list: neither `user-a` nor `user-b` was created, and nothing reported a refusal for them
