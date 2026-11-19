Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: b2081c323ecd8661726294a0adb85fa67872f44a

# DeleteLink releases a link's routing rules but keeps their client-token records, so a later CreateLinkRoutingRule reusing that token is refused 400

_Filed: 2026-11-19T00:14:55Z_
_Operation: DeleteLink | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `b2081c323ecd8661726294a0adb85fa67872f44a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ComAmazonawsRtbfabricRtbfabricState::release_link` (called by the DeleteLink handler) removes the link record and every routing rule that routed for it, but leaves the `idempotency` records those rule creates wrote behind. The gateway-release path in the same module does the opposite for its own resource: it prunes the client-token records so that "the id a replayed token would answer with is never one the store no longer holds", and a test pins that rule for gateways. The surviving record is observable: because `CreateLinkRoutingRule` fingerprints `linkId` into the record, a caller that reuses the same `clientToken` for a rule on a freshly created link is refused with 400 ValidationException ("clientToken ... was already used ... with different parameters") even though the rule the token minted, and the link it routed for, are both gone. `release_dependents` has the same hole: it releases the orphaned links' routing rules (state.rs:581-582) while only pruning idempotency records whose `resource_id` equals the gateway id (state.rs:585-586), so a rule's token record outlives the gateway delete too.

## Evidence

The gap, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:503-509 — rules go, their token records stay:
```
    pub fn release_link(&mut self, gateway_id: &str, link_id: &str) -> Option<Link> {
        let key = self.find_link_key(gateway_id, link_id)?;
        let released = self.links.remove(&key)?;
        self.routing_rules
            .retain(|(rule_link_id, _), _| *rule_link_id != released.link_id);
        Some(released)
    }
```
The sibling rule it skips, state.rs:570-586 — the comment states the rule and the code only honours it for the gateway's own id:
```
    /// * every client-token record that minted a resource of that gateway, so the id
    ///   a replayed token would answer with is never one the store no longer holds.
...
        self.routing_rules
            .retain(|(rule_link_id, _), _| !orphaned.contains(rule_link_id));
...
        self.idempotency
            .retain(|_, record| record.resource_id != gateway_id);
```
The refusal the stale record causes, src/ops/create_link_routing_rule.rs:214-235: the fingerprint is `format!("gatewayId={:?}|linkId={:?}|priority={:?}|conditions={:?}|tags={}", ...)` and a mismatch returns `ValidationException` "clientToken {} was already used for a {} request with different parameters".
DeleteLink calls the helper at src/ops/delete_link.rs:168: `let removed = store.release_link(&input.gateway_id, &input.link_id);`
The same rule pinned for gateways, tests/all/requester_gateway_deletion.rs:268-275: "The client token that minted the released gateway is forgotten with it: a retry mints a new gateway rather than replaying an id the store no longer holds." No test covers a routing rule's token record after DeleteLink.

## Steps to Reproduce

1. Create a requester and a responder gateway, then POST /gateway/{requesterId}/create-link to get link L1.
2. POST /responder-gateway/{responderId}/link/{L1}/routing-rule with `clientToken` "t-rule", priority 10 and a condition — answers 200 with a ruleId.
3. DELETE /gateway/{requesterId}/link/{L1} — answers 200; the rule is gone (GET of the rule answers 404).
4. POST /gateway/{requesterId}/create-link again for the same pair to get a new link L2.
5. POST /responder-gateway/{responderId}/link/{L2}/routing-rule reusing `clientToken` "t-rule" with the same priority and conditions.
6. Observed: HTTP 400 ValidationException "clientToken t-rule was already used for a CreateLinkRoutingRule request with different parameters", caused by the record left behind for a rule and a link the store no longer holds. Expected (the rule state.rs:570-572 states for the gateway path): the record is released with the resource, so the call creates the rule.
