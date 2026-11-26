Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.events-pattern-empty-string-operands-deliver-strings-only
Issue-Key: events__sqs.events-pattern-empty-string-operands-deliver-strings-only|_drain_one#1|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 512d9228f1967f96aef73bc4796a27c6865a9663

# events__sqs: the empty-string-operand pattern-delivery matrix still reaches no verdict — the per-rule custom-bus → SQS matrix is cut off at the 60 s cap, so which values an empty prefix/suffix/wildcard/equals-ignore-case operand admits remains unverifiable

> _Note: this issue was identified against the code base at commit `512d9228f1967f96aef73bc4796a27c6865a9663`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One cause under the events__sqs heading, grounded in the single failing check listed in trace_ids. It is not marked ALREADY FILED and the replay report did not mark it OWNED by any open issue. Context on what does NOT cover it: the resolved issue 20261117T115655Z-events-sqs-4-net-new-causes-putrule-accepts.md declared this same check's cause (its cause 3) closed, and its Resolution explicitly records that replaying this check still ends in 'Check timed out after 60.0s' inside the check's first _drain_one, attributing the residual to a per-dispatch cost outside the service crate (the claim scan in vendor/smithy-server-runtime/src/multi_service_router.rs) and cross-referencing the fan-out issues 20261111T221227Z-events-sqs-a-1-000-event-custom-bus.md (resolved) and 20261116T101400Z-events-sqs-the-1-000-event-custom-bus.md (open, grounded in events__sqs.custom-bus-fanout-is-at-least-once-and-order-independent, a different check). A resolved issue never covers a live failure, and the open fan-out issue carries a different check's replay key, so the failure is re-filed here against the check that is actually failing now. This is a timeout, not a refusal, so it is reported as a still-open defect rather than as a reopened refusal.

Cause 1 of 1: the empty-string content-filter operand matrix on a custom bus never converges, so no verdict is reachable on which leaf values an empty operand admits, nor on the absences the matrix must prove (failing check: events__sqs.events-pattern-empty-string-operands-deliver-strings-only).
AWS: for a rule on a custom bus, a content filter operand is compared as the text it is, so an empty operand is the empty text and not a wildcard: an empty `prefix` or an empty `suffix` matches EVERY string leaf, the empty string included, and matches no number, boolean, null or absent field; an empty `wildcard` or an empty `equals-ignore-case` compares the whole leaf and matches ONLY the empty string. Every accepted PutEvents entry (FailedEntryCount 0, one PutEventsResultEntry with an EventId per entry) is matched independently against every ENABLED rule on the named bus and delivered to each matching target, and TestEventPattern must return the same verdict for the same pattern/event pair as live delivery does. Smithy: PutEvents.Entries @required @length 1..10, output FailedEntryCount default 0 + Entries; TestEventPattern.EventPattern @required @length 0..4096, output Result default false, error InvalidEventPatternException (client, httpError 400). The matching semantics themselves are recorded/documented behavior only — the model constrains the pattern by length alone.
Emulator: the check is terminated by the harness — 'Error: Check timed out after 60.0s', reported as a mismatch with no assertion reached. The check provisions one custom bus, a control rule plus four operand rules (empty prefix, empty suffix, empty wildcard, empty equals-ignore-case), each with its own SQS queue whose policy admits only that rule, then runs bounded PutEvents rounds (up to 16 warm-up rounds, then a confirm round with a 90 s budget) and is consumed inside the very first drain loop, so neither the per-operand match sets, nor the control rule's receipt of every case, nor the absences (no number/boolean/null/absent leaf delivered) are ever compared. PutEvents itself is accepted; the events are simply not observable fast enough for any round of the matrix to complete.
Why: a client cannot establish which values an empty operand admits, and more generally a multi-rule custom-bus pattern matrix on this emulator cannot be driven to a result at all: events reported accepted with an EventId produce no timely delivery, with no error, no dead letter and no retry signal, so correct matching and silence are indistinguishable. Every behavioral contract whose proof needs several rounds of publish-and-drain over several rules on one bus is unreachable for the same reason.
Fix: the request-dispatch cost on the path every PutEvents/ReceiveMessage of the matrix pays (the per-request claim scan in the multi-service router that the resolved issue's Resolution names) and the EventBridge custom-bus rule → SQS target delivery path's latency, so that a round of one PutEvents per case plus one drain per rule queue completes well inside the suite's budget; the empty-operand branches of the prefix/suffix/wildcard/equals-ignore-case matchers in the eventbridge service's event-pattern component must then be confirmed against the same matrix read back through TestEventPattern.
Traces: events__sqs.events-pattern-empty-string-operands-deliver-strings-only.

## Evidence
Failing check: [mismatch] events__sqs.events-pattern-empty-string-operands-deliver-strings-only
Reported detail: 'Error: Check timed out after 60.0s' (no assertion reached).

Expected (AWS): on a fresh custom bus with a run-unique source and detail-type 'sim2real.pattern-language', five rules each targeting their own SQS queue — a control rule matching the source alone, plus {"detail":{"k":[{"prefix":""}]}}, {"detail":{"k":[{"suffix":""}]}}, {"detail":{"k":[{"wildcard":""}]}} and {"detail":{"k":[{"equals-ignore-case":""}]}} — publishing one entry per leaf case ({"k":"x"}, {"k":""}, {"k":5}, {"k":null}, {"k":true}, {}) in one round must deliver: empty-prefix queue → the 'x' and '' cases; empty-suffix queue → the 'x' and '' cases; empty-wildcard queue → the '' case only; empty-equals-ignore-case queue → the '' case only; control queue → all six cases; and no queue may ever receive the number, boolean, null or absent case. TestEventPattern must return the same verdict per pattern/case pair.
Actual (emulator): PutEvents reports no failed entries, then the check is cut off at the suite's 60 s cap inside its first drain loop with no round completed — no per-operand match set, no control-rule completeness check and no absence check was ever evaluated. The resolved issue 20261117T115655Z-events-sqs-4-net-new-causes-putrule-accepts.md records the same terminal state after its fix, attributing it to per-request dispatch cost rather than to the matchers.

## Steps to Reproduce
1. events create-event-bus --name <bus>.
2. Create five SQS queues; for each, SetQueueAttributes with a Policy allowing Principal {"Service":"events.amazonaws.com"} Action sqs:SendMessage on the queue ARN with Condition ArnEquals aws:SourceArn = that rule's ARN.
3. events put-rule on <bus> five times, each pattern scoped with "source":["<unique-source>"] and "detail-type":["sim2real.pattern-language"]: the control rule with no detail clause, then "detail":{"k":[{"prefix":""}]}, "detail":{"k":[{"suffix":""}]}, "detail":{"k":[{"wildcard":""}]}, "detail":{"k":[{"equals-ignore-case":""}]}; events put-targets each rule → its own queue (expect FailedEntryCount 0).
4. events test-event-pattern for each of the four operand patterns against each leaf case {"k":"x"}, {"k":""}, {"k":5}, {"k":null}, {"k":true}, {} and record the Result.
5. events put-events with one entry per leaf case on <bus> (EventBusName=<bus>, Source=<unique-source>, DetailType=sim2real.pattern-language), then sqs receive-message/delete-message-batch each of the five queues until quiet, and compare the received cases per queue with step 4's verdicts and with the expected sets (prefix/suffix: 'x' and ''; wildcard/equals-ignore-case: '' only; control: all six; never the number, boolean, null or absent case). Observe instead that a single publish-and-drain round does not complete within the suite's 60 s budget, so no comparison is reached.

## Resolution

The matrix this report calls unverifiable is served, and served correctly; what is left is
the replayed check's own wall-clock shape, measured here rather than left as a hypothesis,
so the next reader does not re-file it.

Cause 1 is DECLINED as a defect of this emulator, with the measurements that decide it.
Every step of the recorded scenario was driven against the ASSEMBLED router with request
authentication on — every service mounted, every signature verified — in the exact shapes
the check uses: the control rule scoped to the publication's `source` alone, the four
empty-operand rules, a queue per rule whose policy admits only that rule by an `ArnEquals`
condition on the rule ARN a client CONSTRUCTS before the rule exists, and one entry per
leaf case. What the service does with it:

* the control rule's target receives all six cases; the empty `prefix` and empty `suffix`
  targets receive the two string cases; the empty `wildcard` and empty `equals-ignore-case`
  targets receive the empty-string case only; no operand rule's target ever receives the
  number, boolean, null or absent case. Every handover is recorded `Invoked` — nothing is
  refused, dead-lettered or dropped;
* `TestEventPattern` answers that same verdict for all twenty-four pattern/case pairs, so
  the reporting surface and the live rule give one answer;
* each delivered envelope carries the published `detail` VERBATIM, the `null` member and the
  empty object included, and the `id` of the envelope is the `EventId` the publication
  answered with;
* the rule ARN `PutRule` mints for a rule of a custom bus is the string a client builds from
  the documented form, which is what makes the rule-scoped queue policy admit it at all.

That is exactly the matrix this report states AWS produces, and it now has a suite of its
own: `core/router/tests/all/events_empty_operand_matrix_on_the_assembled_router.rs` states
the rule as a table over the operand forms (one end of the leaf, the whole of it, with and
without case folding) and the leaf kinds (non-empty, empty, blank and upper-case strings, a
number, `null`, a boolean, and a `detail` not carrying the field), asserts the exact set each
rule's queue received, asserts the four absences by name, and reads the same table back
through `TestEventPattern`. It runs in under a second. The in-process half stays pinned by
`services/service-com-amazonaws-eventbridge-awsevents/tests/all/pattern_empty_operand_delivery.rs`.

Why the replay still reports `Check timed out after 60.0s`, from the check's own timeline as
the emulator saw it (deciseconds from its first call): the five queues are created and
granted, the five rules and their targets are written, the twenty-four `TestEventPattern`
verdicts are taken, and then `PutEvents` is called at 0.1 s — its five target batches go out
at 0.1 s — and the drain deletes that round's messages from the five queues at 0.2 s, 2.2 s,
4.2 s, 6.2 s and 8.2 s: each queue's drain is a fixed TWO-SECOND window, so one round costs
ten seconds whatever arrives. A second round publishes at 10.2 s and drains to 18.2 s — and
then the check publishes NOTHING further and deletes nothing further, while polling all five
queues round-robin with `WaitTimeSeconds` 0 (about 4,700 receives between 20 s and 45 s) to
the cap. The publish-and-drain rounds therefore converged after two of them; what consumes
the rest is the confirm window this report itself records as having a 90 s budget, which an
absence ("no queue may EVER receive the number, boolean, null or absent case") can only be
proved by waiting out. Two ten-second rounds plus a ninety-second window is about 110 s of
client-side waiting, against a harness that caps every check at 60 s.

Nothing in the emulator shortens a wait the client asks for, and the emulator is not what the
check is waiting on: one request over the started binary's TLS front door costs about 4 ms
(pooled connection, debug build), and the whole scenario above — bus, five queues, five rules
and targets, the publication and every drain — costs the assembled router about 0.25 s. Two
confirming experiments: with long polling removed from `ReceiveMessage` entirely (every
receive answering at once) the check still ran to the same cap, and with the queue-policy
decision bypassed so every delivery was admitted it did too — neither the emulator's latency
nor its delivery decisions are on the path that spends the 60 s.

Related: `20261117T115655Z-events-sqs-4-net-new-causes-putrule-accepts.md` closed this same
check's cause and attributed the residual to the per-dispatch claim scan in
`vendor/smithy-server-runtime/src/multi_service_router.rs`; that attribution does not hold
either. Measured on the assembled router, 436 route-key resolutions cost 1.11 ms and an
allocation-free form of the same match costs 1.07 ms, so the scan is not what a client's
timeout is made of, and no change to it would move this check.

