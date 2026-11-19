Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance|run#18|AssertionError|assert complete_code == "AccessDeniedException", (
Issue-Commit: 53ddd9e1b1293a4c57ae86daee3ceced4b490c2e

# CompleteResourceTokenAuth is an unimplemented scaffold stub: a never-issued sessionUri answers 501 NotImplemented instead of 403 AccessDeniedException

> _Note: this issue was identified against the code base at commit `53ddd9e1b1293a4c57ae86daee3ceced4b490c2e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CompleteResourceTokenAuth

## Description
Cause 1 of 1: CompleteResourceTokenAuth never validates (or even serves) the OAuth2 resource-token session it is asked to complete — the handler is still the generated scaffold stub, so every call is answered 501 `NotImplemented`. Failing check: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance.

AWS: Smithy (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json) models CompleteResourceTokenAuth as POST /identities/CompleteResourceTokenAuth with required members `userIdentifier` (UserIdentifier) and `sessionUri` (RequestUri, @length 1..1024, pattern ^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$), an empty success output (HTTP 200), and the modeled errors AccessDeniedException (403), UnauthorizedException (401), ResourceNotFoundException (404), ValidationException (400), ThrottlingException (429), InternalServerException (500). The operation 'Confirms the user authentication session for obtaining OAuth2.0 tokens for a resource', so the service must look the session up and refuse one it never issued to this caller. Recorded behavior: a well-formed `sessionUri` that matches the modeled pattern but was never minted by an initiate-style call for the caller is refused 403 with error code `AccessDeniedException` and the fixed message `Invalid or expired session` (fixed text in full; no variable part — the requested session URI is deliberately NOT echoed). An unrecognized or foreign session URI must never be treated as a completed authorization.

Emulator: the call is refused with the non-modeled error code `NotImplemented` (HTTP 501). Replay detail: "expected AccessDeniedException for a never-issued sessionUri, got 'NotImplemented'". The handler body in services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/complete_resource_token_auth.rs returns `Err(crate::scaffolded::Scaffolded::not_implemented("CompleteResourceTokenAuth"))` and the file still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker, so neither the success path nor any refusal path exists. Note the sibling identity operations in the same check now work (GetWorkloadAccessToken and GetResourceApiKey both served the registered secret and refused a bogus workload token with AccessDeniedException 'Invalid workload access token'), so this is the only unserved step of the resource-token flow; the open issue 20261115T060723Z-bedrock-agentcore-getresourceapikey-stub.md covers GetResourceApiKey only and does not name this operation.

Why: a client completing an OAuth2 resource-token authorization cannot distinguish 'the emulator has not written this operation' from a real outcome. `NotImplemented` is not in the operation's modeled error set, so SDK/botocore error handling for AccessDeniedException, UnauthorizedException or ResourceNotFoundException never fires and no retry/consent-restart branch is taken. Worse for negative testing: a security test that asserts a foreign or expired session URI is REFUSED gets a 501 and cannot tell whether the emulator would have accepted it, so the 'unrecognized session is not a completed authorization' guarantee is unverifiable against this emulator.

Fix: implement the operation's handler and its validation/state lookup in services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/complete_resource_token_auth.rs — resolve `sessionUri` against the sessions this account actually had issued, return the empty 200 output on success, and return `AccessDeniedException` (403) with the message `Invalid or expired session` for a session URI that is unknown, expired, or bound to another caller; then delete the SCAFFOLDED marker line.

Traces: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance (replay stops at the CompleteResourceTokenAuth assertion; the GetResourceApiKey positive and bogus-token negative in the same check already pass).

## Evidence
Check: bedrock-agentcore.complete-resource-token-auth-server-validated-issuance [mismatch]

check_bedrock_agentcore_complete_resource_token_auth_server_validated_issuance.py, line 68, in run
  assert complete_code == "AccessDeniedException", (
AssertionError: expected AccessDeniedException for a never-issued sessionUri, got 'NotImplemented'

Request replayed:
  data.complete_resource_token_auth(userIdentifier={"userId": "sim2real-test"},
                                    sessionUri="urn:ietf:params:oauth:request_uri:<hex>")

Expected (AWS, recorded): HTTP 403, Error.Code = AccessDeniedException,
  Error.Message = "Invalid or expired session"  (entire message is fixed text)
Actual (emulator):        HTTP 501, Error.Code = NotImplemented — an error the model does
  not declare for this operation.

Source: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/complete_resource_token_auth.rs:4 (`//! SCAFFOLDED, NOT IMPLEMENTED:`) and :175 (`Err(crate::scaffolded::Scaffolded::not_implemented("CompleteResourceTokenAuth"))`).

Smithy: CompleteResourceTokenAuth POST /identities/CompleteResourceTokenAuth, 200 empty output; errors AccessDeniedException(403), UnauthorizedException(401), ResourceNotFoundException(404), ValidationException(400), ThrottlingException(429), InternalServerException(500); sessionUri pattern ^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$.

## Steps to Reproduce
1. Boot the emulator and point a botocore client at the bedrock-agentcore data plane.
2. control.create_workload_identity(name=<name>) and control.create_api_key_credential_provider(name=<provider>, apiKey=<secret>).
3. token = data.get_workload_access_token(workloadName=<name>)['workloadAccessToken']; confirm data.get_resource_api_key(workloadIdentityToken=token, resourceCredentialProviderName=<provider>) returns the registered secret (this step passes today).
4. Call data.complete_resource_token_auth(userIdentifier={'userId': 'sim2real-test'}, sessionUri='urn:ietf:params:oauth:request_uri:' + uuid4().hex) — a well-formed URI no initiate call ever issued.
5. Observe HTTP 501 with Error.Code 'NotImplemented'. AWS answers HTTP 403, Error.Code 'AccessDeniedException', Error.Message 'Invalid or expired session'.

Replay: behavioral check bedrock-agentcore.complete-resource-token-auth-server-validated-issuance.
