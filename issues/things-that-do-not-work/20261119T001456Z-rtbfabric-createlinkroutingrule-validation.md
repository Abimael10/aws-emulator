Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: b2081c323ecd8661726294a0adb85fa67872f44a

# CreateLinkRoutingRule accepts a second rule at a priority the link already uses instead of refusing with ConflictException (409)

_Filed: 2026-11-19T00:14:55Z_
_Operation: CreateLinkRoutingRule | Class: validation_

> _Note: this issue was identified against the code base at commit `b2081c323ecd8661726294a0adb85fa67872f44a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model's `RulePriority` shape documents that a routing rule's priority must be unique per link among non-deleted rules, and that the API layer enforces it with HTTP 409. The handler never looks at the priorities the link's existing rules already hold: it checks the link exists, that `conditions` states at least one condition, the idempotency token and the store's quotas, then mints and inserts the rule unconditionally. Two rules on the same link can therefore be stored at the identical priority, both answering 200, which also leaves the documented "lower number = evaluated first" evaluation order ambiguous for that link. `ConflictException` is declared for this operation and is present in the handler's own error enum, but the handler never constructs it on any path.

## Evidence

Smithy, `smithy_query(query="doc", shape="RulePriority")` — verbatim: "WAF-style evaluation priority. Lower number = evaluated first (priority 1 before 10).\nGaps are allowed (1, 10, 20 is valid). Must be between 1 and 1000 inclusive.\nUniqueness per link among non-deleted rules is enforced at the API layer (HTTP 409 on conflict)."

Handler, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_link_routing_rule.rs:276-297 — the only checks before the insert are link existence (189-196), `conditions_stated` (202-209), the token (222-246) and the quotas (249-274); no rule of the link is consulted for its priority:
```
        let rule_id = store.mint_rule_id();
        let rule = crate::state::LinkRoutingRule {
            rule_id: rule_id.clone(),
            link_id: link_id.clone(),
            priority: input.priority,
...
        store.routing_rules.insert((link_id, rule_id.clone()), rule);
```
The modelled 409 is available and unused: create_link_routing_rule.rs:117 `ConflictException(ConflictException),` inside `CreateLinkRoutingRuleErrorModeled` (grep for `ConflictException` in this file finds only the enum variant and the doc header line 87, never a construction).

Test coverage: services/service-com-amazonaws-rtbfabric-rtbfabric/tests/all/link_routing_rules.rs exercises priorities 1, 2, 7, 42, 500, 999, 1000 — one rule per link per case (ROWS at line 293, `a_rule_round_trips_through_its_link_from_either_end` at 332) and bounds only (`a_rule_must_state_a_condition_and_a_priority_inside_its_bounds` at 568, rows at 596-598). No case creates two rules on one link, so nothing pins the current acceptance.

## Steps to Reproduce

1. POST /requester-gateway and POST /responder-gateway to mint a gateway pair.
2. POST /gateway/{requesterId}/create-link with `peerGatewayId` = the responder id and the required `logSettings`; keep the returned `linkId`.
3. POST /responder-gateway/{responderId}/link/{linkId}/routing-rule with `{"clientToken":"t1","priority":10,"conditions":{"pathPrefix":"/bid"}}` — answers 200 with a `ruleId`.
4. POST the same path again with `{"clientToken":"t2","priority":10,"conditions":{"pathExact":"/bid/openrtb"}}` — a different rule at the SAME priority on the SAME link.
5. Observed: HTTP 200 and a second `ruleId`; GET both rules back and they report `priority` 10. Expected per the `RulePriority` documentation: the second call is refused with HTTP 409 `ConflictException` and no second rule is stored.
