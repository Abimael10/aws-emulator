Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6bae9ee14af172aae629fb16389ee320a8de1819

# DeleteMeeting releases the meeting-store lock before recording the ARN withdrawal the shared tag directory exists for

_Filed: 2026-11-25T01:37:36Z_
_Operation: DeleteMeeting | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6bae9ee14af172aae629fb16389ee320a8de1819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteMeeting` removes the meeting under this service's own state lock, RELEASES that lock, and only then records the withdrawal of the meeting's ARN in the family's shared ARN-keyed tag directory. The two writes that together make a deleted meeting un-taggable are therefore not atomic.

The handler's own comment states the rule it then breaks: the withdrawal record exists precisely because "the member that serves the next tagging call may be a sibling that cannot read this store". In the window between `drop(store)` and `withdraw(..)` the meeting is already gone from this service's store while the shared directory still holds the meeting's tags and no withdrawal record — which is exactly the state the shared directory's `withdraw` documentation says must never be observable ("without this it would report a deleted resource as a live, untagged one").

A sibling Chime service serving the family's shared `/tags` routes during that window asks the directory about an ARN of a resource type it does not own, so `addresses_taggable_resource(arn, None)` admits it and `withdrawn_refusal(arn)` answers `None`: the deleted meeting is served as a live resource and its stale tags are reported back (or new pairs written onto it).

This crate's own state module already sanctions the fix: it declares a lock order in which a writer may hold the meeting store and the tag directory at the same time, in that order. `DeleteMeeting` is the one writer that publishes a removal to both stores without doing so.

## Evidence

Handler, `src/ops/delete_meeting.rs:178-181` — the comment states WHY both facts must be recorded:
```
        // A deleted meeting leaves no tags behind, and its ARN stops addressing a resource
        // at all: both facts are recorded in the family's shared ARN-keyed directory,
        // because the member that serves the next tagging call may be a sibling that
        // cannot read this store.
```
and the code under it records them only after releasing the lock, `src/ops/delete_meeting.rs:203-214`:
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
        }
```

This crate's declared lock order, which the fix needs and which nothing else violates — `src/state.rs:983-984`:
```
/// LOCK ORDER: a writer takes [`service_state`] first and this directory second, and no
/// reader holds this one while taking another store, so the order cannot invert.
```

The symptom named verbatim by the directory the handler writes to — `services/common/src/chime_tags.rs:226-229`:
```
/// own store because the family's three generic tagging routes are served by ONE member
/// for every member's resource (see this module's header): the member serving the call
/// cannot read a sibling's store, so without this it would report a deleted resource as
/// a live, untagged one.
```
and the predicate that admits an un-withdrawn ARN of a type the serving member does not own — `services/common/src/chime_tags.rs:327-329`:
```
pub fn addresses_taggable_resource(resource_arn: &str, own_resource_exists: Option<bool>) -> bool {
    crate::arn::is_resource_arn_of(resource_arn, CHIME_ARN_NAMESPACE)
        && own_resource_exists.unwrap_or(true)
```

Existing coverage pins only the single-threaded outcome: `tests/all/resource_tagging.rs:3-15` ("a tagging call whose ARN names a MEETING this store holds no meeting under — one that never existed, or one `DeleteMeeting` has ended — is refused") and `tests/all/delete_meeting_idempotence.rs:9-14`; neither suite interleaves a delete with a tagging call, so the window is untested.

## Steps to Reproduce

1. POST /meetings signed for `chime` with {"ClientRequestToken":"tok-1","MediaRegion":"us-east-1","ExternalMeetingId":"ext-1","Tags":[{"Key":"k","Value":"v"}]} and keep MeetingArn.
2. Issue DELETE /meetings/{MeetingId} concurrently with a GET /tags?arn=<MeetingArn> served by a SIBLING Chime service that binds the same shared /tags route (e.g. chime-sdk-identity), so the sibling's `tagging_refusal` answers `own_resource_exists == None` for a meeting ARN.
3. Interleave so the sibling's read lands after `src/ops/delete_meeting.rs:204` (`drop(store)`) and before `src/ops/delete_meeting.rs:209` (`withdraw`).
4. Actual: the sibling answers HTTP 200 with {"Tags":[{"Key":"k","Value":"v"}]} for a meeting that no longer exists, because `withdrawn_refusal` is still `None` and `addresses_taggable_resource(arn, None)` is `true`. Expected per the handler's own comment (`src/ops/delete_meeting.rs:178-181`) and `chime_tags.rs:226-229`: the removal and the withdrawal are published together, so no caller observes the meeting as a live, untagged resource.
5. Same interleaving with POST /tags?operation=tag-resource through the sibling writes new pairs under the deleted meeting's ARN, which no later `DeleteMeeting` will withdraw.
