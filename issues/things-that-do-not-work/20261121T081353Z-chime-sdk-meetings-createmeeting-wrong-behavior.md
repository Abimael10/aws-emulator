Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c67756afb02bd263e617d5e2893b4bdd72d02c83

# Write CreateMeeting's create-time Tags while still holding the meeting store lock

_Filed: 2026-11-21T08:13:53Z_
_Operation: CreateMeeting | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c67756afb02bd263e617d5e2893b4bdd72d02c83`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateMeeting` performs its two writes — the meeting record in this service's own store, and the create-time `Tags` in the family's shared ARN-keyed tag directory — under two different, non-overlapping critical sections. It inserts the meeting, projects the response, RELEASES the store lock (`drop(store)`), and only then takes the tag directory lock to write the tags.

Because the two writes are not atomic, a `DeleteMeeting` for the same meeting that runs in the window between them destroys the invariant the crate states for a deleted meeting: `DeleteMeeting` removes the meeting (so the store no longer holds it) and records its ARN as WITHDRAWN in the shared directory, and only after that does the still-running `CreateMeeting` write its tags under that same ARN. The directory is then left holding tags keyed by the ARN of a meeting that does not exist.

Nothing ever removes them. Every later tagging call naming that ARN is refused by `state::tagging_refusal` with `TaggingRefusal::InvalidMeetingId` before the directory is touched (`tag_resource.rs:158-173`, `untag_resource.rs:164-181`, `list_tags_for_resource.rs:159-176`), so neither `UntagResource` nor a second `DeleteMeeting` (the meeting is already gone, `forget_meeting` answers `None`, so `withdraw` is never called again) can clean them up. The orphaned entry sits in the shared directory until it is evicted by `MAX_TAGGED_RESOURCES` pressure.

`CreateMeetingWithAttendees` writes its create-time tags with exactly the same post-`drop(store)` sequence, so the same window exists there. The fix is local and changes no response and no existing test: keep the store guard alive across the tag write, which is precisely the order the state module documents for a writer (state first, directory second).

## Evidence

The crate states the invariant the window breaks, in `DeleteMeeting`'s own handler:

`src/ops/delete_meeting.rs:178-181`:
```
// A deleted meeting leaves no tags behind, and its ARN stops addressing a resource
// at all: both facts are recorded in the family's shared ARN-keyed directory,
// because the member that serves the next tagging call may be a sibling that
// cannot read this store.
```
and it, too, records those facts only after releasing the store lock — `src/ops/delete_meeting.rs:203-213`:
```
let removed = store.forget_meeting(&input.meeting_id);
drop(store);
if let Some(arn) = removed
    .as_ref()
    .and_then(|stored| stored.meeting.meeting_arn.as_deref())
{
    tag_directory.lock().withdraw(
        arn,
        ::service_common::chime_tags::WithdrawnClass::InvalidRequest,
        crate::state::INVALID_MEETING_ID_MESSAGE,
    );
```

The create side writes its tags after dropping the store guard — `src/ops/create_meeting.rs:291-305`:
```
let meeting = created.announced_meeting(ctx);
drop(store);

// The pairs go to the family's shared directory, which is the one place any
// tagging call reads them from. ...
let tags: Vec<(String, String)> = input
    .tags
    .unwrap_or_default()
    .into_iter()
    .map(|tag| (tag.key, tag.value))
    .collect();
if !tags.is_empty() {
    tag_directory.lock().tag(&meeting_arn, tags);
}
```
while the comment above it claims atomicity for exactly this pair of writes — `src/ops/create_meeting.rs:198-200`:
```
// The tags given at create time go in the family's shared, ARN-keyed tag
// directory, which is where the tagging operations read them from; resolved
// before anything is written so a create cannot half-succeed.
```

The same sequence in the sibling create — `src/ops/create_meeting_with_attendees.rs:426-439`: `let meeting = stored.announced_meeting(ctx); drop(store);` … `tag_directory.lock().tag(&meeting_arn, tags);`.

Holding both guards is the documented discipline, not a new rule — `src/state.rs:911-912`:
```
/// LOCK ORDER: a writer takes [`service_state`] first and this directory second, and no
/// reader holds this one while taking another store, so the order cannot invert.
```

Cleanup is unreachable once the ARN is withdrawn: `src/state.rs:556-577` (`tagging_refusal` answers `InvalidMeetingId` for any ARN of this service whose meeting the store does not hold) and the three tagging handlers return that refusal before touching the directory.

Test coverage: the invariant is pinned only SEQUENTIALLY, in `tests/all/resource_tagging.rs:442-467` — "The resource's tags go with the resource: once the meeting is deleted, its ARN carries a `MeetingId` this service holds no meeting under, so every tagging call naming it is refused as invalid" — and in `tests/all/resource_tagging.rs:477-525`; no test interleaves a create with a delete, so the window is untested.

## Steps to Reproduce

1. Start the emulator and drive two concurrent requests against one shared engine.
2. Request A: `CreateMeeting` with `ClientRequestToken='tok-race'`, `MediaRegion='us-east-1'`, `ExternalMeetingId='ext-race'` and `Tags=[{Key:'env',Value:'dev'}]`. Note the `Meeting.MeetingArn` it mints.
3. Let A proceed past `src/ops/create_meeting.rs:292` (`drop(store);`) but not yet reach the `tag_directory.lock().tag(...)` call at line 304 — the store now holds the meeting, the directory holds no tags for its ARN.
4. Request B: `DeleteMeeting` on that `MeetingId` runs to completion: `forget_meeting` removes the meeting and `withdraw(arn, InvalidRequest, "Invalid meeting id")` clears and withdraws the ARN (`src/ops/delete_meeting.rs:203-213`).
5. Let A resume: it writes `{env: dev}` into the shared directory under the ARN of the now-deleted meeting.
6. `ListTagsForResource(ResourceARN=<that ARN>)` answers 400 `BadRequest` "Invalid meeting id" (the refusal from `tagging_refusal`), and `UntagResource` on it is refused the same way — so the tags written in step 5 can never be read or removed, yet they stay in the shared `ChimeTagDirectory`, contradicting `delete_meeting.rs:178` ("A deleted meeting leaves no tags behind").
