Issue-Source: trace-sweep-divergence
Issue-Trace: sqs-dead-letter-queue-and-message-move
Issue-Step: task#15
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sqs-dead-letter-queue-and-message-move sqs-dead-letter-queue-configuration sqs-dead-letter-queue-redrive-configuration sqs-message-send-receive-delete sqs-queue-purge-control sqs-message-visibility-and-inflight sqs-queue-create-url-shape sqs-queue-permissions-and-tags sqs-queue-tags-and-permissions-management
Issue-Shape-Id: com.amazonaws.sqs#AmazonSQS
Issue-Key: sqs|ListDeadLetterSourceQueues|4xx->4xx|$.message
Issue-Key: sqs|SendMessage|4xx->4xx|$.message
Issue-Key: sqs|PurgeQueue|4xx->4xx|$.message
Issue-Key: sqs|ChangeMessageVisibility|4xx->4xx|$.message
Issue-Key: sqs|DeleteQueue|4xx->4xx|$.message
Issue-Key: sqs|AddPermission|4xx->4xx|$.message
Issue-Key: sqs|RemovePermission|4xx->4xx|$.message
Issue-Key: sqs|TagQueue|4xx->4xx|$.message
Issue-Key: sqs|UntagQueue|4xx->4xx|$.message
Issue-Commit: 51772985bc72cfea1a888e0d513791715cd162c8

# sqs: nine queue-resolving wire operations refuse with the legacy WSDL NonExistentQueue sentence instead of AWS's message

> _Note: this issue was identified against the code base at commit `51772985bc72cfea1a888e0d513791715cd162c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
This issue bundles 9 net-new wire divergences of ONE defect: the SQS QueueDoesNotExist refusal is rendered with AWS's retired query/WSDL-era sentence instead of the current one. HTTP status (400) and error code (AWS.SimpleQueueService.NonExistentQueue / __type com.amazonaws.sqs#QueueDoesNotExist) are correct everywhere; only the `message` member diverges, and it diverges identically on every one of the nine operations below, so this is one shared error constructor in the SQS queue-resolution path, not nine per-operation slips.

Shared AWS requirement for all nine causes:
AWS: Smithy (com.amazonaws.sqs) error shape `QueueDoesNotExist` — errorKind=client, httpError=400, awsQueryError code `AWS.SimpleQueueService.NonExistentQueue` (httpResponseCode 400), single member `message` targeting `ExceptionMessage`; doc: "Ensure that the QueueUrl is correct and that the queue has not been deleted." `QueueDoesNotExist` is a modeled error of every one of the nine operations in this bundle (confirmed via smithy_query on ListDeadLetterSourceQueues, SendMessage, PurgeQueue, ChangeMessageVisibility, DeleteQueue, AddPermission, RemovePermission, TagQueue, UntagQueue). The model does not carry the message string, so for the text this is recorded behavior only: every recorded response in this corpus answers HTTP 400, code `AWS.SimpleQueueService.NonExistentQueue`, message `The specified queue does not exist.` That entire sentence is FIXED text — this error class interpolates NO variable part at all: no queue name, no queue URL, no account id, no member name and no request id appears in it, which is why the recorded message is byte-identical across all nine operations and all nine fixtures.
Shared Emulator behavior: the emulator answers HTTP 400 with the right code but message `The specified queue does not exist for this wsdl version.` — the same fixed sentence plus the retired trailing clause ` for this wsdl version`.
Shared Why: an error message is part of this error class's contract. A client that asserts on, logs, or pattern-matches SQS's not-found sentence (negative-path suites, CloudFormation/CDK and Cloud Control flows that use native SQS as ground truth after a delete, EventBridge/Pipes/Lambda plumbing that embeds the SQS refusal in a dead-letter record or a Step Functions `cause`) sees a sentence AWS never produces and treats a correctly-absent queue as an unrecognized failure. It additionally signals, falsely, that the emulator is serving a legacy SQS API version.
Shared Fix: the SQS service's single shared QueueDoesNotExist/NonExistentQueue error construction — the one place the queue-resolution step of the SQS handlers builds this refusal — must emit the fixed sentence `The specified queue does not exist.` with no suffix, leaving HTTP 400 and code `AWS.SimpleQueueService.NonExistentQueue` unchanged. Every operation below then inherits the corrected text; no per-operation handler change should be needed, but each of the nine fixture steps listed must be re-replayed to confirm, because each is a distinct code path reaching that constructor.

Cause 1 of 9 — ListDeadLetterSourceQueues on an unresolvable queue URL. Emulator returns 400 with message `The specified queue does not exist for this wsdl version.`; AWS returns 400 with `The specified queue does not exist.` Fix: the queue-resolution validation of the ListDeadLetterSourceQueues handler, via the shared error constructor. Traces: fixture `sqs-dead-letter-queue-and-message-move` step `task#15` (also sqs-dead-letter-queue-configuration#task#10, sqs-dead-letter-queue-redrive-configuration#task#9).

Cause 2 of 9 — SendMessage on an unresolvable queue URL. Same wrong trailing clause on the same 400/NonExistentQueue refusal. Fix: SendMessage's queue-resolution validation, via the shared error constructor. Traces: fixture `sqs-message-send-receive-delete` step `task#7`.

Cause 3 of 9 — PurgeQueue on an unresolvable queue URL. Same divergence. Fix: PurgeQueue's queue-resolution validation. Traces: fixture `sqs-message-send-receive-delete` step `task#10` (also sqs-queue-purge-control#task#5).

Cause 4 of 9 — ChangeMessageVisibility on an unresolvable queue URL. Same divergence (note the queue must be resolved BEFORE the receipt handle is judged, so the refusal is QueueDoesNotExist and its message must be AWS's). Fix: ChangeMessageVisibility's queue-resolution validation. Traces: fixture `sqs-message-visibility-and-inflight` step `task#6`.

Cause 5 of 9 — DeleteQueue on an already-deleted / never-created queue URL. Same divergence. Fix: DeleteQueue's queue-resolution validation. Traces: fixture `sqs-queue-create-url-shape` step `task#5`.

Cause 6 of 9 — AddPermission on an unresolvable queue URL. Same divergence. Fix: AddPermission's queue-resolution validation. Traces: fixture `sqs-queue-permissions-and-tags` step `task#12` (also sqs-queue-tags-and-permissions-management#task#10).

Cause 7 of 9 — RemovePermission on an unresolvable queue URL. Same divergence. Fix: RemovePermission's queue-resolution validation. Traces: fixture `sqs-queue-permissions-and-tags` step `task#13` (also sqs-queue-tags-and-permissions-management#task#12).

Cause 8 of 9 — TagQueue on an unresolvable queue URL. Same divergence. Fix: TagQueue's queue-resolution validation. Traces: fixture `sqs-queue-permissions-and-tags` step `task#14` (also sqs-queue-tags-and-permissions-management#task#11).

Cause 9 of 9 — UntagQueue on an unresolvable queue URL. Same divergence. Fix: UntagQueue's queue-resolution validation. Traces: fixture `sqs-queue-permissions-and-tags` step `task#15`.

Note on existing open issues: 20261115T171209Z-sqs-the-queuedoesnotexist-refusal-carries-the-legacy-wsdl.md describes the same wording defect but is grounded ONLY in behavioral check ids from the simparity-behavioral-aws-v1 suite and carries no wire replay key; 20261115T165311Z-sqs-the-aws-simplequeueservice-nonexistentqueue-refusal-carries-the.md, 20261115T161642Z-cloudcontrol-sqs-the-queuedoesnotexist-refusal-message-is-the-legacy.md and the several cloudformation__*/lambda__sns__sqs/s3__sns__sqs/sns__sqs 'deleted queue carries the legacy WSDL wording' issues are each scoped to one caller's path. None of them carries any of the nine fixture#step bindings filed here (the replay report reports these nine clusters as unowned), which is why these nine wire steps are still failing and need their own replay pointer. The named operations here — ListDeadLetterSourceQueues, PurgeQueue, ChangeMessageVisibility, DeleteQueue, AddPermission, RemovePermission and UntagQueue — are not covered by any of those issues' evidence.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator: 283 steps, 236 passed, 47 mismatched, score 83.4%. Nine unowned divergence clusters, all verdict=mismatch, all service=sqs, all with the identical `detail`:
  $.message: value_mismatch (expected 'The specified queue does not exist.', got 'The specified queue does not exist for this wsdl version.')

Expected (AWS, every cause): HTTP 400; x-amzn-query-error 'AWS.SimpleQueueService.NonExistentQueue;Sender'; __type 'com.amazonaws.sqs#QueueDoesNotExist'; message 'The specified queue does not exist.' — wholly fixed text, no interpolated value.
Actual (emulator, every cause): HTTP 400; same query-error header and __type (both correct); message 'The specified queue does not exist for this wsdl version.' — the fixed sentence plus the retired ' for this wsdl version' clause.

Per-cause cluster evidence (divergence_key -> representative step, affected_count):
1. service=sqs|ListDeadLetterSourceQueues|status=400->400|keys=$.message -> sqs-dead-letter-queue-and-message-move#task#15, 3 affected
2. service=sqs|SendMessage|status=400->400|keys=$.message -> sqs-message-send-receive-delete#task#7, 1 affected
3. service=sqs|PurgeQueue|status=400->400|keys=$.message -> sqs-message-send-receive-delete#task#10, 2 affected
4. service=sqs|ChangeMessageVisibility|status=400->400|keys=$.message -> sqs-message-visibility-and-inflight#task#6, 1 affected
5. service=sqs|DeleteQueue|status=400->400|keys=$.message -> sqs-queue-create-url-shape#task#5, 1 affected
6. service=sqs|AddPermission|status=400->400|keys=$.message -> sqs-queue-permissions-and-tags#task#12, 2 affected
7. service=sqs|RemovePermission|status=400->400|keys=$.message -> sqs-queue-permissions-and-tags#task#13, 2 affected
8. service=sqs|TagQueue|status=400->400|keys=$.message -> sqs-queue-permissions-and-tags#task#14, 2 affected
9. service=sqs|UntagQueue|status=400->400|keys=$.message -> sqs-queue-permissions-and-tags#task#15, 1 affected

Model citation: smithy_query(shape=QueueDoesNotExist) on services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json -> httpError 400, awsQueryError code 'AWS.SimpleQueueService.NonExistentQueue', member `message` -> ExceptionMessage. smithy_query(operation=...) confirms QueueDoesNotExist is a modeled error of all nine operations.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). Nine sqs clusters mismatch on $.message only.
2. Or reproduce by hand against the emulator's SQS endpoint. Create a queue, delete it (or build a queue URL for a name that was never created), then call each queue-resolving operation on that URL:
     aws sqs send-message --queue-url <gone-url> --message-body x
     aws sqs purge-queue --queue-url <gone-url>
     aws sqs change-message-visibility --queue-url <gone-url> --receipt-handle h --visibility-timeout 30
     aws sqs delete-queue --queue-url <gone-url>
     aws sqs list-dead-letter-source-queues --queue-url <gone-url>
     aws sqs add-permission --queue-url <gone-url> --label l --aws-account-ids 000000000000 --actions SendMessage
     aws sqs remove-permission --queue-url <gone-url> --label l
     aws sqs tag-queue --queue-url <gone-url> --tags a=b
     aws sqs untag-queue --queue-url <gone-url> --tag-keys a
3. Each answers HTTP 400 with code AWS.SimpleQueueService.NonExistentQueue (correct) but message 'The specified queue does not exist for this wsdl version.' instead of AWS's fixed 'The specified queue does not exist.'
4. Re-replay all nine fixture#step bindings on the pointer (sqs-dead-letter-queue-and-message-move#task#15 plus the eight in trace_ids) to confirm the shared error constructor was corrected for every path.
