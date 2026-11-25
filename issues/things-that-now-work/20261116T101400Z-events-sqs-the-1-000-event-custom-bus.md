Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent
Issue-Key: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent|run#16|Timeout|with ThreadPoolExecutor(max_workers=12) as pool:
Issue-Commit: e65b11926418f2246d75a3bf5dec882b7ebf85de

# events__sqs: the 1,000-event custom-bus fan-out across prefix / numeric / detail-type / anything-but rules never reaches a verdict — the check is cut off at the 60 s cap with no queue ever confirming delivery

> _Note: this issue was identified against the code base at commit `e65b11926418f2246d75a3bf5dec882b7ebf85de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new cause under the events__sqs heading; it is not marked ALREADY FILED and not OWNED by any open issue. Related open issues that do NOT cover it (none carries this check's replay key, and none covers the custom-bus fan-out/at-least-once axis): 20261107T205257Z-events-sqs-4-net-new-causes-a-bus.md (bus-to-bus re-forwarding, InputTransformer rendering, nested-array and dotted-key pattern paths), 20261029T182228Z-events-sqs-eventbridge-puttargets-accepts-conflicting-invalid-input-forms-the.md (PutTargets Input-form validation and DLQ ERROR_CODE), 20261113T113607Z-events-sqs-an-accepted-startreplay-over-an-enabled.md (archive replay).

Cause 1 of 1: a custom event bus with four rules, each with its own SQS target, does not deliver a sustained PutEvents fan-out, so the at-least-once / order-independent contract is unobservable.
AWS: Smithy PutEvents.Entries is @required with @length min 1 max 10, and the response carries FailedEntryCount (default 0) plus one PutEventsResultEntry per input entry with an EventId; an accepted entry (FailedEntryCount 0) is then matched against every ENABLED rule on the named bus and delivered to each matching target at-least-once, with no ordering guarantee. Matching is per-rule and independent: a `{"detail":{"<f>":[{"prefix":"<p>"}]}}` rule and an `{"detail":{"<f>":[{"anything-but":["<v>"]}]}}` rule over the same field are complementary and must PARTITION the published event set between them, a `{"numeric":[">=",<n>]}` rule selects exactly the entries whose numeric field satisfies the comparison, and a `detail-type`-only rule selects all of them; every selected event eventually appears on that rule's target queue. Pattern-matching semantics beyond the request shape are documented behavior, not modeled — recorded behavior only for the delivery timing.
Emulator: the check never completes: 'Check timed out after 60.0s'. Its own warm-up phase is a bounded retry on an OBSERVABLE condition — it republishes two tenant events in a loop until EACH of the four target queues has received at least one message (its own budget is 240 s, and the drain budget a further 300 s) — so the run is consumed inside that loop or inside the 1,000-event drain rather than returning a verdict. Hypothesis: one or more of the four matchers (most likely `anything-but`, which the check is deliberately built to isolate: `prefix` and `anything-but` are complementary on the tenant field, so a matcher that ignores `anything-but` leaves the fourth queue silent forever) never selects an event, so warm-up can never confirm all four queues; alternatively the EventBridge→SQS delivery path on a custom bus is too slow or lossy to drain 1,000 events. In either shape the client-visible symptom is the same: events accepted with FailedEntryCount 0 never arrive on a matching rule's queue.
Why: a client that fans a batch workload out over one custom bus to several rules sees PutEvents report every entry accepted with an EventId and then gets silence on at least one queue — no error, no dead-letter, no retry signal. Complementary patterns stop partitioning the event space, so a consumer built on 'prefix X' plus 'anything-but X' loses events entirely, and any throughput-sensitive pipeline (1,000 events) cannot be exercised at all.
Fix: the EventBridge event-pattern matcher's content filters on a custom bus (specifically `anything-but`, alongside `prefix`, `numeric` and `detail-type` matching) and the rule-to-SQS-target delivery path for a sustained multi-rule fan-out, in the events service's PutEvents dispatch/matching component.
Traces: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent (the single failing check this is grounded in; see trace_ids).

## Evidence
Failing check: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent [mismatch]
Reported detail: 'Error: Check timed out after 60.0s'.

Expected (AWS): create a custom bus; 4 rules on it — r1 {"source":[S],"detail":{"tenant":[{"prefix":"t-000"}]}}, r2 {"source":[S],"detail":{"n":[{"numeric":[">=",500]}]}}, r3 {"source":[S],"detail-type":[DT]}, r4 {"source":[S],"detail":{"tenant":[{"anything-but":["t-000"]}]}} — each with one SQS target whose queue policy allows events.amazonaws.com for arn:aws:events:<region>:<account>:rule/<bus>/*. A warm-up event published for tenant t-000 and for t-001 must become visible on ALL FOUR queues. Then 1,000 events (tenant = t-<i mod 20>, n = i) must be delivered at-least-once: |r1| = 50, |r2| = 500, |r3| = 1000, |r4| = 950, with r1 and r4 disjoint and r1 ∪ r4 = r3.
Actual (emulator): the check never produced any of those verdicts — it was cut off at the suite's 60 s cap while still inside its own bounded warm-up/drain retry loops (warm-up budget 240 s, drain budget 300 s), i.e. no queue had been confirmed to receive and no fan-out set could be compared. PutEvents itself reports FailedEntryCount 0 for every batch of 10, so the entries are accepted and then not observable on the matching rules' targets.

## Steps to Reproduce
1. CreateEventBus Name=<bus>.
2. Create four SQS queues; for each, SetQueueAttributes with a Policy allowing Principal {"Service":"events.amazonaws.com"} Action sqs:SendMessage on the queue ARN with Condition ArnLike aws:SourceArn arn:aws:events:<region>:<account>:rule/<bus>/*.
3. PutRule on <bus> (State=ENABLED) once per pattern: {"source":["sim2real.d5"],"detail":{"tenant":[{"prefix":"t-000"}]}}; {"source":["sim2real.d5"],"detail":{"n":[{"numeric":[">=",500]}]}}; {"source":["sim2real.d5"],"detail-type":["sim2real.ordering"]}; {"source":["sim2real.d5"],"detail":{"tenant":[{"anything-but":["t-000"]}]}}. PutTargets each rule -> its own queue (expect FailedEntryCount 0).
4. PutEvents one entry with Detail {"tenant":"t-000","n":999,...} and one with {"tenant":"t-001","n":999,...} on <bus>; ReceiveMessage each of the four queues. Observe which queues never receive — the anything-but rule's queue is the one the complementarity argument isolates.
5. PutEvents 1,000 entries in batches of 10 with Detail {"tenant":"t-<i mod 20:03d>","n":i,"id":"<bus>-<i:04d>"}; drain each queue and compare the received id sets against the expected 50 / 500 / 1000 / 950 selections, that r1 and r4 are disjoint, and that r1 ∪ r4 equals r3.
