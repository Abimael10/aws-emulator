Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance|run#14|AssertionError|assert bad_token_code == "AccessDeniedException", (
Issue-Commit: b235f43ec24f1e947704718b25995d7af0836105

# bedrock-agentcore: GetResourceApiKey refuses a bogus workload identity token with 401 UnauthorizedException instead of AWS's 403 AccessDeniedException "Invalid workload access token"

> _Note: this issue was identified against the code base at commit `b235f43ec24f1e947704718b25995d7af0836105`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceApiKey

## Description
Cause 1 of 1: the workload-identity-token gate of GetResourceApiKey classifies an unrecognized token as the wrong modeled error.

AWS: GetResourceApiKey (POST /identities/api-key) models both AccessDeniedException (HTTP 403) and UnauthorizedException (HTTP 401) — Smithy: GetResourceApiKey errors = AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), UnauthorizedException(401), ValidationException(400); input member workloadIdentityToken -> WorkloadIdentityTokenType @length 1..131072, @sensitive. The model scopes UnauthorizedException to "the JWT bearer token is invalid or not found for OAuth bearer token based access" — i.e. the inbound OAuth bearer credential of the call — while AccessDeniedException is the insufficient-permission refusal. Recorded behavior: when the workloadIdentityToken member carries a syntactically valid but never-issued workload identity token, AWS refuses with error code AccessDeniedException and message "Invalid workload access token". That message is entirely fixed text — it names no token, provider, account or id, so nothing in it comes from the request or from state (the token is @sensitive and is never echoed).

Emulator: the same request is refused with error code 'UnauthorizedException' (HTTP 401). The positive path is correct — GetResourceApiKey with a token minted by GetWorkloadAccessToken returns exactly the secret registered by CreateApiKeyCredentialProvider — so this is purely the classification of the negative. The refusal is produced where the token check's failure is mapped: the token-validation block maps every workload_of_identity_token refusal to UnauthorizedException, and the message is whatever that refusal carries rather than AWS's fixed sentence.

Why: a client that distinguishes AWS's refusal classes gets the wrong one. AccessDeniedException is a terminal authorization verdict on the workload token presented in the body; UnauthorizedException/401 is what SDKs and middleware treat as a problem with the caller's own bearer credential, so a workload SDK sees a 401 and retries after refreshing/re-signing its transport credential — which can never fix an invalid workloadIdentityToken — instead of surfacing a permanent AccessDeniedException. Any error-code branch ("except AccessDeniedException") silently misses the case, and the 401 vs 403 status difference also changes retry and metric classification.

Fix: the token-validation arm of the GetResourceApiKey handler — it must answer the modeled AccessDeniedException (403) with the fixed message "Invalid workload access token" for a token this service did not issue or whose window has closed, leaving the provider-resolution refusal (ResourceNotFoundException 404) and the IAM authorization refusal unchanged. Note the handler's module doc states the 401 choice deliberately, so the documented contract in that file needs correcting alongside the mapping.

Note: the open issue 20261115T060723Z-bedrock-agentcore-getresourceapikey-stub.md ("Implement bedrock-agentcore GetResourceApiKey: unimplemented scaffold stub") does not cover this — the operation now serves the happy path and returns the real stored key; only the negative's error class diverges. The same check also asserts CompleteResourceTokenAuth refuses a never-issued sessionUri with AccessDeniedException "Invalid or expired session", but the check aborts at the GetResourceApiKey assertion before that call is made, so that behavior is unobserved and is not claimed here.

Traces: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance.

## Evidence
Check bedrock-agentcore.complete-resource-token-auth-server-validated-issuance, [mismatch]:

  File "check_bedrock_agentcore_complete_resource_token_auth_server_validated_issuance.py", line 52, in run
    assert bad_token_code == "AccessDeniedException", (
  AssertionError: expected AccessDeniedException for a bogus workload identity token, got 'UnauthorizedException'

Expected (AWS, from the recording): HTTP 403, Error.Code = AccessDeniedException, Error.Message = "Invalid workload access token" (fixed text in full; no request- or state-derived parts).
Actual (emulator): Error.Code = UnauthorizedException, HTTP 401.

The preceding positive assertions in the same check passed: GetResourceApiKey with a token from GetWorkloadAccessToken returned exactly the apiKey registered via CreateApiKeyCredentialProvider, so the operation is implemented and only the bogus-token refusal class diverges.

Source of the divergence: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/get_resource_api_key.rs lines 188-204 — every crate::state::workload_of_identity_token refusal is mapped to GetResourceApiKeyErrorModeled::UnauthorizedException, with the refusal's own message; the module doc at lines 10-13 declares that 401 as the intended contract.

## Steps to Reproduce
1. control = bedrock-agentcore-control client; data = bedrock-agentcore client.
2. control.create_workload_identity(name=<fresh alnum name <=40 chars>).
3. control.create_api_key_credential_provider(name=<fresh alnum name <=40 chars>, apiKey=<secret>).
4. token = data.get_workload_access_token(workloadName=<name from step 2>)["workloadAccessToken"].
5. Positive control: data.get_resource_api_key(workloadIdentityToken=token, resourceCredentialProviderName=<name from step 3>) -> returns the secret from step 3 (passes today).
6. data.get_resource_api_key(workloadIdentityToken="not-a-real-workload-identity-token." + uuid4().hex, resourceCredentialProviderName=<name from step 3>).
   Observed: ClientError with Code 'UnauthorizedException' (HTTP 401).
   Expected: ClientError with Code 'AccessDeniedException' (HTTP 403) and Message 'Invalid workload access token'.
