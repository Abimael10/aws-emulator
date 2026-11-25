Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-multi-region-endpoint-lifecycle
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-multi-region-endpoint-lifecycle
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|CreateMultiRegionEndpoint|2xx->2xx|$.EndpointId
Issue-Commit: 9b644f2075576c170e34ded6fb832e3c5e4e0784

# sesv2: CreateMultiRegionEndpoint mints an EndpointId in the wrong shape

> _Note: this issue was identified against the code base at commit `9b644f2075576c170e34ded6fb832e3c5e4e0784`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMultiRegionEndpoint

## Description
Cause 1 of 1: the multi-region-endpoint identifier CreateMultiRegionEndpoint hands back does not have the shape SES gives it.

AWS: Smithy: CreateMultiRegionEndpoint -> CreateMultiRegionEndpointResponse with members Status (enum Status: CREATING|READY|FAILED|DELETING) and EndpointId (com.amazonaws.sesv2#EndpointId, a plain string carrying NO @pattern and NO @length, documented only as "The ID of the multi-region endpoint (global-endpoint)"). The model therefore says nothing about the identifier's format, so for the format this is recorded behavior only: the fixture does not pin the id's VALUE (it is masked as '<shaped-not-asserted>', since a minted id differs on every run) but it does pin its SHAPE, and the shape the recorded SES response carries is not the shape the emulator answers with. The same id is read back by GetMultiRegionEndpoint/ListMultiRegionEndpoints and is the trailing component of the endpoint's arn:aws:ses:<region>:<account>:multi-region-endpoint/<EndpointId>, so the format is part of the response contract even though the value is per-run. Status for this step is 200 in both the recording and the emulator; only $.EndpointId diverges.

Emulator: CreateMultiRegionEndpoint answers 200 and sets EndpointId to a canonical hyphenated RFC-4122-style UUID (36 characters, 8-4-4-4-12 hex groups) minted for the resource; the replay reports '$.EndpointId: value_mismatch (expected \'<shaped-not-asserted>\', got <a 36-char hyphenated UUID>)' — i.e. the emulator's identifier fails the recorded id's shape assertion. The id is minted once at create and stored on the endpoint record, so every later read of that endpoint repeats the same wrongly shaped value.

Why: a client that does anything with the identifier beyond echoing it is misled. Code that validates or stores the id (a fixed-width column, a regex for the SES id format), that reconstructs or parses the multi-region-endpoint ARN from it, or that round-trips it into a send through the global endpoint, accepts the emulator's hyphenated UUID and then rejects or mis-parses the real AWS identifier (or vice versa). Only the shape is a requirement here: the particular id value in any one recording is not, and must never be reproduced literally.

Fix: the identifier minting for this resource, not the response serializer — the id generator the CreateMultiRegionEndpoint handler consumes (the sesv2 state's endpoint-id mint, which currently routes through the shared UUID mint) must produce the identifier shape SES hands out, and because the id is persisted on the endpoint record at create, the same change carries through GetMultiRegionEndpoint, ListMultiRegionEndpoints and the endpoint's ARN. No other open sesv2 issue covers the multi-region-endpoint identifier: 20261026T041000Z-sesv2-getmultiregionendpoint-maintainability.md is about factoring an error-document wrapper and does not touch id minting.

Traces: sesv2-multi-region-endpoint-lifecycle#task#1 (CreateMultiRegionEndpoint, 200 vs 200, $.EndpointId shape). Code: services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/create_multi_region_endpoint.rs:233 (`store.next_endpoint_id(...)`) and services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs:1843 (`next_endpoint_id` -> `minted_uuid`).

## Evidence
Fixture sesv2-multi-region-endpoint-lifecycle, step task#1, operation CreateMultiRegionEndpoint.
Expected status 200, actual status 200 — the divergence is the body member only.
Replay detail: $.EndpointId: value_mismatch (expected '<shaped-not-asserted>', got '8e0eae95-71aa-4942-a1c8-9ca4438e8efd').
Expected: EndpointId whose shape matches the identifier SES returns (value itself not asserted, because it is minted per run).
Actual: a canonical hyphenated 36-character UUID (8-4-4-4-12), minted by state.rs `next_endpoint_id` via `minted_uuid`; the quoted value is one run's id and is NOT itself a requirement.
Divergence key: service=sesv2|CreateMultiRegionEndpoint|status=200->200|keys=$.EndpointId (affected_count 1, owned_by empty, stub_path empty).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture sesv2-multi-region-endpoint-lifecycle, step task#1: POST /v2/email/multi-region-endpoints with a valid EndpointName (1..64, ^[\w\-_]+$) and Details.RoutesDetails containing exactly one route configuration.
3. Inspect $.EndpointId in the 200 response: the emulator returns a hyphenated 36-character UUID, which fails the fixture's shape assertion on the identifier SES returns.
4. Equivalently, GET /v2/email/multi-region-endpoints/{EndpointName} and observe the same wrongly shaped id read back from the stored record.
