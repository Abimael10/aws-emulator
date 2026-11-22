Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 0b600564062992274bd01b2b8270b52f52f6e224

# ListTagsForResource reports an empty tag set for a connection in the terminal `deleted` state, while GetConnection still reports that record's tags for the whole retention window

_Filed: 2026-11-22T02:14:41Z_
_Operation: ListTagsForResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `0b600564062992274bd01b2b8270b52f52f6e224`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A `DeleteConnection` leaves the record in the store for `DELETED_RECORD_RETENTION` (300 s) so that a caller polling the delete can read the terminal state. During that window the two readers of the SAME stored tag set disagree:

* `GetConnection(identifier)` resolves the record through `ComAmazonawsInterconnectInterconnectState::connection_in`, which applies only the Region predicate, and returns `stored.connection.clone()` — including the `tags` member with every key the store still holds.
* `ListTagsForResource(arn)` resolves the SAME record through `connection_in_for_arn`, which additionally filters `exists_as_resource` (`state != "deleted"`). For a settled delete that filter removes the record, `found` is `None`, and the handler answers HTTP 200 with `tags: {}`.

So for ~300 s after the delete settles, `GetConnection` says the connection carries `{"owner":"me"}` and `ListTagsForResource` on that same connection's ARN says it carries nothing. Nothing removed those keys: no `UntagResource` ran, and the store still holds them (which `GetConnection` proves on the same lock, in the same instant).

The handler's own doc states the opposite rule, and the justification it gives for the empty answer is false in exactly this case: the empty set is explained as "an `arn` of this scope resolving to no resource at all, which nothing has ever tagged" — but a deleted-yet-readable connection HAS been tagged and its tags are still in the store. The crate's own suite states the same rule it breaks: "The stored record is what `GetConnection` and `ListTagsForResource` both read back" — and asserts the two agree, but only on a live record, so the deleted case is unpinned.

The boundary is also abrupt rather than principled: while the connection is merely `deleting` (`exists_as_resource` is still true) `ListTagsForResource` DOES report the tags — the suite pins the 200 for that window — and the keys vanish from the listing at the instant the clock flips `deleting` -> `deleted`, even though the record, its identifier and its ARN all keep resolving for `GetConnection`.

## Evidence

The handler's stated rule and its justification — services/service-com-amazonaws-interconnect-interconnect/src/ops/list_tags_for_resource.rs:152-168:
```
    /// "List all current tags on the specified resource. Currently this supports
    /// Connection resources."
    ///
    /// `@readonly`: the answer is the tag set the resource the `arn` names CURRENTLY
    /// holds — exactly what `CreateConnection$tags`, `TagResource` and
    /// `UntagResource` last left in the store, with no key the store does not hold
    /// and none of the ones it does left out. Listing tags never changes a tag set,
    /// so two calls with no tag write in between answer the same. The listing does
    /// not validate that the resource exists: an `arn` of the scope the request
    /// addresses that resolves to no resource carries no tags, and the EMPTY tag set
    /// is that answer rather than a refusal — nothing was ever tagged, which is what
    /// a read asked.
```

The code that produces the empty answer — src/ops/list_tags_for_resource.rs:205-222:
```
        let found = {
            let mut store = state.lock();
            store.materialise_due_transitions(::std::time::Instant::now());
            store
                .connection_in_for_arn(&region, &input.arn)
                .map(|stored| stored.connection.tags.clone())
        };
        ...
        Ok(ListTagsForResourceOutput {
            tags: Some(found.flatten().unwrap_or_default()),
        })
```

The filter that drops a settled-deleted record from THIS reader only — src/state.rs:548-551:
```
    pub fn connection_in_for_arn(&self, region: &str, arn: &str) -> Option<&StoredConnection> {
        let id = connection_id_from_arn(arn)?;
        self.connection_in(region, id).filter(|stored| exists_as_resource(stored))
    }
```
and src/state.rs:1712-1717:
```
/// Whether `stored` is still a resource of this account: a connection that has reached
/// the terminal `deleted` state is gone, while the record itself stays readable by its
/// identifier so a caller polling a delete sees that terminal state.
fn exists_as_resource(stored: &StoredConnection) -> bool {
    stored.connection.state != DELETED_CONNECTION_STATE
}
```

The sibling reader that keeps reporting the same record's tags — src/state.rs:457-461:
```
    pub fn connection_in(&self, region: &str, identifier: &str) -> Option<&StoredConnection> {
        self.connections
            .get(identifier)
            .filter(|stored| visible_in(stored, region))
    }
```
reached by src/ops/get_connection.rs:149-173:
```
        let found = {
            let mut store = state.lock();
            store.materialise_due_transitions(::std::time::Instant::now());
            store
                .connection_in(&region, &input.identifier)
                .map(|stored| stored.connection.clone())
        };
        ...
        Ok(GetConnectionOutput {
            connection: Some(connection),
        })
```

The window in which the two disagree — src/state.rs:84-92:
```
/// How long a `deleted` connection stays readable before the store drops it.
///
/// A deleted connection has to stay readable for a while — the terminal state is
/// what a caller polling after `DeleteConnection` must see ...
pub const DELETED_RECORD_RETENTION: Duration = Duration::from_secs(300);
```

The crate's own suite states the rule that is broken — tests/all/tag_resource.rs:215-218:
```
/// RULE: `TagResource` merges the requested tags into the resource's persisted tag
/// set — each requested key takes the requested value, every other key keeps the
/// value it had — changes nothing else about the resource, and is idempotent. The
/// stored record is what `GetConnection` and `ListTagsForResource` both read back.
```
and asserts the two agree — tests/all/tag_resource.rs:271-277:
```
        assert_eq!(
            listed_tags(&emulator, &arn, &case.name).await,
            case.expected_tags(),
            "case {:?}: expected ListTagsForResource to report the same merged set as GetConnection, got {:?}",
```
but every row of that table runs on a live connection, so the deleted case is unpinned. The only suite that reads tags after a delete asserts the STATUS inside the `deleting` window only, never the tag contents, and never after the state settles — tests/all/attach_point_arn_stops_at_the_delete.rs:320-329:
```
        let (status, tags) = emulator
            .ic_in(region, "ListTagsForResource", json!({ "arn": arn }))
            .await;
        if state_now(&emulator, region, &id).await == "deleting" {
            rows_inside_the_window += 1;
            assert_eq!(
                status, 200,
```

## Steps to Reproduce

1. Boot the emulator with Interconnect and Direct Connect on one engine, and create a Direct Connect gateway so the account holds a valid attach point.
2. Call ListEnvironments for the signing Region and take its `environmentId` and one of its available bandwidths.
3. CreateConnection on that environment with `tags={"owner":"me"}`; note the returned `id` and `arn`.
4. Call ListTagsForResource(arn=<arn>) and GetConnection(identifier=<id>): both report `{"owner":"me"}` — the two readers agree while the connection is live.
5. Call DeleteConnection(identifier=<id>). The connection enters `deleting`.
6. Wait ~500 ms (PROVISIONING_SPAN is 400 ms, src/state.rs:70) so the clock settles the connection into `deleted`, and confirm with GetConnection(identifier=<id>) that `connection.state == "deleted"`.
7. In the same GetConnection response, observe `connection.tags == {"owner":"me"}` — the store still holds the keys.
8. Call ListTagsForResource(arn=<arn>) again, well inside the 300 s DELETED_RECORD_RETENTION window. Observed: HTTP 200 with `tags: {}`. Expected: the same tag set GetConnection reports for that record, per the handler doc ("none of the ones it does [hold] left out") and tests/all/tag_resource.rs:218.
