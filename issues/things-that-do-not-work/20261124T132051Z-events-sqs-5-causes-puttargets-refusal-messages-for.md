Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.events-delivery-and-failure-unquoted-plain-text-template-rejected events__sqs.put-targets-upsert-with-invalid-input-rejected-and-live-target-keeps-delivering events__sqs.bus-to-bus-forwarding-happens-once-and-preserves-the-envelope events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent events__sqs.replay-redelivers-each-archived-event-exactly-once-and-never-the-out-of-window-one
Issue-Key: events__sqs.events-delivery-and-failure-unquoted-plain-text-template-rejected|run#14|AssertionError|assert message == expected, f"unexpected ValidationException message {message!r}"
Issue-Key: events__sqs.put-targets-upsert-with-invalid-input-rejected-and-live-target-keeps-delivering|run#11|AssertionError|assert message == (
Issue-Key: events__sqs.bus-to-bus-forwarding-happens-once-and-preserves-the-envelope|_drain#46|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=wait)
Issue-Key: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent|run#16|Timeout|observed = {rule: [i for f in fs for i in f.result()] for rule, fs in futures.items()}
Issue-Key: events__sqs.replay-redelivers-each-archived-event-exactly-once-and-never-the-out-of-window-one|_drain#44|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# events__sqs: 5 causes — PutTargets refusal messages for a non-JSON InputTemplate and a non-JSON Input are emulator-authored, and bus-to-bus forwarding, 1,000-event custom-bus fan-out and archive replay reach no verdict

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
Five net-new root causes in EventBridge (service heading events__sqs). Each must be closed before submitting.

Cause 1 of 5: PutTargets refuses an InputTransformer whose InputTemplate is not valid JSON with the wrong message.
AWS: the refusal is ValidationException (HTTP 400, awsJson1_1) and its message reports the parse failure with the parser's source location: fixed text "Invalid InputTemplate for target " + <TargetId> + " : [Source: (String)\"" + <template> + "\"; line: 1, column: 1]." — where <TargetId> is the Id from the request's Targets entry and <template> is the submitted InputTemplate with every <placeholder> declared in InputPathsMap read as null (so the bare sentence "order <id> by <who>" is reported as "order null by null"). Smithy (com-amazonaws-eventbridge-awsevents, PutTargets): modeled errors are ConcurrentModificationException, InternalException, LimitExceededException, ManagedRuleException, ResourceNotFoundException — ValidationException is not in the model, so the wording above is recorded behavior only, taken from the recorded refusal.
Emulator: ValidationException with the message 'InputTemplate for target text is not valid JSON.' — a wholly emulator-authored sentence; it drops the "Invalid InputTemplate for target" lead-in and the entire [Source: (String)"..."; line: 1, column: 1] parse-location clause. ("text" in the observed message is only this run's target Id; the Id is the template's variable part.)
Why: a client that classifies or surfaces the refusal by message — the common pattern for PutTargets, whose ValidationException is unmodeled and therefore undiscriminated by code alone — cannot tell which target or which placeholder expansion was rejected, and message assertions against AWS fail.
Fix: the events PutTargets input-transformer validation and its ValidationException message builder.

Cause 2 of 5: PutTargets refuses a non-JSON `Input` on a target upsert with the wrong message.
AWS: ValidationException (HTTP 400) with fixed text "JSON syntax error in input for target " + <TargetId> + ": [Source: (String)\"" + <Input> + "\"; line: 1, column: 1]" (no trailing period), where <TargetId> is the Id from the request and <Input> is the submitted Input string verbatim. Smithy: as above, ValidationException is unmodeled for PutTargets, so this is recorded behavior only.
Emulator: ValidationException with 'Input for target t1 is not valid JSON.' — again emulator-authored, with no "JSON syntax error in input for target" lead-in and no source/line/column clause ("t1" is only this run's target Id).
Why: same as cause 1; additionally the two refusals (bad Input vs bad InputTemplate) are indistinguishable in the emulator's wording where AWS words them differently.
Fix: the events PutTargets Input validation and its ValidationException message builder.

Cause 3 of 5: bus-to-bus forwarding reaches no verdict — nothing the check waits for is observed.
AWS: a rule on bus A whose target Arn is another same-Region bus B (with a RoleArn granting events:PutEvents) forwards the event to B, where B's own SQS-target rule delivers the untouched envelope (same source, detail-type, detail, resources, account, region; no replay-name), and B does NOT forward it on to C — EventBridge forwards a given event between same-Region buses at most once. Recorded behavior only for the timings.
Emulator: the check is cut off at the harness cap (Error: Check timed out after 60.0s) before any of its awaited observables appears: the B->C liveness delivery into the bus-C queue, the A->B forwarded delivery into the bus-B queue, and the direct reference delivery on B. Nothing it polls for arrives, so neither envelope preservation nor the at-most-once hop limit is observable.
Why: a client that routes events between buses sees no delivery at all, and the hop-limit contract (which protects against forwarding loops) cannot be relied on.
Fix: the events rule-evaluation/target-dispatch path for a target whose Arn is an event bus (forwarded PutEvents, envelope passthrough, and the one-hop guard), and the SQS-target delivery it depends on.

Cause 4 of 5: the 1,000-event custom-bus fan-out across four matcher families reaches no verdict.
AWS: 1,000 PutEvents entries on a custom bus fan out at-least-once to four rules using prefix, numeric range, detail-type and anything-but matchers over the same event space; the match-everything rule receives all 1,000, and the prefix 't-000' and anything-but 't-000' rules partition exactly what it received. Recorded behavior only.
Emulator: the check never reaches a verdict — it is cut off at the harness cap (Error: Check timed out after 60.0s), beginning with the per-queue warm-up that waits for one observable arrival per queue. Fan-out at scale, and the prefix/anything-but partition, are therefore unverifiable.
Why: a consumer cannot establish that every published event reaches every matching rule, nor that complementary matchers do not double-deliver or drop.
Fix: the events bus fan-out/dispatch path and the pattern matchers for prefix, numeric and anything-but on a custom bus. NOTE: open issue 20261116T101400Z-events-sqs-the-1-000-event-custom-bus.md describes the same fan-out family but does not carry this check's replay key, so this cause is filed separately and should be closed together with it.

Cause 5 of 5: archive replay never redelivers the archived events, so exactly-once replay and window exclusion reach no verdict.
AWS: StartReplay over an ENABLED archive re-emits each archived event inside [EventStartTime, EventEndTime) exactly once to the matching rules, stamps the redelivered envelope with replay-name, and never re-emits an event archived outside the window. Recorded behavior only.
Emulator: the check is cut off at the harness cap (Error: Check timed out after 60.0s) with nothing redelivered, so neither the exactly-once property nor the out-of-window exclusion is observable.
Why: a client that uses archives for recovery gets a replay that is accepted but moves no events, and cannot distinguish "nothing matched the window" from "replay does nothing".
Fix: the events archive/replay engine (StartReplay re-emission into rule matching) and the replay-name stamping on redelivered envelopes. NOTE: open issues 20261027T000839Z-events-sqs-an-accepted-eventbridge-replay-still-re.md and 20261113T113607Z-events-sqs-an-accepted-startreplay-over-an-enabled.md report the same non-redelivery for other checks but do not carry this check's replay key.

Traces: the five failing check ids listed in trace_ids, one per cause in the order above.

## Evidence
Cause 1 — expected ValidationException message (AWS): 'Invalid InputTemplate for target text : [Source: (String)"order null by null"; line: 1, column: 1].' (fixed text: "Invalid InputTemplate for target ", " : [Source: (String)\"", "\"; line: 1, column: 1]."; values: target Id from the request, template with each InputPathsMap placeholder read as null). Actual (replay): AssertionError: unexpected ValidationException message 'InputTemplate for target text is not valid JSON.' — error Code ValidationException matched; only the message diverges.

Cause 2 — expected (AWS): 'JSON syntax error in input for target t1: [Source: (String)"sim2real not json"; line: 1, column: 1]' (fixed text: "JSON syntax error in input for target ", ": [Source: (String)\"", "\"; line: 1, column: 1]"; values: target Id and the submitted Input). Actual (replay): AssertionError: an upsert with a non-JSON Input was refused with 'Input for target t1 is not valid JSON.'

Cause 3 — Actual (replay): Error: Check timed out after 60.0s for events__sqs.bus-to-bus-forwarding-happens-once-and-preserves-the-envelope; the check's own budgets are 120s liveness / 90s delivery, and no forwarded or direct delivery is ever observed on either observing queue. Expected: the A->B forwarded envelope arrives on bus B's SQS target with identical source/detail-type/detail/resources/account/region and no replay-name, and bus C receives nothing for that event.

Cause 4 — Actual (replay): Error: Check timed out after 60.0s for events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent (N_EVENTS=1000, 4 rules, drain budget 300s); the warm-up never confirms an arrival per queue. Expected: the detail-type match-everything rule receives all 1,000 events and the prefix/anything-but rules partition exactly that set.

Cause 5 — Actual (replay): Error: Check timed out after 60.0s for events__sqs.replay-redelivers-each-archived-event-exactly-once-and-never-the-out-of-window-one; nothing is redelivered. Expected: each in-window archived event redelivered exactly once carrying replay-name, and the out-of-window event never redelivered.

## Steps to Reproduce
Cause 1: PutRule with pattern {"source":[<src>]}; PutTargets with Targets=[{Id:"text", Arn:<queue arn>, InputTransformer:{InputPathsMap:{"id":"$.detail.id","who":"$.detail.who"}, InputTemplate:"order <id> by <who>"}}]. Read Error.Code/Error.Message off the ClientError.
Cause 2: PutRule with an SQS target t1 carrying InputPath "$.detail"; then PutTargets again with Targets=[{Id:"t1", Arn:<same queue arn>, Input:"sim2real not json"}]. Read Error.Message.
Cause 3: create buses A, B, C; a role allowing events:PutEvents on B and C; rule on A targeting B's ARN with that RoleArn; rule on B targeting C's ARN with that RoleArn; rules on B and C targeting one SQS queue each (with an events.amazonaws.com queue policy). PutEvents on B and poll queue_c (liveness), then PutEvents once on A and poll queue_b.
Cause 4: create a custom bus; four rules with patterns {detail.tenant prefix "t-000"}, {detail.n numeric >= 500}, {detail-type ...}, {detail.tenant anything-but "t-000"}, each to its own queue; PutEvents 1,000 entries across 20 tenants and drain all four queues.
Cause 5: create an archive on a bus, PutEvents a set of events, then StartReplay with an EventStartTime/EventEndTime window that includes some and excludes one, and drain the rule's queue.
