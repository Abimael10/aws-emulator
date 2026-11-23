Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative
Issue-Key: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative|_poll_for_objects#25|Timeout|time.sleep(interval_s)
Issue-Commit: ba6912b745241f4a8c68b84730217fa4109e44cb

# s3__sagemaker: endpoint data capture still never lands — the capture writer publishes no object under DataCaptureConfig.DestinationS3Uri, so the capture-delivery IAM boundary is unobservable

> _Note: this issue was identified against the code base at commit `ba6912b745241f4a8c68b84730217fa4109e44cb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeEndpoint

## Description
One net-new cause. Cause 1 of 1: an endpoint created from an EndpointConfig carrying DataCaptureConfig (EnableCapture=true, InitialSamplingPercentage=100, CaptureOptions Input+Output) serves inference but never publishes any capture object beneath DataCaptureConfig.DestinationS3Uri, and the capture write is not performed under the endpoint's ExecutionRoleArn, so there is no delivery/authorization boundary to observe either.

NOTE — same cause as the resolved issue 20261116T013127Z-s3-sagemaker-endpoint-data-capture-never-lands-so.md, which carries this check's replay key (Issue-Key: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative|_poll_for_objects#25|Timeout). That issue is marked fixed, yet the check still fails identically on this run, so the capture-delivery side effect is still absent. It is not a refusal (no error code is returned; inference answers 200), so this is filed as the still-present missing side effect rather than a refusal regression.

AWS: Smithy (com.amazonaws.sagemaker) DataCaptureConfig — doc "Configuration to control how SageMaker AI captures inference data"; members EnableCapture (boolean, "Whether data capture should be enabled or disabled (defaults to enabled)"), InitialSamplingPercentage -> SamplingPercentage required @range 0..100 ("The percentage of requests SageMaker AI will capture"), DestinationS3Uri required @length 0..512 @pattern ^(https|s3)://([^/])/?(.*)$ ("The Amazon S3 location used to capture the data"), CaptureOptions -> CaptureOptionList required @length 1..32 with CaptureOption.CaptureMode a required enum (Input | Output | InputAndOutput, "Specify the boundary of data to capture"), plus optional KmsKeyId and CaptureContentTypeHeader. With EnableCapture=true and a 100% sampling percentage, AWS writes a capture file beneath DestinationS3Uri, partitioned under the endpoint name / variant name / date-time, each record pairing the invocation's endpointInput with the matching endpointOutput for the configured CaptureOptions. For the delivery leg itself the model states nothing — recorded behavior only: the recording shows that the capture write is attempted by the endpoint under its ExecutionRoleArn, so an endpoint whose role lacks s3:PutObject on the destination prefix still answers InvokeEndpoint with HTTP 200 (inference is unaffected) while no capture object ever appears under that prefix, whereas a sibling endpoint whose role does hold s3:PutObject on the same prefix does get capture objects for the same payloads. Capture permissions never affect hosting: both endpoints reach EndpointStatus InService.

Emulator: the check never reaches its capture assertions — replay detail "Error: Check timed out after 60.0s" (60 s is the harness budget, not a requirement). The only unbounded wait after both endpoints are terminal is the positive-control poll, ListObjectsV2 under <capture-prefix>/withwrite/ with a 240 s budget and 15 s interval, which returns early only once at least one key exists. Hypothesis: no capture object is ever written beneath DestinationS3Uri for the granted (withwrite) endpoint either, so that poll spins until the harness kills the check. Because the positive control never converges, the negative half — delivery suppressed when, and only when, the execution role lacks s3:PutObject on the destination prefix — is never exercised at all.

Why: a Model Monitor, audit or drift consumer configures DataCaptureConfig and then reads the capture files to join the request and response legs of each invocation; against this emulator it reads an empty prefix forever and cannot distinguish "capture disabled", "capture configured but silently dropped" and "capture blocked by a missing s3:PutObject grant". Any client or test that asserts the capture-delivery permission boundary by comparing a granted endpoint against a denied one sees both behave identically (no objects), so a real IAM misconfiguration on the capture destination is indistinguishable from a correct deployment and a capture-delivery regression is invisible.

Fix: the endpoint data-plane capture path — DataCaptureConfig handling in the CreateEndpointConfig/CreateEndpoint handlers and the state that records the endpoint's DataCaptureConfig and ExecutionRoleArn, plus the capture writer invoked from the sagemaker-runtime InvokeEndpoint handler. It must write a capture object beneath DestinationS3Uri keyed under the endpoint name for sampled invocations (pairing endpointInput with endpointOutput per the configured CaptureOptions), and must perform that write as the endpoint's execution role so delivery is suppressed — with inference still answering 200 — when the role lacks s3:PutObject on the destination prefix.

Traces: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative

## Evidence
Behavioral suite: 2286/2681 passed (85.3%); 312 mismatched, 83 errored, 0 skipped, 0 unloadable. Every other failing check in the report is marked OWNED by an open issue or ALREADY FILED behind an unimplemented scaffold stub; this is the single net-new failure attributable to the emulator.

Check: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative [mismatch]
  Expected (AWS): both endpoints reach EndpointStatus InService (capture permissions do not affect hosting); three InvokeEndpoint calls against the nowrite endpoint each answer HTTP 200; ListObjectsV2 under <capture-prefix>/withwrite/ eventually returns >= 1 capture object for the positive control; ListObjectsV2 under <capture-prefix>/nowrite/ stays empty for the window calibrated from the positive control's measured delivery latency.
  Actual (emulator): "Error: Check timed out after 60.0s" — the check never reaches its capture assertions. The only unbounded wait past endpoint readiness is _poll_for_objects(s3, capture_bucket, f"{capture_prefix}/withwrite/", timeout_s=240), which returns only when a key appears; no capture object appears, so it never terminates inside the budget.

No status code, error code or error message diverges here: InvokeEndpoint is answered 200 as AWS does. The divergence is a missing side effect (no capture object beneath DestinationS3Uri) and the missing authorization gate on that side effect, observed as a non-converging poll.

Smithy grounding (com.amazonaws.sagemaker):
  DataCaptureConfig.EnableCapture (boolean) — "Whether data capture should be enabled or disabled (defaults to enabled)."
  DataCaptureConfig.InitialSamplingPercentage -> SamplingPercentage, required, @range 0..100.
  DataCaptureConfig.DestinationS3Uri -> required, @length 0..512, @pattern ^(https|s3)://([^/])/?(.*)$ — "The Amazon S3 location used to capture the data."
  DataCaptureConfig.CaptureOptions -> CaptureOptionList, required, @length 1..32; CaptureOption.CaptureMode required enum Input | Output | InputAndOutput.
  DataCaptureConfig.KmsKeyId / CaptureContentTypeHeader optional.

Prior art: resolved issue 20261116T013127Z-s3-sagemaker-endpoint-data-capture-never-lands-so.md describes this same cause and carries this check's replay key; open issue 20261112T232719Z-s3-sagemaker-5-net-new-causes-autopilot-bestcandidate.md describes non-delivery of capture for a DIFFERENT check (s3__sagemaker.sagemaker-endpoint-invocation-output-captured-to-s3) and does not carry this check's key, so it is named for cross-reference only and does not cover this cause.

## Steps to Reproduce
Prerequisite: git_clone(), then run_behavioral_suite(); the check below is replayed against the booted emulator.

1. s3:CreateBucket an artifact bucket and a capture bucket (with CreateBucketConfiguration.LocationConstraint outside us-east-1); PutObject an XGBoost model.tar.gz at model/xgboost-model.tar.gz in the artifact bucket.
2. iam:CreateRole two execution roles trusting sagemaker.amazonaws.com:
   - "nowrite": inline policy allows s3:GetObject on the artifact object and the logs:CreateLogGroup/CreateLogStream/PutLogEvents actions only — NO grant on the capture bucket.
   - "withwrite": the same, plus s3:PutObject on arn:aws:s3:::<capture-bucket>/<capture-prefix>/*.
3. For each role: sagemaker:CreateModel with PrimaryContainer {Image: the region's built-in XGBoost image, ModelDataUrl: s3://<artifact-bucket>/model/xgboost-model.tar.gz} and that ExecutionRoleArn; sagemaker:CreateEndpointConfig with one ProductionVariant (ml.m5.large, InitialInstanceCount 1) and DataCaptureConfig {EnableCapture: true, InitialSamplingPercentage: 100, DestinationS3Uri: s3://<capture-bucket>/<capture-prefix>/<label>, CaptureOptions: [{CaptureMode: Input}, {CaptureMode: Output}]}; sagemaker:CreateEndpoint.
4. Poll DescribeEndpoint on both endpoints until EndpointStatus is InService.
5. sagemaker-runtime:InvokeEndpoint three times against each endpoint with ContentType text/csv and body "0.5,0.5,0.5,0.5\n"; each answers HTTP 200.
6. ListObjectsV2 under <capture-prefix>/withwrite/ — observe that no capture object ever appears (the positive-control poll never converges and the check is killed at its budget), instead of >= 1 capture file pairing endpointInput with endpointOutput under the endpoint-name partitioning.
7. Because step 6 never converges, the absence window for <capture-prefix>/nowrite/ is never reached, so the permission boundary (delivery suppressed only when the execution role lacks s3:PutObject on the destination prefix, inference still 200) is never demonstrated.
