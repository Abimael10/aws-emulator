Issue-Source: trace-sweep-divergence
Issue-Trace: supplychain-instance-error-paths
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: supplychain-instance-error-paths
Issue-Shape-Id: com.amazonaws.supplychain#GalaxyPublicAPIGateway
Issue-Key: supplychain|GetDataIntegrationFlow|4xx->2xx|$.message
Issue-Key: supplychain|GetDataIntegrationFlow|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: supplychain|GetDataIntegrationFlow|4xx->2xx|status
Issue-Commit: d33d509d5311768bd1282d02eb2794d638dad9b8

# supplychain GetDataIntegrationFlow answers 200 for a deleted flow instead of 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `d33d509d5311768bd1282d02eb2794d638dad9b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDataIntegrationFlow

## Description
ONE net-new divergence cluster reported under the wire service token `supplychain` (the same AWS Supply Chain gateway the corpus elsewhere reports as `scn`; sdkId `SupplyChain`, served by the bootstrapped crate `services/service-com-amazonaws-supplychain-galaxypublicapigateway`, so this is a fidelity finding and not a bootstrap gap). It is filed separately from the `scn`-token bundle because an issue carries exactly one service token.

Cause 1 of 1 — GetDataIntegrationFlow answers HTTP 200 with the flow body for a flow that has been deleted, on the error-path fixture that probes the service's refusals (fixture `supplychain-instance-error-paths`, step `task#13`). AWS: GetDataIntegrationFlow is GET /api/data-integration/instance/{instanceId}/data-integration-flows/{name}, @readonly, SUCCESS_STATUS 200, output {flow: DataIntegrationFlow @required}, and declares ResourceNotFoundException @httpError 404, documented "Request references a resource which does not exist" (Smithy: input {instanceId UUID @httpLabel @required, name DataIntegrationFlowName @httpLabel @required @pattern ^[A-Za-z0-9-]+$}; errors ResourceNotFoundException 404, AccessDeniedException 403, ValidationException 400, ConflictException 409, ServiceQuotaExceededException 402, ThrottlingException 429, InternalServerException 500). DeleteDataIntegrationFlow is DELETE on that same URI (@idempotent, 200); a name it has removed is no longer readable, and the recorded response is HTTP 404 carrying the restJson1 `x-amzn-errortype` header and a `message` whose FIXED text is "Flow " + " does not exist in instance ", with exactly two VARIABLE parts: the flow name from the request's `name` @httpLabel and the instance id from the request's `instanceId` @httpLabel. Emulator: HTTP 200 with the flow projection, `$.message` absent and `$headers.x-amzn-errortype` absent. The 404 branch the emulator would take is additionally mis-worded: it returns the fixed constant `crate::state::FLOW_NOT_FOUND_MESSAGE` = "Flow not found." (src/state.rs:641), which echoes neither the flow name nor the instance id — its own doc comment says the sentence was guessed from this service's not-found family and must be re-verified against a recording, and this step is that recording. Why: a client cannot tell a deleted flow from a live one. Delete looks like a no-op, a "wait until the flow is gone" poll never terminates, a re-create of the same name collides with a record the service has already reported deleted, and error-handling code that branches on ResourceNotFoundException never reaches its branch — it gets a success body for a resource that does not exist. Fix: DeleteDataIntegrationFlow's handler must remove the (instanceId, name) entry from the store's `flows` map so GetDataIntegrationFlow's existing lookup misses, and GetDataIntegrationFlow's ResourceNotFoundException message must render AWS's name-and-instance template above rather than the fixed sentence held in the state module. Hypothesis: the 200 appears because DeleteDataIntegrationFlow is still a scaffolded 501 stub, so nothing is ever removed.

Related open issues, none of which owns this cluster (the replay reports `owned_by: []` and `stub_path: ""`): the companion bundle filed in this same pass under the `scn` token covers the identical fault on fixtures `supplychain-data-integration-flow-dataset-target#task#9` / `supplychain-data-integration-flow-s3#task#13` plus the flow/event projection gaps; `20261104T203648Z-scn-deleted-data-lake-resources-stay-readable-getdatalakedataset.md` covers the same delete-visibility fault on the data-LAKE resources; `20261029T180412Z-scn-refusal-messages-are-emulator-authored-prose-instead.md` covers GetInstance/CreateDataLakeNamespace refusal prose.

Traces: supplychain-instance-error-paths#task#13.

## Evidence
Replay summary: trace_set_id=simparity-wire-aws-v1, emulator_up=true ("/__admin/health answered 200 and the :45631 data listener accepts"), bootstrap_signal=false, fixtures_replayed=9, steps_total=101, passed=42, mismatched=58, blocked=1, errored=0, divergence_count=17, score_pct=41.6, not_implemented=23.

Cluster — fixture supplychain-instance-error-paths, step task#13, operation GetDataIntegrationFlow, service supplychain, verdict mismatch, expected_status 404 / actual_status 200, affected_count 1, stub_path "", owned_by []:
  detail:
    status: expected 404, got 200
    $.message: missing (expected 'Flow <flowName> does not exist in instance <instanceId>', got None)
    $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  divergence_key: service=supplychain|GetDataIntegrationFlow|status=404->200|keys=$.message,$headers.x-amzn-errortype,status
  Message template: fixed text "Flow " + " does not exist in instance "; variable parts = the flow name (request `name` @httpLabel) and the instance id (request `instanceId` @httpLabel). The recorded literal 'Flow sim2real-probe-supplychain-flow-f99c2135 does not exist in instance 3fbfc84a-baf0-464c-bae8-1b4f55087f0e' is per-run fixture data filling that template, not the requirement.

Code evidence (checkout, services/service-com-amazonaws-supplychain-galaxypublicapigateway/):
- src/ops/delete_data_integration_flow.rs:149-167 — handle() is the scaffold body `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteDataIntegrationFlow"))`; it mutates no state, so the flow record outlives its delete.
- src/ops/get_data_integration_flow.rs:178-188 — answers 200 from `store.flow(&input.instance_id, &input.name)` whenever the key is present; the None branch returns `crate::state::FLOW_NOT_FOUND_MESSAGE`.
- src/state.rs:641 — `FLOW_NOT_FOUND_MESSAGE = "Flow not found."`, a fixed sentence echoing nothing from the request, documented as un-verified against any recording.
- src/state.rs:725-728 — the store's `flows: BTreeMap<FlowKey, DataIntegrationFlowRecord>` keyed by (instanceId, name): the entry a working delete must remove.

Model evidence (services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json, com.amazonaws.supplychain#GalaxyPublicAPIGateway, restJson1):
- GetDataIntegrationFlow: GET /api/data-integration/instance/{instanceId}/data-integration-flows/{name}, @readonly, code 200, output {flow: DataIntegrationFlow @required}; errors include ResourceNotFoundException (client, httpError 404, doc "Request references a resource which does not exist").
- DeleteDataIntegrationFlow: DELETE on the same URI, @idempotent, code 200, output {instanceId, name @required}.
- ResourceNotFoundException: structure {message: String}, errorKind client, httpError 404 — the restJson1 error carried by the `x-amzn-errortype` header the 404 must set.

## Steps to Reproduce
1. git clone the mockforge-aws-emulator repo and boot the emulator built from that checkout (the SupplyChain crate service-com-amazonaws-supplychain-galaxypublicapigateway is bootstrapped and answering).
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Replay fixture `supplychain-instance-error-paths` through step `task#13`: CreateInstance, PUT /api/data-integration/instance/{instanceId}/data-integration-flows/{name} to create a flow, DELETE that same flow URI, then GET it.
4. Observe HTTP 200 with the flow body, no `x-amzn-errortype` response header and no `message` in the body. AWS answers HTTP 404 ResourceNotFoundException with the x-amzn-errortype header and message "Flow <name> does not exist in instance <instanceId>".
5. Diff the response status, the x-amzn-errortype header and the body's `message` against the fixture's expected response.
6. Re-replay supplychain-instance-error-paths#task#13 to confirm a fix: the delete must remove the record (status) AND the not-found message must render the name-and-instance template (body), or the step still diverges on $.message.
