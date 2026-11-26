Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemaker.deploy-model-endpoint-cfn-serves-inference sagemaker.model-endpoint-config-update-flow
Issue-Key: sagemaker.deploy-model-endpoint-cfn-serves-inference|_wait_for_stack_terminal#20|Timeout|resp = cfn.describe_stacks(StackName=stack_name)
Issue-Key: sagemaker.model-endpoint-config-update-flow|_poll_until_terminal#24|Timeout|time.sleep(interval_s)
Issue-Commit: c70e3ddac00fd37a33d2fb8485b3b6679c96266c

# sagemaker: the end-to-end model→endpoint flows reach no verdict — training job and endpoint lifecycles never settle inside the replay budget

> _Note: this issue was identified against the code base at commit `c70e3ddac00fd37a33d2fb8485b3b6679c96266c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTrainingJob

## Description
Cause 1 of 1: every end-to-end SageMaker hosting flow that must walk a training job to a terminal status and then an endpoint to InService produces no observable outcome inside the replay budget, so the checks end on the harness's 'Check timed out after 60.0s' instead of on any wire answer. Two checks fail on this one cause.

AWS: DescribeTrainingJob reports TrainingJobStatus walking InProgress → Completed with SecondaryStatus phases and ModelArtifacts.S3ModelArtifacts naming the written archive; DescribeEndpoint reports EndpointStatus Creating → InService, UpdateEndpoint to a second EndpointConfig passes through Updating back to InService with EndpointConfigName/ProductionVariants reporting the new config, and after DeleteEndpoint, DescribeEndpoint is refused ValidationException with the message 'Could not find endpoint "<name>".' — the fixed text 'Could not find endpoint "' + the endpoint name from the request + '".'. (Smithy: DescribeTrainingJob output TrainingJobStatus/SecondaryStatus required enums; DescribeEndpoint output EndpointStatus required enum, EndpointConfigName, ProductionVariants; UpdateEndpoint input EndpointName/EndpointConfigName. The wall-clock pace of the walk, and the not-found message text, are recorded behavior only.)

Emulator: neither lifecycle becomes terminal within a client-scale poll window, so the flows never get as far as their assertions. The emulator's own handler documentation describes the mechanism: CreateTrainingJob 'stores it as InProgress together with the instant the clock owes its terminal status', with the SecondaryStatus stages 'materialised by the clock' (services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_training_job.rs, module docs lines 10-52), and CreateEndpoint 'stores the endpoint in Creating, with the instant the clock owes it its outcome — InService, or Failed' (services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_endpoint.rs, module docs lines 3-10). Hypothesis: those clock-owed instants are scheduled on a real-AWS wall-clock scale (a training run plus ~10-15 min to InService), so a chain of them cannot be traversed in one replay; the per-call responses themselves are never shown to be wrong, the flow simply never arrives.

Why: a client that deploys a model the documented way — train, create model, create endpoint config, create endpoint, wait for InService, invoke, update to a second config, delete — can never complete that sequence against the emulator. Every behavior downstream of a settled job or a live endpoint (the inference answer, the post-update EndpointConfigName/variant report, the post-delete ValidationException) is unreachable, so the whole hosting surface is unverifiable end to end.

Fix: the lifecycle settle schedule in the SageMaker state — the clock-owed instants behind TrainingJob::settle_status / publish_when_due and the endpoint Creating→InService / Updating→InService transitions in services/service-com-amazonaws-sagemaker-sagemaker/src/state.rs — so the intermediate statuses stay observable in order while the full lifecycle settles in a window an API client's poll loop can traverse.

Traces: sagemaker.deploy-model-endpoint-cfn-serves-inference, sagemaker.model-endpoint-config-update-flow.

## Evidence
Replay output, service heading `### sagemaker`:
[mismatch] sagemaker.deploy-model-endpoint-cfn-serves-inference — 'Error: Check timed out after 60.0s'.
[mismatch] sagemaker.model-endpoint-config-update-flow — 'Error: Check timed out after 60.0s'.

Both bodies (as replayed) begin by training a model and polling it to a terminal status before anything is asserted: deploy-model-endpoint-cfn-serves-inference calls `_train_xgboost_model` → `_wait_for_training_job_terminal(sm, job_name, timeout_s=900.0, interval_s=15.0)` and asserts TrainingJobStatus == 'Completed', then creates the Model+EndpointConfig+Endpoint stack, waits for CREATE_COMPLETE and asserts `DescribeEndpoint.EndpointStatus == 'InService'` before invoking; model-endpoint-config-update-flow calls `_train_tiny_xgboost_model` → `_poll_until_terminal(describe_training_job, status_key='TrainingJobStatus', terminal={'Completed','Failed','Stopped'}, timeout_s=1500, interval_s=15)` and raises 'prerequisite training job did not complete' otherwise, then creates two endpoint configs, waits for InService, UpdateEndpoints and finally expects DescribeEndpoint to be refused ValidationException 'Could not find endpoint "<name>".'. Expected: each poll converges and the asserted statuses/refusal are observed. Actual: the budget is exhausted with no verdict — no terminal TrainingJobStatus and no InService EndpointStatus is reached, so neither the inference answer nor the update/delete contract is ever evaluated.

## Steps to Reproduce
1. Replay the behavioral suite and run sagemaker.deploy-model-endpoint-cfn-serves-inference and sagemaker.model-endpoint-config-update-flow.
2. Or by hand: create a bucket and a role trusting sagemaker.amazonaws.com, upload a tiny CSV, call CreateTrainingJob and poll DescribeTrainingJob — record how long TrainingJobStatus stays non-terminal. Then CreateModel + CreateEndpointConfig + CreateEndpoint and poll DescribeEndpoint — record how long EndpointStatus stays Creating. Observe that the chain does not settle inside a client-scale poll window, so InvokeEndpoint, UpdateEndpoint to a second config and the post-DeleteEndpoint ValidationException are never reached.
