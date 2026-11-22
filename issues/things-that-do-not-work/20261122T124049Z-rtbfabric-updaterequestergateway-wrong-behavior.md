Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 7c67dbf4cf71b9b5df392b9ac7fe4964acac2537

# A retried UpdateRequesterGateway clientToken re-applies its change over a newer one once another update has intervened

_Filed: 2026-11-22T12:40:49Z_
_Operation: UpdateRequesterGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `7c67dbf4cf71b9b5df392b9ac7fe4964acac2537`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateRequesterGateway` keeps the `@idempotencyToken` of only the MOST RECENT accepted change, in the single `last_update` slot on the `RequesterGateway` record. The replay test is `gateway.last_update.as_ref().filter(|prior| prior.key == key)`, so once a second update under a different token has been accepted, the first token is no longer recognised at all: a retry of the FIRST call is neither replayed nor refused — it falls through to the apply block below and is applied a second time, overwriting the `description` the second update stored and moving the gateway back to `PENDING_UPDATE` with a fresh `updatedAt`.

So a plain SDK retry — which is exactly what an `@idempotencyToken` exists to make safe — silently REVERTS a newer, already-accepted configuration change: a gateway that reported `description: "Two"` reports `"One"` again.

The handler's own comment states the rule it breaks on this path ("a retry of the change the gateway last took is answered with that gateway rather than applied to it again, and the same token carrying DIFFERENT parameters is refused"): a token that is not the newest does neither. The three creates in this same crate DO honour the rule for every token ever presented, because they keep their records in the `store.idempotency` MAP rather than in a single slot on the resource.

Note: an open issue reports the identical defect in the sibling handler `UpdateResponderGateway` (20261122T064257Z-rtbfabric-updaterespondergateway-wrong-behavior). That issue cites only `src/ops/update_responder_gateway.rs`; this is the second, independent copy of the same single-slot replay in `src/ops/update_requester_gateway.rs`, which a fix to the responder handler would not touch.

## Evidence

The single-slot replay test, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/update_requester_gateway.rs:206-227 — only the most recent token is recognised, and anything else falls through to the apply below:
```
        // The record of the change this gateway most recently took lives on the
        // gateway itself, so a retry is answered from the resource it addressed.
        if let Some(prior) = gateway.last_update.as_ref().filter(|prior| prior.key == key) {
            if prior.fingerprint != fingerprint {
                return Err(UpdateRequesterGatewayErrorModeled::ValidationException(
...
            // The same change under the same token: report the record it already
            // produced rather than apply it a second time.
            return Ok(UpdateRequesterGatewayOutput {
```

The rule the same handler states, src/ops/update_requester_gateway.rs:184-190: "`clientToken` is an `@idempotencyToken`: a retry of the change the gateway last took is answered with that gateway rather than applied to it again, and the same token carrying DIFFERENT parameters is refused rather than silently answering for parameters the caller did not send." On the path above a stale token does neither — it re-applies.

What the fall-through then does, src/ops/update_requester_gateway.rs:240-251:
```
        // A member the caller states replaces what the record holds; one it leaves out
        // is not a request to clear the stored value.
        if input.description.is_some() {
            gateway.description = input.description;
        }
        gateway.updated_at = now;
        gateway.last_update = Some(crate::state::IdempotentUpdate { key, fingerprint });
        // The gateway is in service, so the change is applied to a live gateway: this
        // call reports the update as pending, and the clock owes the gateway its way
        // back into service.
        gateway.status = crate::state::GATEWAY_STATUS_PENDING_UPDATE.to_string();
```

The one-slot store it reads, src/state.rs:183-186:
```
    /// The `@idempotencyToken` of the most recent accepted change of this
    /// gateway's configuration, and a digest of that call's other parameters;
    /// `None` until one is accepted. See [`IdempotentUpdate`].
    pub last_update: Option<IdempotentUpdate>,
```

The sibling rule in the same crate that retains EVERY token, src/state.rs:505-509: "Keyed by [`idempotency_key`], which folds the operation and the caller's `clientToken` into one 32-byte digest: a token is scoped to the operation it was presented to." — and its use in src/ops/create_requester_gateway.rs:199 (`if let Some(prior) = store.idempotency.get(&token).cloned()`), where a replay works for any token ever presented, not only the latest.

Test coverage: tests/all/requester_gateway_deletion.rs:648-696 (`a_retry_of_the_token_the_record_last_took_is_answered_rather_than_applied_again`) retries the token with NO intervening update — "expected the retry to leave the record as the first update left it" (line 694). tests/all/requester_gateway_deletion.rs:796-850 only reuses the newest token with different parameters. No test interleaves two different update tokens on a requester gateway, so the behaviour reported here is unpinned.

## Steps to Reproduce

1. POST /requester-gateway with {"vpcId": "vpc-0123456789abcdef0", "subnetIds": ["subnet-1"], "securityGroupIds": ["sg-1"], "description": "Created by the suite", "clientToken": "token-create"} — note the returned gatewayId G.
2. Poll GET /requester-gateway/G until status is ACTIVE.
3. POST /requester-gateway/G/update with {"clientToken": "A", "description": "One"} -> 200, status PENDING_UPDATE. Poll until ACTIVE; GET reports description "One".
4. POST /requester-gateway/G/update with {"clientToken": "B", "description": "Two"} -> 200. Poll until ACTIVE; GET reports description "Two".
5. Retry the FIRST call verbatim (the retry an SDK would send): POST /requester-gateway/G/update with {"clientToken": "A", "description": "One"}.
6. Observed: HTTP 200 with status PENDING_UPDATE, and GET /requester-gateway/G reports description "One" again with a moved updatedAt — the retry re-applied its change and reverted the accepted later one.
7. Expected per the handler's own stated contract and its own test: the retry of token A is answered with the record that call produced and applies nothing a second time (or, if token A is no longer retained, is refused rather than silently re-applied).
