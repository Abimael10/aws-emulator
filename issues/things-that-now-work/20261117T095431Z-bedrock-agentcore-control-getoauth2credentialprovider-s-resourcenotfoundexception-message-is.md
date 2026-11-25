Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions|run#19|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 6467559f5313a198a0c73115b08bb15d3259e538

# bedrock-agentcore-control: GetOauth2CredentialProvider's ResourceNotFoundException message is emulator prose that leaks the requested provider name

> _Note: this issue was identified against the code base at commit `6467559f5313a198a0c73115b08bb15d3259e538`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOauth2CredentialProvider

## Description
1 net-new root cause, on a check the suite report lists under `### bedrock-agentcore-control` and marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: GetOauth2CredentialProvider's not-found refusal carries a reworded message that names the provider type and echoes the requested name.

AWS: POST /identities/GetOauth2CredentialProvider for a `name` that holds no OAuth2 credential provider — including a provider that was just removed by DeleteOauth2CredentialProvider — is refused with HTTP 404 ResourceNotFoundException and the message `CredentialProvider not found`. That sentence is ENTIRELY fixed text: it has no variable part, it does not name the requested `name`, and it does not name the provider TYPE. Smithy: GetOauth2CredentialProvider (POST /identities/GetOauth2CredentialProvider, input `name` @required, CredentialProviderName @length 1..128 @pattern ^[a-zA-Z0-9\-_]+$) models ResourceNotFoundException with @httpError 404; the model does not state the message, so the sentence above is recorded behavior. Note the sibling GetApiKeyCredentialProvider refusal DOES name the requested value, so the two templates genuinely differ and the OAuth2 one must not be derived from the shared "<Type> not found for <name>" template.

Emulator: answers 404 ResourceNotFoundException (the code and status match) with the message `Oauth2CredentialProvider not found for <name>` — the provider type plus the name taken from the request, where AWS emits neither. Fixed text in the emulator's sentence is `Oauth2CredentialProvider not found for `; the trailing token is a per-run value echoed from the request and is not part of any AWS template.

Why: a client or test that matches AWS's sentence (`CredentialProvider not found`) never matches, so error-classification and message-assertion logic diverges; and because the emulator echoes the requested name on a path where AWS deliberately says nothing about it, cross-type probing of GetOauth2CredentialProvider becomes a name-existence oracle.

Fix: the not-found path of GetOauth2CredentialProvider's handler and the shared credential-provider not-found message helper it calls in the service state (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/get_oauth2_credential_provider.rs and the `oauth2_credential_provider_not_found` message in src/state.rs) — give the OAuth2 read its own fixed, parameterless sentence instead of the `<Type> not found for <name>` template. The same wording defect is described, from other recorded units, as Cause 2 of the open issue 20261109T203443Z-bedrock-agentcore-control-6-net-new-causes-oauth2.md and as Cause 7 of the open issue 20261110T043203Z-bedrock-agentcore-control-oauth2-credential-provider-surface-diverges-representation-update-delete.md; neither carries this check's replay key (the suite marks this check neither ALREADY FILED nor OWNED), so this issue files it with that key. A fixer closing this one should check those two.

Traces: bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions.

## Evidence
Failing check bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions [mismatch]:

  File "check_bedrock_agentcore_control_oauth2_credential_provider_lifecycle_transitions.py", line 67, in run
    assert message == expected, f"expected message {expected!r}, got {message!r}"
  AssertionError: expected message 'CredentialProvider not found', got 'Oauth2CredentialProvider not found for sim2reals3ocpc1dc26623491'

The check created an OAuth2 credential provider (GoogleOauth2 vendor), read it, updated it, deleted it with DeleteOauth2CredentialProvider, then called GetOauth2CredentialProvider(name=<same name>). The error CODE asserted immediately before (ResourceNotFoundException) MATCHED, and the HTTP status is 404; only the message diverges.

Expected (AWS): 404 / ResourceNotFoundException / message exactly `CredentialProvider not found` — no variable part.
Actual (emulator): 404 / ResourceNotFoundException / message `Oauth2CredentialProvider not found for <name>`, where `<name>` is the name from the request (the generated name in the quoted run is a per-run value, not part of the template).

The check's next assertion, which the emulator never reached, requires the post-delete UpdateOauth2CredentialProvider refusal to be ResourceNotFoundException with the message `CredentialProvider not found for name: <name>` — fixed text `CredentialProvider not found for name: ` plus the request's `name`. Smithy: UpdateOauth2CredentialProvider also models ResourceNotFoundException @httpError 404, so that sentence is part of the same not-found message surface and should be fixed together.

## Steps to Reproduce
1. Replay bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions.
2. Or directly: CreateOauth2CredentialProvider(name=N, credentialProviderVendor='GoogleOauth2', oauth2ProviderConfigInput={'googleOauth2ProviderConfig': {'clientId': '...', 'clientSecret': '...'}}).
3. DeleteOauth2CredentialProvider(name=N).
4. GetOauth2CredentialProvider(name=N) -> the emulator answers 404 ResourceNotFoundException with message 'Oauth2CredentialProvider not found for N' instead of AWS's fixed 'CredentialProvider not found'.
5. UpdateOauth2CredentialProvider(name=N, credentialProviderVendor='GoogleOauth2', oauth2ProviderConfigInput={...}) -> expect 404 ResourceNotFoundException with message 'CredentialProvider not found for name: N'.
