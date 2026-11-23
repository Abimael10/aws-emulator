Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c439b0c06ad4c0c74cea84caf475018e0421a0f9

# GetResponderGateway's derived link counts contradict the crate's own definitions: activeLinksCount drops a PENDING_UPDATE link the crate calls "carrying traffic", and linksRequestedCount counts links the gateway itself requested

_Filed: 2026-11-19T12:47:35Z_
_Operation: GetResponderGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c439b0c06ad4c0c74cea84caf475018e0421a0f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetResponderGateway` answers `activeLinksCount`, `totalLinksCount` and `linksRequestedCount` from `ComAmazonawsRtbfabricRtbfabricState::link_counts`, and two of the three counters disagree with the definitions this crate writes down for them.

(1) `LinkCounts::active` is documented in state.rs as "Those carrying traffic", and the crate states in exactly one place what "carrying traffic" means: `Link::is_up()` — ACTIVE *or* PENDING_UPDATE, with the comment spelling out that PENDING_UPDATE is a link "carrying traffic while a configuration change it has already taken is applied". `link_counts` does not call that helper; it compares `link.status == LINK_STATUS_ACTIVE` literally. So the moment `UpdateLink` moves a live link to PENDING_UPDATE (which `UpdateLink` does by calling `is_up()` — the one caller of the helper), the responder gateway reports `activeLinksCount` 0 for a link the crate itself classifies as up, until the clock settles it back to ACTIVE.

(2) `linksRequestedCount` is modelled as "The count of requested links waiting for the responder gateway to accept or reject", i.e. the gateway's inbound pending queue. `link_counts` counts every outstanding link the gateway is *either* end of (`link.has_end(gateway_id)` + `request_outstanding()`), with no regard for which end opened it. `CreateLink` called with a responder gateway's own id stores that gateway as the link's opener (`gateway_id`) and sets `direction = RESPONSE`, so such a link is waiting for the PEER to answer — yet it is counted in the opener's own `linksRequestedCount`.

Both symptoms are one class on one operation: the derived counters are computed by status alone, ignoring the helper and the orientation the rest of the crate uses.

## Evidence

The counter's own definition, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:356-366:
```
/// The number of links a gateway is an end of, by status. Every member is
/// derived by scanning [`ComAmazonawsRtbfabricRtbfabricState::links`].
#[derive(Debug, Default, Clone, Copy)]
pub struct LinkCounts {
    /// Every link this gateway is an end of.
    pub total: i32,
    /// Those carrying traffic.
    pub active: i32,
    /// Those whose request is still outstanding.
    pub requested: i32,
}
```

What the crate says "carrying traffic" means, src/state.rs:314-320:
```
    /// Whether this link is up: carrying traffic, or carrying traffic while a
    /// configuration change it has already taken is applied. A further change
    /// to a link in one of these statuses is applied to a live link, which is
    /// what `PENDING_UPDATE` reports, so the clock owes it `ACTIVE` again.
    pub fn is_up(&self) -> bool {
        self.status == LINK_STATUS_ACTIVE || self.status == LINK_STATUS_PENDING_UPDATE
    }
```

The computation that skips that helper, src/state.rs:536-549:
```
    /// Count the stored links `gateway_id` is an end of, by status.
    pub fn link_counts(&self, gateway_id: &str) -> LinkCounts {
        let mine = self.links.values().filter(|link| link.has_end(gateway_id));
        let mut counts = LinkCounts::default();
        for link in mine {
            counts.total = counts.total.saturating_add(1);
            if link.status == LINK_STATUS_ACTIVE {
                counts.active = counts.active.saturating_add(1);
            }
            if link.request_outstanding() {
                counts.requested = counts.requested.saturating_add(1);
            }
```

The sibling path that DOES use the helper for the same question, src/ops/update_link.rs:203-209:
```
        if link.is_up() {
            // The link is carrying traffic, so the change is applied to a live
            // link: this call reports the update as pending, and the clock owes
            // the link its way back up.
            link.status = crate::state::LINK_STATUS_PENDING_UPDATE.to_string();
```

Where the counts are answered, src/ops/get_responder_gateway.rs:198 and 224-227:
```
        let counts = store.link_counts(&input.gateway_id);
...
            // Derived, never stored: the links this gateway is an end of.
            active_links_count: Some(counts.active),
            total_links_count: Some(counts.total),
            links_requested_count: Some(counts.requested),
```

The modelled meaning of the second counter, `smithy_query(query="doc", shape="GetResponderGatewayResponse", member="linksRequestedCount")`:
"The count of requested links waiting for the responder gateway to accept or reject."

That a responder gateway can be the OPENER of an outstanding link, src/ops/create_link.rs:202-205:
```
        let direction = if store.requester_gateways.contains_key(&input.gateway_id) {
            crate::state::LINK_DIRECTION_REQUEST
        } else if store.responder_gateways.contains_key(&input.gateway_id) {
            crate::state::LINK_DIRECTION_RESPONSE
```
and `request_outstanding` (src/state.rs:302-304) is status-only: `self.status == LINK_STATUS_PENDING_REQUEST || self.status == LINK_STATUS_REQUESTED`.

Test coverage: tests/all/gateway_link_lifecycle.rs:1291-1300 pins only the requester-opened case ("The settled link now counts as requested against the responder gateway", expecting 1), and tests/all/gateway_link_lifecycle.rs:739-742 pins `activeLinksCount` 0 "while the request is outstanding". No test reads either counter for a PENDING_UPDATE link or for a link a responder gateway opened itself, so neither behaviour is pinned.

## Steps to Reproduce

1. Create a requester gateway (POST /requester-gateway) and a responder gateway (POST /responder-gateway).
2. POST /gateway/{requesterId}/create-link with the responder as peerGatewayId, then POST /gateway/{responderId}/link/{linkId}/accept and poll GET /gateway/{responderId}/link/{linkId} until status is ACTIVE.
3. GET /responder-gateway/{responderId} — activeLinksCount is 1.
4. PATCH /gateway/{requesterId}/link/{linkId} with {"timeoutInMillis":2000} — answers 200 with status PENDING_UPDATE.
5. Immediately GET /responder-gateway/{responderId}: observed activeLinksCount 0 for a link the crate's own `Link::is_up()` classifies as carrying traffic; expected the counter documented as "Those carrying traffic" to include it (as UpdateLink's `is_up()` branch does).
6. For the second symptom: create a second requester gateway R2, then POST /gateway/{responderId}/create-link with peerGatewayId R2 — the responder gateway is the opener (direction RESPONSE).
7. GET /responder-gateway/{responderId}: observed linksRequestedCount counts that self-opened link, although the model defines it as "The count of requested links waiting for the responder gateway to accept or reject" — this link waits for R2 to answer, not for this gateway.
