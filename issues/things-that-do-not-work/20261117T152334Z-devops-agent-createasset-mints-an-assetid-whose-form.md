Issue-Source: trace-sweep-divergence
Issue-Trace: devops-agent-agent-space-asset-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: devops-agent-agent-space-asset-lifecycle
Issue-Shape-Id: com.amazonaws.devopsagent#DevOpsAgent
Issue-Key: devops-agent|CreateAsset|2xx->2xx|$.asset.assetId
Issue-Commit: 51b8bb69457e9ba0c27a35f7945a5491a7c1a544

# devops-agent CreateAsset mints an assetId whose form does not match the recorded asset identifier

> _Note: this issue was identified against the code base at commit `51b8bb69457e9ba0c27a35f7945a5491a7c1a544`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAsset

## Description
Single cause (Cause 1 of 1): the identifier `CreateAsset` returns in `$.asset.assetId` does not have the form the recorded service issues, so the created asset is named by a value of the wrong shape.

AWS: recorded behavior only for the FORM of the identifier. The Smithy model (services/service-com-amazonaws-devopsagent-devopsagent/smithy-model/com-amazonaws-devopsagent-devopsagent.json) constrains `Asset$assetId` only as `ResourceId` — `@length(min:1,max:128)`, `@pattern("^[a-zA-Z0-9_.-]+$")`, documented as "The unique identifier for this asset" / "Generic resource identifier" — and `CreateAsset` is modeled `POST /asset/agent-space/{agentSpaceId}/assets` with success status 201 and output member `asset` (required). The model pins no narrower form, so the requirement comes from the recording: the corpus does not assert the recorded identifier's literal text (it is per-run), it asserts its SHAPE, and the recorded shape is the form the real service issues for an asset identifier. The recorded step's status (201) is matched; only the identifier's form diverges.

Emulator: the handler mints a bare canonical hyphenated lowercase UUID for every asset and serves it as `asset.assetId`. On the replayed step the response body carried `$.asset.assetId = "d032624e-4d09-4c62-957d-e9353299ae55"` (36 characters, `8-4-4-4-12` hexadecimal groups) where the fixture expected the recorded shaped identifier, and the replay reported `$.asset.assetId: value_mismatch (expected '<shaped-not-asserted>', got 'd032624e-4d09-4c62-957d-e9353299ae55')`, verdict `mismatch`, expected status 201 / actual status 201. The emulator's identifier therefore satisfies `ResourceId` but is not of the recorded asset-identifier form. Hypothesis: the recorded form is not a bare UUID — every other per-resource identifier this service mints carries its plane's prefix (`goal-<uuid>`, `rec-<uuid>`), while the asset mint deliberately emits no prefix ("no prefix of this emulator's own invention, and no counter"), so the most likely gap is a missing prefix/opaque-token form on the asset identifier. The fixer should read the shaped expectation the fixture records for `$.asset.assetId` and mint exactly that form rather than guessing.

Why: `assetId` is the handle every later asset call is made with — `GetAsset`, `UpdateAsset`, `ListAssetFiles`, `GetAssetFile`, `DeleteAsset` all take it as a path/identifier member — and clients persist it, log it, embed it in ARNs/URLs and frequently validate it against the identifier form the real service issues. A client that round-trips or validates an identifier of the recorded form rejects or mangles the emulator's bare UUID, and any assertion on the identifier's form (a fixture, a contract test, a regex in client code) fails against the emulator while passing against AWS. The divergence is also not confined to one recording: every asset this emulator creates is named by an identifier of the wrong form.

Fix: the asset identifier mint and nothing else — the per-asset identifier is produced by the service state's asset identifier mint and handed to the `CreateAsset` handler, which stores it on the asset record and renders it through the record's `to_api()`. Change the minted form there so every asset operation (create, read, list) agrees; do not special-case the create response. Keep the uniqueness property the mint already has (a never-reused draw number inside the digest, so the identifier is neither a counter nor derived from request content) and keep the result inside `ResourceId`'s `^[a-zA-Z0-9_.-]+$` charset and 1..128 length.

Traces: fixture `devops-agent-agent-space-asset-lifecycle`, step `task#2` (operation `CreateAsset`, divergence key `service=devops-agent|CreateAsset|status=201->201|keys=$.asset.assetId`). No other step in the corpus exercises `CreateAsset`; 39 of 40 steps passed.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200): 40 steps, 39 passed, 1 mismatched, score 97.5%.

Divergence cluster (verbatim):
  fixture_id: devops-agent-agent-space-asset-lifecycle
  step_key:   task#2
  service:    devops-agent
  operation:  CreateAsset
  verdict:    mismatch
  expected_status: 201   actual_status: 201
  detail:     $.asset.assetId: value_mismatch (expected '<shaped-not-asserted>', got 'd032624e-4d09-4c62-957d-e9353299ae55')
  affected:   [devops-agent-agent-space-asset-lifecycle#task#2] (1)
  stub_path:  (none)   owned_by: (none)

Expected (AWS, recorded): HTTP 201 with `asset.assetId` of the recorded asset-identifier form; the literal text is per-run and is not asserted, the FORM is.
Actual (emulator): HTTP 201 with `asset.assetId` = a bare canonical hyphenated lowercase UUID (`8-4-4-4-12` hex, 36 chars). The quoted value `d032624e-4d09-4c62-957d-e9353299ae55` is one run's draw and is NOT part of the requirement — the shape of the value is.

Emulator code the value comes from:
  services/service-com-amazonaws-devopsagent-devopsagent/src/state.rs:1428-1470 — `AssetIdentifierMint::next()` renders SHA-256 bytes through `service_common::uuid_from_random_bytes`, i.e. a bare UUID; `ASSET_ID_LEN = 36`.
  services/service-com-amazonaws-devopsagent-devopsagent/src/state.rs:1510-1515 — `mint_asset_id()`.
  services/service-com-amazonaws-devopsagent-devopsagent/src/ops/create_asset.rs:236-278 — the handler takes the minted id, stores it on the `AssetRecord` and serves `stored.to_api()`.

Model citations: `Asset$assetId` -> `ResourceId` (`@length 1..128`, `@pattern ^[a-zA-Z0-9_.-]+$`), required; `CreateAsset` http `POST /asset/agent-space/{agentSpaceId}/assets`, code 201.

No open issue covers this: the open devops-agent issues are the `GetAssetFile` and `DeleteAssetFile` scaffold stubs and a CloudFormation resource-type issue, none of which touch the minted `assetId` form.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: `replay_wire_corpus()` over trace set simparity-wire-aws-v1.
2. Observe step `task#2` of fixture `devops-agent-agent-space-asset-lifecycle` (operation `CreateAsset`, service `devops-agent`).
3. Equivalently by hand: create an agent space, then POST `/asset/agent-space/{agentSpaceId}/assets` with a valid `assetType`, `content` (a single file) and the metadata that type requires.
4. The response is HTTP 201 and `$.asset.assetId` is a bare hyphenated UUID, which does not match the asset-identifier shape the fixture records for that member; the step is reported `mismatch` with `$.asset.assetId: value_mismatch`.
5. Re-replay the same fixture/step after changing the asset identifier mint to confirm the fix.
