Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemakertrainingsessionruntime.sampling-operation-generates-text-from-running-session sagemakertrainingsessionruntime.sampling-operation-result-not-found-for-unknown-operation-id
Issue-Shape-Id: com.amazonaws.sagemakertrainingsessionruntime#LegoRouterService
Issue-Key: sagemakertrainingsessionruntime.sampling-operation-generates-text-from-running-session|run#37|AssertionError|assert status == "Completed", f"expected the sampling operation to complete, got {status!r}"
Issue-Key: sagemakertrainingsessionruntime.sampling-operation-result-not-found-for-unknown-operation-id|run#35|AssertionError|assert control["status"] == "Completed", (
Issue-Commit: c12a418991a039510418a5f65565a057a482789e

# sagemakertrainingsessionruntime: a sampling operation submitted against a Running training session always settles in Failed with no operationResult, so GetSamplingOperationResult never reports Completed

> _Note: this issue was identified against the code base at commit `c12a418991a039510418a5f65565a057a482789e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSamplingOperationResult

## Description
One net-new behavioral root cause (Cause 1 of 1).

Cause 1 of 1: the submit-then-poll happy path of a sampling operation never completes — every operation whose execution would need a model runtime is settled into the terminal status `Failed` with `errorDetails`, and `operationResult` is never populated for any `operationConfig`.

AWS:      Smithy (com.amazonaws.sagemakertrainingsessionruntime#LegoRouterService): `ExecuteSamplingOperation` POST /v1/training-sessions/{trainingSessionName}/sampling-operations, 200, output `operationId` (required, @length 1..64, @pattern ^[a-zA-Z0-9-]+$); `GetSamplingOperationResult` GET /v1/training-sessions/{trainingSessionName}/sampling-operations/{operationId}, 200, output `status` (RequestStatus, required, one of Pending|InProgress|Completed|Failed|Skipped), `operationResult` (SampleOperationResult — "The result of a completed sampling operation. Exactly one member is populated, corresponding to the operation type that was submitted"), `errorDetails` (String). `SampleOperationResult$sample` -> `SampleResponse{sequences (required, SampledSequenceList), promptLogprobs, topkPromptLogprobs}`; `SampledSequence{stopReason (required, StopReason: stop|length), tokens (required), logprobs, tokenUsage}`; `SampleTokenUsage{prefill (required), decode (required)}`. So for a `sample` config submitted against a Running session, AWS answers 200 with `status="Completed"` and `operationResult.sample.sequences[]` carrying generated `tokens`, a `stopReason` in {stop,length}, and `tokenUsage` whose `prefill` is the submitted prompt's token count and whose `decode` is the number of generated tokens. `Failed`/`errorDetails` is reserved for an operation that actually failed.

Emulator: `ExecuteSamplingOperation` accepts the submit and returns an `operationId`, but polling `GetSamplingOperationResult` terminates at `status="Failed"` (never `Completed`) and carries no `operationResult`; the `errorDetails` is the emulator's own text from `no_model_runtime()`: "This SageMaker training-session runtime emulator runs no model, tokenizer or training engine, so the <kind> operation was not executed and produced no result." This happens for a plain text-generation `sample` config (prompt chunks of encoded_text, numSamples=1, samplingParams.maxTokens/temperature) on a session that `DescribeTrainingSession` already reports as `Running`, i.e. on the fully-provisioned happy path, and it happens identically when the completed operation is only needed as a positive control for an unknown-operationId check.

Why:      this is the service's entire purpose: a client submits a sampling operation and polls for its result. Against the emulator a polling loop waiting for `Completed` never gets there, and every consumer of `operationResult.sample.sequences` (tokens, logprobs, stopReason, tokenUsage.prefill/decode) sees nothing. Worse, the failure is reported in the service's own modelled failure channel, so a client cannot distinguish a genuine operation failure from an emulator that simply does not execute operations — error-handling and retry/alarm paths fire on a healthy request. Any check that needs a resolvable, completed operation id (for example to contrast it with an unknown one) cannot establish its precondition either.

Fix:      the sampling completion path in the service's state — the advance/complete-sampling step that currently diverts every model-backed config into the `no_model_runtime` `Failed` status — must settle a sampling operation into `Completed` and populate `SampleOperationResult` with the union member matching the submitted `operationConfig`, so `GetSamplingOperationResult`'s handler reports `status="Completed"` plus a `sample` result whose `sequences[0]` has a non-empty `tokens`, a `stopReason` of `stop` or `length`, and `tokenUsage.prefill` = the submitted prompt token count and `tokenUsage.decode` = the generated token count. `errorDetails` must then be emitted only for operations that genuinely fail.

Traces:   behavioral checks sagemakertrainingsessionruntime.sampling-operation-generates-text-from-running-session (asserts `status == "Completed"`, got 'Failed') and sagemakertrainingsessionruntime.sampling-operation-result-not-found-for-unknown-operation-id (its positive control asserts the submitted operation completes, got 'Failed'). Source corroboration: services/service-com-amazonaws-sagemakertrainingsessionruntime-legorouterservice/src/state.rs:104-116 (`no_model_runtime`), src/ops/get_sampling_operation_result.rs (reports the operation's never-populated result).

Related but not covering: the open wire-corpus issue 20261117T022256Z-sagemakertrainingsessionruntime-sampling-operations-never-produce-a-result-getsamplingoperationresult.md describes the same completion gap from the wire fixture side; it carries only wire replay keys (sagemakertrainingsessionruntime|GetSamplingOperationResult|2xx->2xx|$.operationResult, $.status) and does not carry these behavioral check ids, so these two checks are filed here with their replay keys. A fixer closing both should re-replay this issue's trace_ids as well.

## Evidence
Replay of suite simparity-behavioral-aws-v1, service sagemakertrainingsessionruntime (3/16 passed).

1. [mismatch] sagemakertrainingsessionruntime.sampling-operation-generates-text-from-running-session — after CreateTrainingSession (LoRA rank 8/alpha 16, public-hub base model, S3 OutputConfig, model-package-group) reached TrainingSessionStatus 'Running', ExecuteSamplingOperation with operationConfig={"sample": {prompt: {chunks: [{tokens: [...], type: "encoded_text"}]}, numSamples: 1, samplingParams: {maxTokens: 512, temperature: 0.7}}} returned an operationId, and polling GetSamplingOperationResult left InProgress into a terminal status:
   AssertionError: expected the sampling operation to complete, got 'Failed'
   Expected (AWS): status == "Completed", operationResult.sample.sequences length 1, sequences[0].tokens non-empty, sequences[0].stopReason in ("stop", "length"), sequences[0].tokenUsage.decode == len(tokens), tokenUsage.prefill == number of prompt tokens submitted.
   Actual (emulator): status == "Failed", no operationResult; errorDetails is the emulator's no_model_runtime text.

2. [mismatch] sagemakertrainingsessionruntime.sampling-operation-result-not-found-for-unknown-operation-id — the same submit on a Running session is used as the check's positive control before it probes an unknown operationId:
   AssertionError: positive control: expected the submitted operation to complete, got 'Failed'

Neither check was marked ALREADY FILED or OWNED by the suite report.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Create the prerequisites a training session needs: an IAM role trusted by training-session.sagemaker.amazonaws.com, an S3 output bucket, and a SageMaker model package group.
3. sagemaker CreateTrainingSession with LoraConfig, RoleArn, ModelConfig.BaseModelArn (SageMakerPublicHub model, AcceptEula=true), OutputConfig.S3Location and ModelPackageGroupArn; poll DescribeTrainingSession until TrainingSessionStatus == "Running".
4. POST /v1/training-sessions/<session>/sampling-operations (ExecuteSamplingOperation) with operationConfig={"sample": {"prompt": {"chunks": [{"tokens": [791,6864,315,9822,374], "type": "encoded_text"}]}, "numSamples": 1, "samplingParams": {"maxTokens": 512, "temperature": 0.7}}}; note the returned operationId.
5. GET /v1/training-sessions/<session>/sampling-operations/<operationId> (GetSamplingOperationResult) and poll while status == "InProgress".
6. Observe the terminal status is "Failed" with errorDetails and no operationResult, where AWS answers status "Completed" with operationResult.sample.sequences[] (tokens, stopReason, tokenUsage.prefill/decode).
7. Re-run the behavioral suite checks sampling-operation-generates-text-from-running-session and sampling-operation-result-not-found-for-unknown-operation-id to see both failures.
