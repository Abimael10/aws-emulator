Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sagemaker.algorithm-delete-allowed-while-referenced
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker.algorithm-delete-allowed-while-referenced|_wait_secondary_status_at_least_downloading#16|AssertionError|raise AssertionError(
Issue-Commit: b5561aeb006573d7ba21a8a050e5e0c3f04cb6be

# sagemaker: a training job's revealed SecondaryStatus phases collapse — an accepted CreateTrainingJob goes InProgress to Completed/Completed with no observable Downloading/Training/Uploading phase

> _Note: this issue was identified against the code base at commit `b5561aeb006573d7ba21a8a050e5e0c3f04cb6be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTrainingJob

## Description
Cause 1 of 1: the training-job lifecycle the emulator reveals has no observable active-container phase, so a client that polls DescribeTrainingJob to learn when a job is genuinely running (and when its artifact exists) never sees one.

AWS: Smithy — DescribeTrainingJob returns TrainingJobStatus (TrainingJobStatus enum) plus SecondaryStatus (com.amazonaws.sagemaker#SecondaryStatus, values Starting, LaunchingMLInstances, PreparingTrainingStack, Downloading, DownloadingTrainingImage, Training, Uploading, Stopping, Stopped, MaxRuntimeExceeded, Completed, Failed, Interrupted, MaxWaitTimeExceeded, Updating, Restarting, Pending) and SecondaryStatusTransitions (SecondaryStatusTransition{Status required, StartTime required, EndTime, StatusMessage}). The model documents the grouping explicitly: while TrainingJobStatus is `InProgress` the secondary status runs `Starting` -> `Downloading` (data being downloaded to the ML storage volumes) -> `Training` -> `Uploading` (model artifacts being uploaded to the S3 location), and only then does the job reach the `Completed` group with SecondaryStatus `Completed`. Each stage the job passed through is reported as its own SecondaryStatusTransition element with a StartTime, so the phases are a sequence a poller traverses, not an instant. On real SageMaker, instance launch plus image download alone keeps a job in the InProgress group long enough that a 10-second poll observes at least one of Downloading/DownloadingTrainingImage/Training/Uploading. The emulator's own handler contract states the same intent: services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_training_job.rs documents that `a caller polling the job observes Downloading, Training and Uploading before the terminal status, and SecondaryStatusTransitions reports every stage the job was in`.

Emulator: for a job accepted with AlgorithmSpecification={AlgorithmName=<a Completed algorithm of this account>, TrainingInputMode=File}, no InputDataConfig (the algorithm's only training channel is IsRequired=false), ResourceConfig ml.m5.large/1/1 GiB and StoppingCondition.MaxRuntimeInSeconds=120, the first DescribeTrainingJob after CreateTrainingJob correctly reports TrainingJobStatus=InProgress, and the very next poll — 10 s later — already reports TrainingJobStatus=Completed with SecondaryStatus=Completed. Polling every 10 s for up to 300 s never observes SecondaryStatus in {Downloading, DownloadingTrainingImage, Training, Uploading}: the replay fails with `AssertionError: training job <name> left InProgress (status=Completed, secondary=Completed) before reaching an active-container phase`. The whole InProgress window — every active-container phase the model groups under it — is revealed inside a sub-poll interval, so in practice it does not exist for any client. (Fixed text of that assertion is the harness's, not AWS's; the requirement is the observable phase sequence, with the job name and the statuses taken from the request and the emulator's own state.)

Why: a client built against AWS polls SecondaryStatus to decide when the container is actually running and, in particular, when model artifacts have been uploaded — the documented `Uploading` stage is the gate for reading s3://<OutputDataConfig.S3OutputPath>/<job>/output/model.tar.gz, and `Downloading`/`Training` are what progress UIs, waiters and test harnesses key on. Against this emulator every such poll loop sees a job teleport from InProgress to Completed, so any logic that must act *while* the job runs (checking that a referenced resource can be mutated mid-run, surfacing progress, reading checkpoints, asserting an artifact is not yet present) can never be exercised, and SecondaryStatusTransitions cannot report a sequence the caller ever had a chance to observe. This also masks the difference between a job that ran and a job that was simply marked done.

Fix: the training-job lifecycle reveal in the sagemaker crate's state — the phase layout recorded by `crate::state::TrainingJob::record_run` and materialised by `crate::state::TrainingJob::settle_status` (bounded by `crate::state::MAX_REVEALED_CONTAINER_WORK`), as contracted by the CreateTrainingJob handler. The active-container phases must occupy a window wide enough that an ordinary poll cadence observes at least one of Downloading/Training/Uploading while TrainingJobStatus is still InProgress — including for a job with no input channels, where the download stage has no data to move — and each observed stage must appear in SecondaryStatusTransitions with its StartTime/EndTime. No modelled status value is wrong here; only the timeline is.

Note: this is NOT the already-open `jobs never execute` family (20261025T095709Z-sagemaker-training-jobs-never-run-an-accepted-createtrainingjob.md, 20261029T211510Z-s3-sagemaker-sagemaker-jobs-never-execute-and-are-never-validated.md), which is about jobs ending Failed with a no-container-runtime reason; here the job ends Completed, and the divergence is that no InProgress/active-container phase is ever observable. It is also not 20261123T120651Z-sagemaker-the-end-to-end-model-endpoint-flows.md (lifecycles never settling): this lifecycle settles too fast, not too slow.

## Evidence
Check sagemaker.algorithm-delete-allowed-while-referenced (verdict: mismatch).

Expected (AWS): after CreateTrainingJob is accepted, DescribeTrainingJob reports TrainingJobStatus=InProgress with SecondaryStatus progressing Starting -> Downloading -> Training -> Uploading before the job reaches TrainingJobStatus=Completed/SecondaryStatus=Completed; a 10 s poll loop observes at least one of {Downloading, DownloadingTrainingImage, Training, Uploading}, and SecondaryStatusTransitions lists each stage with StartTime/EndTime.

Actual (emulator): first DescribeTrainingJob after create -> TrainingJobStatus=InProgress (correct). Next poll 10 s later -> TrainingJobStatus=Completed, SecondaryStatus=Completed. No active-container SecondaryStatus is ever returned within the 300 s poll budget.

Replay output:
  File "check_sagemaker_algorithm_delete_allowed_while_referenced.py", line 73, in run
    _wait_secondary_status_at_least_downloading(sm, job_name, timeout_s=300.0)
  File "check_sagemaker_algorithm_delete_allowed_while_referenced.py", line 227, in _wait_secondary_status_at_least_downloading
    raise AssertionError(
  AssertionError: training job sim2real-behav-train-69c5d8eb0282 left InProgress (status=Completed, secondary=Completed) before reaching an active-container phase

(The job name is a per-run generated value, not part of the requirement; the requirement is the status/secondary-status sequence.)

Because the check aborts here, the rest of its assertions — DeleteAlgorithm succeeding while a live job references the algorithm, and DescribeAlgorithm/second DeleteAlgorithm answering ValidationException "Algorithm <AlgorithmArn> does not exist." — were never exercised and are not claimed as divergences.

## Steps to Reproduce
1. iam.CreateRole with a trust policy allowing sagemaker.amazonaws.com to sts:AssumeRole; attach AmazonSageMakerFullAccess. Create an S3 output bucket.
2. sagemaker.CreateAlgorithm with TrainingSpecification naming a pullable built-in-algorithm ECR image, SupportedTrainingInstanceTypes=[ml.m5.large] and one TrainingChannel {Name: training, IsRequired: false, SupportedContentTypes: [text/csv], SupportedInputModes: [File]}; poll DescribeAlgorithm until AlgorithmStatus=Completed.
3. sagemaker.CreateTrainingJob with AlgorithmSpecification={AlgorithmName: <that algorithm>, TrainingInputMode: File}, RoleArn=<the role>, ResourceConfig={InstanceType: ml.m5.large, InstanceCount: 1, VolumeSizeInGB: 1}, OutputDataConfig={S3OutputPath: s3://<bucket>/output}, StoppingCondition={MaxRuntimeInSeconds: 120}, and NO InputDataConfig.
4. Immediately call sagemaker.DescribeTrainingJob — observe TrainingJobStatus=InProgress.
5. Poll sagemaker.DescribeTrainingJob every 10 s and record SecondaryStatus.
   Expected: at least one poll returns SecondaryStatus in {Downloading, DownloadingTrainingImage, Training, Uploading} while TrainingJobStatus=InProgress, and SecondaryStatusTransitions accumulates those stages.
   Actual: the second poll already returns TrainingJobStatus=Completed with SecondaryStatus=Completed; no active-container phase is ever observed.
