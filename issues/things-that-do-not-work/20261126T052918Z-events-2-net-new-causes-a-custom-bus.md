Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it|_receive#21|Timeout|for m in sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5,
Issue-Key: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers|_drain#39|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 3db4d56b02e21f2a4281b49c781769e9a83f4fad

# events: 2 net-new causes — a custom-bus rule's SQS/dead-letter delivery is never observable, so neither the DISABLED-rule dead-letter withholding nor the PutEvents 11-entry whole-request refusal reaches a verdict

> _Note: this issue was identified against the code base at commit `3db4d56b02e21f2a4281b49c781769e9a83f4fad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Two net-new root causes, grounded in the two failing checks the suite lists under `events` that are neither ALREADY FILED nor OWNED. Both must be closed before this issue is submitted; each names its own failing check so each can be re-replayed separately.

Cause 1 of 2: a custom event bus's ENABLED rule with an SQS target plus DeadLetterConfig produces no observable delivery and no observable dead letter, so the DISABLED-then-ENABLED dead-letter contract cannot be graded at all. Failing check: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it.
AWS: Smithy (eventbridge): PutTargets.Targets accepts per-target RetryPolicy and DeadLetterConfig; PutEvents.Entries is @length min 1 max 10 and PutEvents answers FailedEntryCount 0 with one EventId per accepted entry. The dead-letter message attributes are not modeled, so their values are recorded behavior only: while a rule is DISABLED, a matching event is evaluated by nothing — it is neither delivered nor dead-lettered, ever, not even late; once EnableRule returns and DescribeRule reports State 'ENABLED', the next matching event whose target send is refused for lack of a queue resource policy is dead-lettered exactly once with ERROR_CODE = the fixed token for the refusal class the target service returned, ERROR_MESSAGE = that service's refusal sentence (fixed text, with the target queue ARN taken from the target and the per-call request id substituted), and RULE_ARN = the rule's ARN from state. Only the request id and the generated bus/rule/queue names are per-run values; the tokens, the status code and the error code in that sentence are fixed text.
Emulator: the check reaches no verdict — the suite cuts it at the harness 60 s cap ("Check timed out after 60.0s") with no assertion result for either the disabled arm (DLQ must stay empty) or the enabled arm (one dead letter with ERROR_CODE/ERROR_MESSAGE/RULE_ARN). Hypothesis: the same custom-bus delivery path that other `events`-family checks in this run exercise makes no observable progress, so every receive window on the custom bus's target and on its dead-letter queue is spent empty; the scenario's own first phase is a 60 s silent drain, so this timeout proves only that nothing arrives, not which of the two arms is wrong. Related open issues record the same no-progress symptom in neighbouring services and do NOT cover this check: 20261125T162859Z-events-sqs-custom-bus-fan-out-to-four.md (custom-bus fan-out makes no observable progress) and 20261125T024526Z-events-sqs-the-empty-string-operand-pattern-delivery.md; 20261123T070333Z-events-3-net-new-causes-an-unauthorized-sqs.md covers the CLASSIFICATION of an unauthorized SQS target's dead letter, not the disabled/enabled withholding contract asserted here.
Why: a client cannot rely on DisableRule actually stopping evaluation (an event sent while disabled must never surface later), nor on a dead letter appearing at all for a target that cannot be written to. With no delivery and no dead letter, an EventBridge consumer on a custom bus observes silence that is indistinguishable from a correctly withheld event, so the commonest misconfiguration (target queue policy missing) is undiagnosable.
Fix: the EventBridge rule-evaluation and target-delivery path for a non-default event bus — rule State gating at evaluation time, SQS target send, and the dead-letter write that fills ERROR_CODE / ERROR_MESSAGE / RULE_ARN on a refused send.
Traces: events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it.

Cause 2 of 2: a PutEvents request of 11 entries is not shown to be refused whole, and the positive control (exactly 10 entries delivered to the rule's SQS target on a custom bus) never becomes observable, so the entry-count limit is ungraded. Failing check: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers.
AWS: Smithy (eventbridge): PutEvents.Entries is required with @length {min: 1, max: 10}; the only modeled error is InternalException (500), so the over-limit refusal is the protocol's constraint failure and is recorded behavior only: HTTP 400 with error code 'ValidationException' and the message `1 validation error detected: Value '[<rendered entries>]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10`. Every word of that sentence is fixed text except the bracketed rendering of the submitted entries, which echoes each rejected entry as `PutEventsRequestEntry(time=null, source=<Source>, resources=null, detailType=<DetailType>, eventGroupId=null, detail=<Detail>, eventBusName=<EventBusName>, traceHeader=null, kmsKeyIdentifier=null, internalMetadata=null)` — the member names and `null` markers are fixed, the Source/DetailType/Detail/EventBusName values come from the request. The refusal is whole-request: no entry is published, so not one of the 11 ever reaches the rule's target, then or later. A request of exactly 10 entries is fully accepted (FailedEntryCount 0, 10 distinct non-empty EventIds) and all 10 are delivered to the target.
Emulator: the check reaches no verdict — "Check timed out after 60.0s". The first thing the check waits on is a liveness publish that must round-trip from the custom bus through the rule to its SQS target before the graded request is sent, so the budget is consumed before the 11-entry refusal is even attempted. Hypothesis (same path as Cause 1): PutEvents on a custom event bus never routes a matching event to an SQS target, so the liveness loop never terminates; whether the 11-entry ValidationException wording is also wrong is unknown because the assertion is never reached.
Why: a client batching events cannot tell an over-limit batch from an accepted one — if the limit is not enforced whole, some entries publish and the client double-publishes on retry; if the message wording diverges, SDK/retry logic keying on ValidationException and on the 'at entries failed to satisfy constraint' text misclassifies the refusal. And with custom-bus→SQS routing unobservable, the accepted 10-entry batch is indistinguishable from a dropped one.
Fix: PutEvents input validation (Entries length enforced before any entry is published, with the ValidationException serializer producing the constraint message above) and the custom-bus rule→SQS target delivery path named in Cause 1.
Traces: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers.

## Evidence
Cause 1 — events.disabled-rule-with-a-dead-letter-configured-target-drops-the-event-instead-of-dead-lettering-it
  Suite verdict: [mismatch] — "Check timed out after 60.0s" (no assertion result reported).
  Scenario: CreateEventBus (custom bus) → PutRule(EventPattern {"source":[<unique source>]}) → CreateQueue 'bare' (no policy) and 'dlq' (policy allowing events.amazonaws.com sqs:SendMessage, Condition ArnEquals aws:SourceArn = rule ARN) → PutTargets(Id 'bare', Arn <bare queue ARN>, RetryPolicy {MaximumRetryAttempts:185, MaximumEventAgeInSeconds:86400}, DeadLetterConfig.Arn <dlq ARN>) → DisableRule → PutEvents → drain DLQ → EnableRule → PutEvents → ReceiveMessage(MessageAttributeNames=['All']).
  Expected: PutTargets FailedEntryCount 0; DescribeRule State 'DISABLED' then 'ENABLED'; DLQ empty for the whole disabled window and the disabled-period event never arriving afterwards; after enabling, exactly one dead letter for the next matching event carrying ERROR_CODE (fixed token for the refusal class), ERROR_MESSAGE (the target service's refusal sentence — fixed text with the bare queue ARN from the target and the per-call request id, which is a per-run value) and RULE_ARN = the rule's ARN.
  Actual: no graded outcome; nothing observed on the dead-letter queue within the run budget.

Cause 2 — events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers
  Suite verdict: [mismatch] — "Check timed out after 60.0s" (no assertion result reported).
  Scenario: CreateEventBus (custom bus) → PutRule(State ENABLED, pattern on a unique source) → CreateQueue + SetQueueAttributes Policy allowing events.amazonaws.com sqs:SendMessage with Condition ArnEquals aws:SourceArn = rule ARN → PutTargets(one SQS target) → republish a liveness entry until it reaches the queue → PutEvents with 11 entries → PutEvents with 10 entries.
  Expected (11 entries): ClientError, HTTP 400, Error.Code 'ValidationException', Error.Message "1 validation error detected: Value '[PutEventsRequestEntry(time=null, source=<Source>, resources=null, detailType=<DetailType>, eventGroupId=null, detail=<Detail>, eventBusName=<EventBusName>, traceHeader=null, kmsKeyIdentifier=null, internalMetadata=null), ...x11]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10" — fixed text except the echoed per-entry values from the request; and none of the 11 markers ever appearing on the target queue, including in a later hold window.
  Expected (10 entries): FailedEntryCount 0, 10 entries each with a distinct non-empty EventId, and all 10 markers arriving on the target queue.
  Actual: the run is cut at the harness cap while waiting for the liveness event to traverse the custom bus to the SQS target; neither the refusal nor the 10-entry control is graded.
  Smithy (eventbridge) PutEvents: input Entries -> PutEventsRequestEntryList, required, length {min:1,max:10}; errors: InternalException (500) only; output FailedEntryCount (default 0) + Entries.

## Steps to Reproduce
Cause 1:
1. CreateEventBus with a generated name. PutRule on that bus with EventPattern {"source": ["<unique-source>"]} and record RuleArn.
2. CreateQueue twice: 'bare' (leave it with no resource policy) and 'dlq'; SetQueueAttributes on the DLQ with a Policy allowing Principal {"Service":"events.amazonaws.com"} sqs:SendMessage on the DLQ ARN, Condition ArnEquals aws:SourceArn = the rule ARN.
3. PutTargets(Rule, EventBusName, Targets=[{Id:'bare', Arn:<bare queue ARN>, RetryPolicy:{MaximumRetryAttempts:185, MaximumEventAgeInSeconds:86400}, DeadLetterConfig:{Arn:<dlq ARN>}}]). Assert FailedEntryCount 0.
4. DisableRule; DescribeRule must report State 'DISABLED'. PutEvents one matching entry and poll the DLQ for a silence window.
   Expected: the DLQ stays empty.
5. EnableRule; DescribeRule must report State 'ENABLED'. PutEvents another matching entry and ReceiveMessage the DLQ with MessageAttributeNames=['All'].
   Expected: exactly one dead letter for this second event, with ERROR_CODE/ERROR_MESSAGE describing the target queue's send refusal and RULE_ARN = the rule ARN; and the event published during step 4 never appears, then or later.
   Emulator: nothing is ever received on the dead-letter queue; the scenario reaches no verdict inside the run budget.

Cause 2:
1. CreateEventBus; PutRule(State 'ENABLED') with EventPattern {"source": ["<unique-source>"]}; CreateQueue and SetQueueAttributes with a Policy allowing events.amazonaws.com sqs:SendMessage on the queue ARN, Condition ArnEquals aws:SourceArn = the rule ARN; PutTargets with that queue as the only target (FailedEntryCount must be 0).
2. PutEvents one entry on that bus and ReceiveMessage the queue until it arrives (routing liveness).
   Emulator: this never arrives, which alone consumes the whole run budget.
3. PutEvents with 11 entries, each {EventBusName, Source: <unique-source>, DetailType, Detail}.
   Expected: refused whole — HTTP 400, ValidationException, message "1 validation error detected: Value '[<each of the 11 entries rendered as PutEventsRequestEntry(...)>]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10"; no entry published.
4. PutEvents with exactly 10 such entries.
   Expected: FailedEntryCount 0, 10 distinct non-empty EventIds, all 10 delivered to the queue, and none of the 11 markers from step 3 ever arriving (including during a later hold window).
