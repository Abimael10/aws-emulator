Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-mlflow-tracking-server-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-mlflow-tracking-server-error-paths sagemaker-mlflow-tracking-server-lifecycle sagemaker-mlflow-tracking-server-presigned-url-error
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|CreatePresignedMlflowTrackingServerUrl|4xx->4xx|$.Message
Issue-Commit: 1b8db06d77ea9b8946e423ddb778f8ee81626f53

# SageMaker MLflow tracking-server ResourceNotFound message text diverges from AWS wording

> _Note: this issue was identified against the code base at commit `1b8db06d77ea9b8946e423ddb778f8ee81626f53`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePresignedMlflowTrackingServerUrl

## Description
Cause 1 of 1: the ResourceNotFound raised for a missing MLflow tracking server carries emulator-invented prose instead of AWS's message text.

AWS: Smithy (services/service-com-amazonaws-sagemaker-sagemaker) models CreatePresignedMlflowTrackingServerUrl with a single modeled error, ResourceNotFound (errorKind client, httpError 400, member Message -> FailureReason). The recorded response for a tracking server that does not exist is HTTP 400 with __type ResourceNotFound and the Message rendered from the template:

    Tracking Server with the following ARN does not exist: <trackingServerArn>

The fixed text is "Tracking Server with the following ARN does not exist: "; the only variable part is the tracking-server ARN, which AWS derives from the request's TrackingServerName plus the caller's partition/region/account (arn:aws<partition>:sagemaker:<region>:<account>:mlflow-tracking-server/<name>). The message is addressed by ARN, not by bare name, and it is not quoted and has no trailing period. The status (400) and the error code (ResourceNotFound) are correct in the emulator; only the message body diverges. Beyond the model's error list, this message template is recorded behavior only.

Emulator: returns HTTP 400 / ResourceNotFound but with Message

    Could not find mlflow tracking server "<trackingServerArn>".

i.e. a different fixed text ("Could not find mlflow tracking server", lowercased service noun), the ARN wrapped in double quotes, and a trailing period. Same divergence appears on the three tracking-server fixtures in this cluster, so the wording comes from one shared not-found formatter rather than from one handler.

Why: clients and test suites that assert on SageMaker error text — and the many SDK/CLI wrappers that surface Message verbatim to users — see a message no AWS region ever emits. Anything matching on the documented "Tracking Server with the following ARN does not exist:" prefix, or parsing the ARN out of the message to retry/branch on the resource identity, fails against the emulator even though the status and error code line up, which is the hardest class of divergence to notice.

Fix: the MLflow tracking-server not-found path in the sagemaker crate — the shared ResourceNotFound construction used by CreatePresignedMlflowTrackingServerUrl, DescribeMlflowTrackingServer and the other tracking-server lookups. The message template, not the status or code, is what needs to change: emit the fixed text "Tracking Server with the following ARN does not exist: " followed by the resolved tracking-server ARN, unquoted and with no trailing period.

Traces: sagemaker-mlflow-tracking-server-error-paths#task#2 (representative), sagemaker-mlflow-tracking-server-lifecycle#task#2, sagemaker-mlflow-tracking-server-presigned-url-error#task#1 — all three are members of the same divergence cluster (service=sagemaker|CreatePresignedMlflowTrackingServerUrl|status=400->400|keys=$.Message). No open issue covers this; list_existing_issues finds no mlflow issue at all.

## Evidence
Replay cluster service=sagemaker|CreatePresignedMlflowTrackingServerUrl|status=400->400|keys=$.Message, 3 affected steps.

Expected status 400, actual status 400 (status and error code agree).
$.Message: value_mismatch
  expected: 'Tracking Server with the following ARN does not exist: arn:aws:sagemaker:us-west-2:000000000000:mlflow-tracking-server/sim2real-probe-sagemaker-sm-mlflow-ts-aa1f92e3'
  actual:   'Could not find mlflow tracking server "arn:aws:sagemaker:us-west-2:000000000000:mlflow-tracking-server/sim2real-probe-sagemaker-sm-mlflow-ts-aa1f92e3".'

Fixed text (requirement): 'Tracking Server with the following ARN does not exist: '. Variable part (from the request plus emulator state/identity): the tracking-server ARN; the recorded region us-west-2, account 000000000000 and name sim2real-probe-sagemaker-sm-mlflow-ts-aa1f92e3 are this run's values filling that slot, not part of the template.

Smithy: CreatePresignedMlflowTrackingServerUrl errors = [ResourceNotFound] (client, httpError 400 defaulted); ResourceNotFound.Message -> FailureReason, @length max 1024.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture sagemaker-mlflow-tracking-server-error-paths, step task#2.
2. The step calls CreatePresignedMlflowTrackingServerUrl with a TrackingServerName that does not exist in emulator state.
3. Observe HTTP 400 with __type ResourceNotFound, and compare $.Message against the recorded response: the emulator emits 'Could not find mlflow tracking server "<arn>".' where AWS emits 'Tracking Server with the following ARN does not exist: <arn>'.
4. The same mismatch reproduces on sagemaker-mlflow-tracking-server-lifecycle#task#2 and sagemaker-mlflow-tracking-server-presigned-url-error#task#1.
