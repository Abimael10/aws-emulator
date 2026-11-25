Issue-Source: trace-sweep-divergence
Issue-Trace: neptunedata-query-languages-live-cluster-round-trip
Issue-Step: task#22
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptunedata-query-languages-live-cluster-round-trip
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata|ExecuteGremlinQuery|4xx->4xx|$.code
Issue-Key: neptunedata|ExecuteGremlinQuery|4xx->4xx|$.requestId
Issue-Key: neptunedata|CreateMLEndpoint|4xx->4xx|$.code
Issue-Key: neptunedata|CreateMLEndpoint|4xx->4xx|$.requestId
Issue-Commit: 95ee4d05f76278f15dd45811b926fa4843973dc6

# neptunedata modelled-error bodies carry the numeric HTTP status in code instead of the exception class name

> _Note: this issue was identified against the code base at commit `95ee4d05f76278f15dd45811b926fa4843973dc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteGremlinQuery, CreateMLEndpoint

## Description
TWO causes, one shared fault, both reached against a RUNNING, serving emulator (31 of 43 steps of the same fixture passed and both of these steps answered with the correct HTTP status), so neither is the open unreachability item 20261109T113928Z-neptunedata-is-still-entirely-unreachable-the-neptune-db.md. In both the emulator classifies the refusal correctly and words detailedMessage correctly, but the `code` member of the modelled Neptune error body carries the NUMERIC HTTP STATUS as a string instead of the exception class name a Neptune client branches on. The neptunedata error-payload builders in this crate hard-code `code` to the status digits (services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state.rs: invalid_query_id ~755-761, invalid_parameter_range ~769-783, missing_parameter ~788-794, invalid_fast_reset_token ~802-809, unsupported_action ~813-823, store_unavailable_* ~833-871, missing_one_of_parameters ~875-885 and src/state/ml.rs: ml_resource_not_found 37-43, ml_endpoint_limit 55-61, ml_job_not_completed 66-77, ml_endpoint_exists 81-91), so this is ONE shared serialization/payload defect surfacing through two operations' refusal paths.

Cause 1 of 2 — ExecuteGremlinQuery malformed-query refusal reports code "400" instead of "MalformedQueryException" (fixture neptunedata-query-languages-live-cluster-round-trip, step task#22).
AWS: Smithy (com.amazonaws.neptunedata) models ExecuteGremlinQuery as POST /gremlin (success 200) with MalformedQueryException among its errors (errorKind client, httpError 400, doc "Raised when a query is submitted that is syntactically incorrect or does not pass additional validation"); the shape's required members are detailedMessage, requestId and code. Recorded behavior shows how Neptune fills that body for this error class: `code` is the EXCEPTION CLASS NAME — the fixed string "MalformedQueryException" — despite the member's prose "The HTTP status code returned with the exception". detailedMessage is the Gremlin parser's own diagnostic and it already MATCHED on replay, so it is not part of this finding. requestId is a per-run value (shaped-not-asserted) and is likewise not part of this finding.
Emulator: for the same syntactically invalid traversal it answers 400 with `$.code` = "400". The handler routes the refusal correctly (src/ops/execute_gremlin_query.rs:232-240 maps a QueryRejection::Malformed to MalformedQueryException via crate::state::malformed_gremlin_query), and only the `code` value on the wire is wrong.
Why: a client that branches on the error class as Neptune reports it in the BODY (the Gremlin/Neptune HTTP clients and every tool that logs or retries on the Neptune `code` string) sees the literal "400" and cannot tell a malformed query from any other 400-class refusal of the same endpoint — MalformedQueryException (do not retry, fix the query) is indistinguishable from e.g. ConstraintViolationException or TooManyRequestsException handling driven off that member.
Fix: the shared neptunedata modelled-error payload construction / error serializer (state.rs and state/ml.rs builders above, and the malformed_gremlin_query builder behind src/ops/execute_gremlin_query.rs:237) — `code` must carry the exception class name for every modelled error of this service, not the status digits.
Traces: neptunedata-query-languages-live-cluster-round-trip#task#22.

Cause 2 of 2 — CreateMLEndpoint unknown-ML-resource refusal reports code "404" instead of "MLResourceNotFoundException" (fixture neptunedata-query-languages-live-cluster-round-trip, step task#29).
AWS: Smithy models CreateMLEndpoint as POST /ml/endpoints (success 200) with MLResourceNotFoundException among its errors (errorKind client, httpError 404, doc "Raised when a specified machine-learning resource could not be found"); required members detailedMessage, requestId, code. Recorded behavior: `code` is the fixed exception class name "MLResourceNotFoundException". The refusal's detailedMessage matched on replay and is wholly fixed text with no variable part, so it is not part of this finding; requestId is a per-run value and is not asserted.
Emulator: for a create naming a model job this engine does not hold it answers 404 with `$.code` = "404" (crate::state::ml_resource_not_found, src/state/ml.rs:37-43, hard-codes code: "404"; reached from src/ops/create_ml_endpoint.rs:235-238 and 266-269).
Why: the same client-visible consequence as Cause 1 on the ML control path — a client cannot distinguish "the id names no ML resource" from any other 404 from that body, so a caller that creates-or-updates an endpoint by reacting to MLResourceNotFoundException cannot drive its branch off the body at all.
Fix: the same shared payload/serializer layer (src/state/ml.rs builders plus the crate's modelled-error serialization), not this one handler.
Traces: neptunedata-query-languages-live-cluster-round-trip#task#29.

Related but NOT covering these two steps (the replay marked neither as owning them): 20261115T153652Z-neptunedata-executeopencypherquery-malformed-query-refusal-body-diverges-code.md (the same `code` fault plus a missing message member and wrong parser text, on ExecuteOpenCypherQuery/task#23 only), 20261109T174758Z-neptunedata-data-plane-responses-diverge-from-neptune-engine.md (engine-status payload, content-type, fast-reset refusal), 20261115T144528Z-neptunedata-refusal-paths-diverge-ml-opencypher-status-cancel.md (501 NotImplemented refusal paths), and 20261104T225928Z-neptunedata-executegremlinquery-stub.md (ExecuteGremlinQuery's evaluation path still answering 501 — its VALIDATION path is implemented and is what diverges here).

## Evidence
Fixture neptunedata-query-languages-live-cluster-round-trip, verdict mismatch on both steps; the emulator was up (/__admin/health 200, data listener accepting) and 31/43 steps of this fixture passed.

Cause 1 — step task#22, ExecuteGremlinQuery, expected_status 400 / actual_status 400:
- $.code: expected 'MalformedQueryException' (fixed exception-class text), got '400'.
- $.requestId: '<shaped-not-asserted>' per-run value, NOT part of this finding.
No other member diverged: detailedMessage and the HTTP status matched.

Cause 2 — step task#29, CreateMLEndpoint, expected_status 404 / actual_status 404:
- $.code: expected 'MLResourceNotFoundException' (fixed exception-class text), got '404'.
- $.requestId: '<shaped-not-asserted>' per-run value, NOT part of this finding.
No other member diverged: detailedMessage ('Invalid ID provided. Please check whether a resource with the given ID exists.') and the HTTP status matched.

In both bodies the only wrong value is `code`: the emulator writes the numeric HTTP status, AWS writes the exception class name.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture 'neptunedata-query-languages-live-cluster-round-trip' (replay_wire_corpus), or reproduce each cause by hand:
2. Point a neptunedata client at the emulator's data listener.
3. Cause 1: POST /gremlin with a syntactically invalid Gremlin traversal (the fixture submits a traversal the parser cannot read). Inspect the raw 400 body: `code` is "400"; Neptune returns `code` = "MalformedQueryException".
4. Cause 2: POST /ml/endpoints with mlModelTrainingJobId (or mlModelTransformJobId) naming a job the engine does not hold. Inspect the raw 404 body: `code` is "404"; Neptune returns `code` = "MLResourceNotFoundException".
5. Confirm the fix by re-replaying fixture neptunedata-query-languages-live-cluster-round-trip steps task#22 AND task#29 (both causes must be closed).
