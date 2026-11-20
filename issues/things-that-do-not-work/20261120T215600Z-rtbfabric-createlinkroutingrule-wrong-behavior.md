Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 98f3508375edd68a74a0898c3bb96f24298a5655

# CreateLinkRoutingRule stores a routing rule on a link in a terminal status, skipping the Link::terminal() check UpdateLink applies to the same link

_Filed: 2026-11-20T21:56:00Z_
_Operation: CreateLinkRoutingRule | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `98f3508375edd68a74a0898c3bb96f24298a5655`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateLinkRoutingRule` resolves the link it routes for with `find_link`, which matches a stored link by id from either end regardless of status, and then stores the rule. It never consults `Link::terminal()`, the one place in this crate that says a link has reached a status nothing moves it out of (REJECTED / FAILED / DELETED).

The sibling handler for the same resource, `UpdateLink`, does consult that helper, for exactly the same question — may this link's configuration still be changed? — and refuses with the modelled `ConflictException` (409) when it cannot. `AcceptLink` and `RejectLink` likewise refuse any transition on a link whose status does not admit it.

The result is that after `RejectLink` leaves a link REJECTED (which `reject_link.rs` records as terminal: "Declining is the end of the link's life: terminal"), `PATCH /gateway/{gatewayId}/link/{linkId}` answers 409 for that link, while `POST /responder-gateway/{gatewayId}/link/{linkId}/routing-rule` answers 200 and persists a routing rule that the clock then settles to ACTIVE — a rule claimed to be routing traffic for a link that can never carry any. The rule is also readable afterwards through `GetLinkRoutingRule` and through the tagging surface. `CreateLinkRoutingRule` declares `ConflictException` (HTTP 409) among its errors, so the refusal the crate already words for this case in `UpdateLink` has a modelled home here too.

## Evidence

The helper the crate has for "this link is finished", services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:326-332:
```
    /// Whether this link has reached a status nothing moves it out of: the peer
    /// declined the request, the link could not be established, or it is gone.
    pub fn terminal(&self) -> bool {
        self.status == LINK_STATUS_REJECTED
            || self.status == LINK_STATUS_FAILED
            || self.status == LINK_STATUS_DELETED
    }
```

The sibling that calls it before changing a link's configuration, src/ops/update_link.rs:185-192:
```
        if link.terminal() {
            // Nothing moves a link out of these statuses, so it has no
            // configuration left to change.
            return Err(UpdateLinkErrorModeled::ConflictException(ConflictException {
                message: link.status_conflict_message(&input.gateway_id, "updated"),
            })
            .into());
        }
```

The path that skips it, src/ops/create_link_routing_rule.rs:187-196 — the only check is existence:
```
        // A rule routes for one stored link, addressed from either of that link's
        // two ends — exactly as the link itself is.
        let Some(link) = store.find_link(&input.gateway_id, &input.link_id) else {
            return Err(CreateLinkRoutingRuleErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(),
                },
            )
            .into());
        };
        let link_id = link.link_id.clone();
```
and it goes on to insert the rule unconditionally, src/ops/create_link_routing_rule.rs:315:
```
        store.routing_rules.insert((link_id, rule_id.clone()), rule);
```

That a REJECTED link stays in the store to be addressed, src/ops/reject_link.rs:195-200:
```
        if link.request_outstanding() {
            // Declining is the end of the link's life: terminal, so nothing
            // further is owed on it and the settle instant is dropped.
            link.status = crate::state::LINK_STATUS_REJECTED.to_string();
            link.settles_at = None;
            link.updated_at = now;
```
and that the crate treats such a link as no longer joining its ends, src/state.rs:542-547: "A link that has reached a terminal status joins them no longer — nothing moves it back out of that status".

Test coverage: tests/all/link_routing_rules.rs:3-33 enumerates the rules this surface is covered for (one rule per priority, a rule not outliving its link, the not-found cases) and names no link-status precondition; a grep of that file for `REJECTED`/`terminal` matches nothing, so no test pins the current behaviour. tests/all/link_rejection.rs:394 onwards pins that every non-outstanding status is refused by `RejectLink`.

## Steps to Reproduce

1. POST /requester-gateway and POST /responder-gateway (each with a clientToken, vpcId, subnetIds, securityGroupIds; the responder with domainName, port, protocol) — note both gatewayIds.
2. POST /gateway/{requesterId}/create-link with {"peerGatewayId":"{responderId}","logSettings":{"applicationLogs":{"sampling":{"errorLog":0,"filterLog":0}}}} — note linkId.
3. POST /gateway/{responderId}/link/{linkId}/reject — answers 200 with status REJECTED (terminal).
4. PATCH /gateway/{requesterId}/link/{linkId} with {"timeoutInMillis":2000} — answers 409 ConflictException, "link ... is REJECTED and cannot be updated" (UpdateLink's terminal() branch).
5. POST /responder-gateway/{responderId}/link/{linkId}/routing-rule with {"clientToken":"t1","priority":1,"conditions":{"pathPrefix":"/bid"}} — observed 200 with status CREATION_IN_PROGRESS, and GET /responder-gateway/{responderId}/link/{linkId}/routing-rule/{ruleId} then reports the rule ACTIVE on the REJECTED link; expected the modelled ConflictException (409) this crate already words for a terminal link in UpdateLink.
