Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: cdaca303dd80edc32c77ea853d2e866117177c76

# TagResource resolves the meeting under one lock and writes the shared tag directory under another, so tags can be written onto a deleted meeting

_Filed: 2026-11-23T07:25:04Z_
_Operation: TagResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `cdaca303dd80edc32c77ea853d2e866117177c76`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`TagResource` performs its decision and its write in three separate, non-overlapping critical sections:

1. it takes the meeting store lock as a temporary, asks `tagging_refusal` whether the ARN names a meeting this service still holds, and RELEASES that lock at the end of the statement (`tag_resource.rs:158`);
2. it takes the shared tag-directory lock as a temporary to read `withdrawn_refusal` and releases it (`tag_resource.rs:178-181`);
3. it takes the directory lock a third time to actually write the pairs (`tag_resource.rs:199-203`).

Nothing holds the meeting store across step 3, so a `DeleteMeeting` for that meeting that completes between the checks and the write leaves the shared `ChimeTagDirectory` holding tags keyed by the ARN of a meeting that no longer exists — which directly contradicts the invariant `DeleteMeeting`'s own handler states ("A deleted meeting leaves no tags behind").

Those tags are then unreachable and unremovable: every later tagging call naming that ARN is refused by `state::tagging_refusal` with `TaggingRefusal::InvalidMeetingId` BEFORE the directory is touched (all three tagging handlers), and a second `DeleteMeeting` cannot clean them up because `forget_meeting` answers `None` for an already-removed meeting, so `withdraw` is never called again. The entry sits in the shared, cross-service directory until capacity pressure evicts it.

The crate already states the discipline that closes the window: a writer takes the service state first and the directory second (`state.rs:983-984`). Keeping the store guard alive across the directory write is local to this handler and changes no response and no existing test.

## Evidence

Repo side 1 — the store lock is a statement temporary, released before anything is written, `src/ops/tag_resource.rs:155-173`:
```
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            TagResourceErrorModeled::ServiceFailureException(crate::state::store_unavailable())
        })?;
        if let Some(refusal) = state.lock().tagging_refusal(&input.resource_arn) {
```
and the write happens in two further, separate critical sections, `src/ops/tag_resource.rs:178-203`:
```
        let withdrawn = directory
            .lock()
            .withdrawn_refusal(&input.resource_arn)
...
        let outcome = directory.lock().tag_within(
            &input.resource_arn,
            pairs,
            crate::state::MAX_TAGS_PER_RESOURCE,
        );
```

Repo side 2 — the invariant that breaks, stated in this crate's own delete handler, `src/ops/delete_meeting.rs:178-181`:
```
        // A deleted meeting leaves no tags behind, and its ARN stops addressing a resource
        // at all: both facts are recorded in the family's shared ARN-keyed directory,
        // because the member that serves the next tagging call may be a sibling that
        // cannot read this store.
```

Repo side 3 — why the orphan can never be read or removed, `src/state.rs:628-648` (`tagging_refusal` answers `InvalidMeetingId` for any ARN of this service whose meeting the store does not hold):
```
        if own_resource_exists == Some(false) {
            return Some(TaggingRefusal::InvalidMeetingId);
        }
```
and that refusal is returned before the directory is touched in all three tagging handlers (`tag_resource.rs:158-173`, `untag_resource.rs:164-181`, `list_tags_for_resource.rs:159-176`).

Repo side 4 — the discipline the handler does not follow, `src/state.rs:983-984`:
```
/// LOCK ORDER: a writer takes [`service_state`] first and this directory second, and no
/// reader holds this one while taking another store, so the order cannot invert.
```

Test coverage: the invariant is pinned only SEQUENTIALLY — `tests/all/resource_tagging.rs:371-375` ("RULE: tags given at CREATE time are the same state the tagging routes read and write … and deleting the resource takes them with it") and `tests/all/resource_tagging.rs:471-477` ("a tagging call whose ARN names a meeting this store holds no meeting under is refused with the modelled 400 … and writes nothing"). No test interleaves a tagging write with a delete, so the window is untested.

## Steps to Reproduce

1. Start the emulator and drive two concurrent requests against one shared engine.
2. Request A: `CreateMeeting` with `ClientRequestToken='tok-1'`, `MediaRegion='us-east-1'`, `ExternalMeetingId='ext-1'`; note the returned `Meeting.MeetingArn` (call it A1) and `MeetingId`.
3. Request B: `POST /tags?operation=tag-resource` with `{"ResourceARN":"<A1>","Tags":[{"Key":"env","Value":"dev"}]}`. Let it get past the `withdrawn_refusal` read at `src/ops/tag_resource.rs:178-181` but not yet reach `tag_within` at line 199 — at this point it holds neither the meeting store lock nor the directory lock.
4. Request C: `DELETE /meetings/{MeetingId}` runs to completion: `forget_meeting` removes the meeting and `withdraw(A1, InvalidRequest, "Invalid meeting id")` clears and withdraws the ARN (`src/ops/delete_meeting.rs:203-213`).
5. Let B resume: it writes `{env: dev}` into the shared `ChimeTagDirectory` under A1, the ARN of a meeting that no longer exists.
6. `GET /tags?arn=<A1>` answers 400 `BadRequestException` "Invalid meeting id", and `POST /tags?operation=untag-resource` on A1 is refused the same way — so the tags written in step 5 can neither be read nor removed, contradicting `delete_meeting.rs:178` ("A deleted meeting leaves no tags behind").
