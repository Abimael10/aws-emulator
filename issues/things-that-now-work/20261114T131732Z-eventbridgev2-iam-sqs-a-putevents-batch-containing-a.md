Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2__iam__sqs.put-events-reserved-aws-source-refused-and-never-delivered
Issue-Key: eventbridgev2__iam__sqs.put-events-reserved-aws-source-refused-and-never-delivered|_collect#18|TimeoutError|raise TimeoutError(f"the run budget ended before {what}; not graded")
Issue-Commit: e3f664225c71d6661c778bedbd316f93d7ff916d

# eventbridgev2__iam__sqs: a PutEvents batch containing a refused aws.-prefixed entry answers PUBLISHED for its valid sibling entry but never delivers it to the subscriber

> _Note: this issue was identified against the code base at commit `e3f664225c71d6661c778bedbd316f93d7ff916d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause in the EventBridge v2 PutEvents ingestion path, surviving on a check whose earlier per-entry wording divergence is no longer the failure.

Related open issue that does NOT cover this failure mode: 20261113T094431Z-eventbridgev2-iam-sqs-9-net-new-causes-putevents.md. Its Cause 1 covers the per-entry ErrorCode/ErrorMessage wording of the reserved-`aws.` refusal, and its replay key for this check is an AssertionError on that per-entry assertion. In this replay that assertion PASSES — the refusal is now coded and worded as recorded — and the check fails later, on the delivery of the OTHER entry of the same batch. That is a different cause in a different component (ingestion/dispatch, not the validation message), so it is filed here rather than folded into that issue.

=== Cause 1 of 1: a well-formed entry published in the same batch as a refused entry is reported PUBLISHED but never dispatched to the bus's subscribers ===
AWS: Smithy — PutEvents takes Entries (PutEventsRequestEntryList, 1..100) and returns FailedEntryCount plus Entries (PutEventsResultEntryList); PutEventsResultEntry's doc states "Result entry for a published event. The position of each entry in the response list aligns 1:1 with the corresponding request entry." PutEventsRequestEntry.Source's doc states "The source of the event. The `aws.` value prefix is service-reserved and cannot be used as a value." Per-entry refusal is therefore reported inside a 200 response, entry-by-entry: the refused entry carries ErrorCode/ErrorMessage and no EventId and no SuccessCode, and FailedEntryCount counts only it. Entry refusal is per entry, NOT per batch: a sibling entry whose result carries SuccessCode "PUBLISHED" has been accepted onto the bus and is delivered to every matching subscriber exactly as if it had been published alone. Recorded behavior: with a mixed batch of one `aws.`-prefixed entry and one well-formed entry, FailedEntryCount is 1, the refused entry carries ErrorCode "ValidationError" and ErrorMessage "Source must not start with the reserved prefix aws." (whole message fixed text, no variable part), and the well-formed entry carries SuccessCode "PUBLISHED" and is the ONE and only event that reaches the subscriber's target queue.
Emulator: the response half is now right — FailedEntryCount 1, the refused entry correctly carries no EventId/SuccessCode, and the sibling entry carries SuccessCode "PUBLISHED" (the check's assertions on all of that pass). The event the emulator reported as PUBLISHED is then never dispatched: the poll of the subscriber's target queue never sees it and ends with "TimeoutError: the run budget ended before the sibling delivery; not graded". The same subscriber (plain InvokeConfiguration {RoleArn, TargetArn: <sqs queue arn>, SqsParameters: {}}) had already received warm-up events published as single-entry batches moments earlier in the same run, so the bus, the subscriber and the plain SQS delivery path are all live; what fails is specifically the batch in which one entry was refused. Hypothesis: the per-entry validation failure aborts dispatch for the whole batch while the per-entry results are still built optimistically, so SuccessCode "PUBLISHED" is reported for an entry that was never enqueued.
Why: PutEvents' per-entry contract is what lets a producer send a mixed batch, read FailedEntryCount, and retry only the entries that carry an ErrorCode. Against this emulator a producer that does exactly that silently loses every valid entry that shared a batch with one bad entry: it is told SuccessCode "PUBLISHED" (and so does not retry), no error is raised anywhere, and the event never reaches any subscriber. That is unacknowledged data loss, the worst class for an event bus.
Fix: the eventbridgev2 PutEvents handler's entry loop — validate and dispatch per entry, so a refused entry only populates its own result entry and every other entry is still enqueued onto the bus and handed to the subscriber dispatcher; SuccessCode "PUBLISHED" must only be written for an entry actually accepted onto the bus.
Traces: eventbridgev2__iam__sqs.put-events-reserved-aws-source-refused-and-never-delivered

## Evidence
Check eventbridgev2__iam__sqs.put-events-reserved-aws-source-refused-and-never-delivered — verdict [error].

Failure point (the per-entry response assertions above it all passed):
  File "check_eventbridgev2__iam__sqs_put_events_reserved_aws_source_refused_and_never_delivered.py", line 285, in run
    bodies = _collect(ctx, sqs, publish_url, lambda b: kept in map(marker_of, b), "the sibling delivery")
  File "...", line 190, in _collect
    raise TimeoutError(f"the run budget ended before {what}; not graded")
  TimeoutError: the run budget ended before the sibling delivery; not graded

Expected (AWS): PutEvents({Entries: [{Source: 'aws.sim2real', ...}, {Source: 'sim2real.ebv2.check', ...}]}) -> 200 with FailedEntryCount 1; Entries[0] = {ErrorCode: 'ValidationError', ErrorMessage: 'Source must not start with the reserved prefix aws.'} with no EventId and no SuccessCode; Entries[1] = {SuccessCode: 'PUBLISHED', EventId: <generated>} AND exactly one message on the subscriber's target queue, whose detail carries the second entry's marker.

Actual (emulator): the 200 response matches (FailedEntryCount 1, refused entry carries neither EventId nor SuccessCode, sibling entry SuccessCode 'PUBLISHED'), but the target queue receives nothing for that batch — the delivery poll never observes the sibling marker. Control in the same run: warm-up events published one-per-batch to the same bus with the same Source did reach the same queue (the check's warm-up loop completed before this point), so delivery is working for batches with no refused entry.

Note on the grading: the poll ended because the runner's remaining budget was exhausted, so the harness labels it "not graded" rather than asserting. The emulator-caused part is unambiguous all the same: the event the emulator acknowledged as PUBLISHED had still not been delivered when the poll ended, while plain single-entry publishes to the same subscriber were being delivered in the same run.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateEventBus (ClientToken set) and wait for State ACTIVE.
3. Create an SQS queue, allow sqs:SendMessage to Principal Service events.amazonaws.com on it, and create an IAM role EventBridge can assume with sqs:SendMessage on that queue ARN.
4. CreateSubscriber on the bus with InvokeConfiguration {RoleArn: <role arn>, TargetArn: <queue arn>, SqsParameters: {}}.
5. Warm up: PutEvents single-entry batches {Source: 'sim2real.ebv2.check', DetailType: 'sim2real-warm-up', Detail: '{"marker":"<m>"}'} every 5 s and receive on the queue until one arrives — this confirms the plain delivery path works.
6. PutEvents one request with TWO entries: entry 0 {Source: 'aws.sim2real', DetailType: 'sim2real-check', Detail: '{"marker":"m-refused"}'}, entry 1 {Source: 'sim2real.ebv2.check', DetailType: 'sim2real-check', Detail: '{"marker":"m-kept"}'}.
7. Observe the response: FailedEntryCount 1, Entries[1].SuccessCode 'PUBLISHED'.
8. Poll ReceiveMessage on the queue for the event whose detail.marker is 'm-kept'. It never arrives, although step 5's publishes to the same subscriber did. Expected: exactly that one message and nothing for 'm-refused'.
