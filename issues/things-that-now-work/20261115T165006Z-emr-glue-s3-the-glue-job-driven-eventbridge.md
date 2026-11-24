Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.eventbridge-rule-missing-target-permission-negative
Issue-Key: emr__glue__s3.eventbridge-rule-missing-target-permission-negative|_receive_all#38|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=5, WaitTimeSeconds=2)
Issue-Commit: 78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5

# emr__glue__s3: the Glue-job-driven EventBridge negative-delivery check never converges — the job run does not settle in a state the client accepts, so the 60 s budget expires before any assertion

> _Note: this issue was identified against the code base at commit `78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
Cause 1 of 1: the Glue job run that drives the EventBridge "Glue Job State Change" delivery differential never becomes observable in the terminal state AWS produces, so the client's bounded start-and-wait loop makes no progress and the check is cut off at the harness budget before it can judge either the permissioned target or the unpermissioned one. Failing check: emr__glue__s3.eventbridge-rule-missing-target-permission-negative.

AWS: Smithy com.amazonaws.glue#StartJobRun returns StartJobRunResponse{JobRunId} for an existing job; com.amazonaws.glue#GetJobRun returns JobRun{JobRunState, ErrorMessage, ExecutionTime, StartedOn, CompletedOn, ...} and com.amazonaws.glue#JobRunState enumerates STARTING, RUNNING, STOPPING, STOPPED, SUCCEEDED, FAILED, TIMEOUT, ERROR, WAITING, EXPIRED. A pythonshell job whose script exits 0 progresses STARTING -> RUNNING -> SUCCEEDED, carries no ErrorMessage, and on that terminal transition AWS publishes an event with source `aws.glue`, detail-type `Glue Job State Change` and a detail carrying the job name and `state: SUCCEEDED`. An EventBridge rule matching that pattern delivers the event to an SQS target whose queue policy allows `sqs:SendMessage` to principal `events.amazonaws.com`, and delivers nothing to a sibling target queue that has no such resource policy. The requirement is that pair of observations (one message on the permissioned queue whose detail.jobName and detail.state echo the job and SUCCEEDED, zero messages on the unpermissioned queue); the run id, queue names, bucket name and timestamps are per-run values, not requirements.
Emulator: the replay reports `Error: Check timed out after 60.0s` — no assertion was reached at all. The check's progress gate is `_start_and_wait_job_run()`, which starts the run and polls GetJobRun until JobRunState is one of SUCCEEDED/FAILED/ERROR/TIMEOUT/STOPPED, then retries the whole run while the terminal run reports an "assume role permissions" ErrorMessage; the 60 s budget is consumed inside that cycle, so the emulator never presents the run in a terminal state the client accepts within the budget. Hypothesis: the Glue job-run executor/state machine does not drive this run to a terminal JobRunState (or settles it with a role-propagation ErrorMessage that keeps the client retrying), so the downstream EventBridge publish and the two SQS reads are never exercised.
Why: a client polling GetJobRun cannot distinguish a run that is still executing from one that will never finish, so it hangs or times out instead of acting on an outcome; everything gated on run completion — the `Glue Job State Change` event, EventBridge rule evaluation and the SQS target-policy boundary — never happens. The permission boundary AWS enforces (delivery only to the queue whose policy grants events.amazonaws.com) therefore becomes unobservable: a correctly-policed target and a missing-policy target produce the same empty result.
Fix: the Glue job-run executor and the StartJobRun/GetJobRun run-state machine in the Glue service crate (the StartJobRun handler plus the job-run state GetJobRun reads), which must execute the referenced script and settle the run in a terminal JobRunState with AWS's ErrorMessage semantics; and the terminal-transition event publication that feeds EventBridge.
Related open issues, none of which the replay report bound to this failure: 20261029T175551Z-glue-job-runs-reached-through-the-emr-glue.md lists this very check id, but its replay key binds the older failure mode (`assert final_state == "SUCCEEDED"` at run#31, i.e. a run that settled FAILED immediately) and the report did not mark this check OWNED, so the observation has changed from "terminal FAILED fast" to "no accepted terminal state inside the budget"; 20261115T052937Z-emr-glue-s3-a-glue-etl-job-run-7a60e7.md and 20261114T025704Z-emr-glue-s3-a-glue-etl-job-run.md record the same non-converging run for other check ids; 20261109T185037Z-glue-sqs-3-net-new-causes-glue-job-state-change.md records the missing `Glue Job State Change` publication. This issue should be closed together with them.
Traces: emr__glue__s3.eventbridge-rule-missing-target-permission-negative.

## Evidence
Failing check (verdict: mismatch): emr__glue__s3.eventbridge-rule-missing-target-permission-negative
Replay output: `Error: Check timed out after 60.0s` — no assertion in the check body was reached.

Scenario replayed: one bucket holding scripts/job.py (a script that prints and exits 0); an IAM role trusting glue.amazonaws.com with AWSGlueServiceRole plus Allow s3:GetObject/s3:ListBucket on that bucket; a Glue job (Command Name=pythonshell, PythonVersion=3.9, MaxCapacity=0.0625, Timeout=5) on that script; an EventBridge rule with pattern {source: [aws.glue], detail-type: ["Glue Job State Change"], detail: {jobName: [<job>], state: [SUCCEEDED]}}; two SQS queues as targets — "ok" with a queue policy allowing sqs:SendMessage to Service events.amazonaws.com conditioned on aws:SourceArn = the rule ARN, and "broken" with no resource policy.

Expected per AWS: start_job_run then get_job_run converges on JobRunState SUCCEEDED (no ErrorMessage); ReceiveMessage on the "ok" queue yields at least one message whose JSON Body has detail.jobName == the job name and detail.state == "SUCCEEDED"; ReceiveMessage on the "broken" queue yields zero messages for the whole bounded 60 s absence window.
Actual from the emulator: the start-and-wait cycle over GetJobRun never yields a terminal state the client accepts, the harness 60 s budget expires inside it, and neither the positive delivery nor the negative absence is ever observed.

## Steps to Reproduce
Boot the emulator and replay emr__glue__s3.eventbridge-rule-missing-target-permission-negative from suite simparity-behavioral-aws-v1, or by hand:
1. create_bucket; put_object scripts/job.py with a trivially succeeding Python body.
2. iam.create_role trusting glue.amazonaws.com, attach AWSGlueServiceRole, put_role_policy allowing s3:GetObject/s3:ListBucket on the bucket and its keys; poll get_role until visible.
3. glue.create_job(Name=<job>, Role=<role arn>, Command={Name: pythonshell, ScriptLocation: s3://<bucket>/scripts/job.py, PythonVersion: 3.9}, MaxCapacity=0.0625, Timeout=5).
4. events.put_rule(Name=<rule>, State=ENABLED, EventPattern={source:[aws.glue], detail-type:["Glue Job State Change"], detail:{jobName:[<job>], state:[SUCCEEDED]}}).
5. sqs.create_queue twice; on the "ok" queue set_queue_attributes with a Policy allowing sqs:SendMessage to Service events.amazonaws.com with Condition ArnEquals aws:SourceArn = <rule arn>; leave the "broken" queue with no policy. events.put_targets with both queue ARNs (Ids "ok" and "broken").
6. glue.start_job_run(JobName=<job>) and poll glue.get_job_run(JobName, RunId) for a terminal JobRunState. AWS: SUCCEEDED within the poll window. Emulator: no accepted terminal state is observed and the check's budget expires here.
7. Then receive_message on both queues: AWS delivers exactly one matching event to the "ok" queue (detail.jobName == <job>, detail.state == "SUCCEEDED") and nothing to the "broken" queue for the full absence window.
