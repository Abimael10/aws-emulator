Issue-Source: trace-sweep-divergence
Issue-Trace: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text
Issue-Shape-Id: com.amazonaws.sagemakertrainingsessionruntime#LegoRouterService
Issue-Key: sagemakertrainingsessionruntime|GetSamplingOperationResult|2xx->2xx|$.operationResult
Issue-Key: sagemakertrainingsessionruntime|GetSamplingOperationResult|2xx->2xx|$.status
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|2xx->2xx|$.operationId
Issue-Commit: c57dff367a65e304f9274fadba3272377fe8966a

# sagemakertrainingsessionruntime sampling operations never produce a result: GetSamplingOperationResult omits operationResult and the minted operationId has the wrong shape

> _Note: this issue was identified against the code base at commit `c57dff367a65e304f9274fadba3272377fe8966a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSamplingOperationResult

## Description
The submit-then-poll happy path of a sampling operation diverges twice: the operation never settles into a result, and the identifier the submit hands back does not have the shape AWS's does. Two causes, each with its own fixture step.

Cause 1 of 2: a submitted sampling operation never reaches Completed and never carries operationResult.
AWS: Smithy: GetSamplingOperationResult output is status (RequestStatus, required, one of Completed|Failed|InProgress|Pending|Skipped), operationResult (SampleOperationResult) "populated only when the status is Completed", errorDetails "populated only when the status is Failed"; ExecuteSamplingOperation is documented as generating text or computing log-probabilities from the model associated with the session. The recording shows the poll after a text-generating sampling operation answering 200 with status Completed and an operationResult carrying sample.sequences[] with per-token logprobs.
Emulator: answers 200 but $.operationResult is MISSING and $.status diverges — the cluster's key set is $.operationResult,$.status. src/state.rs settles any operation whose execution would need a model runtime into the service's own Failed status with errorDetails from no_model_runtime() ("This SageMaker training-session runtime emulator runs no model, tokenizer or training engine, so the <kind> operation was not executed and produced no result."), so the sampling result member is never populated for any config.
Why: this is the service's entire purpose — a client submits a sampling operation and polls for its result. Against the emulator the poll always terminates in Failed with no operationResult, so every consumer of sample.sequences / logprobs sees a failed operation; code that distinguishes a genuine operation failure (errorDetails) from a completed sample can never take the success branch, and a polling loop waiting for Completed never gets there.
Fix: the sampling completion path in the service's state (Session::advance_sampling / the complete-sampling step that currently diverts every model-backed config into the no_model_runtime Failed status) must settle a sampling operation into Completed and populate SampleOperationResult (the union member matching the submitted operationConfig) so GetSamplingOperationResult's handler can report it.
Traces: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#task#8.

Cause 2 of 2: the operationId ExecuteSamplingOperation mints does not match the shape the recording asserts.
AWS: recorded behavior only — the fixture asserts the SHAPE of $.operationId rather than its value (expected '<shaped-not-asserted>'), and the emulator's value fails that shape assertion. Smithy constrains OperationId only to length 1..64 and pattern ^[a-zA-Z0-9-]+$, so the model does not state the format; the recording's identifier shape is the requirement. Hypothesis: AWS returns an opaque identifier with no service-authored kind prefix, whereas the emulator prefixes the operation kind.
Emulator: returns 'sampling-' followed by 24 lowercase hex characters (src/state.rs Session::mint_operation_id formats "{namespace}-{first 24 hex of sha256(seed)}" with namespace SAMPLING_NAMESPACE / TRAINING_NAMESPACE), e.g. a value of the form sampling-<24 hex>. The per-run digest is not the problem — the shape of the template is.
Why: a client that recognises or parses operation identifiers by their shape (length, character class, absence of a kind prefix), or that stores them in a shape-validated column, rejects the emulator's ids; fixture comparisons that shape-assert the id fail on every submit even though the id round-trips within the emulator.
Fix: the operation-id minting in the service's state (Session::mint_operation_id), which must emit an identifier of the shape AWS's recorded operationId has rather than a kind-prefixed digest; the training namespace mints ids the same way.
Traces: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#task#7.

No open issue covers this service; the only existing issue naming it (20261021T231759Z-bootstrap-service-com-amazonaws-sagemakertrainingsessionruntime-legorouterservice.md) is the resolved bootstrap issue.

## Evidence
Replay of trace set simparity-wire-aws-v1, service sagemakertrainingsessionruntime, neither cluster carrying stub_path or owned_by:

1. GetSamplingOperationResult — fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text step task#8, 200 vs 200, divergence key keys=$.operationResult,$.status: "$.operationResult: missing (expected {'sample': {'sequences': [{'logprobs': [-4.8144998..., -6.4570441..., ... ]}]}}, got None)" — i.e. AWS returned a completed sample with per-token log-probability sequences, the emulator returned no operationResult and a different status.
2. ExecuteSamplingOperation — same fixture step task#7, 200 vs 200: "$.operationId: value_mismatch (expected '<shaped-not-asserted>', got 'sampling-e0d5dcefb1af516ab5c24492')" — the recorded assertion is on the identifier's shape, and the emulator's kind-prefixed 24-hex id does not satisfy it.

Source corroboration: services/service-com-amazonaws-sagemakertrainingsessionruntime-legorouterservice/src/state.rs:104-116 (no_model_runtime: every model-backed operation ends Failed with no result), src/state.rs:255-281 (mint_operation_id formats "{namespace}-{24 hex}"), src/ops/get_sampling_operation_result.rs:199-203 (the handler reports op.result, which is never populated for a sampling config).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. POST /v1/training-sessions/<session>/sampling-operations with a sample (text generation) operationConfig -> note the returned operationId: it is 'sampling-' + 24 hex chars, which fails the recorded shape assertion (fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#task#7).
3. GET /v1/training-sessions/<session>/sampling-operations/<operationId> and poll -> the emulator never answers status Completed with operationResult; it settles to Failed with errorDetails about having no model runtime, where AWS answered 200 Completed with operationResult.sample.sequences[].logprobs (same fixture, step task#8).
4. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) to see both clusters.
