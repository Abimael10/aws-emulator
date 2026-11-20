Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-apikey-credential-provider-errors
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-apikey-credential-provider-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteApiKeyCredentialProvider|2xx->4xx|$
Issue-Key: bedrock-agentcore-control|DeleteApiKeyCredentialProvider|2xx->4xx|status
Issue-Commit: 0be2d8cde67a061e1bdd818ed7a4b8faca2d2bae

# DeleteApiKeyCredentialProvider is not idempotent: a missing provider is refused 404 instead of succeeding 204

> _Note: this issue was identified against the code base at commit `0be2d8cde67a061e1bdd818ed7a4b8faca2d2bae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteApiKeyCredentialProvider

## Description
This issue has 1 cause.

Cause 1 of 1: DeleteApiKeyCredentialProvider for an API-key credential provider name that does not exist in the token vault (fixture bedrock-agentcore-control-apikey-credential-provider-errors, step task#4).

AWS: the delete is idempotent — removing a name that is already absent is a success, not a refusal. AWS answers HTTP 204 with an EMPTY body ({}), no __type and no error message. Smithy: DeleteApiKeyCredentialProvider carries the @idempotent trait, its @http binding declares code 204, and its output shape DeleteApiKeyCredentialProviderResponse has NO members, so a successful response has no body content. ResourceNotFoundException (@httpError 404) is modeled on the operation, but the recording shows AWS does not use it for an absent name on this delete — it reserves it for a missing token vault / unresolvable resource path. The only input member is `name` (CredentialProviderName, @required, @length 1..128, @pattern ^[a-zA-Z0-9\-_]+$), and the recorded request satisfies those constraints, so there is nothing to validate-reject either.

Emulator: answers HTTP 404 with a JSON error body {"__type": "ResourceNotFoundException", "message": "ApiKeyCredentialProvider not found for <name>"} — i.e. it looks the provider up and refuses when the lookup misses, instead of treating the delete as satisfied. In that message "ApiKeyCredentialProvider not found for " is fixed text the emulator invented and <name> is the value from the request's `name` member; neither the fixed text nor the error class is AWS's behavior here, because AWS emits no message at all for this call.

Why: clients and teardown/cleanup code rely on idempotent deletes. An SDK caller that deletes a provider it is not sure exists (retry after a timed-out delete, a test fixture's teardown, an infrastructure converge loop) gets a ResourceNotFoundException thrown where AWS returns success, so retries of an already-applied delete fail, teardown aborts leaving later steps unrun, and converge loops cannot reach a steady state without swallowing 404s that would hide real failures. A negative test asserting 204 on a second delete also fails against the emulator.

Fix: the DeleteApiKeyCredentialProvider handler in the bedrock-agentcore-control identity/token-vault surface — make the state removal idempotent: when the named provider is absent, skip the not-found refusal and return the modeled empty 204 response instead of raising ResourceNotFoundException.

Traces: bedrock-agentcore-control-apikey-credential-provider-errors#task#4.

Related open issue, which does NOT cover this cause: 20261108T204036Z-bedrock-agentcore-control-credential-provider-and-policy-error-paths-diverge-in.md bundles CreateApiKeyCredentialProvider duplicate-name, CreatePaymentCredentialProvider secret validation, CreatePolicy, GetPolicy and GetGatewayRateLimit refusal paths — it says nothing about DeleteApiKeyCredentialProvider or about delete idempotency.

## Evidence
Fixture bedrock-agentcore-control-apikey-credential-provider-errors, step task#4, operation DeleteApiKeyCredentialProvider on a name that was never created (recorded as sim2real-probe-bedrock-agentcore-control-apikeycred-missing-<suffix>; the suffix is a per-run value, not part of the requirement).

Expected (recorded AWS): status 204, body {} — no __type, no message.
Actual (emulator): status 404, body {"__type": "ResourceNotFoundException", "message": "ApiKeyCredentialProvider not found for <name>"}.

Replay detail: "status: expected 204, got 404" and "$: value_mismatch (expected {}, got {'__type': 'ResourceNotFoundException', 'message': 'ApiKeyCredentialProvider not found for ...'})".

Model backing for the expected response: DeleteApiKeyCredentialProvider is @idempotent, http {method: POST, uri: /identities/DeleteApiKeyCredentialProvider, code: 204}, output shape DeleteApiKeyCredentialProviderResponse with no members.

## Steps to Reproduce
1. Replay fixture bedrock-agentcore-control-apikey-credential-provider-errors against the emulator.
2. At step task#4 the fixture POSTs /identities/DeleteApiKeyCredentialProvider with a `name` that was never created in this account's token vault (a name satisfying @length 1..128 and ^[a-zA-Z0-9\-_]+$).
3. Observe the emulator answers 404 with __type ResourceNotFoundException and a "ApiKeyCredentialProvider not found for <name>" message, where AWS answers 204 with an empty body.
4. Equivalently: create an API-key credential provider, delete it, then delete it a second time — the second delete must also answer 204.
