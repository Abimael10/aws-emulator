Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it|_receive#21|Timeout|for m in sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5,
Issue-Key: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers|_drain#39|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: d9773982178bffc206ea9531196a8841a020d5e3

# events: 2 net-new causes — the DISABLED-rule / post-enable dead-letter sequence reaches no verdict, and the PutEvents 10-entry boundary check never completes its delivery leg

> _Note: this issue was identified against the code base at commit `d9773982178bffc206ea9531196a8841a020d5e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Two net-new root causes on the EventBridge custom-bus delivery path. Both checks consume their entire replay budget without the awaited target effect appearing. Related but NON-covering open issues: 20261115T165311Z-events-the-eventbridge-sqs-target-dead-letter-error.md and 20261111T033241Z-events-eventbridge-sqs-target-dead-letters-misclassify-an-unauthorized.md (dead-letter ERROR_CODE/ERROR_MESSAGE wording), 20261120T033758Z-events-a-cloudformation-deployed-eventbridge-rule-target-with-deadletterconfig.md (CFN-deployed target). None carries the two replay keys below.

Cause 1 of 2: the DISABLED-rule silence window plus the post-enable dead-letter of an undeliverable SQS target never completes — no dead letter with the required attributes is produced.
AWS: Smithy (eventbridge) — PutRule, DisableRule/EnableRule (RuleState enum ENABLED|DISABLED|ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS), PutTargets.Targets[].DeadLetterConfig.Arn ("The ARN of the SQS queue specified as the target for the dead-letter queue") and Targets[].RetryPolicy. While a rule's State is DISABLED it is not evaluated at all: a matching event published to its bus is dropped, never delivered and never dead-lettered, and it is NOT evaluated late after EnableRule. After EnableRule (DescribeRule then reports State "ENABLED"), the next matching event delivered to a target queue that no resource-based policy permits is dead-lettered to DeadLetterConfig.Arn with SQS message attributes ERROR_CODE = "NO_PERMISSIONS", RULE_ARN = the rule's ARN, and ERROR_MESSAGE carrying SQS's relayed refusal — fixed text "User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <target queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <request id>; Proxy: null)", whose only variable parts are the target queue ARN (from the target) and the request id (per call).
Emulator: the check reaches no verdict inside its budget — reported as [mismatch] with "Check timed out after 60.0s". Neither leg completes: no dead letter for the post-enable event is observed on the DeadLetterConfig queue, so ERROR_CODE / ERROR_MESSAGE / RULE_ARN cannot be read back at all, and the disabled-period behavior is never established.
Why: a client that relies on DeadLetterConfig to surface undeliverable events gets silence — a permission mistake on a target queue is unreportable, and it is indistinguishable from a rule that is simply disabled. Operators lose the single signal AWS gives for a target that can never accept the event.
Fix: the EventBridge target-invocation and dead-letter path — the dispatcher's rule-State gate (a DISABLED rule must not be evaluated, and must not replay on enable) and the dead-letter writer that must stamp ERROR_CODE, ERROR_MESSAGE and RULE_ARN as SQS message attributes on the DeadLetterConfig queue.
Traces: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it

Cause 2 of 2: the PutEvents entry-count boundary check never completes its delivery leg — a 10-entry request's entries do not all reach the rule's SQS target.
AWS: Smithy (eventbridge) — PutEventsRequest.Entries targets PutEventsRequestEntryList with @length min 1, max 10, and the operation is an all-or-nothing admission at that bound. An 11-entry request is refused whole with ValidationException and the constraint message whose fixed text is "1 validation error detected: Value '[<rendered entries>]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10" — the variable part is the rendered list of the submitted entries; no entry of a refused request is ever delivered, then or later. A 10-entry request is fully accepted (FailedEntryCount 0, 10 Entries each with its own non-empty unique EventId) and every one of the 10 envelopes reaches the matching rule's SQS target exactly once.
Emulator: the check reaches no verdict inside its budget — reported as [mismatch] with "Check timed out after 60.0s". A bounded wait never completes: either the rule never routes the liveness entry to the target queue, or the 10 accepted entries do not all arrive, so the drain for the full marker set never satisfies its predicate.
Why: a client batching up to the modeled maximum of 10 entries per PutEvents call gets an accepted response (FailedEntryCount 0 with 10 EventIds) while some or all envelopes never reach the target. Events are silently lost at exactly the batch size the API is documented to accept, and the all-or-nothing guarantee at the boundary cannot be relied on.
Fix: the PutEvents handler's per-entry fan-out into the rule dispatcher for a custom event bus, and the SQS target delivery path it feeds (every accepted entry of one request must be matched and enqueued, exactly once).
Traces: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers

## Evidence
Both checks are reported as [mismatch] with detail "Error: Check timed out after 60.0s".

Cause 1 — events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it:
  Expected: on a custom bus, a rule with pattern {"source":[S]} and one target {Id:"bare", Arn:<queue with NO policy>, RetryPolicy:{MaximumRetryAttempts:185, MaximumEventAgeInSeconds:86400}, DeadLetterConfig:{Arn:<DLQ>}}; after DisableRule, DescribeRule State == "DISABLED" and the DLQ receives NOTHING for an event published during that window (and nothing late, after enable); after EnableRule, DescribeRule State == "ENABLED" and the next matching event appears on the DLQ with message attributes ERROR_CODE == "NO_PERMISSIONS", RULE_ARN == the rule ARN, and ERROR_MESSAGE == "User: events.amazonaws.com is not authorized to perform: sqs:sendmessage on resource: <bare queue ARN> because no resource-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)".
  Actual: the check is cut off at its 60.0 s cap with no dead letter observed on the DLQ; none of ERROR_CODE / ERROR_MESSAGE / RULE_ARN is produced.

Cause 2 — events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers:
  Expected: PutEvents of 11 entries is refused ValidationException "1 validation error detected: Value '[...]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10" with none of the 11 delivered then or during a later hold window; PutEvents of exactly 10 entries returns FailedEntryCount 0 with 10 distinct non-empty EventIds and all 10 markers arrive on the rule's SQS target exactly once.
  Actual: the check is cut off at its 60.0 s cap inside a bounded wait for delivery — the full set of accepted markers never arrives on the target queue (the preceding liveness publish/receive loop or the 10-marker drain never satisfies its predicate).

Bus names, rule names, queue names, markers, EventIds and request ids above are per-run generated values; only the statuses, error code, attribute names and the message's fixed text are requirements.

## Steps to Reproduce
Cause 1:
1. events.create_event_bus(Name=B); events.put_rule(Name=R, EventBusName=B, EventPattern={"source":[S]}).
2. sqs.create_queue(BARE) (leave it with NO resource policy); sqs.create_queue(DLQ) and set a policy on DLQ allowing sqs:SendMessage to Service events.amazonaws.com with Condition ArnEquals aws:SourceArn = the rule ARN.
3. events.put_targets(Rule=R, EventBusName=B, Targets=[{Id:"bare", Arn:<BARE arn>, RetryPolicy:{MaximumRetryAttempts:185, MaximumEventAgeInSeconds:86400}, DeadLetterConfig:{Arn:<DLQ arn>}}]) — expect FailedEntryCount 0.
4. events.disable_rule(Name=R, EventBusName=B); events.describe_rule → State "DISABLED". events.put_events one matching entry on B; drain DLQ for a window → expect zero messages.
5. events.enable_rule(Name=R, EventBusName=B); events.describe_rule → State "ENABLED". events.put_events one matching entry on B; receive from DLQ with MessageAttributeNames=["All"] → expect the dead letter with ERROR_CODE "NO_PERMISSIONS", RULE_ARN = rule ARN and the SQS AccessDenied ERROR_MESSAGE above. Observed: nothing arrives and the check is cut off at its 60 s cap.

Cause 2:
1. events.create_event_bus(Name=B); events.put_rule(Name=R, EventBusName=B, EventPattern={"source":[S]}, State="ENABLED").
2. sqs.create_queue(Q); set its policy to allow sqs:SendMessage to Service events.amazonaws.com with Condition ArnEquals aws:SourceArn = the rule ARN; events.put_targets(Rule=R, EventBusName=B, Targets=[{Id:"q", Arn:<Q arn>}]).
3. Publish a throwaway matching entry and receive it from Q to prove routing is live.
4. events.put_events with 11 matching entries → expect ValidationException with the max-10 constraint message and no entry delivered.
5. events.put_events with exactly 10 matching entries, each carrying its own marker → expect FailedEntryCount 0, 10 distinct EventIds, and all 10 markers received from Q. Observed: the markers never all arrive; the check is cut off at its 60 s cap.
