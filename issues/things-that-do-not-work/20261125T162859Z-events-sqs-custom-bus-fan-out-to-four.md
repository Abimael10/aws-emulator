Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent
Issue-Key: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent|run#16|Timeout|with ThreadPoolExecutor(max_workers=12) as pool:
Issue-Commit: fbb1a06f1c20de0c01a698c058987963365385aa

# events__sqs: custom-bus fan-out to four pattern rules makes no observable progress — the at-least-once/order-independent fan-out check reaches no verdict

> _Note: this issue was identified against the code base at commit `fbb1a06f1c20de0c01a698c058987963365385aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One root cause.

Cause 1 of 1: events published to a custom event bus are not fanned out to the SQS targets of four concurrently-configured rules within any usable time, so the check cannot even pass its readiness barrier (which requires every one of the four queues to receive one warm-up event) and is killed with no verdict. Failing check: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent.

AWS: Smithy (services/service-com-amazonaws-eventbridge-awsevents) PutEvents takes Entries (@length 1..10) and answers FailedEntryCount plus one PutEventsResultEntry per entry with an EventId; PutTargets attaches the SQS target. EventBridge's documented delivery semantics for an accepted entry are at-least-once to every rule on that bus whose EventPattern matches, with no ordering guarantee — so for N published events each rule's SQS target must receive at least one copy of every event its pattern selects and nothing it does not select. The four pattern families the check exercises over the same event space are prefix (detail.tenant prefix "t-000"), numeric (detail.n numeric >= 500), detail-type equality, and anything-but (detail.tenant anything-but "t-000"); prefix and anything-but are complementary by construction, so their delivered sets must partition exactly what the match-everything rule receives. Delivery is expected promptly after an accepted PutEvents (recorded behaviour: every accepted batch reports FailedEntryCount 0 and its events arrive on the targets while the check's drain is running).

Emulator: every PutEvents batch is accepted, but the fan-out never produces enough deliveries for the check to proceed; the run ends "Error: Check timed out after 60.0s" with no assertion reached. Because the check's warm-up barrier only advances when each of the four rules' queues has yielded at least one message, at least one of those four pattern families never delivers to its SQS target (the anything-but rule is the prime suspect, since an engine that silently ignores anything-but would deliver nothing matching the complementary pattern), and/or the custom-bus fan-out path is far too slow to drain a 1,000-event publication. Nothing the emulator returns distinguishes "accepted and dropped" from "accepted and not yet delivered": FailedEntryCount stays 0 either way.

Why: a client that publishes to a custom bus and relies on EventBridge's at-least-once fan-out sees accepted PutEvents responses (FailedEntryCount 0, an EventId per entry) while matched events never reach one or more of its queues. Silent, unreported loss of matched events is indistinguishable from an upstream bug, and pattern semantics that are not applied (notably anything-but) make a rule either over- or under-deliver with no error anywhere.

Fix: the EventBridge custom-bus delivery path — the rule-matching engine's pattern operators (prefix, numeric, anything-but) and the per-target SQS delivery worker that must deliver every matched event at least once for a batch PutEvents has accepted.

No open issue names this check: the other open events__sqs items (e.g. 20261124T132051Z-events-sqs-5-causes-puttargets-refusal-messages-for.md and 20261125T024526Z-events-sqs-the-empty-string-operand-pattern-delivery.md) cover PutTargets refusal wording and the empty-string operand pattern, not custom-bus fan-out across these four operators.

Traces: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent.

## Evidence
Failing check: events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent — verdict mismatch, "Error: Check timed out after 60.0s". No assertion in the check was reached, so the failure is an absence of delivery, not a wrong value.

Expected (AWS): on a custom bus with four ENABLED rules, each with one SQS target and the patterns {source + detail.tenant prefix "t-000"}, {source + detail.n numeric >= 500}, {detail-type equality}, {source + detail.tenant anything-but "t-000"}, publishing 1,000 events in batches of 10 (each batch accepted with FailedEntryCount 0) results in: every queue receiving at least one copy of each event its pattern selects and none it does not; the detail-type rule receiving all 1,000 distinct events; and the prefix and anything-but rules partitioning exactly that set (disjoint, union equal).

Actual (emulator): all PutEvents batches are accepted (FailedEntryCount 0 with an EventId per entry) but the queues do not fill — the check cannot get past its readiness barrier, which requires one warm-up event to arrive on each of the four queues, and is cut off at the harness's 60 s cap with no verdict.

The bus/rule/queue names, the 1,000-event workload size and the tenant strings are per-run values from the request; the requirement is at-least-once delivery of every matched event to each rule's target and correct application of the prefix, numeric and anything-but operators.

## Steps to Reproduce
1. CreateEventBus with a fresh name.
2. For each of the four patterns below, CreateQueue, set a queue policy allowing events.amazonaws.com to sqs:SendMessage, PutRule on that bus with State=ENABLED, and PutTargets with the queue ARN (FailedEntryCount must be 0):
   r1 {"source":[S],"detail":{"tenant":[{"prefix":"t-000"}]}}
   r2 {"source":[S],"detail":{"n":[{"numeric":[">=",500]}]}}
   r3 {"source":[S],"detail-type":[D]}
   r4 {"source":[S],"detail":{"tenant":[{"anything-but":["t-000"]}]}}
3. Publish a couple of warm-up events (one with tenant t-000, one with tenant t-001) and ReceiveMessage on each of the four queues. Expected: each queue yields at least one message. Observed: the barrier never completes.
4. Publish 1,000 events in batches of 10 with Detail {"tenant":"t-0NN","n":i,"id":...}; each PutEvents must report FailedEntryCount 0.
5. Drain each queue. Expected: r3 receives all 1,000 ids, r1 receives exactly the 50 t-000 ids, r2 the 500 ids with n >= 500, r4 the 950 non-t-000 ids, with r1 and r4 disjoint and their union equal to r3's set. Observed: no verdict — the check exhausts its budget with the queues not filling.
