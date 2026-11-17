Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|ImportApiKeys|2xx->2xx|$.ids[*]
Issue-Commit: 8bfca88889e1b421dc97dac5447f26fe2e538409

# apigateway ImportApiKeys mints a new API key id instead of returning the existing key's id

> _Note: this issue was identified against the code base at commit `8bfca88889e1b421dc97dac5447f26fe2e538409`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportApiKeys

## Description
ImportApiKeys returns an identity for an imported CSV row that is not continuous with the API key the same fixture already created in this account: the emulator mints a brand-new key id where the recording returns the id of the key that already exists.

Cause 1 of 1: ImportApiKeys' ApiKeyIds.ids entry for a CSV row whose key already exists in the account is a freshly minted key id instead of that existing key's id.

AWS: Smithy (apigateway / BackplaneControlService): ImportApiKeys -> POST /apikeys?mode=import, HTTP 201, output shape ApiKeyIds with members ids (ListOfString, "A list of all the ApiKey identifiers") and warnings (ListOfString, "A list of warning messages"). The model documents nothing further about identity continuity, so for the identity rule this is recorded behavior only: in the recorded run the ids entry for this import is the identifier of the API key that an earlier step of the same fixture had already created from the same CSV material - i.e. an import that re-states an already-present key resolves to that key's existing identifier (optionally alongside a warning) rather than creating a second key. The replay harness substitutes the current run's captured identifiers into the expectation, which is why the expected value is a concrete id minted earlier in THIS run and not a recording-only literal.

Emulator: the replayed ImportApiKeys answers 201 with ids[0] set to an identifier that exists nowhere else in the run - the emulator created an additional API key for the CSV row rather than resolving it to the key already held in state. Replay detail: "$.ids[0]: value_mismatch (expected 'nk2nmd5t36', got '4d8stnq7hm')" (the two concrete ids are per-run values that fill the template; the requirement is that the returned id is the id of the key already present for that CSV row, not that it equals any literal).

Why: a client that imports a key CSV more than once (bulk provisioning, re-running a provisioning script, or the documented overwrite/retry flow) ends up with two API keys for one logical key, and the id it gets back names the duplicate. Every follow-up addressed by that id - GetApiKey, CreateUsagePlanKey, usage metering, DeleteApiKey - then operates on the wrong key, and the account's key listing contains a phantom duplicate that never appears against real API Gateway.

Why (status/code/message): the status and shape are otherwise correct (201, ApiKeyIds); no error wording diverges in this step. The divergence is confined to the identity carried in ids.

Fix: ImportApiKeys' handler and the API-key state it writes - the CSV row must be resolved against the existing API keys (by key name / key value, as the import already does for its warnings) and the existing key's identifier returned in ids, with a new key minted only for rows that name no existing key.

Traces: apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation#task#7 (ImportApiKeys, expected 201 / actual 201, ids[0] diverges). No other open issue carries this step's replay key; the nearest open apigateway issues (20261113T185449Z-apigateway-2-net-new-causes-importdocumentationparts-failonwarnings-rejection.md on ImportDocumentationParts warnings, and 20261107T204751Z-apigateway-api-gateway-documentation-parts-method-location-parts-dropped.md on ImportDocumentationParts merge ids) cover documentation-part imports, not API-key imports, and do not cover this behavior.

## Evidence
Replay of fixture apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation, step task#7, operation ImportApiKeys, verdict mismatch, expected_status 201, actual_status 201.

Expected (recorded, with this run's captured ids substituted): ids[0] = the identifier of the API key already created earlier in the fixture ('nk2nmd5t36' in this run).
Actual (emulator): ids[0] = '4d8stnq7hm', a newly minted key id not bound to any earlier step of the run.

Raw divergence detail: "$.ids[0]: value_mismatch (expected 'nk2nmd5t36', got '4d8stnq7hm')"
divergence_key: service=apigateway|ImportApiKeys|status=201->201|keys=$.ids[0]
affected: apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation#task#7 (1 step)
Smithy consulted: ImportApiKeys (POST /apikeys?mode=import, 201, output ApiKeyIds{ids: ListOfString, warnings: ListOfString}).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture apigateway-import-api-keys-csv-bulk-and-failonwarnings-validation up to step task#7: the earlier steps import/create the API keys from the CSV material and the harness captures the resulting key id.
3. At task#7, POST /apikeys?mode=import with a CSV that re-states a key already present in the account (format=csv).
4. Observe the 201 response's ids[0]: the emulator returns a newly minted key id, while the recording returns the id of the key already created in step 2. Confirm with GetApiKeys that the emulator now holds two keys for the one logical CSV row.
