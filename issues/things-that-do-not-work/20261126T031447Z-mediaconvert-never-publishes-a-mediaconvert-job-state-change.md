Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mediaconvert.job-canceled-event-delivered-via-eventbridge
Issue-Shape-Id: com.amazonaws.mediaconvert#MediaConvert
Issue-Key: mediaconvert.job-canceled-event-delivered-via-eventbridge|_drain_job_statuses#38|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=10)
Issue-Commit: 51c9c2cc53e5121f9175b35e2a43309f7fc0be15

# MediaConvert never publishes a "MediaConvert Job State Change" event, so an EventBridge rule on aws.mediaconvert never fires for a job that leaves SUBMITTED

> _Note: this issue was identified against the code base at commit `51c9c2cc53e5121f9175b35e2a43309f7fc0be15`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
Cause 1 of 1: MediaConvert emits no job lifecycle events onto the account's EventBridge default bus, so no rule can ever observe a job state change.

AWS: recorded behavior only — the Smithy model describes only the request/response surface (JobStatus = SUBMITTED | PROGRESSING | COMPLETE | CANCELED | ERROR, "A job's status can be SUBMITTED, PROGRESSING, COMPLETE, CANCELED, or ERROR."); it does not model events. Per AWS's documented behavior (https://docs.aws.amazon.com/mediaconvert/latest/ug/how-mediaconvert-jobs-progress.html) a job that begins processing publishes CloudWatch Events / EventBridge notifications onto the account's default event bus with source "aws.mediaconvert" and detail-type "MediaConvert Job State Change", whose detail carries at least jobId, status, queue and the role, one event per state the job reaches (STATUS_UPDATE / PROGRESSING / COMPLETE / ERROR). A job that is canceled before it ever began processing emits no Job State Change event at all — the absence is itself the documented behavior. A rule whose EventPattern matches {"source":["aws.mediaconvert"],"detail-type":["MediaConvert Job State Change"]} with an SQS target therefore receives the terminal event of a job submitted to an ACTIVE queue, and receives nothing for the job canceled on a PAUSED queue.

Emulator: nothing is ever published. The emulator's job state machine drives a job out of SUBMITTED entirely inside its own store (ComAmazonawsMediaconvertMediaconvertState::settle / settle_job, and the cancel path) and never hands an event to the EventBridge service, whose routing is reached only through the client-facing PutEvents data plane (see the eventbridge state module: "PutEvents is the data plane: it routes a published event to every enabled rule on the entry's bus whose EventPattern matches"). With a rule and an SQS target correctly wired (PutTargets returns FailedEntryCount 0) and the queue policy allowing events.amazonaws.com, the SQS queue stays empty for the whole polling window: the check drains the queue with ReceiveMessage(WaitTimeSeconds=10) in a loop waiting for the terminal event of the job on the ACTIVE queue and never sees one, so the check does not finish and is reported as a timeout rather than reaching its assertions on detail.status / detail.jobId / source.

Why: a client that reacts to transcode completion/failure through EventBridge — the normal MediaConvert integration pattern (rule -> SQS/Lambda/SNS) instead of polling GetJob — never gets a notification against the emulator. Its pipeline hangs forever on a job that the control plane has already settled, and a negative test that asserts "no event for a job canceled before processing" passes vacuously, so the emulator cannot distinguish the required silence from total silence.

Fix: the MediaConvert job state machine (its settle/cancel transitions in the mediaconvert service's state) must publish a Job State Change event onto the calling account/region's default EventBridge bus on each state transition out of SUBMITTED, with source "aws.mediaconvert", detail-type "MediaConvert Job State Change", resources containing the job ARN and a detail carrying the job's own id, status, queue and userMetadata — going through the same bus routing PutEvents uses so existing rule matching, target delivery and the Invocations/FailedInvocations counters apply; and it must publish nothing for a job canceled while it was still SUBMITTED.

Traces: mediaconvert.job-canceled-event-delivered-via-eventbridge (reported as [mismatch] "Check timed out after 60.0s" — the timeout is in the event poll, no event is ever delivered).

Related but distinct open issues, neither of which covers event emission: 20261024T051247Z-mediaconvert-getjob-wrong-behavior.md (every job terminates in ERROR ~750ms, no PROGRESSING/COMPLETE) and 20261022T040904Z-mediaconvert-queue-job-control-plane-4-fidelity-divergences.md (control-plane validation).

## Evidence
Expected (AWS): after SQS CreateQueue + EventBridge PutRule with EventPattern {"source":["aws.mediaconvert"],"detail-type":["MediaConvert Job State Change"]} + PutTargets(sqs queue arn) + a queue Policy allowing events.amazonaws.com with aws:SourceArn = the rule arn, a job created on an ACTIVE queue delivers a message to that SQS queue whose JSON body has source == "aws.mediaconvert", detail-type == "MediaConvert Job State Change", detail.jobId == the CreateJob job id and detail.status == the job's reached status (ERROR for a job that fails, COMPLETE for one that succeeds). A job created on a PAUSED queue and then CancelJob'd reaches CANCELED with NO message for its jobId ever appearing on the queue.

Actual (emulator): PutRule/PutTargets/SetQueueAttributes all succeed (FailedEntryCount 0), the job's own status does settle in the control plane, but ReceiveMessage on the target queue returns no Messages for the entire poll window — zero events for any jobId. The check never reaches its detail.status/detail.jobId/source assertions and is reported as: [mismatch] mediaconvert.job-canceled-event-delivered-via-eventbridge — "Error: Check timed out after 60.0s".

Code: the mediaconvert state module documents settle/settle_job as the only work done on job state and lists no event publication (services/service-com-amazonaws-mediaconvert-mediaconvert/src/state.rs, module docs lines 1-80 and settle_job); the eventbridge state module documents routing as reachable only from PutEvents (services/service-com-amazonaws-eventbridge-awsevents/src/state.rs lines 14-20).

## Steps to Reproduce
1. Boot the emulator; point mediaconvert, events, sqs and iam clients at it.
2. sqs.create_queue(QueueName='mc-evt'); read its QueueArn via get_queue_attributes(AttributeNames=['QueueArn']).
3. events.put_rule(Name='mc-rule', EventPattern='{"source":["aws.mediaconvert"],"detail-type":["MediaConvert Job State Change"]}'); events.put_targets(Rule='mc-rule', Targets=[{'Id':'sqs-target','Arn':<queue arn>}]) — assert FailedEntryCount == 0.
4. sqs.set_queue_attributes(QueueUrl=..., Attributes={'Policy': <allow events.amazonaws.com sqs:SendMessage on the queue with ArnEquals aws:SourceArn = the rule arn>}).
5. iam.create_role with a mediaconvert.amazonaws.com trust policy; mediaconvert.create_queue(Name='q-active'); mediaconvert.create_job(Role=<role arn>, Queue='q-active', Settings={'Inputs':[{'FileInput':'s3://b/in.mp4'}],'OutputGroups':[{'OutputGroupSettings':{'Type':'FILE_GROUP_SETTINGS','FileGroupSettings':{'Destination':'s3://b/out/'}},'Outputs':[{'ContainerSettings':{'Container':'MP4'},'VideoDescription':{'CodecSettings':{'Codec':'H_264','H264Settings':{'RateControlMode':'QVBR','QvbrSettings':{'QvbrQualityLevel':7},'MaxBitrate':5000000}}}}]}]}).
6. Poll mediaconvert.get_job(Id=<id>) until it leaves SUBMITTED (the emulator settles it within ~1s).
7. Loop sqs.receive_message(QueueUrl=..., MaxNumberOfMessages=10, WaitTimeSeconds=10) for 60s. Expect a message whose body has source 'aws.mediaconvert', detail-type 'MediaConvert Job State Change', detail.jobId == <id> and detail.status == the job's terminal status; observe no messages at all.
8. Control for the required silence: mediaconvert.create_queue(Name='q-paused', Status='PAUSED'), create a job on it, cancel_job(Id=...) and confirm it reaches CANCELED — correctly no event, but indistinguishable from step 7's total silence.
