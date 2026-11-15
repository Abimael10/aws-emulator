Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.delete-api-key-credential-provider-nonexistent-not-found
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteApiKeyCredentialProvider|ResourceNotFoundException
Issue-Commit: 41fa950ddd6915a1c40615232d76af513e3c005f

# DeleteApiKeyCredentialProvider is not idempotent: an unknown provider name is refused 404 ResourceNotFoundException instead of succeeding as a no-op

> _Note: this issue was identified against the code base at commit `41fa950ddd6915a1c40615232d76af513e3c005f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteApiKeyCredentialProvider

## Description
One net-new root cause in bedrock-agentcore-control.

Cause 1 of 1: DeleteApiKeyCredentialProvider refuses a name that no provider exists under, instead of treating the delete as an idempotent no-op.

AWS: Smithy model (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json) marks DeleteApiKeyCredentialProvider `@idempotent`, with POST /identities/DeleteApiKeyCredentialProvider, success status 204 and an EMPTY output shape (DeleteApiKeyCredentialProviderResponse has no members). Recorded behavior: deleting a never-created provider name succeeds — it returns the 204 empty success, and repeating the same delete on the still-absent name succeeds again, while other providers in the account are left untouched. ResourceNotFoundException (HTTP 404) is modeled for this operation but, per the recording, is not raised for a merely-absent name; the recording shows the not-found refusal only on the READ path, where GetApiKeyCredentialProvider for an absent name answers ResourceNotFoundException with message `ApiKeyCredentialProvider not found for <name>` (fixed text `ApiKeyCredentialProvider not found for `, variable part = the `name` from the request).

Emulator: the delete looks the name up in state and converts a miss into a refusal. The replayed check gets `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DeleteApiKeyCredentialProvider operation: ApiKeyCredentialProvider not found for <name>` (HTTP 404) on the first delete of a never-created name, so the call never reaches the following idempotent re-delete, the untouched-neighbour assertion, or the positive control that deletes the real provider.

Why: a client relying on AWS's behavior gets a hard failure where AWS gives it success. Idempotent deletes are how teardown/cleanup code, retry wrappers and CloudFormation-style reconcilers converge: a retried delete whose first attempt already removed the provider, or a cleanup that deletes a provider it is not sure exists, now raises ResourceNotFoundException and aborts the teardown instead of completing. Any retry of a delete whose response was lost in flight becomes a spurious error.

Fix: the DeleteApiKeyCredentialProvider handler. Removing an absent key must not be an error — drop the `ok_or_else(... ResourceNotFoundException ...)` on the store removal and return the empty 204 output whether or not a record was present, keeping the not-found refusal on the read/update paths where the recording shows it.

Note: the open stub issue 20261108T132909Z-bedrock-agentcore-control-deleteapikeycredentialprovider-stub.md does NOT cover this — the operation is now served (it answers a modeled 404, not NotImplemented); the defect is the refusal's existence, not a missing implementation.

Traces: bedrock-agentcore-control.delete-api-key-credential-provider-nonexistent-not-found. (That same check also asserts an UpdateApiKeyCredentialProvider not-found message, which is unreachable here and is already covered by the filed UpdateApiKeyCredentialProvider stub issue.)

## Evidence
Check: bedrock-agentcore-control.delete-api-key-credential-provider-nonexistent-not-found [error]

Expected (AWS, recorded): client.delete_api_key_credential_provider(name=<never-created-name>) succeeds — HTTP 204, empty body — and succeeds again on a second call; the real provider created earlier is still readable via GetApiKeyCredentialProvider.

Actual (emulator):
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DeleteApiKeyCredentialProvider operation: ApiKeyCredentialProvider not found for sim2real-apikeycred-missing-6fa8c2c46f85

Status/code/message quoted exactly: HTTP 404, code `ResourceNotFoundException`, message `ApiKeyCredentialProvider not found for <name>` — fixed text `ApiKeyCredentialProvider not found for `, variable part the request's `name` (the `sim2real-apikeycred-missing-...` suffix is this run's generated name, not part of any requirement). The requirement is that NO refusal is emitted at all for this call.

Code: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_api_key_credential_provider.rs lines 165-173 — `store.api_key_credential_providers.remove(&input.name).ok_or_else(|| ...ResourceNotFoundException...)?`.

Model: DeleteApiKeyCredentialProvider is `@idempotent`, http {method: POST, uri: /identities/DeleteApiKeyCredentialProvider, code: 204}, output shape has no members.

## Steps to Reproduce
1. Create an API key credential provider: create_api_key_credential_provider(name=<real-name>, apiKey="dummy-api-key-value-123").
2. Pick a second name that was never created, <missing-name>.
3. get_api_key_credential_provider(name=<missing-name>) -> ResourceNotFoundException "ApiKeyCredentialProvider not found for <missing-name>" (correct, matches AWS).
4. delete_api_key_credential_provider(name=<missing-name>) -> AWS: HTTP 204 success. Emulator: 404 ResourceNotFoundException "ApiKeyCredentialProvider not found for <missing-name>".
5. Repeat step 4 -> AWS: success again (idempotent). Emulator: same 404.
6. get_api_key_credential_provider(name=<real-name>) -> still present in both (never reached in the emulator run because step 4 raised).
