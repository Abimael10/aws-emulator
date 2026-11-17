Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__kms__scheduler__sqs.create-schedule-nonexistent-kms-key-arn-rejected
Issue-Key: iam__kms__scheduler__sqs.create-schedule-nonexistent-kms-key-arn-rejected|_error#13|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# iam__kms__scheduler__sqs: CreateSchedule accepts a KmsKeyArn naming a nonexistent key instead of refusing with ValidationException

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSchedule

## Description
Cause 1 of 1: EventBridge Scheduler does not resolve the schedule's KmsKeyArn against KMS.
  AWS: recorded behavior only — CreateSchedule with a well-formed KmsKeyArn for a key that does not exist is refused 400 ValidationException with the message 'Unable to access the KMS key <kmsKeyArn>. Check that the key exists, and that the IAM principal you use to call EventBridge Scheduler has permission to access the key, then try again.' — fixed text around the key ARN, which is taken verbatim from the request. No schedule is created.
  Emulator: the call returns 200 with a ScheduleArn ('arn:aws:scheduler:<region>:<account>:schedule/default/<name>'), creating a schedule whose customer managed key does not exist.
  Why: a client relying on the service to validate the key at create time gets a schedule that could never run encrypted in AWS, and the refusal its error handling is written against never arrives.
  Fix: the CreateSchedule handler's KmsKeyArn validation (resolve the key in KMS state and refuse before writing the schedule).
  Traces: iam__kms__scheduler__sqs.create-schedule-nonexistent-kms-key-arn-rejected.

## Evidence
create-schedule-nonexistent-kms-key-arn-rejected — AssertionError: expected the call to be refused, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'ScheduleArn': 'arn:aws:scheduler:us-west-2:<acct>:schedule/default/sim2real-behav-sched-<id>'}. Expected ('ValidationException', 'Unable to access the KMS key <arn>. Check that the key exists, and that the IAM principal you use to call EventBridge Scheduler has permission to access the key, then try again.').

## Steps to Reproduce
1. Build a well-formed but nonexistent key ARN: arn:aws:kms:<region>:<account>:key/<random uuid>.
2. scheduler.create_schedule(Name=<name>, KmsKeyArn=<that arn>, ScheduleExpression=..., Target=..., FlexibleTimeWindow=...) -> expect 400 ValidationException with the message above and no schedule created.
