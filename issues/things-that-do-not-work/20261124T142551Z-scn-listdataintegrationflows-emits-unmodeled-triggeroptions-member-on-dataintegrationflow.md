Issue-Source: trace-sweep-divergence
Issue-Trace: supplychain-data-integration-flow-s3
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: supplychain-data-integration-flow-s3
Issue-Shape-Id: com.amazonaws.supplychain#GalaxyPublicAPIGateway
Issue-Key: scn|ListDataIntegrationFlows|2xx->2xx|$.flows[name="sim2real-probe-supplychain-flow-f78abe90"].triggerOptions
Issue-Commit: 2a76591b9f2cb1e07a23d0fccab7ab8509532f71

# scn ListDataIntegrationFlows emits unmodeled triggerOptions member on DataIntegrationFlow entries

> _Note: this issue was identified against the code base at commit `2a76591b9f2cb1e07a23d0fccab7ab8509532f71`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDataIntegrationFlows

## Description
Single cause (Cause 1 of 1): the emulator's ListDataIntegrationFlows response adds a member to each DataIntegrationFlow entry that the AWS Supply Chain API does not define.

AWS: Smithy model services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json. ListDataIntegrationFlows is GET /api/data-integration/instance/{instanceId}/data-integration-flows, HTTP 200, output shape ListDataIntegrationFlowsResponse { flows: DataIntegrationFlowList (required), nextToken }. The element shape com.amazonaws.supplychain#DataIntegrationFlow has EXACTLY seven members and no others: instanceId, name, sources, transformation, target, createdTime, lastModifiedTime. There is no `triggerOptions` member anywhere in the DataIntegrationFlow closure (and no triggerOptions input on CreateDataIntegrationFlow, whose input is instanceId/name/sources/transformation/target/tags). The recorded AWS response for a flow created with an S3 source therefore contains no `triggerOptions` key at all for the listed flow.

Emulator: for the same listing the emulator returns 200 but each flow object carries an extra key, e.g. $.flows[name="<flowName>"].triggerOptions = {"triggerType": "SOURCE_CHANGE"}. The replay reports this as a type_mismatch at that JSON path: expected the member to be absent (None), got an object. The value "SOURCE_CHANGE" and the flow name in the path are per-run values from the recording, not part of the requirement; the requirement is that no `triggerOptions` key is serialized for any flow, whatever its source type.

Why: a client that round-trips a listed flow back into CreateDataIntegrationFlow/UpdateDataIntegrationFlow, or that validates responses against the modeled shape (strict SDK deserializers, schema validators, generated typed clients in languages that reject unknown members, snapshot/diff tooling, or IaC drift detection), sees a member AWS never sends. Code generated from the real model has no field to bind it to, and any consumer that treats the response as the canonical flow definition will believe a trigger configuration exists on the resource when AWS exposes none. Tests written against real AWS also diff against the emulator purely because of this injected key.

Fix: the ListDataIntegrationFlows handler/serializer in the supply chain service crate — the DataIntegrationFlow projection used to render stored flow records into the response must emit only the modeled members (instanceId, name, sources, transformation, target, createdTime, lastModifiedTime) and must drop internal bookkeeping such as the trigger descriptor. If the trigger descriptor is kept in the stored flow record for internal scheduling purposes, it belongs in state only and must be stripped on the way out; the same projection should be checked for GetDataIntegrationFlow so the two stay consistent.

Traces: supplychain-data-integration-flow-s3#task#8 (ListDataIntegrationFlows, expected 200 / actual 200, body member divergence). Relevant code: services/service-com-amazonaws-supplychain-galaxypublicapigateway/src/ops/ (ListDataIntegrationFlows operation handler) and .../src/state.rs (stored flow record). Two other scn divergences in this corpus (CreateDataLakeNamespace validation wording, GetDataIntegrationFlowExecution not-found wording) are already owned by open issues 20261029T180412Z-scn-refusal-messages-are-emulator-authored-prose-instead.md and 20261117T020156Z-scn-data-integration-event-ingest-diverges-dataset-load.md and are not part of this issue.

## Evidence
Replay cluster (trace set simparity-wire-aws-v1), fixture supplychain-data-integration-flow-s3, step task#8, service scn, operation ListDataIntegrationFlows, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.

detail: $.flows[name="sim2real-probe-supplychain-flow-f78abe90"].triggerOptions: type_mismatch (expected None, got {'triggerType': 'SOURCE_CHANGE'})

Expected (AWS, recorded): the listed flow object has only the modeled DataIntegrationFlow members — instanceId, name, sources, transformation, target, createdTime, lastModifiedTime — and no triggerOptions key.
Actual (emulator): the same flow object additionally contains "triggerOptions": {"triggerType": "SOURCE_CHANGE"}.

Model evidence: smithy_query query=shape shape=DataIntegrationFlow on services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json lists exactly those seven members; triggerOptions is absent from the shape and from CreateDataIntegrationFlowRequest.

## Steps to Reproduce
1. Boot the emulator from this checkout (the scn / AWS Supply Chain service is bootstrapped; get_bootstrap_status reports no pending services).
2. Create a Supply Chain instance, then create a data-integration flow whose single source is an S3 source (sourceType S3, s3Source.bucketName + s3Source.prefix) with a SQL transformation and a dataset target — i.e. replay fixture supplychain-data-integration-flow-s3 up to step task#8.
3. Call GET /api/data-integration/instance/{instanceId}/data-integration-flows (ListDataIntegrationFlows) for that instance.
4. Inspect the returned flows[] entries: the emulator includes a "triggerOptions": {"triggerType": ...} member. AWS returns the flow with only the modeled DataIntegrationFlow members and no triggerOptions key.
5. Equivalently: replay_wire_corpus() and observe the mismatch cluster keyed service=scn|ListDataIntegrationFlows|status=200->200|keys=$.flows[...].triggerOptions.
