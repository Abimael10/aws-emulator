Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.update-provisioned-throughput-concurrent-race-one-wins bedrock.e2e-evaluation-job-runs-and-is-purged
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock.update-provisioned-throughput-concurrent-race-one-wins|run#3|AssertionError|assert code == "ValidationException", (
Issue-Key: bedrock.e2e-evaluation-job-runs-and-is-purged|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# bedrock: 2 net-new causes — commitment-free CreateProvisionedModelThroughput over a PT-ineligible model is refused ServiceQuotaExceededException instead of ValidationException, and an automated evaluation job never shows a terminal status to the first read

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProvisionedModelThroughput

## Description
Two net-new root causes in the Amazon Bedrock control plane, both grounded in failing behavioral checks of this suite. Neither is covered by an open issue: the existing BatchDeleteEvaluationJob issues (20261108T100038Z-bedrock-batchdeleteevaluationjob-echoes-the-full-job-arn-in.md, 20261030T151259Z-bedrock-batchdeleteevaluationjob-validation-message-omits-the-rejected-jobidentifiers.md, 20261026T233704Z-bedrock-batchdeleteevaluationjob-s-non-terminal-status-refusal-uses.md) all concern BatchDeleteEvaluationJob's own response/refusal, not the two causes below.

=== Cause 1 of 2: CreateProvisionedModelThroughput misclassifies a model that is not eligible for Provisioned Throughput as a model-unit quota shortage ===
Failing check: bedrock.update-provisioned-throughput-concurrent-race-one-wins

AWS: CreateProvisionedModelThroughput (Smithy: POST /provisioned-model-throughput, success 201; modeled errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 400, ThrottlingException 429, TooManyTagsException 400, ValidationException 400). The model documents nothing about which base models may back a Provisioned Throughput — recorded behavior only: a commitment-free purchase (no `commitmentDuration`) over a model that is not offered for Provisioned Throughput is refused with HTTP 400 `ValidationException` and the FIXED sentence `The provided model is not supported for provisioned throughput.` (no variable part at all — the message quotes neither the model id nor the region). The recording covers three unrelated model families — `amazon.titan-text-express-v1`, `amazon.titan-text-premier-v1:0` and `meta.llama3-70b-instruct-v1:0` — and AWS answers that identical refusal for each, i.e. the model-ineligibility verdict is reached BEFORE any model-unit quota is consulted.

Emulator: for the first two model families it does answer that ValidationException, but for `meta.llama3-70b-instruct-v1:0` it answers HTTP 400 `ServiceQuotaExceededException` with `Your account does not currently have any no commitment model units reserved for <modelId>. Please see <support-case URL for the request's region> to request a service quota increase.` — a quota message whose fixed text is the two sentences and whose variable parts are the model id taken from the request and the region taken from the request. The replayed assertion reads: `expected ValidationException creating a PT on 'meta.llama3-70b-instruct-v1:0', got 'ServiceQuotaExceededException'`. Hypothesis (from the handler's own prose): the emulator's catalogue marks this model family as supporting PROVISIONED inference, so `supports_provisioned_throughput` passes and the request falls through to the no-commitment grant gate, which is granted at zero.

Why: a client gets the wrong error CLASS and the wrong remedy for a non-retryable condition. `ServiceQuotaExceededException` tells the caller to open a service-limit-increase case and that the purchase would succeed once units are granted; AWS's `ValidationException` tells the caller the model can never back a Provisioned Throughput and the request must name a different model. SDK retry/classification layers and any code branching on the error code take the wrong path.

Fix: the model-eligibility decision this operation makes — the foundation-model catalogue's Provisioned-Throughput offering for the Llama 3 family and the ordering of the eligibility gate ahead of the granted-model-unit gate — in the bedrock control-plane state consumed by this operation's handler. Every model family the corpus records the fixed not-supported sentence for must reach that refusal, whatever the commitment reads.

Traces: bedrock.update-provisioned-throughput-concurrent-race-one-wins (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/ops/create_provisioned_model_throughput.rs lines 306-340; services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/state/limits.rs lines 659-740).

=== Cause 2 of 2: an accepted automated evaluation job never presents a terminal status to the read that follows its creation ===
Failing check: bedrock.e2e-evaluation-job-runs-and-is-purged

AWS: CreateEvaluationJob is accepted 202 with a job ARN and GetEvaluationJob (GET /evaluation-jobs/{jobIdentifier}, 200) reports `status` out of `EvaluationJobStatus` [InProgress, Completed, Failed, Stopping, Stopped, Deleting]; a successful automated job ends `Completed` with its result objects under `outputDataConfig.s3Uri`, after which BatchDeleteEvaluationJob purges it so GetEvaluationJob answers ResourceNotFoundException and ListEvaluationJobs stops returning it. Recorded behavior: the recorded run reaches that `Completed` read, finds output objects under the job's prefix, and completes the purge — every one of those reads returns promptly.

Emulator: the check never gets past the first lifecycle read — it is killed with `Check timed out after 60.0s`, so no terminal status is ever observed. The job's completion is owed 3 seconds after the create and is only materialised by a later call that touches the record, and that same first read is what performs the whole configured run inline (the dataset prompts, the inference over each and the result objects). So either the only read inside the budget lands inside the InProgress window, or that read performs the run inline and does not return in time; in neither case does a caller observe the Completed status, the S3 output objects or the purge that AWS's recording shows. The later assertions of this check (output objects present, batch delete reporting no errors, GetEvaluationJob turning into ResourceNotFoundException, ListEvaluationJobs dropping the job) were never reached and so are unverified here.

Why: a client driving the documented automated-evaluation lifecycle — create, poll to a terminal status, read results from S3, purge — hangs or stalls on the first poll and can never reach its results; any workflow gated on `status == Completed` does not proceed.

Fix: the evaluation-job lifecycle and its run in the bedrock control-plane state — the span after which an accepted job's non-terminal status is owed its terminal one, and the inline performance of the run on the first read that touches the record (make the read answer the stored record promptly rather than carrying the whole run). The status projection belongs to this operation's read handler; the run belongs in the evaluation-run state module.

Traces: bedrock.e2e-evaluation-job-runs-and-is-purged (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/ops/create_evaluation_job.rs lines 347-398; services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/state/limits.rs lines 466-476; services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/state/evaluation_run.rs).

## Evidence
Cause 1 — bedrock.update-provisioned-throughput-concurrent-race-one-wins:
  Request: CreateProvisionedModelThroughput{modelUnits=1, provisionedModelName=<fresh name>, modelId="arn:aws:bedrock:<region>::foundation-model/meta.llama3-70b-instruct-v1:0"} (no commitmentDuration).
  Expected (AWS, recorded for all three probed families): HTTP 400 ValidationException, message exactly "The provided model is not supported for provisioned throughput." — wholly fixed text, no values from the request.
  Actual (emulator): HTTP 400 ServiceQuotaExceededException, message "Your account does not currently have any no commitment model units reserved for meta.llama3-70b-instruct-v1:0. Please see https://support.console.aws.amazon.com/support/home?region=us-west-2#/case/create?issueType=service-limit-increase to request a service quota increase." (fixed text = the two sentences and the case-form URL template; variable parts = the model id from the request and the region the request addressed — those values are from this one run, not part of the requirement).
  Assertion: "expected ValidationException creating a PT on 'meta.llama3-70b-instruct-v1:0', got 'ServiceQuotaExceededException'". The same request over amazon.titan-text-express-v1 and amazon.titan-text-premier-v1:0 was refused correctly, so the divergence is per-model-family classification, not the whole gate.

Cause 2 — bedrock.e2e-evaluation-job-runs-and-is-purged:
  Flow: CreateBucket + PutBucketTagging, CreateRole/PutRolePolicy, CreateEvaluationJob(automated, Builtin.BoolQ / Builtin.Accuracy, outputDataConfig.s3Uri=s3://<bucket>/eval-output/), then GetEvaluationJob polled for status in (Completed, Failed, Stopped).
  Expected (AWS, recorded): the job reaches status "Completed", ListObjectsV2 under the output prefix returns KeyCount > 0, BatchDeleteEvaluationJob returns no errors, GetEvaluationJob then answers ResourceNotFoundException and ListEvaluationJobs no longer lists the jobArn.
  Actual (emulator): "Error: Check timed out after 60.0s" — no terminal status was ever observed, so none of the subsequent reads happened.
  Emulator source of the stall: the record is stored status "InProgress" with completion owed 3s later and materialised only by a later call touching the record, and the run itself (datasets, inference, result objects) is performed by the first call that reads the job.

## Steps to Reproduce
Cause 1:
  1. Call ListProvisionedModelThroughputs to confirm reachability.
  2. Call CreateProvisionedModelThroughput with modelUnits=1, a fresh provisionedModelName, modelId="arn:aws:bedrock:<region>::foundation-model/meta.llama3-70b-instruct-v1:0", and NO commitmentDuration.
  3. Observe the error code/message: the emulator answers 400 ServiceQuotaExceededException (no-commitment model units) where AWS answers 400 ValidationException "The provided model is not supported for provisioned throughput.".
  4. Repeat with amazon.titan-text-express-v1 and amazon.titan-text-premier-v1:0 to see the correct refusal, confirming the divergence is per model family.
  Replay: behavioral check bedrock.update-provisioned-throughput-concurrent-race-one-wins.

Cause 2:
  1. Create an S3 bucket and an IAM role Bedrock can assume with read/write on that bucket.
  2. Call CreateEvaluationJob with evaluationConfig.automated (dataset Builtin.BoolQ, metric Builtin.Accuracy), inferenceConfig.models=[amazon.nova-lite-v1:0], outputDataConfig.s3Uri=s3://<bucket>/eval-output/.
  3. Immediately call GetEvaluationJob(jobIdentifier=<jobArn>) and poll for status in (Completed, Failed, Stopped) within the suite's check budget.
  4. Observe that no terminal status is reached inside the budget (the check is killed at 60s), so the S3 output read, BatchDeleteEvaluationJob purge, the following ResourceNotFoundException and the ListEvaluationJobs absence are never observable.
  Replay: behavioral check bedrock.e2e-evaluation-job-runs-and-is-purged.
