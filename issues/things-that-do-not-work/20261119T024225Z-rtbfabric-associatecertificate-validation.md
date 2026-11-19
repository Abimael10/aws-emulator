Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# AssociateCertificate ignores clientToken entirely, so the same token with a different acmCertificateArn silently creates a second association instead of being refused

_Filed: 2026-11-19T02:42:25Z_
_Operation: AssociateCertificate | Class: validation_

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

AssociateCertificate's `clientToken` is an `@idempotencyToken` whose modelled documentation states that a retry carrying the same token but different parameters FAILS. The handler never looks at `clientToken` at all: it keys only on the (gatewayId, acmCertificateArn) pair, writes no idempotency record and never computes a parameter fingerprint. The consequence is that a caller who reuses one token for a second, DIFFERENT certificate on the same gateway gets a second association created and a 200 answer, where the documented contract is a failure.

The crate already implements exactly this rule three times for its other `@idempotencyToken` creates, with the same helper pair (`crate::state::idempotency_key` + `crate::state::parameter_fingerprint`), and its own test states the contract: "Same token, different parameters: the documented contract is a failure, and the only client-input error these operations model is ValidationException (400)." AssociateCertificate is the one create on this service that needs that helper and does not call it, even though it models `ValidationException` (400) just like the others.

The handler's own comment justifies only the OTHER case — a retry of the same (gateway, certificate) pair, with any token — which would keep working unchanged if the mismatch check were added: a different token for the same pair finds no prior token record, falls through, and is answered with the in-force association exactly as today.

## Evidence

Smithy, AssociateCertificateRequest$clientToken (smithy_query doc, mirrored verbatim in the handler at services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/associate_certificate.rs:61-62):
  "If you retry the operation with the same <code>clientToken</code>, but with different parameters, the retry fails with an <code>IdempotentParameterMismatch</code> error."

The handler, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/associate_certificate.rs:204-209 — the token is deliberately dropped, and only the same-pair retry is reasoned about:
```
        // This operation mints no id: the (gateway, certificate) pair IS the
        // association's identity, so a retry — with the caller's `clientToken` or
        // without it — addresses the record already there rather than making a
        // second one. ...
        let key = (input.gateway_id.clone(), input.acm_certificate_arn.clone());
```
`input.client_token` appears nowhere in the handle body (lines 165-257).

The sibling that DOES implement the rule, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/create_requester_gateway.rs:179-205:
```
        // `clientToken` is an `@idempotencyToken`: the same token replays the first
        // call's answer, and the same token with different parameters is refused
        // rather than silently answering for parameters the caller did not send.
        // The model declares no `IdempotentParameterMismatch`, so the refusal is
        // this operation's `ValidationException` (400).
        let fingerprint = crate::state::parameter_fingerprint(&format!(...));
        let token = crate::state::idempotency_key(Self::NAME, &input.client_token);
        if let Some(prior) = store.idempotency.get(&token).cloned() {
            if prior.fingerprint != fingerprint {
                return Err(... ValidationException ...)
```
The same block exists in create_responder_gateway.rs:252-285 and create_link_routing_rule.rs:211-235.

Test coverage: tests/all/gateway_link_lifecycle.rs:836-847 pins the rule for the gateway creates ("Same token, different parameters: the documented contract is a failure"). tests/all/certificate_association.rs:424-449 (`two_certificates_on_one_gateway_are_two_records`) uses a DISTINCT token per certificate (`token-associate-{index}`), and tests/all/certificate_association.rs:365-367 pins only "A repeat of the association — a fresh client token and all — asks for the record that is already there"; neither exercises one token with two different ARNs, so no test pins the current behaviour.

## Steps to Reproduce

1. CreateResponderGateway with gatewayType=EXTERNAL and wait for ACTIVE; note its gatewayId.
2. POST /responder-gateway/{gatewayId}/certificate with {"acmCertificateArn": <ARN A>, "clientToken": "token-1"} — 200, association on ARN A.
3. POST /responder-gateway/{gatewayId}/certificate with {"acmCertificateArn": <ARN B, a different valid ACM ARN>, "clientToken": "token-1"} — the same token with different parameters.
4. Observe 200 and a SECOND stored association (GetCertificateAssociation for ARN B answers 200), where the modelled clientToken contract is that the retry fails; the three sibling creates answer 400 ValidationException for the same input shape.
