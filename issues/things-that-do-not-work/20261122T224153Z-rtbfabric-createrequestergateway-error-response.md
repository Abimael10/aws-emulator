Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: a9b1f91879b3ed1957cfd9e0361182ba83602a4e

# A reused clientToken carrying different parameters is answered with ValidationException, not the IdempotentParameterMismatch error the clientToken documentation names

_Filed: 2026-11-22T22:41:53Z_
_Operation: CreateRequesterGateway | Class: error-response_

> _Note: this issue was identified against the code base at commit `a9b1f91879b3ed1957cfd9e0361182ba83602a4e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every `@idempotencyToken` operation of this service words the "same token, different parameters" refusal itself, as this operation's modelled `ValidationException` (HTTP 400, wire code `ValidationException`). The model's own `clientToken` documentation states which error that retry fails with, and it is not ValidationException: "If you retry the operation with the same clientToken, but with different parameters, the retry fails with an IdempotentParameterMismatch error."

The handlers justify the substitution with "The model declares no `IdempotentParameterMismatch`, so the refusal is this operation's `ValidationException` (400)" — but "the model does not declare the shape" is not a reason this crate accepts elsewhere: the `/tags/{resourceArn}` surface answers `InternalServerErrorException`, a code in no operation's modelled error set, through the `Scaffolded::unmodeled` channel that exists for exactly that case ("A failure whose wire error CODE this service's model does not declare for the operation answering it"). So the crate has a channel for a code outside the modelled set and chooses not to use it for the one failure mode the model's documentation names by code.

The consequence is on the wire a client dispatches on: a retry bug (same token, changed parameters) is reported as `ValidationException` / `__type: ValidationException`, so an SDK or caller matching the documented `IdempotentParameterMismatch` never sees it and cannot tell a token-mismatch from an ordinary input-constraint refusal.

The same substitution is in seven implemented handlers that share this wording — CreateRequesterGateway, CreateResponderGateway, CreateInboundExternalLink, CreateOutboundExternalLink, CreateLinkRoutingRule, UpdateRequesterGateway and UpdateResponderGateway — and all carry the same documentation sentence, so this is one gap with seven copies rather than seven gaps.

## Evidence

The model sentence, from `smithy_query(query="doc", shape="CreateRequesterGatewayRequest", member="clientToken")`:

> "If you retry the operation with the same <code>clientToken</code>, but with different parameters, the retry fails with an <code>IdempotentParameterMismatch</code> error."

and carried verbatim into the handler's own header, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_requester_gateway.rs:57-58:
```
//!   a random one for you. If you retry the operation with the same clientToken, but with
//!   different parameters, the retry fails with an IdempotentParameterMismatch error.
```
(identical text at src/ops/update_requester_gateway.rs:66-67, src/ops/update_responder_gateway.rs:82, src/ops/create_responder_gateway.rs:68, src/ops/create_link_routing_rule.rs:59, src/ops/create_inbound_external_link.rs:62, src/ops/create_outbound_external_link.rs:62.)

What the code answers instead, src/ops/create_requester_gateway.rs:185-211:
```
        // `clientToken` is an `@idempotencyToken`: the same token replays the first
        // call's answer, and the same token with different parameters is refused
        // rather than silently answering for parameters the caller did not send.
        // The model declares no `IdempotentParameterMismatch`, so the refusal is
        // this operation's `ValidationException` (400).
...
        if let Some(prior) = store.idempotency.get(&token).cloned() {
            if prior.fingerprint != fingerprint {
                return Err(CreateRequesterGatewayErrorModeled::ValidationException(
                    ValidationException {
                        message: format!(
                            "clientToken {} was already used for a {} request with different parameters",
                            input.client_token,
                            Self::NAME,
                        ),
                    },
                )
                .into());
            }
```
The same block, same code, in the sibling handlers: src/ops/create_responder_gateway.rs:277-289, src/ops/update_requester_gateway.rs:208-220, src/ops/update_responder_gateway.rs:256-268, src/ops/create_link_routing_rule.rs:223-235, src/ops/create_inbound_external_link.rs:216-228, src/ops/create_outbound_external_link.rs:216-228.

The crate's own channel for a code the model does not declare, src/scaffolded.rs:180-188:
```
/// A failure whose wire error CODE this service's model does not declare for the
/// operation answering it, carried explicitly because no modeled shape names it.
///
/// Reach for it only where a recording shows AWS answering a code that is not in the
/// operation's modeled error set: the `/tags/{resourceArn}` surface answers
/// `crate::state::TAGGING_UNRESOLVED_CODE` for an ARN naming no resource, where the
/// rest of this service answers the modeled `ResourceNotFoundException`.
```
and the surface that uses it, src/state/tagging.rs:48-72:
```
/// The wire error code a tagging call whose `resourceArn` names no resource of this
/// service is answered with.
///
/// Not one of the error shapes the model declares for these operations: ...
pub const TAGGING_UNRESOLVED_CODE: &str = "InternalServerErrorException";
...
pub fn tagging_unresolved<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::unmodeled(
        TAGGING_UNRESOLVED_CODE,
        TAGGING_UNRESOLVED_STATUS,
        TAGGING_UNRESOLVED_MESSAGE,
    )
}
```

Test coverage, and the reason it states: tests/all/gateway_link_lifecycle.rs:840-851 asserts only the status, on the stated ground that no other error is modelled —
```
        // Same token, different parameters: the documented contract is a failure,
        // and the only client-input error these operations model is
        // ValidationException (400).
```
— which is the inference the quoted documentation sentence contradicts: the documentation does name an error for this failure mode, it is simply not in the modelled error set, exactly the situation `Scaffolded::unmodeled` is for. tests/all/requester_gateway_deletion.rs:824-838 pins the code and wording (`__type` == "ValidationException", "clientToken token-shared was already used for a UpdateRequesterGateway request with different parameters"), and tests/all/link_routing_rules.rs:531-550 and tests/all/responder_gateway_listing.rs:1157 pin the same for the rule create and the responder update, so a fix touches those assertions.

## Steps to Reproduce

1. POST /requester-gateway with {"clientToken":"token-a","vpcId":"vpc-0123456789abcdef0","subnetIds":["subnet-0123456789abcdef0"],"securityGroupIds":["sg-0123456789abcdef0"]} — answers 200 and mints a gatewayId.
2. POST /requester-gateway again with the SAME "clientToken":"token-a" but a different vpcId (e.g. "vpc-ffffffffffffffff0").
3. Observe the refusal: HTTP 400 with x-amzn-errortype / __type "ValidationException" and message "clientToken token-a was already used for a CreateRequesterGateway request with different parameters". The documented failure mode for this retry is an IdempotentParameterMismatch error (clientToken's own modelled documentation), so a caller dispatching on that code sees a plain ValidationException instead.
4. Repeat with any sibling: POST /responder-gateway, POST /requester-gateway/{gatewayId}/update, POST /responder-gateway/{gatewayId}/update, POST /responder-gateway/{gatewayId}/link/{linkId}/routing-rule, POST /requester-gateway/{gatewayId}/outbound-external-link, POST /responder-gateway/{gatewayId}/inbound-external-link — each answers the same ValidationException code for the same documented failure mode.
5. For contrast, GET /tags/arn:aws:rtbfabric:{region}:{account}:gateway/rtb-gw-unknown — the same crate answers the unmodelled code "InternalServerErrorException" through Scaffolded::unmodeled, showing a code outside the modelled error set is reachable here.
