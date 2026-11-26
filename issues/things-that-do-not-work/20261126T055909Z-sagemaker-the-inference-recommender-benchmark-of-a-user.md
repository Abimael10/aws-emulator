Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemaker.get-scaling-configuration-recommendation-references-endpoint
Issue-Key: sagemaker.get-scaling-configuration-recommendation-references-endpoint|_poll_until#30|Timeout|time.sleep(interval_s)
Issue-Commit: b96b426a4fa9e6c671ac79c65a6c458f5a323b74

# sagemaker: the Inference Recommender benchmark of a user-owned endpoint never settles, so DescribeInferenceRecommendationsJob reports no terminal Status and GetScalingConfigurationRecommendation is unreachable

> _Note: this issue was identified against the code base at commit `b96b426a4fa9e6c671ac79c65a6c458f5a323b74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInferenceRecommendationsJob

## Description
One net-new cause under the `### sagemaker` suite heading.

Cause 1 of 1: a Default Inference Recommender job created over an existing, InService endpoint (InputConfig.Endpoints naming it, ContainerConfig naming the XGBOOST/CLASSIFICATION zoo row) never reaches a terminal Status inside a client-scale poll window, so neither EndpointPerformances nor GetScalingConfigurationRecommendation is ever observable. The check ends on the harness's 'Check timed out after 60.0s' instead of on any wire answer.

AWS: Smithy (com.amazonaws.sagemaker) DescribeInferenceRecommendationsJob — input JobName required (@length 1..64, @pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,63}$); output JobName, JobType, JobArn, RoleArn, Status (required enum RecommendationJobStatus), CreationTime, LastModifiedTime, InputConfig all required, plus optional CompletionTime, FailureReason, InferenceRecommendations (@length 1..10) and EndpointPerformances (@length 0..1), each entry carrying EndpointInfo.EndpointName. A job AWS accepts walks that Status to a terminal member (COMPLETED, or FAILED with FailureReason naming what stopped it), and a COMPLETED job that benchmarked a user-supplied endpoint lists that endpoint under EndpointPerformances[].EndpointInfo.EndpointName. GetScalingConfigurationRecommendation (input InferenceRecommendationsJobName required, optional EndpointName @length 0..63; errors ResourceNotFound, HTTP 400) then echoes InferenceRecommendationsJobName and EndpointName and returns a DynamicScalingConfiguration whose capacity bounds are ordered (MinCapacity >= 1, MaxCapacity >= MinCapacity). The wall-clock pace of the benchmark is recorded behavior only; the requirement is that the Status reaches a terminal member so a poller terminates.

Emulator: the check never reaches its first post-benchmark assertion. Replay detail: 'Error: Check timed out after 60.0s' (60 s is the harness budget, not a requirement). Its only unbounded waits are DescribeEndpoint to InService and then DescribeInferenceRecommendationsJob to {COMPLETED, FAILED, STOPPED} with a 2400 s budget and a 30 s interval. The emulator's own handler documentation describes where the time goes: CreateInferenceRecommendationsJob accepts the job in PENDING and 'the move out of it is owed by the clock (crate::state::InferenceRecommendationsJob::settle_status)', while the load test itself 'is this emulator's own (crate::state::run_inference_recommendations_job) ... It is performed HERE, before this handler answers' — it loads the model behind each endpoint under test out of the emulator's own S3 and invokes it BENCHMARK_INVOCATIONS times per endpoint (services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_inference_recommendations_job.rs, module docs lines 3-35). Hypothesis: either that synchronous in-handler benchmark does not return inside a client's budget, or the accepted job never leaves PENDING, so the poll spins; no per-call status, error code or message diverges — the job simply never settles and no FailureReason is published either.

Why: a client that uses Inference Recommender the documented way — create the job, poll DescribeInferenceRecommendationsJob until Status is terminal, read EndpointPerformances, then ask GetScalingConfigurationRecommendation for the benchmarked endpoint's scaling bounds — blocks forever on the poll. Nothing tells it the job is stuck rather than slow (no terminal Status, no FailureReason), and the whole recommendation surface (InferenceRecommendations, EndpointPerformances, DynamicScalingConfiguration) is unreachable, so capacity-planning automation built on it cannot run at all.

Fix: the Inference Recommender job lifecycle and the run behind it — InferenceRecommendationsJob::settle_status and run_inference_recommendations_job in the SageMaker state, reached from services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_inference_recommendations_job.rs and materialised by the DescribeInferenceRecommendationsJob handler. The transient PENDING/IN_PROGRESS statuses must stay observable in order while the job reaches a terminal Status inside a window a poll loop can traverse, a benchmark that could not run must settle FAILED with a FailureReason, and a COMPLETED job must report the user-supplied endpoint under EndpointPerformances so GetScalingConfigurationRecommendation can answer for it.

Cross-reference, covering nothing here: open issue 20261123T120651Z-sagemaker-the-end-to-end-model-endpoint-flows.md describes non-converging SageMaker hosting flows but carries only the replay keys of sagemaker.deploy-model-endpoint-cfn-serves-inference and sagemaker.model-endpoint-config-update-flow, not this check's, and it does not mention the Inference Recommender job at all.

Traces: sagemaker.get-scaling-configuration-recommendation-references-endpoint

## Evidence
Behavioral suite this run: 21/28 passed (75.0%); 6 mismatched, 1 errored. Per-service: sagemaker 7/11 (4 failed). Of those four, sagemaker.deploy-model-endpoint-cfn-serves-inference and sagemaker.e2e-model-package-approval-to-endpoint-deploy-flow are marked OWNED and skipped, and sagemaker.deploy-endpoint-cfn-invoke-succeeds errored inside the runner's own environment (ImportError: cannot import name 'image_uris' from 'sagemaker' — a missing runner tool, not an emulator answer), leaving this one net-new failure.

[mismatch] sagemaker.get-scaling-configuration-recommendation-references-endpoint
  Expected (AWS): after CreateInferenceRecommendationsJob (JobType 'Default', InputConfig {ModelName, Endpoints:[{EndpointName: <the live endpoint>}], ContainerConfig {Domain MACHINE_LEARNING, Task CLASSIFICATION, Framework XGBOOST, NearestModelName xgboost, PayloadConfig.SamplePayloadUrl, SupportedInstanceTypes [ml.m5.large], SupportedEndpointType RealTime}}), DescribeInferenceRecommendationsJob reaches Status 'COMPLETED'; EndpointPerformances contains an entry whose EndpointInfo.EndpointName is that endpoint; GetScalingConfigurationRecommendation(InferenceRecommendationsJobName=<job>, EndpointName=<endpoint>) echoes both and returns DynamicScalingConfiguration with MinCapacity >= 1 and MaxCapacity >= MinCapacity.
  Actual (emulator): 'Error: Check timed out after 60.0s' — no terminal Status is observed, so EndpointPerformances is never read and GetScalingConfigurationRecommendation is never called. The check's waits are _poll_until(describe_endpoint, {'InService','Failed'}, timeout_s=900, interval_s=20) and _poll_until(describe_inference_recommendations_job, {'COMPLETED','FAILED','STOPPED'}, timeout_s=2400, interval_s=30).
  No status code, error code or error message diverges here: no terminal answer is reached at all, and no FailureReason is published for the stalled job.

Emulator source consulted for the Fix pointer (file/line citations only):
  src/ops/create_inference_recommendations_job.rs lines 3-35 — the job is accepted in PENDING with the move out of it owed to the clock (state::InferenceRecommendationsJob::settle_status), and the load test (state::run_inference_recommendations_job) is performed inside the handler before it answers, invoking the model behind each endpoint under test BENCHMARK_INVOCATIONS times.
  src/state.rs line 255 — ENDPOINT_IN_SERVICE_AFTER = Duration::from_millis(300): the endpoint's own clock-owed span is compressed, so the endpoint leg is not what consumes the budget.

Smithy grounding (com.amazonaws.sagemaker):
  DescribeInferenceRecommendationsJob output: JobName, JobType, JobArn, RoleArn, Status (required, enum RecommendationJobStatus), CreationTime, LastModifiedTime, InputConfig required; CompletionTime, FailureReason (@length 0..1024), InferenceRecommendations (@length 1..10), EndpointPerformances (@length 0..1) optional. Error ResourceNotFound, HTTP 400.
  GetScalingConfigurationRecommendation: input InferenceRecommendationsJobName required (@length 1..64), EndpointName (@length 0..63), TargetCpuUtilizationPerCore @range 1..100; output InferenceRecommendationsJobName, EndpointName, Metric, DynamicScalingConfiguration. Error ResourceNotFound, HTTP 400.

## Steps to Reproduce
Prerequisite: git_clone(), then run_behavioral_suite(); the check below is replayed against the booted emulator.

1. s3:CreateBucket (CreateBucketConfiguration.LocationConstraint outside us-east-1); iam:CreateRole an execution role trusting sagemaker.amazonaws.com with an inline policy granting s3:GetObject/PutObject on that bucket.
2. PutObject a small CSV (label,f1,f2 rows) at train/train.csv; sagemaker:CreateTrainingJob with the region's built-in XGBoost image, that role, one 'train' channel over the CSV, OutputDataConfig.S3OutputPath s3://<bucket>/output/, ResourceConfig ml.m5.large x1, HyperParameters {objective: binary:logistic, num_round: 5}; poll DescribeTrainingJob to Completed and take ModelArtifacts.S3ModelArtifacts.
3. CreateModel over that artifact, CreateEndpointConfig with one ProductionVariant (ml.m5.large, InitialInstanceCount 1), CreateEndpoint; poll DescribeEndpoint to InService and confirm InvokeEndpoint answers a non-empty body for '1.0,2.0' (text/csv).
4. PutObject a tar.gz sample-payload archive at payload/sample.tar.gz.
5. sagemaker:CreateInferenceRecommendationsJob with JobType 'Default', that RoleArn, and InputConfig {ModelName: <model>, Endpoints: [{EndpointName: <endpoint>}], ContainerConfig {Domain: MACHINE_LEARNING, Task: CLASSIFICATION, Framework: XGBOOST, FrameworkVersion: 1.5-1, NearestModelName: xgboost, PayloadConfig {SamplePayloadUrl: s3://<bucket>/payload/sample.tar.gz, SupportedContentTypes: ['text/csv']}, SupportedResponseMIMETypes: ['text/csv'], SupportedInstanceTypes: ['ml.m5.large'], SupportedEndpointType: RealTime}} (no JobDurationInSeconds — Advanced-only).
6. Poll DescribeInferenceRecommendationsJob every 30 s — observe that Status never reaches COMPLETED, FAILED or STOPPED inside a client-scale window and no FailureReason is published, so the replayed check is killed at its budget.
7. Because step 6 never converges, EndpointPerformances[].EndpointInfo.EndpointName is never read and GetScalingConfigurationRecommendation(InferenceRecommendationsJobName=<job>, EndpointName=<endpoint>) is never called, so the echoed job/endpoint identity and the ordered DynamicScalingConfiguration bounds are never observable.
