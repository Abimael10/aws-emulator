Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sns.eventbridge-sns-target-missing-topic-policy-not-published
Issue-Key: events__lambda__sns.eventbridge-sns-target-missing-topic-policy-not-published|_wait_for_matching_message#15|Timeout|resp = sqs.receive_message(
Issue-Commit: 2c0d8b9f0e4568b2774877fc481a224c15ff713e

# events__lambda__sns: the EventBridge SNS-target topic-policy scenario never reaches a verdict — the check is cut at the 60 s budget with nothing observed on the subscriber queue

> _Note: this issue was identified against the code base at commit `2c0d8b9f0e4568b2774877fc481a224c15ff713e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One root cause (Cause 1 of 1).

Attribution note: the suite lists this check under the composite heading 'events__lambda__sns'. get_bootstrap_status() in this checkout reports an EMPTY 'pending' list and names crate-style tokens, so no constituent service is pending: events -> service-com-amazonaws-eventbridge-awsevents / service-com-amazonaws-cloudwatchevents-awsevents, lambda -> service-com-amazonaws-lambda-awsgirapiservice, sns -> service-com-amazonaws-sns-amazonsimplenotificationservice are all in the bootstrapped set. Gate verdict: FILE. The operation whose effect is in question is the EventBridge one (PutEvents target delivery to an SNS topic).

Related OPEN issues that do NOT cover this failure (the suite marked this check neither 'OWNED by' nor 'ALREADY FILED'): 20261026T070928Z-events-lambda-sns-sqs-a-putevents-event-matching.md reports a three-target fan-out (Lambda+SQS+SNS) reaching none of its targets and carries the replay key of a DIFFERENT check (events__lambda__sns__sqs.eventbridge-fanout-lambda-sns-sqs); 20261107T143733Z-events-lambda-sns-an-asynchronous-invoke-s-stdout.md owns this service's sibling check (eventbridge-fanout-both-targets-denied-no-delivery) on the Lambda-stdout leg, which this check never touches. Neither carries this check's replay key, and this check is a single-target SNS scenario with no Lambda invoke and no log read, so closing either will not make it pass.

Cause 1 of 1: the rule-triggered SNS-target delivery scenario produces no observable message in either phase, so the scenario cannot be graded.
AWS: recorded behavior only for the delivery effect (PutEvents' modeled output only reports per-entry acceptance — FailedEntryCount and Entries[].EventId — so the request/response pair cannot show whether a target was reached; Smithy: PutEvents -> PutEventsResponse, no delivery member). AWS's documented rule for the target kinds EventBridge reaches through the target's own document is 'for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based policies': a topic target whose access policy grants events.amazonaws.com nothing receives NO publish (the delivery fails and is dead-lettered/counted on FailedInvocations), and once a statement Allow'ing SNS:Publish for Principal {"Service": "events.amazonaws.com"} on that topic ARN — optionally Condition ArnEquals aws:SourceArn = the invoking rule's ARN — is written with SetTopicAttributes(AttributeName='Policy'), an identical PutEvents entry matching the same rule DOES publish, and the topic's sqs subscriber receives the notification envelope within seconds. Those two phases are what the check grades, after a positive control (a direct Publish by the topic owner) proves the SNS->SQS observation path.
Emulator: the scenario never converges. The subscriber queue is polled with receive_message(WaitTimeSeconds=5) in a 60 s loop for the no-grant phase and again for the post-grant phase, and the harness kills the whole check first: verbatim detail 'Error: Check timed out after 60.0s'. No API call raises — create_topic, create_queue, set_queue_attributes, subscribe, publish, put_rule, put_targets and put_events are all accepted — so at the cut the only observable fact is that no rule-triggered notification has appeared on the subscribed queue, and the restoring-grant leg (the one that proves a missing topic policy, rather than a target dispatcher that never publishes at all, was what withheld the message) is never reached. That is the same shape as the open fan-out issue above, where all three target kinds including SNS stayed empty indefinitely.
Why: a client cannot tell an EventBridge SNS target that was correctly withheld for lack of a topic-policy grant from one that is never dispatched. Both read as 'no message'. An integration that notifies by routing events to a topic gets 200s from PutRule/PutTargets/PutEvents, a FailedEntryCount of 0, and silence downstream; and the authorization contract AWS documents (grant the statement, delivery resumes) is unverifiable on this emulator.
Fix: the rule-target dispatch path behind PutEvents in the EventBridge service and the topic-policy verdict it reads — the SNS branch of the handover (services/service-com-amazonaws-eventbridge-awsevents/src/ops/put_events.rs, topic_delivery_refusal / topic_attributes) together with the shared resource-policy decision it calls (services/service-com-amazonaws-eventbridge-awsevents/src/state.rs, target_policy_admits / target_policy_decision) must (a) actually publish the envelope to a topic target whose policy Allows SNS:Publish for the events service principal, including when that statement is scoped by Condition ArnEquals aws:SourceArn to the invoking rule's ARN and when its Action is written in AWS's mixed-case 'SNS:Publish' form, and (b) do so promptly enough that a subscriber observes the notification well inside one polling window, so the negative and restored phases of this scenario can both be graded inside the suite's budget.
Traces: events__lambda__sns.eventbridge-sns-target-missing-topic-policy-not-published.

## Evidence
Suite simparity-behavioral-aws-v1 (548/632 passed): service heading 'events__lambda__sns' 20/22 passed, 2 failed. The second failure (eventbridge-fanout-both-targets-denied-no-delivery) is marked 'OWNED by 20261107T143733Z-events-lambda-sns-an-asynchronous-invoke-s-stdout.md'; THIS failure carries no 'OWNED by' and no 'ALREADY FILED' marker.

Failing check, verbatim detail:
  [mismatch] events__lambda__sns.eventbridge-sns-target-missing-topic-policy-not-published
  Error: Check timed out after 60.0s

Shape of the replayed run (from the check source in the report):
  1. create_topic + create_queue; queue policy allows sns.amazonaws.com sqs:SendMessage with Condition ArnEquals aws:SourceArn = topic ARN; sns.subscribe(Protocol='sqs', ReturnSubscriptionArn=True).
  2. Positive control: sns.publish(TopicArn, Message='{"marker": "<control-marker>"}') then poll up to 60 s for an envelope whose Message contains the marker. Assertion on failure: 'positive control publish was never observed on the subscriber queue'.
  3. put_rule(EventPattern='{"source": ["<src>"]}', State='ENABLED'); put_targets(Targets=[{Id:'1', Arn:<topic arn>}]) with NO topic-policy grant for events.amazonaws.com; put_events(Entries=[{Source:<src>, DetailType:'sim2real.test', Detail:'{"marker": "<marker>"}'}]); poll up to 60 s expecting []. Assertion on failure: 'message was published without a topic policy grant: ...'.
  4. get_topic_attributes -> append {Sid:'AllowEventBridgePublish', Effect:'Allow', Principal:{Service:'events.amazonaws.com'}, Action:'SNS:Publish', Resource:<topic arn>, Condition:{ArnEquals:{aws:SourceArn:<rule arn>}}} -> set_topic_attributes(AttributeName='Policy'); re-publish an identical entry under a fresh marker; poll up to 60 s expecting a message. Assertion on failure: 'granting the topic policy did not unblock delivery of the identical rule/target'.

Expected: steps 2-4 all satisfied — control envelope observed, nothing observed while the grant is missing, and the identical rule-triggered publish observed once the statement is written.
Actual: the harness kills the check at its 60 s bound without any API error; no rule-triggered notification is ever observed on the subscribed queue, and the step-4 grading never runs. HTTP status/error code: none — every call answered 2xx, so there is no diverging error code or message to quote; the divergence is the missing delivery effect plus the scenario's failure to converge.

## Steps to Reproduce
1. Boot the emulator; point botocore sns / sqs / events clients at it.
2. sns.create_topic; sqs.create_queue; read QueueArn; sqs.set_queue_attributes with a Policy allowing Principal {"Service":"sns.amazonaws.com"} sqs:SendMessage on that queue with Condition ArnEquals aws:SourceArn = the topic ARN; sns.subscribe(TopicArn=<topic>, Protocol='sqs', Endpoint=<queue arn>, ReturnSubscriptionArn=True).
3. Control: sns.publish(TopicArn=<topic>, Message='{"marker":"control"}') and sqs.receive_message until the envelope's Message contains 'control' — confirms the observation path.
4. events.put_rule(Name=<rule>, EventPattern='{"source": ["my.src"]}', State='ENABLED'); events.put_targets(Rule=<rule>, Targets=[{'Id':'1','Arn':<topic arn>}]). Do NOT add any topic-policy statement for events.amazonaws.com.
5. events.put_events(Entries=[{'Source':'my.src','DetailType':'sim2real.test','Detail':'{"marker":"denied"}'}]) and poll the queue briefly: expect nothing (this leg matches AWS).
6. sns.get_topic_attributes -> append {'Sid':'AllowEventBridgePublish','Effect':'Allow','Principal':{'Service':'events.amazonaws.com'},'Action':'SNS:Publish','Resource':<topic arn>,'Condition':{'ArnEquals':{'aws:SourceArn':<rule arn>}}} to Statement -> sns.set_topic_attributes(TopicArn=<topic>, AttributeName='Policy', AttributeValue=<json>).
7. events.put_events with the identical Source/DetailType and a fresh marker, then sqs.receive_message(MaxNumberOfMessages=10, WaitTimeSeconds=5) in a loop. Expect the notification envelope within seconds; observe the queue stay empty (and, in the suite, the whole check killed at the 60 s bound before this leg is even graded).
8. Full grounding: replay events__lambda__sns.eventbridge-sns-target-missing-topic-policy-not-published.
