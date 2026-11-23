Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: b743c8ed5789be1c53f63ef71d8598fcef7945b3

# A deleted stream whose archive holds fragments keeps answering archived-media reads: once the registry record is removed, resolve_stream falls back to the stale archive entry

_Filed: 2026-11-23T14:49:18Z_
_Operation: ListFragments | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b743c8ed5789be1c53f63ef71d8598fcef7945b3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every read of this plane resolves its stream through `ComAmazonawsKinesisvideoarchivedmediaAwsacuityreaderState::resolve_stream`. That helper has two branches: when the Kinesis Video Streams registry HOLDS a record it enforces "ACTIVE only" (a record marked for deletion is refused 404), and when the registry holds NO record at all it falls back to this service's own archive map, which is keyed by stream NAME (plus an `arn_index`) and is never reconciled with a deletion.

The two branches contradict each other across the delete lifecycle:

1. While `DeleteStream` has the record in `DELETING`, a read is refused 404 `ResourceNotFoundException` ("not found or not active").
2. When the deletion falls due the control plane REMOVES the record (`settle_clock`), so the next read takes the fallback branch, finds the archive entry still held under the same name, and answers HTTP 200 with the deleted stream's fragments — `ListFragments` lists them, `GetMediaForFragmentList` returns their stored media bytes, `GetClip` muxes them, `GetImages` generates images from them and the two streaming-session operations mint sessions over them. A stream becomes LESS readable while it is being deleted and readable again after the deletion completes.

Nothing purges the archive: the control-plane crate contains no reference to this service's store, and the only site that replaces an archive entry is `put_stream`, which the ingest plane calls on the NEXT ingest. So the same stale entry is also attached to a re-created stream: after `DeleteStream`/`CreateStream` of one name the registry holds the NEW incarnation (a new ARN, which carries the creation instant) while `resolve_stream` looks the archive up by `registered.name` alone with no ARN comparison, so the new stream's first `ListFragments` reports the deleted incarnation's fragments — even though `ResolvedStream`'s own documentation says the archive "is empty for a registered stream nothing has been ingested into".

## Evidence

The rule the crate states for a deleted stream — src/state.rs:830-837 (`resolve_stream` doc):
```
    /// change is visible to the next read. A registered stream is READ only
    /// while its record is in the one status that serves reads, `ACTIVE`: a
    /// record the registry still holds in a non-terminal status — created and
    /// not yet active, or marked for deletion — is as unreadable as one the
    /// registry does not hold at all, and is reported with the same
    /// [`STREAM_NOT_FOUND_MESSAGE`] ("not found or not active").
```
and the code that enforces it only while the record EXISTS — src/state.rs:885-889:
```
            if !registered.readable() || !serves(&registered.arn) {
                return Err(StreamLookupError::NotFound(
                    STREAM_NOT_FOUND_MESSAGE.to_string(),
                ));
            }
```
The other side: with no registry record the archive answers for itself, by name (or by the stale `arn_index`) — src/state.rs:903-918:
```
        // No control-plane record: the archive's own entry is the registration
        // of a stream whose fragments were ingested into this store directly,
        // so it answers for itself rather than being reported absent.
        let archive = match (stream_name, stream_arn) {
            (Some(name), _) => self.streams.get(name),
            (_, Some(arn)) => self.arn_index.get(arn).and_then(|name| self.streams.get(name)),
            (None, None) => None,
        };
        match archive {
            Some(held) if serves(&held.arn) => Ok(ResolvedStream {
```
The registry really does remove the record, so that branch is reached — services/service-com-amazonaws-kinesisvideo-kinesisvideo-20170930/src/state.rs:570-572 (called by this crate's `registered_stream`, src/state.rs:1038 `registry.settle_clock();`):
```
        for (stream_name, stream_arn) in due {
            self.streams.remove(&stream_name);
            self.stream_arn_index.remove(&stream_arn);
```
The crate's own test states the rule the fallback breaks — tests/all/stream_readable_state.rs:408-412 and 443-460:
```
/// RULE (the other half): the state a read resolves against is the registry's as
/// of THIS call, so a deletion the clock has already made due is observed by the
/// read itself — no control-plane call has to run in between for the stream to
/// stop being readable, and a client polling a deleted stream is refused rather
/// than served for as long as nobody touches the control plane.
```
```
    // From the delete onwards every read refuses, through the whole life of the
    // record: while the registry still holds it as deleting, and after the
    // removal the delete made due
...
                assert!(
                    reply.is_not_found_or_not_active(),
```
That test passes only because its stream is never seeded: `a_deleted_stream_stops_being_read_once_its_removal_falls_due` (tests/all/stream_readable_state.rs:413-417) calls `harness.create_stream(name)` with no `harness.seed_archive(...)`, unlike `only_an_active_registered_stream_is_read` (line 358), which does seed. Add the one seeding line and the assertion at line 456 fails after the removal: the read answers 200.

The re-created-name half: the registered branch attaches the archive by NAME with no ARN check — src/state.rs:890 `let archive = self.streams.get(&registered.name);` — while this crate states elsewhere that identity includes the ARN, src/state.rs:760-763 (`put_stream` doc):
```
    /// Replacing a stream drops the ARN the previous entry was indexed under:
    /// a re-created stream has a new ARN (its creation instant is part of it),
    /// and a stale ARN that still resolved to the live stream would answer a
    /// request that named a stream which no longer exists.
```
and `ResolvedStream`'s doc promises the opposite of what is returned — src/state.rs:364-366:
```
/// this service's archive for that stream, which is empty for a registered stream
/// nothing has been ingested into.
```
Test coverage: no test in tests/all/ deletes a stream whose archive holds fragments, and none re-creates a stream name, so nothing pins the current behaviour.

## Steps to Reproduce

1. Create a stream through the control plane: POST /createStream with `StreamName: "cam"` and `DataRetentionInHours: 24`.
2. Seed its archive the way the ingest plane does — PutMedia on the Kinesis Video Media inlet, or `state.put_stream(...)` + `state.append_fragment("cam", ...)` as tests/all/stream_readable_state.rs:239-253 does.
3. POST /listFragments with `StreamName: "cam"` and a selector covering the fragments: HTTP 200 with the fragments — the baseline.
4. POST /deleteStream for that stream. Poll /listFragments: while the registry holds the record as DELETING the read is refused 404 ResourceNotFoundException ("The requested stream is not found or not active.").
5. Keep polling /listFragments only (no control-plane call) until the removal falls due — `registered_stream` settles the clock, so the read itself removes the record.
6. Observe that the read now answers HTTP 200 with the DELETED stream's fragments instead of staying 404; POST /getMediaForFragmentList with those fragment numbers returns the deleted stream's stored media bytes.
7. For the second symptom: after the removal, POST /createStream with `StreamName: "cam"` again (a new ARN), then POST /listFragments with `StreamName: "cam"` before anything is ingested — the fresh stream reports the previous incarnation's fragments instead of an empty `Fragments` list.
