Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.gateway-rate-limit-lifecycle-transitions bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|GetGatewayRateLimit|ResourceNotFoundException
Issue-Key: bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions|run#23|AssertionError|assert update_message == expected, f"expected message {expected!r}, got {update_message!r}"
Issue-Commit: 0a24b18ae59262f0cdae7b98999a169258844aef

# BatchPutGatewayRateLimits destroys rate limits it does not name, GetGatewayRateLimit's 404 message diverges, and UpdateOauth2CredentialProvider's 404 drops the name

> _Note: this issue was identified against the code base at commit `0a24b18ae59262f0cdae7b98999a169258844aef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchPutGatewayRateLimits

## Description
This bundle has 3 causes, all on service bedrock-agentcore-control: two on the gateway rate-limit surface and one on the OAuth2 credential-provider surface.

Cause 1 of 3: BatchPutGatewayRateLimits treats the submitted list as a destructive REPLACE of the gateway's whole rate-limit set, so every rate limit the batch does not name is deleted and becomes unresolvable.
AWS: Smithy BatchPutGatewayRateLimits (PUT /gateways/{gatewayIdentifier}/rate-limits/batch -> 200, output rateLimits @required) documents "Atomically creates or updates multiple rate limits for a gateway. The operation updates existing limits with matching keys and creates new limits for new keys. If the operation fails, the service applies no changes." — matching-key upsert plus create, with no removal of a limit whose keys the batch does not mention. Recorded behavior confirms it: after a CreateGatewayRateLimit over dimensionKeys ['targetName'] and a following BatchPutGatewayRateLimits submitting two limits over ['toolName'] and ['qualifiedModelId'], all three rateLimitIds are still readable with GetGatewayRateLimit -> 200 status ACTIVE and all three appear in ListGatewayRateLimits. (The rateLimits member's own doc prose, "the complete set ... replaces all existing rate limits", reads as full replacement, but the recording shows the unmentioned limit surviving, so the upsert rule is the requirement.)
Emulator: the handler assigns the submitted set over the stored set wholesale (`gateway.rate_limits = written;`), so the pre-existing limit over a different dimension-key scope is dropped. The next GetGatewayRateLimit on its id answers HTTP 404 ResourceNotFoundException instead of 200 — the check dies there with 'An error occurred (ResourceNotFoundException) when calling the GetGatewayRateLimit operation'.
Why: a client adding or adjusting one rate limit through the batch operation silently loses every other limit on that gateway — throttles the operator believes are in force stop existing, ids held by clients 404, and a cleanup or read-after-write over the full set fails. Silent data loss on an operation AWS documents as additive.
Fix: the BatchPutGatewayRateLimits handler's write into gateway rate-limit state — upsert the submitted entries onto the existing map (matching by rateLimitId, and by dimension-key set where no id is supplied) instead of overwriting the map.
Traces: check bedrock-agentcore-control.gateway-rate-limit-lifecycle-transitions; handler services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/batch_put_gateway_rate_limits.rs line 300.

Cause 2 of 3: GetGatewayRateLimit's ResourceNotFoundException message is not AWS's wording for a missing rate limit.
AWS: GetGatewayRateLimit (GET /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}) models ResourceNotFoundException at HTTP 404; recorded behavior for an id that names no rate limit of the gateway (for example after DeleteGatewayRateLimit) is the message 'Rate limit not found: <rateLimitId>' — fixed text 'Rate limit not found: ' followed by the rateLimitId taken from the request path.
Emulator: answers 404 ResourceNotFoundException with the generic store wording 'Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier.', which states no rateLimitId at all.
Why: a client or operator matching on the refusal message cannot tell which rate limit was missing, and message assertions against AWS fail. The status and code are right; only the message diverges.
Fix: the not-found refusal the GetGatewayRateLimit handler raises (crate::state::resource_not_found for RESOURCE_GATEWAY_RATE_LIMIT) must carry AWS's per-resource wording with the requested rateLimitId interpolated. Related open issue 20261110T043203Z-bedrock-agentcore-control-gateway-rate-limit-timestamps-are-epoch-numbers-updates.md names this same AWS message while reporting that DeleteGatewayRateLimit was not honored at all; it does not cover the message wording now that a 404 is produced.
Traces: check bedrock-agentcore-control.gateway-rate-limit-lifecycle-transitions (the observed 404 message); handler services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/get_gateway_rate_limit.rs lines 185-189.

Cause 3 of 3: UpdateOauth2CredentialProvider's ResourceNotFoundException omits the name, using the read path's shorter wording.
AWS: Smithy UpdateOauth2CredentialProvider (POST /identities/UpdateOauth2CredentialProvider, input name @required, CredentialProviderName) models ResourceNotFoundException at HTTP 404. Recorded behavior distinguishes the two paths: GetOauth2CredentialProvider on a deleted provider answers 'CredentialProvider not found', while UpdateOauth2CredentialProvider on the same deleted provider answers 'CredentialProvider not found for name: <name>' — fixed text 'CredentialProvider not found for name: ' plus the name member taken from the request.
Emulator: the update handler raises the family's shared refusal (crate::state::oauth2_credential_provider_not_found()), so it answers 404 ResourceNotFoundException with 'CredentialProvider not found', dropping the ' for name: <name>' suffix. The Get path's message is correct; only the update's is wrong.
Why: a client that updates by name and matches the refusal cannot tell which provider name failed to resolve, and a parity assertion on the update's message fails even though the status and code are right.
Fix: UpdateOauth2CredentialProvider's not-found refusal — it needs its own message carrying the requested name, not the read path's name-less wording.
Traces: check bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions; handler services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/update_oauth2_credential_provider.rs lines 218-223 and 256-264.

## Evidence
Replay of suite simparity-behavioral-aws-v1 (148 checks, 72 passed, 48.6%).

Cause 1 — check bedrock-agentcore-control.gateway-rate-limit-lifecycle-transitions [error]:
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetGatewayRateLimit operation: Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier.
  The check creates one rate limit (dimensionKeys ['targetName']), polls it to ACTIVE, updates it, then BatchPutGatewayRateLimits two further limits (['toolName'], ['qualifiedModelId']) and polls GetGatewayRateLimit for each of the three ids expecting 200/ACTIVE. Expected: 200 with status ACTIVE for the create-time rateLimitId (and its presence in ListGatewayRateLimits). Actual: 404 ResourceNotFoundException, because batch_put_gateway_rate_limits.rs replaces the stored map (`gateway.rate_limits = written;`) rather than upserting, so the ['targetName'] limit was deleted by a batch that never named it.

Cause 2 — same check, the message of that 404:
  expected 'Rate limit not found: <rateLimitId>' (fixed text + the rateLimitId from the request path)
  actual   "Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier."
  Status 404 and code ResourceNotFoundException are correct; the message is not AWS's.

Cause 3 — check bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions [mismatch]:
  AssertionError: expected message 'CredentialProvider not found for name: <name>', got 'CredentialProvider not found'
  (the recording's name value fills the template; it is not part of it). The preceding assertion on GetOauth2CredentialProvider's message 'CredentialProvider not found' passed, so the divergence is specific to UpdateOauth2CredentialProvider. Code ResourceNotFoundException / HTTP 404 are correct.

Model citations: BatchPutGatewayRateLimits PUT /gateways/{gatewayIdentifier}/rate-limits/batch, rateLimits @required BatchPutLimitEntries @length 1..50, errors include ResourceNotFoundException (404); GetGatewayRateLimit GET /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}, errors include ResourceNotFoundException (404); UpdateOauth2CredentialProvider POST /identities/UpdateOauth2CredentialProvider, name @required CredentialProviderName (len 1..128), errors include ResourceNotFoundException (404).

## Steps to Reproduce
Cause 1 and 2 (replay check bedrock-agentcore-control.gateway-rate-limit-lifecycle-transitions):
1. CreateGateway (authorizerType AWS_IAM, protocolType MCP) and poll it to READY.
2. CreateGatewayRateLimit on it with dimensionKeys=['targetName'] and one entry; poll GetGatewayRateLimit to ACTIVE. Keep the returned rateLimitId.
3. BatchPutGatewayRateLimits on the same gateway submitting two NEW limits, dimensionKeys ['toolName'] and ['qualifiedModelId'], neither carrying a rateLimitId.
4. GetGatewayRateLimit on the rateLimitId from step 2 -> emulator answers 404 ResourceNotFoundException; AWS answers 200 with status ACTIVE, and ListGatewayRateLimits still lists all three ids.
5. Inspect the 404's message -> "Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier." instead of 'Rate limit not found: <rateLimitId>'. The same wrong wording is what AWS's 'Rate limit not found: <rateLimitId>' should be after a genuine DeleteGatewayRateLimit.

Cause 3 (replay check bedrock-agentcore-control.oauth2-credential-provider-lifecycle-transitions):
1. CreateOauth2CredentialProvider with credentialProviderVendor GoogleOauth2 and a googleOauth2ProviderConfig, under some name N.
2. DeleteOauth2CredentialProvider(name=N).
3. GetOauth2CredentialProvider(name=N) -> 404 'CredentialProvider not found' (correct).
4. UpdateOauth2CredentialProvider(name=N, ...) -> 404 with message 'CredentialProvider not found'; AWS answers 'CredentialProvider not found for name: N'.
