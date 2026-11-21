Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9d0591483a26da063c45639c4a10cbe50c5f123c

# ListTagsForResource serializes an empty Tags list, the exact rendering CreateRule in the same crate deliberately omits

_Filed: 2026-11-21T21:53:17Z_
_Operation: ListTagsForResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9d0591483a26da063c45639c4a10cbe50c5f123c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both `CreateRule` and `ListTagsForResource` render the SAME optional modelled `TagList` member (`Tags`) out of the SAME stored field (`crate::state::Rule::tags`), and the two handlers apply opposite rules for an untagged rule:

* `create_rule.rs` applies a normalisation: when `rule.tags` is empty the member is left UNSET, so the body carries no `Tags` key at all. The comment above it states that rule in so many words.
* `list_tags_for_resource.rs` skips that normalisation: it always sets `tags: Some(rule.tags.clone())`, so an untagged rule answers `"Tags": []`. The comment above it states the OPPOSITE rule — that a modelled list is reported even when empty.

One of the two is wrong; the crate cannot hold both rules for one member read off one field. The crate's own established rule is the CreateRule one: the fixed issue `20261022T174824Z-rbin-retention-rules-never-report-the-pending-status.md` removed exactly this `"Tags": []` rendering from `CreateRule` ("an absent optional member and a present empty collection are different on the wire"), and `ListTagsForResource` is the one remaining renderer in the crate that still emits it. Nothing in the crate justifies the divergence: the two comments simply contradict each other, and no test pins the `ListTagsForResource` side (the suite's reader collapses an absent member and an empty list into the same empty map), so the inconsistency is unguarded and free to drift further.

## Evidence

CreateRule omits an empty Tags, with the rule stated in the comment — services/service-com-amazonaws-rbin-amazonrecyclebin/src/ops/create_rule.rs:372-377:
```
            // `Tags` reports "the tags assigned to the retention rule": a rule carrying
            // no tags has no tag assignment to report, so the OPTIONAL member is left
            // unset rather than serialized as an empty list. (The resource-selection
            // members below are reported either way — they are always part of a rule's
            // record, empty or not.)
            tags: (!stored.tags.is_empty()).then(|| stored.tags.clone()),
```

ListTagsForResource states the opposite rule and emits `[]` — services/service-com-amazonaws-rbin-amazonrecyclebin/src/ops/list_tags_for_resource.rs:174-178:
```
        // The tag list is reported even when empty: `Tags` is a modelled list, so an
        // untagged rule answers `[]` rather than omitting the member.
        Ok(ListTagsForResourceOutput {
            tags: Some(rule.tags.clone()),
        })
```

Both read the same stored field — services/service-com-amazonaws-rbin-amazonrecyclebin/src/state.rs:337-338:
```
    /// The rule's own AWS tags (`TagResource`/`ListTagsForResource`).
    pub tags: Vec<Tag>,
```

The member is optional on both shapes, so either rendering is wire-legal and the model does not settle the contradiction — smithy_query(query="operation", operation="ListTagsForResource"): `"output": {"Tags": {"target": "TagList", "length": {"min": 0, "max": 200}, "type": "list"}}` (no `required`).

The crate's established rule, from the FIXED issue that changed CreateRule — issues/things-that-now-work/20261022T174824Z-rbin-retention-rules-never-report-the-pending-status.md: "Cause 2 of 2: CreateRule emits Tags as an empty list when the request carried no tags, instead of omitting the member. … Fix: the CreateRule handler's response construction in this crate — leave the Tags member unset when the rule has no tags rather than serializing an empty list."

Test coverage: nothing pins the ListTagsForResource side. services/service-com-amazonaws-rbin-amazonrecyclebin/tests/all/resource_tags.rs:209-212 reads the member as
```
fn tags_of(body: &Value, member: &str) -> Tags {
    let Some(list) = body.get(member).and_then(Value::as_array) else {
        return BTreeMap::new();
    };
```
so an absent member and `[]` both become the empty map. By contrast the CreateRule side IS pinned, deliberately: services/service-com-amazonaws-rbin-amazonrecyclebin/tests/all/rule_status_transitions.rs:63-68 — "A response's `Tags` as a key -> value map, or `None` when the member is ABSENT — the difference this suite is about, so an absent member and an empty list are not collapsed into one another."

## Steps to Reproduce

1. POST /rules (CreateRule) signed for any Region with body {"ResourceType":"EBS_SNAPSHOT","RetentionPeriod":{"RetentionPeriodValue":7,"RetentionPeriodUnit":"DAYS"}} and NO Tags member; note the 201 body has no `Tags` key at all (create_rule.rs:377).
2. Take the `RuleArn` the create reported and call GET /tags/{ResourceArn} (ListTagsForResource) for the same untagged rule.
3. Observe the 200 body is {"Tags": []} — the member present and empty (list_tags_for_resource.rs:176-178), i.e. the exact rendering the create path of this same crate refuses to emit for the same stored, empty `Rule::tags`.
4. Compare the two comments (create_rule.rs:372-377 vs list_tags_for_resource.rs:174-175): they state opposite rules for the same optional member.
