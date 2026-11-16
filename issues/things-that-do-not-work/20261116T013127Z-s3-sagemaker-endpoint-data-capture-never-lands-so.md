Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative
Issue-Key: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative|_poll_for_objects#25|Timeout|time.sleep(interval_s)
Issue-Commit: f22ccf27ef051903376e0d46dbb4d489e16f4aae

# s3__sagemaker: endpoint data capture never lands, so the IAM capture-delivery boundary is unmeasurable (positive control never produces a capture object)

> _Note: this issue was identified against the code base at commit `f22ccf27ef051903376e0d46dbb4d489e16f4aae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeEndpoint

## Description
One net-new root cause. Cause 1 of 1: an endpoint created from an EndpointConfig that carries DataCaptureConfig serves inference but never publishes a capture object beneath DataCaptureConfig.DestinationS3Uri, and capture delivery is not gated on the execution role's S3 permissions.

AWS: Smithy (com.amazonaws.sagemaker) DataCaptureConfig is documented as "Configuration to control how SageMaker AI captures inference data" with required members InitialSamplingPercentage (SamplingPercentage, @range 0..100), DestinationS3Uri (@length 0..512, @pattern ^(https|s3)://([^/])/?(.*)$) and CaptureOptions (CaptureOptionList, @length 1..32, each CaptureOption.CaptureMode a required enum of Input | Output | InputAndOutput); KmsKeyId and CaptureContentTypeHeader are optional. With EnableCapture=true and InitialSamplingPercentage=100, AWS writes a capture file beneath DestinationS3Uri keyed under the endpoint's name (endpoint/variant/date-time partitioning) for the sampled invocations, each record pairing the invocation's endpointInput with the matching endpointOutput. Capture delivery is performed by the endpoint using its ExecutionRoleArn: for the delivery leg itself the model says nothing (recorded behavior only), and the recording shows that an endpoint whose execution role lacks s3:PutObject on the capture destination still answers InvokeEndpoint with HTTP 200 — inference is unaffected — while no capture object ever appears under that prefix; a sibling endpoint whose role does hold s3:PutObject on the same prefix does get capture objects for the same payload.

Emulator: the check cannot get past the capture-delivery leg at all — it exhausts its entire budget (replay detail: "Error: Check timed out after 60.0s"; the 60 s figure is the harness budget, not a requirement). The only unbounded wait the check performs after the endpoints are terminal is the positive-control poll (ListObjectsV2 under <capture-prefix>/withwrite/, 240 s budget, 15 s interval), which only returns early once a key exists. Hypothesis: no capture object is ever written beneath DestinationS3Uri, so that poll spins until the harness kills the check — the same non-delivery that open issue 20261112T232719Z-s3-sagemaker-5-net-new-causes-autopilot-bestcandidate.md describes as its Cause 5 for a DIFFERENT check (s3__sagemaker.sagemaker-endpoint-invocation-output-captured-to-s3); that issue does not carry this check's replay key, so it is named here for cross-reference only and does not cover this cause. Because the positive control never converges, the negative half — that capture delivery is suppressed when, and only when, the execution role lacks s3:PutObject on the destination prefix — is not exercised at all, so the emulator has no observable capture-delivery authorization boundary.

Why: a Model Monitor, audit or drift consumer configures DataCaptureConfig and then reads the capture files to join the request and response legs of each invocation; against this emulator it gets an empty prefix forever and cannot distinguish "capture disabled", "capture configured but silently dropped" and "capture blocked by a missing s3:PutObject grant". Any client or test that asserts a permission boundary by comparing a granted endpoint against a denied one sees both endpoints behave identically (no objects), so a real IAM misconfiguration on the capture destination is indistinguishable from a correct deployment, and a capture-delivery regression is invisible.

Fix: the endpoint data-plane capture path — DataCaptureConfig handling on CreateEndpointConfig/CreateEndpoint, and the capture writer invoked on the sagemaker-runtime InvokeEndpoint path together with the state that records the endpoint's DataCaptureConfig and ExecutionRoleArn. It must write a capture object beneath DestinationS3Uri keyed under the endpoint name for sampled invocations (pairing endpointInput with endpointOutput per the configured CaptureOptions), and must perform that write under the endpoint's execution role so delivery is suppressed — with inference still answering 200 — when the role lacks s3:PutObject on the destination prefix.

Traces: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative

## Evidence
Behavioral suite: 16/28 passed (57.1%); 6 mismatched, 6 errored, 0 skipped. All other failing checks under the `### s3__sagemaker` and `### sagemaker` headings are marked OWNED by an already-open issue; this is the one net-new failure attributable to the emulator.

Check: s3__sagemaker.sagemaker-endpoint-data-capture-missing-s3-write-permission-negative [mismatch]
  Expected (AWS): both endpoints reach EndpointStatus InService (capture permissions do not affect hosting); three InvokeEndpoint calls against the nowrite endpoint each answer HTTP 200; ListObjectsV2 under <capture-prefix>/withwrite/ eventually returns >=1 capture object for the positive control; ListObjectsV2 under <capture-prefix>/nowrite/ stays empty for the calibrated absence window.
  Actual (emulator): "Error: Check timed out after 60.0s" — the check never reaches its capture assertions. The only unbounded wait past endpoint readiness is the positive-control capture poll `_poll_for_objects(s3, capture_bucket, f"{capture_prefix}/withwrite/", timeout_s=240)`, which returns only when a key appears; no capture object appears, so it never terminates inside the budget.

Smithy grounding (com.amazonaws.sagemaker):
  DataCaptureConfig.EnableCapture (boolean, "Whether data capture should be enabled or disabled (defaults to enabled)").
  DataCaptureConfig.InitialSamplingPercentage -> SamplingPercentage, required, @range 0..100.
  DataCaptureConfig.DestinationS3Uri -> required, @length 0..512, @pattern ^(https|s3)://([^/])/?(.*)$, "The Amazon S3 location used to capture the data".
  DataCaptureConfig.CaptureOptions -> CaptureOptionList, required, @length 1..32; CaptureOption.CaptureMode required enum Input | Output | InputAndOutput, "Specifies data Model Monitor will capture".
  Note: no status/error code diverges here — the divergence is a missing side effect (no capture object) and a missing authorization gate on that side effect, observed as a non-converging poll.

## Steps to Reproduce
Prerequisite: git_clone(), then run_behavioral_suite(); the check below is replayed against the booted emulator.

1. s3:CreateBucket an artifact bucket and a capture bucket (with CreateBucketConfiguration.LocationConstraint outside us-east-1); PutObject an XGBoost model.tar.gz at model/xgboost-model.tar.gz in the artifact bucket.
2. iam:CreateRole two execution roles trusting sagemaker.amazonaws.com:
   - "nowrite": inline policy allows s3:GetObject on the artifact object and the logs:* actions only — NO grant on the capture bucket.
   - "withwrite": the same, plus s3:PutObject on arn:aws:s3:::<capture-bucket>/<capture-prefix>/*.
3. For each role: sagemaker:CreateModel with PrimaryContainer {Image: the region's built-in XGBoost image, ModelDataUrl: s3://<artifact-bucket>/model/xgboost-model.tar.gz} and that ExecutionRoleArn; sagemaker:CreateEndpointConfig with one ProductionVariant (ml.m5.large, InitialInstanceCount 1) and DataCaptureConfig {EnableCapture: true, InitialSamplingPercentage: 100, DestinationS3Uri: s3://<capture-bucket>/<capture-prefix>/<label>, CaptureOptions: [{CaptureMode: Input}, {CaptureMode: Output}]}; sagemaker:CreateEndpoint.
4. Poll DescribeEndpoint on both until InService.
5. sagemaker-runtime:InvokeEndpoint three times against each endpoint with ContentType text/csv and body "0.5,0.5,0.5,0.5\n"; each answers 200.
6. ListObjectsV2 under <capture-prefix>/withwrite/ — observe that no capture object ever appears (the positive-control poll never converges and the check is killed at its budget), instead of >=1 capture file pairing endpointInput with endpointOutput.
7. Consequently step 6's absence window for <capture-prefix>/nowrite/ is never reached, so the permission boundary (delivery suppressed only when the role lacks s3:PutObject) is never demonstrated.
