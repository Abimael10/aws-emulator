Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# CreateResponderGateway never calls settle_all, so its idempotent replay reports a status the clock has already moved past until some unrelated call settles the record

_Filed: 2026-11-19T02:42:25Z_
_Operation: CreateResponderGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Same defect as in CreateRequesterGateway, in the other gateway create: the handler takes the state lock and answers an `@idempotencyToken` replay with `gateway.status.clone()` read live out of the store, but never calls `store.settle_all(now)` — the one call every other handler in this crate makes right after locking, which materialises the PENDING_CREATION -> ACTIVE transition the clock has already made due.

A replay issued after PROVISIONING_DELAY therefore reports PENDING_CREATION, while GetResponderGateway at the same instant reports ACTIVE; and once any settling call has run, the identical replay starts answering ACTIVE. The answer depends on unrelated traffic instead of on the clock.

## Evidence

The crate's own rule, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:422-427:
```
    /// Apply every status transition the clock has already made due.
    ///
    /// A reporting operation calls this before it answers: the change was owed
    /// at this same instant whether or not anybody asked...
    pub fn settle_all(&mut self, now: SystemTime) {
```

The handler that skips it, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_responder_gateway.rs:211 and 286-294 (no `settle_all` call exists anywhere in the file):
```
        let mut store = state.lock();
...
            if let Some(gateway) = store.responder_gateways.get(&prior.resource_id) {
                return Ok(CreateResponderGatewayOutput {
                    gateway_id: gateway.gateway_id.clone(),
                    status: gateway.status.clone(),
                    listener_config: gateway.listener_config.clone(),
                    external_inbound_endpoint: gateway.external_inbound_endpoint.clone(),
                    client_routing_policy: gateway.client_routing_policy.clone(),
                });
            }
```

The sibling reader, src/ops/get_responder_gateway.rs:194-196:
```
        let mut store = state.lock();
        // Reporting only: see `get_requester_gateway`.
        store.settle_all(now);
```
and the sibling create-with-replay that does settle first, src/ops/associate_certificate.rs:178-179 + 210-216.

`ResponderGateway::settle` writes the settled status into the stored record (src/state.rs:203-208), and the record is created with `settles_at: Some(now + crate::state::PROVISIONING_DELAY)` (create_responder_gateway.rs:331, with PROVISIONING_DELAY = 500ms at src/state.rs:57).

Test coverage: tests/all/gateway_link_lifecycle.rs:1261-1289 pins that every reader sees ACTIVE once the transition is due; the replay test at tests/all/gateway_link_lifecycle.rs:811-833 asserts only `gatewayId`, so the replayed status is unpinned.

## Steps to Reproduce

1. POST /responder-gateway with a valid body and {"clientToken":"token-b"} — 200, status PENDING_CREATION.
2. Sleep longer than PROVISIONING_DELAY (500ms).
3. Repeat the identical POST — the replay answers PENDING_CREATION.
4. GET /responder-gateway/{gatewayId} — 200 with status ACTIVE (this call settles the record).
5. Repeat the identical POST once more — the same replay now answers ACTIVE, proving the replayed status is decided by unrelated traffic rather than by the clock.
