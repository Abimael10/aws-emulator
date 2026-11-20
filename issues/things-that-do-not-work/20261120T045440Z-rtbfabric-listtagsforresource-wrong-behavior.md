Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 07a81b42592fdd972bbc8a5b9a2141006a4e2532

# The tagging surface never settles the clock, so it reports (and writes to) a routing rule whose deletion the clock has already completed

_Filed: 2026-11-20T04:54:40Z_
_Operation: ListTagsForResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `07a81b42592fdd972bbc8a5b9a2141006a4e2532`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteLinkRoutingRule` is asynchronous: it leaves the rule record in `DELETION_IN_PROGRESS` with a due instant, and `ComAmazonawsRtbfabricRtbfabricState::settle_all` is what finishes the job — it moves the rule to `DELETED` and then DROPS it from `routing_rules`. Every other handler that resolves a rule (`GetLinkRoutingRule`, `DeleteLinkRoutingRule`, `CreateLinkRoutingRule`, and the link reads) calls `settle_all(now)` as its first act, which is exactly why the routing-rules module can state that after the clock makes the deletion due "no later read reports it".

The three handlers of the `/tags/{resourceArn}` surface — `ListTagsForResource`, `TagResource` and `UntagResource` — never call `settle_all`, and `tagged_resource`/`resource_tags`/`resource_tags_mut` resolve a routing-rule ARN straight through `find_routing_rule`/`find_routing_rule_mut`. So once a rule's deletion has come due, and until some unrelated call happens to settle the store:

* `ListTagsForResource` answers 200 with the tags of a rule that `GetLinkRoutingRule` answers 404 for — two reads of the same record disagree about whether it exists;
* `TagResource` answers 204 and writes tags onto that dead record, and `UntagResource` answers 204 for it; the next settling call drops the record, so the write is silently lost. The crate's own test states the invariant these calls break: "A tag written through that surface is on the same record the rule read reports."

The fix is the one line the sibling handlers already have — settle the store under the lock before the ARN is resolved — in all three handlers of this surface, so the tagging surface answers about exactly the set of rules the rest of the crate holds.

## Evidence

The rule the crate writes down for a settled deletion — services/service-com-amazonaws-rtbfabric-rtbfabric/src/state/routing_rules.rs:10-14:
```
//! A rule of its own is released by `DeleteLinkRoutingRule`, which the service
//! answers before the rule is gone: the record holds `DELETION_IN_PROGRESS` until
//! the clock makes the deletion due, and
//! [`settle_all`](super::ComAmazonawsRtbfabricRtbfabricState::settle_all) then drops
//! it, so no later read reports it.
```

Where that drop happens — src/state.rs:455-464:
```
        // A rule whose deletion the clock has now completed is gone, exactly as
        // the record a synchronous delete removes is: nothing reports a rule that
        // reached the terminal status, and the store does not grow with them. Its
        // client-token record goes with it, as on every other release path.
        let held_before = self.routing_rules.len();
        self.routing_rules
            .retain(|_, rule| rule.status != RULE_STATUS_DELETED);
```

The sibling read that DOES settle — src/ops/get_link_routing_rule.rs:172-181:
```
        let mut store = state.lock();
        // Reporting only: the sole write is the status transition the clock had
        // already made due at this instant, owed whether or not anybody read.
        store.settle_all(now);
...
        let Some(rule) =
            store.find_routing_rule(&input.gateway_id, &input.link_id, &input.rule_id)
```

The read that does not — src/ops/list_tags_for_resource.rs:159-164 (no `now`, no `settle_all` anywhere in the file):
```
        let store = state.lock();

        let resource = crate::state::tagged_resource(&input.resource_arn, &region)
            .ok_or_else(crate::state::tagging_unresolved)?;
        let tags = crate::state::resource_tags(&store, resource)
            .ok_or_else(crate::state::tagging_unresolved)?;
```
and the same omission on the write side, src/ops/tag_resource.rs:153-158 and src/ops/untag_resource.rs:153-158:
```
        let mut store = state.lock();

        let resource = crate::state::tagged_resource(&input.resource_arn, &region)
            .ok_or_else(crate::state::tagging_unresolved)?;
        let tags = crate::state::resource_tags_mut(&mut store, resource)
            .ok_or_else(crate::state::tagging_unresolved)?;
```

That this surface really addresses the rule record — src/state/tagging.rs:169-176:
```
        TaggedResource::LinkRoutingRule {
            gateway_id,
            link_id,
            rule_id,
        } => state
            .find_routing_rule(gateway_id, link_id, rule_id)
            .map(|rule| &rule.tags),
```

Test coverage: tests/all/link_routing_rules.rs:685-757 pins that the tagging surface and `GetLinkRoutingRule` report ONE record ("A tag written through that surface is on the same record the rule read reports.", line 736), and tests/all/link_routing_rules.rs:1196-1208 reaches the post-deletion state only through `fabric.gone(...)`/`get_rule`, i.e. through a handler that settles — so no test observes the window in which the tagging surface still holds the rule, and nothing pins the current behaviour.

## Steps to Reproduce

1. Create a requester gateway and a responder gateway, then POST /gateway/{requesterId}/create-link with the responder as peerGatewayId to get a linkId.
2. POST /responder-gateway/{responderId}/link/{linkId}/routing-rule with a clientToken, priority 1, conditions {"pathPrefix":"/bid"} and tags {"tier":"gold"} — note the ruleId.
3. DELETE /responder-gateway/{responderId}/link/{linkId}/routing-rule/{ruleId} — answers 200 with status DELETION_IN_PROGRESS.
4. Wait longer than PROVISIONING_DELAY (500 ms) and issue NO other request in between.
5. GET /tags/arn:aws:rtbfabric:{region}:{account}:gateway/{responderId}/link/{linkId}/routing-rule/{ruleId}: observed HTTP 200 with {"tags":{"tier":"gold"}} for a rule whose deletion the clock has completed; expected the same answer this surface gives for a rule the store does not hold (the 500 InternalServerErrorException/"Internal server error." refusal of src/state/tagging.rs:55-72).
6. POST to that same /tags/... path with {"tags":{"owner":"ads"}}: observed HTTP 204, i.e. a successful tag write onto the dead record.
7. Now GET /responder-gateway/{responderId}/link/{linkId}/routing-rule/{ruleId}: HTTP 404 ResourceNotFoundException — this call settles the store, the record is dropped, and the tag written in the previous step is gone without ever having been reportable by the rule's own read.
