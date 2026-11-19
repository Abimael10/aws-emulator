Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# CreateRequesterGateway never calls settle_all, so an idempotent replay reports PENDING_CREATION or ACTIVE depending on whether an unrelated call happened to settle the record

_Filed: 2026-11-19T02:42:25Z_
_Operation: CreateRequesterGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other handler in this crate calls `store.settle_all(now)` immediately after taking the state lock, because the store keeps a status transition the clock has already made due (`settles_at`) unmaterialised until some handler settles it. CreateRequesterGateway never calls it, yet its `@idempotencyToken` replay path answers with `gateway.status.clone()` read live out of the store.

The result is an answer that depends on unrelated traffic rather than on the clock: replay the create 1s after the original (PROVISIONING_DELAY is 500ms) and it answers PENDING_CREATION; call GetRequesterGateway once in between — which does settle the record — and the very same replay answers ACTIVE. Two answers about the same record at the same instant disagree, and which one a caller gets is decided by whether anybody else touched the store.

## Evidence

The crate's own rule, services/service-com-amazonaws-rtbfabric-rtbfabric/src/state.rs:422-427:
```
    /// Apply every status transition the clock has already made due.
    ///
    /// A reporting operation calls this before it answers: the change was owed
    /// at this same instant whether or not anybody asked, so materialising it
    /// here is not the read mutating the resource.
    pub fn settle_all(&mut self, now: SystemTime) {
```

The handler that skips it, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_requester_gateway.rs:177 and 206-212 — the lock is taken and the stored status is reported with no settle in between (no `settle_all` call occurs anywhere in the file):
```
        let mut store = state.lock();
...
            if let Some(gateway) = store.requester_gateways.get(&prior.resource_id) {
                return Ok(CreateRequesterGatewayOutput {
                    gateway_id: gateway.gateway_id.clone(),
                    domain_name: gateway.domain_name.clone(),
                    status: gateway.status.clone(),
                });
            }
```

The siblings that do settle before answering a stored status:
- src/ops/create_link.rs:196-198: `let mut store = state.lock();` / `store.settle_all(now);`
- src/ops/associate_certificate.rs:178-179 then the replay at 210-216 answers `existing.status.clone()` AFTER settling.
- src/ops/get_requester_gateway.rs:172-175: `store.settle_all(now);` with the comment "Reporting only: the sole write is the status transition the clock had already made due at this instant".

State that makes the divergence observable, src/state.rs:165-171 (`RequesterGateway::settle` writes `status = ACTIVE` into the stored record) and src/state.rs:57 (`PROVISIONING_DELAY: Duration = Duration::from_millis(500)`).

Test coverage: tests/all/gateway_link_lifecycle.rs:1246-1289 pins the clock contract for the GET path ("Once the clock has made the transition due, every reader sees the settled status"); tests/all/gateway_link_lifecycle.rs:800-834 exercises the replay but asserts only `gatewayId`, so no test pins the replayed status either way.

## Steps to Reproduce

1. POST /requester-gateway with {"vpcId":..., "subnetIds":[...], "securityGroupIds":[...], "clientToken":"token-a"} — 200, status PENDING_CREATION.
2. Sleep longer than PROVISIONING_DELAY (500ms).
3. Repeat the identical POST (same clientToken) — the replay answers status PENDING_CREATION, a status the clock has already moved past.
4. Now run GET /requester-gateway/{gatewayId} (which calls settle_all) — 200 with status ACTIVE.
5. Repeat the identical POST again — the same replay now answers ACTIVE. The replay's answer is decided by whether an unrelated call settled the record, not by the clock.
