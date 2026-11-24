Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.glue-job-failed-event-missing-target-permission-negative
Issue-Key: athena__glue__s3.glue-job-failed-event-missing-target-permission-negative|run#23|Timeout|resp = sqs.receive_message(QueueUrl=denied_url, MaxNumberOfMessages=10, WaitTimeSeconds=10)
Issue-Commit: ff2a85f16a37fa0b688f1b1dee68f28a0eaac3c8

# athena__glue__s3: the Glue-job FAILED negative-delivery check reaches no verdict — the job-run → 'Glue Job State Change' → EventBridge/SQS path never produces the event, so the whole per-check budget is consumed

> _Note: this issue was identified against the code base at commit `ff2a85f16a37fa0b688f1b1dee68f28a0eaac3c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartJobRun

## Description
Cause 1 of 1: the Glue job-run → EventBridge → SQS delivery path makes no progress for a pythonshell job whose script exits non-zero, so the check that grades EventBridge's SELECTIVE delivery (one SQS target permitted for events.amazonaws.com, one not) never reaches either of its assertions and is killed at the harness budget.

AWS: recorded behavior only for the event emission (the Glue Smithy model describes the job-run surface — StartJobRun returns JobRunId, GetJobRun reports JobRun.JobRunState as a JobRunState enum and JobRun.ErrorMessage — but says nothing about EventBridge). The recording shows: a pythonshell job whose script calls sys.exit(1) settles in terminal JobRunState 'FAILED'; on that terminal transition Glue publishes an event on the default bus with source 'aws.glue', detail-type 'Glue Job State Change' and a detail object carrying at least jobName (the job definition's name) and state (the terminal JobRunState); a rule whose pattern matches that source/detail-type and narrows on detail.jobName/detail.state delivers it to the SQS target whose queue policy allows events.amazonaws.com sqs:SendMessage for that rule ARN, and EventBridge DROPS the delivery to the sibling target whose queue has no such policy — PutTargets itself succeeds for both, no error is surfaced, and the unpermitted queue simply stays empty.

Emulator: nothing in that chain completes within the check's budget. The check's own waits are the Glue run reaching a terminal state (GetJobRun polled every 10 s) and then ReceiveMessage on the permitted queue (long-poll, up to 180 s); the replay produced no verdict at all — 'Error: Check timed out after 60.0s' — so the permitted queue never received a 'Glue Job State Change' message and the negative half (asserting the unpermitted queue receives nothing) was never even reached. Nothing of this check's grading is observable.

Why: EventBridge's authorization behavior for Glue-driven rules is untestable. A client that wires post-failure work (alert, retry, DLQ, archive) to a 'Glue Job State Change' FAILED rule never runs that work, and — because the emulator reports no error from PutRule/PutTargets — gets no signal that the notification was lost; a consumer blocked on the queue blocks forever instead of failing. The negative half is worse than silent: a reviewer cannot distinguish 'EventBridge correctly refused the unpermitted target' from 'no event was ever published to any target', so a missing queue policy looks the same as a correct deny.

Fix: the Glue job-run state machine behind StartJobRun/GetJobRun (services/service-com-amazonaws-glue-awsglue — the job-run executor/state writer) must settle a failing pythonshell run in terminal JobRunState FAILED and publish the 'Glue Job State Change' envelope into the EventBridge default bus on that terminal transition, handing it to the events crate (services/service-com-amazonaws-eventbridge-awsevents) for pattern matching and per-target delivery; that delivery site is also where the target queue's resource policy for events.amazonaws.com must be evaluated so an unpermitted target is dropped silently rather than erroring or being skipped wholesale.

Traces: athena__glue__s3.glue-job-failed-event-missing-target-permission-negative.

Related open issue that does NOT carry this check's replay key (and so does not cover it): 20261109T085123Z-athena-glue-s3-3-net-new-causes-glue.md, whose Cause 1 describes the same missing 'Glue Job State Change' publication but is grounded only in the SUCCEEDED-path checks athena__glue__s3.eventbridge-rule-filters-glue-job-name-positive and athena__glue__s3.glue-job-succeeded-event-triggers-eventbridge-rule-positive; neither it nor any other open issue lists this check, and no open issue covers the per-target queue-policy evaluation this check grades.

## Evidence
athena__glue__s3.glue-job-failed-event-missing-target-permission-negative — replay detail: 'Error: Check timed out after 60.0s'. No assertion was reached.

Expected (AWS): (a) GetJobRun(JobName=<job>, RunId=<run>)['JobRun']['JobRunState'] == 'FAILED' for a pythonshell script of `import sys\nsys.exit(1)`; (b) ReceiveMessage on the queue whose policy grants events.amazonaws.com sqs:SendMessage (conditioned on aws:SourceArn == the rule ARN) returns one message whose JSON body has body['detail']['jobName'] == <job> and body['detail']['state'] == 'FAILED'; (c) ReceiveMessage on the sibling queue with NO resource policy returns no message for the following 45 s drain window (`assert received == []`).

Actual (emulator): the check produced no verdict inside its budget — the permitted queue never yielded a matching message (its poll is the check's only unbounded wait before the negative drain), so assertion (b) never completed and assertions (a)/(c) were never graded. The control-plane prerequisites are not implicated: CreateBucket/PutObject, CreateRole + AttachRolePolicy(AWSGlueServiceRole) + PutRolePolicy, glue CreateJob(Command={Name:'pythonshell', ScriptLocation:'s3://<bucket>/scripts/<script>.py', PythonVersion:'3.9'}, MaxCapacity=0.0625, Timeout=5), sqs CreateQueue x2, GetQueueAttributes(QueueArn), SetQueueAttributes(Policy), events PutRule(State='ENABLED'), TagResource and PutTargets(Ids=['denied','allowed']) all had to answer before the first wait is entered.

## Steps to Reproduce
1. Create an S3 bucket and put a pythonshell script `import sys\nsys.exit(1)` at scripts/<name>.py.
2. Create an IAM role trusting glue.amazonaws.com, attach arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole and an inline policy granting s3:GetObject/PutObject/ListBucket on the bucket.
3. glue CreateJob(Name=<job>, Role=<role-arn>, Command={'Name':'pythonshell','ScriptLocation':'s3://<bucket>/scripts/<name>.py','PythonVersion':'3.9'}, MaxCapacity=0.0625, Timeout=5).
4. sqs CreateQueue twice; read each QueueArn. Give only ONE of them a policy: Allow Principal {'Service':'events.amazonaws.com'} Action sqs:SendMessage on its own ARN, Condition ArnEquals aws:SourceArn == the rule ARN.
5. events PutRule(Name=<rule>, EventPattern={'source':['aws.glue'],'detail-type':['Glue Job State Change'],'detail':{'jobName':[<job>],'state':['FAILED']}}, State='ENABLED') then PutTargets with both queues ('denied' = unpolicied, 'allowed' = policied).
6. glue StartJobRun(JobName=<job>) and poll GetJobRun until JobRunState is terminal — AWS reports FAILED.
7. ReceiveMessage on the policied queue — AWS delivers one 'Glue Job State Change' message with detail.jobName == <job> and detail.state == 'FAILED'; the emulator delivers nothing and the poll runs to its deadline.
8. ReceiveMessage on the unpolicied queue for 45 s — AWS delivers nothing (silent drop).
Re-replay check athena__glue__s3.glue-job-failed-event-missing-target-permission-negative.
