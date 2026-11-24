Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.eventbridge-rule-missing-target-permission-negative
Issue-Key: emr__glue__s3.eventbridge-rule-missing-target-permission-negative|_receive_all#38|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=5, WaitTimeSeconds=2)
Issue-Commit: 83417ec3dff2f3efed8690d96b9cf1f73f5857f5

# emr__glue__s3: the EventBridge Glue-job-state negative-delivery differential still reaches no verdict — the pythonshell job run never settles in a terminal JobRunState, so neither the permissioned SQS target's delivery nor the unpermissioned target's silence is observed

> _Note: this issue was identified against the code base at commit `83417ec3dff2f3efed8690d96b9cf1f73f5857f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetJobRun

## Description
Cause 1 of 1: the Glue job run that drives the EventBridge "Glue Job State Change" delivery differential never becomes observable in a terminal JobRunState, so the client's bounded start-and-wait loop makes no progress and the check is cut off at the harness budget before any delivery assertion is reached. Failing check: emr__glue__s3.eventbridge-rule-missing-target-permission-negative.

NOTE ON OVERLAP: open issue 20261123T183657Z-emr-glue-s3-the-eventbridge-glue-job-state.md records this same check id, and 20261029T175551Z-glue-job-runs-reached-through-the-emr-glue.md, 20261118T205847Z-emr-glue-s3-a-glue-pythonshell-job-run.md and 20261115T165006Z-emr-glue-s3-the-glue-job-driven-eventbridge.md record non-converging Glue job runs for other check ids. The replay report did NOT mark this check ALREADY FILED or OWNED by any of them — their replay keys bind different steps — so it is filed here and should be closed together with them; it is the same defect still present, not a new one.

AWS: Smithy com.amazonaws.glue#StartJobRun returns StartJobRunResponse{JobRunId} for an existing job; com.amazonaws.glue#GetJobRun returns JobRun{JobRunState, ErrorMessage, StartedOn, CompletedOn, ExecutionTime, ...} and com.amazonaws.glue#JobRunState enumerates STARTING, RUNNING, STOPPING, STOPPED, SUCCEEDED, FAILED, TIMEOUT, ERROR, WAITING, EXPIRED. A pythonshell job (PythonVersion 3.9, MaxCapacity 0.0625) whose script exits 0 progresses STARTING -> RUNNING -> SUCCEEDED and carries no ErrorMessage. On that terminal transition AWS publishes an event with source `aws.glue`, detail-type `Glue Job State Change` and a detail carrying the job name and state SUCCEEDED; an ENABLED EventBridge rule matching that pattern delivers it to an SQS target whose queue policy allows sqs:SendMessage to Service events.amazonaws.com (conditioned on aws:SourceArn = the rule ARN), and delivers nothing to a sibling target queue that has no resource policy at all. The requirement is that pair of observations — at least one message on the permissioned queue whose JSON body has detail.jobName equal to the request's job name and detail.state "SUCCEEDED", and zero messages on the unpermissioned queue for the whole bounded absence window. Run id, queue names, bucket name and timestamps are per-run values, not requirements.
Emulator: the replay reports `Error: Check timed out after 60.0s` — no assertion in the check body was reached. The check's only progress gate before its first assertion is the start-and-wait cycle over StartJobRun/GetJobRun, which polls until JobRunState is terminal and retries the whole run while a terminal run reports an assume-role ErrorMessage; the budget is consumed inside that cycle, so the emulator never presents the run in a terminal JobRunState the client accepts. Hypothesis: the Glue job-run executor and its run-state machine do not drive the run to a terminal JobRunState (or settle it with a role-propagation ErrorMessage that keeps the client retrying), so the downstream terminal-transition event publication, the rule evaluation and the two SQS reads are never exercised.
Why: a client polling GetJobRun cannot distinguish a run still executing from one that will never finish, so it hangs or times out instead of acting on an outcome. Everything gated on run completion — the `Glue Job State Change` event, EventBridge rule evaluation, and the SQS target queue-policy boundary — never happens, so the permission boundary AWS enforces becomes unobservable: a correctly-policed target and a missing-policy target produce the same empty result and a client cannot tell that its event delivery is silently broken.
Fix: the Glue job-run executor and the StartJobRun/GetJobRun run-state machine in the glue service crate (the StartJobRun handler plus the job-run state GetJobRun reads), which must execute the referenced script and settle the run in a terminal JobRunState with AWS's ErrorMessage semantics; and the terminal-transition `Glue Job State Change` event publication that feeds EventBridge rule evaluation and SQS target delivery.
Traces: emr__glue__s3.eventbridge-rule-missing-target-permission-negative.

## Evidence
Failing check (verdict: mismatch): emr__glue__s3.eventbridge-rule-missing-target-permission-negative
Replay output: `Error: Check timed out after 60.0s` — no assertion in the check body was reached.

Scenario replayed: one S3 bucket holding scripts/job.py (a script that prints and exits 0); an IAM role trusting glue.amazonaws.com with AWSGlueServiceRole attached plus an inline Allow on s3:GetObject/s3:ListBucket for that bucket and its keys; a Glue job (Command Name=pythonshell, PythonVersion=3.9, ScriptLocation=s3://<bucket>/scripts/job.py, MaxCapacity=0.0625, Timeout=5); an ENABLED EventBridge rule with pattern {source: ["aws.glue"], detail-type: ["Glue Job State Change"], detail: {jobName: [<job>], state: ["SUCCEEDED"]}}; and two SQS queues registered as rule targets — target "ok" with a queue policy allowing sqs:SendMessage to Service events.amazonaws.com conditioned on aws:SourceArn = <rule arn>, and target "broken" with no resource policy at all (the single thing broken).

Expected per AWS: start_job_run then get_job_run converges on JobRunState "SUCCEEDED" with no ErrorMessage; ReceiveMessage on the "ok" queue yields at least one message whose JSON body has detail.jobName == the job name and detail.state == "SUCCEEDED"; ReceiveMessage on the "broken" queue yields zero messages for the whole bounded absence window.
Actual from the emulator: the start-and-wait cycle over GetJobRun never yields a terminal JobRunState the client accepts, the 60 s budget expires inside it, and neither the positive delivery nor the negative absence is ever observed.

## Steps to Reproduce
Boot the emulator and replay emr__glue__s3.eventbridge-rule-missing-target-permission-negative from suite simparity-behavioral-aws-v1, or by hand:
1. s3.create_bucket; s3.put_object key scripts/job.py with a trivially succeeding Python body (print('ok')).
2. iam.create_role trusting glue.amazonaws.com; attach arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole; put_role_policy allowing s3:GetObject/s3:ListBucket on arn:aws:s3:::<bucket> and arn:aws:s3:::<bucket>/*.
3. glue.create_job(Name=<job>, Role=<role arn>, Command={Name: 'pythonshell', ScriptLocation: 's3://<bucket>/scripts/job.py', PythonVersion: '3.9'}, MaxCapacity=0.0625, Timeout=5).
4. events.put_rule(Name=<rule>, State='ENABLED', EventPattern={source: ['aws.glue'], detail-type: ['Glue Job State Change'], detail: {jobName: [<job>], state: ['SUCCEEDED']}}).
5. sqs.create_queue twice. On the 'ok' queue, set_queue_attributes with a Policy allowing sqs:SendMessage to Service events.amazonaws.com conditioned on aws:SourceArn = <rule arn>; leave the 'broken' queue with no policy. events.put_targets(Rule=<rule>, Targets=[{Id: 'ok', Arn: <ok queue arn>}, {Id: 'broken', Arn: <broken queue arn>}]).
6. glue.start_job_run(JobName=<job>) and poll glue.get_job_run(JobName=<job>, RunId=<id>) for a terminal JobRunState. AWS: SUCCEEDED within the poll window. Emulator: no accepted terminal JobRunState is ever observed and the check's 60 s budget expires here.
7. Then sqs.receive_message on both queues. AWS: exactly one matching event on the 'ok' queue (body detail.jobName == <job>, detail.state == 'SUCCEEDED') and nothing on the 'broken' queue for the full absence window. Emulator: neither observation is reached.
