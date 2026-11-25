Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-batch-transform-missing-role-permission-negative
Issue-Key: s3__sagemaker.sagemaker-batch-transform-missing-role-permission-negative|_poll_until#22|Timeout|time.sleep(interval_s)
Issue-Commit: c70e3ddac00fd37a33d2fb8485b3b6679c96266c

# s3__sagemaker: the batch-transform missing-PutObject boundary reaches no verdict — the end-to-end train→model→transform flow never settles inside the replay budget

> _Note: this issue was identified against the code base at commit `c70e3ddac00fd37a33d2fb8485b3b6679c96266c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTrainingJob

## Description
Cause 1 of 1: a SageMaker end-to-end flow built on job lifecycles (CreateTrainingJob → CreateModel → CreateTransformJob) produces no observable terminal state inside the replay budget, so the permission-boundary check never reaches a verdict at all. The run ends with the harness's own 'Check timed out after 60.0s' rather than any response from the emulator being wrong on the wire.

AWS: an accepted CreateTrainingJob is reported by DescribeTrainingJob as TrainingJobStatus walking InProgress → Completed, with SecondaryStatus passing through its phases and ModelArtifacts.S3ModelArtifacts naming the written archive; an accepted CreateTransformJob likewise reaches TransformJobStatus Completed (or Failed with a FailureReason naming the missing s3:PutObject grant on the output destination) and writes/does not write the output object accordingly. (Smithy: DescribeTrainingJob output TrainingJobStatus/SecondaryStatus/ModelArtifacts are required members of the enum-typed status pair; CreateTransformJob errors ResourceInUse/ResourceLimitExceeded/ResourceNotFound. The wall-clock pace of the walk is recorded behavior only.)

Emulator: the flow makes no progress to a terminal status that a polling client can observe: the check polls DescribeTrainingJob for a terminal TrainingJobStatus and is still non-terminal when the budget is spent. The emulator's own handler documentation states the design that produces this: CreateTrainingJob 'stores it as InProgress together with the instant the clock owes its terminal status' and the SecondaryStatus phases are 'materialised by the clock' so 'a caller that describes the job straight after creating it still observes InProgress' (services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_training_job.rs, module docs lines 10-52). Hypothesis: the clock-owed settle instants are scheduled on a real-AWS wall-clock scale, so a multi-stage flow (train, then model, then transform, then the S3 listing of the declared output prefix) cannot be walked to its end — the earlier failure recorded for this very check was an assertion on TrainingJobStatus != 'Completed', i.e. a terminal status was once reachable; now nothing terminal is seen at all.

Why: the negative-delivery contract this check exists to verify (an execution role that may read the model artifact and input but may not write the output must leave no output object, while the fully-permissioned control must write one) is unobservable. A client that creates a transform job and polls to a terminal status never gets one, so no IAM/S3 permission verdict — positive or negative — can be reached through the emulator.

Fix: the training-job and transform-job lifecycle schedule in the SageMaker state (the clock-owed settle instants behind TrainingJob::settle_status / publish_when_due in services/service-com-amazonaws-sagemaker-sagemaker/src/state.rs and the equivalent transform-job path), so that the stages remain observable in order yet the whole lifecycle settles in a window an API client's poll loop can traverse, as the suite's other settled lifecycles do.

Related but NOT covering this: open issue 20261029T211510Z-s3-sagemaker-sagemaker-jobs-never-execute-and-are-never-validated.md names this check in its trace ids, but its recorded replay key for it is an AssertionError on final_train['TrainingJobStatus'] == 'Completed' — a different, now-absent failure (the job then settled Failed immediately; it now does not settle at all), so the suite does not treat this failure as owned.

Traces: s3__sagemaker.sagemaker-batch-transform-missing-role-permission-negative.

## Evidence
Replay output, service heading `### s3__sagemaker`:
[mismatch] s3__sagemaker.sagemaker-batch-transform-missing-role-permission-negative — 'Error: Check timed out after 60.0s'.

The check's body (as replayed) does, in order: put the training CSV to S3, CreateTrainingJob with a fully-permissioned role, then `_wait_training_job` polling DescribeTrainingJob for TrainingJobStatus in {Completed, Failed, Stopped} and asserting `final_train["TrainingJobStatus"] == "Completed"`; only then CreateModel (full and restricted roles), CreateTransformJob for the positive control and ListObjectsV2 on `transform-out-ok/`, and finally the restricted-role job. Expected: each Describe* poll converges on a terminal status and the output listing is non-empty for the permissioned control and empty for the restricted one. Actual: the harness's 60 s budget is exhausted with no verdict — no terminal status is observed, so neither the positive control nor the negative is ever evaluated.

## Steps to Reproduce
1. Replay the behavioral suite and run s3__sagemaker.sagemaker-batch-transform-missing-role-permission-negative.
2. Or by hand: create a bucket, upload a training CSV, create an execution role that trusts sagemaker.amazonaws.com with s3:GetObject/PutObject/ListBucket on the bucket, call CreateTrainingJob, then poll DescribeTrainingJob and record how long TrainingJobStatus stays non-terminal; repeat with CreateTransformJob and DescribeTransformJob. Observe that the chain of lifecycles does not reach terminal statuses inside a client-scale poll window, so the transform output prefix is never listed and no permission verdict is reached.
