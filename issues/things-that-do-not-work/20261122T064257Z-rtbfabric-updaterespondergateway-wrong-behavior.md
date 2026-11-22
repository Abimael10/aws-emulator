Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d0b2351dc36d57baec301bc6f4dd9245937531bc

# A retried UpdateResponderGateway clientToken re-applies its change over a newer one once another update has intervened, instead of replaying its own answer

_Filed: 2026-11-22T06:42:57Z_
_Operation: UpdateResponderGateway | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d0b2351dc36d57baec301bc6f4dd9245937531bc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateResponderGateway` remembers the `@idempotencyToken` of only the MOST RECENT accepted change, in the single `last_update` slot on the gateway record. The replay test is `gateway.last_update.as_ref().filter(|prior| prior.key == key)`, so as soon as a second update under a different token has been accepted, the first token is no longer recognised at all: a retry of the FIRST call is neither replayed nor refused — it falls through and is applied a second time, overwriting the configuration the second update stored and moving the gateway back to PENDING_UPDATE.

So a plain SDK retry (which is what an `@idempotencyToken` exists to make safe) silently REVERTS a newer, already-accepted configuration change: a gateway that reported `description: "Two"` reports `"One"` again, and the stored `clientRoutingPolicy`, `listenerConfig`, `trustStoreConfiguration` and `managedEndpointConfiguration` of the newer call are rolled back the same way.

The handler's own comment states the rule it breaks here ("the same token replays the first call's answer, and the same token with different parameters is refused"): on this path the token does neither. The crate's own test for this operation states the same rule — "expected the retry to apply nothing a second time" — but only exercises a retry with no intervening update, so nothing pins the broken path. The three creates in this crate do honour the rule for every token ever presented, because they keep their records in the `store.idempotency` MAP rather than in a single slot on the resource.

## Evidence

The single-slot replay test, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/update_responder_gateway.rs:254-276 — only the most recent token is recognised, and anything else falls through to the apply below:
```
        // The record of the change this gateway most recently took lives on the
        // gateway itself, so a retry is answered from the resource it addressed.
        if let Some(prior) = gateway.last_update.as_ref().filter(|prior| prior.key == key) {
            if prior.fingerprint != fingerprint {
                return Err(UpdateResponderGatewayErrorModeled::ValidationException(
...
            // The same change under the same token: report the record it already
            // produced rather than apply it a second time.
            return Ok(UpdateResponderGatewayOutput {
```
The rule the same handler states, update_responder_gateway.rs:222-226: "`clientToken` is an `@idempotencyToken`: the same token replays the first call's answer, and the same token with different parameters is refused rather than silently answering for parameters the caller did not send." On the path above, a token that is not the newest does neither — it re-applies.

What the fall-through then does, update_responder_gateway.rs:315-339:
```
        if input.description.is_some() {
            gateway.description = input.description;
        }
...
        gateway.last_update = Some(crate::state::IdempotentUpdate { key, fingerprint });
...
        gateway.status = crate::state::GATEWAY_STATUS_PENDING_UPDATE.to_string();
```

The one-slot store it reads, src/state.rs:413-421: `pub struct IdempotentUpdate { /// The token that call carried ... pub key: [u8; 32],` held as `pub last_update: Option<IdempotentUpdate>` (src/state.rs:220-223: "The `@idempotencyToken` of the most recent accepted change of this gateway's configuration").

The sibling rule in the same crate, which keeps EVERY token: src/state.rs:478-482 — "Keyed by [`idempotency_key`], which folds the operation and the caller's `clientToken` into one 32-byte digest: a token is scoped to the operation it was presented to." — and its use in src/ops/create_requester_gateway.rs:199-219 (`if let Some(prior) = store.idempotency.get(&token).cloned()`), where a replay works for any token ever presented, not only the latest.

Test coverage: tests/all/responder_gateway_listing.rs:1045-1092 (`a_retried_client_token_answers_the_record_the_first_call_produced`) asserts "case {case:?}: expected the retry to apply nothing a second time, got {after:?} after {settled:?}" — but it retries the token with NO intervening update. tests/all/responder_gateway_listing.rs:1096-1189 only reuses the newest token with different parameters. No test interleaves two tokens, so nothing pins the behaviour reported here.

## Steps to Reproduce

1. POST /responder-gateway with vpcId/subnetIds/securityGroupIds/port/protocol/domainName, description "Created by the suite" and a clientToken; note the gatewayId G. Poll GET /responder-gateway/G until status is ACTIVE.
2. POST /responder-gateway/G/update with {"port": <same>, "protocol": <same>, "clientToken": "A", "description": "One"} -> 200, status PENDING_UPDATE. Poll until ACTIVE; GET reports description "One".
3. POST /responder-gateway/G/update with {"port": <same>, "protocol": <same>, "clientToken": "B", "description": "Two"} -> 200. Poll until ACTIVE; GET reports description "Two".
4. Retry the FIRST call verbatim (the retry an SDK would send): POST /responder-gateway/G/update with {"port": <same>, "protocol": <same>, "clientToken": "A", "description": "One"}.
5. Observed: HTTP 200 with status PENDING_UPDATE, and GET /responder-gateway/G now reports description "One" again with a moved updatedAt — the retry re-applied its change and reverted the accepted later one.
6. Expected per the handler's own stated contract and its own test: the retry of token A replays the answer that call produced and applies nothing a second time (or, if token A is no longer retained, is refused rather than silently re-applied).
