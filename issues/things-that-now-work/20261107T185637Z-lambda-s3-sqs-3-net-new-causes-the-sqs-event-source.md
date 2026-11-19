Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__sqs.sqs-esm-direct-sdk-trigger-positive lambda__s3__sqs.sqs-esm-batch-size-affects-invocation-count-positive lambda__s3__sqs.sqs-esm-message-attributes-preserved-positive lambda__s3__sqs.sqs-esm-filter-criteria-drops-nonmatching-negative lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative lambda__s3__sqs.sqs-esm-wrong-queue-arn-mismatch-negative lambda__s3__sqs.sqs-esm-lambda-error-triggers-retry-positive lambda__s3__sqs.s3-notification-to-sqs-lambda-esm-drains-queue-positive lambda__s3__sqs.deploy-lambda-destination-onfailure-to-sqs-positive lambda__s3__sqs.deploy-lambda-destination-missing-permission-negative lambda__s3__sqs.cli-put-bucket-notification-missing-permission-negative
Issue-Key: lambda__s3__sqs.sqs-esm-direct-sdk-trigger-positive|run#12|AssertionError|assert matched, f"lambda was never invoked with message body {marker!r}; reports seen={reports}"
Issue-Key: lambda__s3__sqs.sqs-esm-batch-size-affects-invocation-count-positive|run#13|AssertionError|assert markers.issubset(seen), f"not all messages were delivered: missing {markers - seen}"
Issue-Key: lambda__s3__sqs.sqs-esm-message-attributes-preserved-positive|run#13|AssertionError|assert matched, f"lambda was never invoked for marker {marker!r}"
Issue-Key: lambda__s3__sqs.sqs-esm-filter-criteria-drops-nonmatching-negative|run#14|AssertionError|assert any(marker_match in rec["body"] for rep in reports for rec in rep["records"]), (
Issue-Key: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative|run#14|AssertionError|assert _matches(calib_reports, calib_marker), (
Issue-Key: lambda__s3__sqs.sqs-esm-wrong-queue-arn-mismatch-negative|run#14|AssertionError|assert any(rec["body"] == marker_mapped for rep in reports for rec in rep["records"]), (
Issue-Key: lambda__s3__sqs.sqs-esm-lambda-error-triggers-retry-positive|_poll_reports#12|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__sqs.s3-notification-to-sqs-lambda-esm-drains-queue-positive|run#16|AssertionError|assert record is not None, f"no lambda-observed S3 event record for key {key!r} within 120s"
Issue-Key: lambda__s3__sqs.deploy-lambda-destination-onfailure-to-sqs-positive|_poll_until#9|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: lambda__s3__sqs.deploy-lambda-destination-missing-permission-negative|_poll_until#10|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: lambda__s3__sqs.cli-put-bucket-notification-missing-permission-negative|run#23|AssertionError|assert positive_control_ok, "positive control failed: function was never invoked even after granting permission"
Issue-Commit: d6cb6c274eaec5e2dce099ad9d2ddf00c4faf8ea

# 3 net-new causes: the SQS event source mapping never polls, Lambda async destinations never deliver, and S3 notifications never invoke the function

> _Note: this issue was identified against the code base at commit `d6cb6c274eaec5e2dce099ad9d2ddf00c4faf8ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Three root causes, all grounded in failing checks this report does NOT mark ALREADY FILED or OWNED. Open issues 20261029T193114Z-lambda-s3-sqs-5-net-new-causes-sqs.md, 20261105T094842Z-lambda-s3-sqs-the-python-lambda-runtime-provides.md and 20261029T072224Z-lambda-s3-sqs-an-sqs-event-source-mapping.md own OTHER checks of this service and do not carry the replay keys below.

Cause 1 of 3: an Enabled SQS event source mapping never receives from its queue, so the function is never invoked.
AWS: once the mapping reports State 'Enabled', the poller calls ReceiveMessage, invokes the function with a Records[] event (each record carrying messageId, body, attributes and messageAttributes), deletes the messages it processed, redelivers on a handler error until maxReceiveCount sends the message to the DLQ, splits deliveries by BatchSize, and applies FilterCriteria so only matching messages are delivered. A Disabled mapping delivers nothing — that absence is only meaningful if the enabled calibration delivered.
Emulator: nothing is ever delivered. The checks report "lambda was never invoked with message body '<payload>'; reports seen=[]", "not all messages were delivered: missing {...}", "lambda was never invoked for marker '<marker>'", "positive control failed: matching message was never delivered", "calibration message was never delivered while the ESM was enabled; an absence measured against this pipeline would prove nothing", "positive control failed: message on the correctly-mapped queue was never delivered", "report predicate not satisfied within 150s; last saw 0 report(s)" and "no lambda-observed S3 event record for key '<key>' within 120s". (The payload/marker/key strings are this run's values.)
Why: every SQS-triggered function is inert, queues are never drained, and all of BatchSize, message-attribute preservation, FilterCriteria, retry/DLQ and the Disabled-mapping negative are unobservable because the positive path never fires.
Fix: the Lambda event-source-mapping poller for SQS — ReceiveMessage on the mapped queue for an Enabled mapping, Records[] assembly (attributes and messageAttributes included), BatchSize batching, FilterCriteria evaluation, Invoke, and delete/retain per the handler outcome.
Traces: lambda__s3__sqs.sqs-esm-direct-sdk-trigger-positive, .sqs-esm-batch-size-affects-invocation-count-positive, .sqs-esm-message-attributes-preserved-positive, .sqs-esm-filter-criteria-drops-nonmatching-negative, .sqs-esm-disabled-mapping-no-invocation-negative, .sqs-esm-wrong-queue-arn-mismatch-negative, .sqs-esm-lambda-error-triggers-retry-positive, .s3-notification-to-sqs-lambda-esm-drains-queue-positive.

Cause 2 of 3: an asynchronous invocation's OnFailure destination never receives anything.
AWS: PutFunctionEventInvokeConfig with DestinationConfig.OnFailure pointing at a queue makes a failing asynchronous Invoke (after its retries) enqueue an invocation record on that queue — a JSON envelope with version, timestamp, requestContext (requestId, functionArn, condition, approximateInvokeCount), requestPayload and responseContext/responsePayload. If the execution role cannot write the destination, no record is delivered.
Emulator: no message ever arrives. The checks report "OnFailure destination message on the SQS queue not met within 90s. Last result: None" and "control OnFailure delivery not met within 90s. Last result: None" — so the permission-negative case is unfalsifiable too.
Why: a client relying on destinations for failure capture loses every failed event silently, and cannot tell a missing permission from a destination feature that does nothing.
Fix: the Lambda asynchronous-invocation pipeline's destination dispatch (OnFailure/OnSuccess), including the role-based write to the destination.
Traces: lambda__s3__sqs.deploy-lambda-destination-onfailure-to-sqs-positive, .deploy-lambda-destination-missing-permission-negative.

Cause 3 of 3: an S3 bucket notification never invokes the function, even once lambda:InvokeFunction is granted to s3.amazonaws.com.
AWS: with a LambdaFunctionConfigurations entry and the matching resource-policy statement, PutObject invokes the function within seconds.
Emulator: "positive control failed: function was never invoked even after granting permission".
Why: the S3-trigger architecture is inert, and the missing-permission negative it gates proves nothing.
Fix: the S3 notification dispatcher's Lambda destination (configuration matching, resource-policy check, asynchronous Invoke).
Traces: lambda__s3__sqs.cli-put-bucket-notification-missing-permission-negative.

## Evidence
Cause 1: expected Records[] deliveries to the function and the queue drained; got no invocation at all — 'lambda was never invoked with message body ...; reports seen=[]', 'calibration message was never delivered while the ESM was enabled', 'not all messages were delivered: missing {...}'.
Cause 2: expected an invocation record on the OnFailure queue; got 'OnFailure destination message on the SQS queue not met within 90s. Last result: None'.
Cause 3: expected the function invoked on PutObject once s3.amazonaws.com was permitted; got 'function was never invoked even after granting permission'.

## Steps to Reproduce
1) Cause 1: CreateEventSourceMapping on a queue (vary BatchSize, FilterCriteria, Enabled=false) and SendMessage; poll for invocations and queue depth.
2) Cause 2: PutFunctionEventInvokeConfig with DestinationConfig.OnFailure = a queue, Invoke asynchronously with a handler that raises, then ReceiveMessage on that queue.
3) Cause 3: PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry, AddPermission for s3.amazonaws.com, PutObject, poll for the invocation.

## Resolution
All three capabilities are served by the current tree; the report was filed against
`d6cb6c2`, before any of them existed, and every replayed unit of the three causes now
passes.

Cause 1 — the queue poller of an event source mapping lives in
services/service-com-amazonaws-lambda-awsgirapiservice/src/state/sqs_poller.rs (receive
through the queue's own delivery bookkeeping, Records[] assembly with the message's
attributes and messageAttributes, the batching window, FilterCriteria, the invoke over the
in-process callback, and delete-or-retain per the attempt's outcome), armed after the call
that makes or changes a mapping by `crate::state::drive_sqs_pollers` in
src/ops/create_event_source_mapping.rs and src/ops/update_event_source_mapping.rs. Pinned
by that crate's tests/all/event_source_mapping_sqs_delivery.rs,
sqs_event_source_handler_run.rs, sqs_event_source_filter_criteria.rs,
sqs_event_source_record_attributes.rs and event_source_mapping_pause_and_resume_delivery.rs
(the paused and the resumed halves of Enabled).

Cause 2 — the invocation record an asynchronous event's last attempt hands the destination
its event-invoke configuration names, written as the function's own execution role, lives in
services/service-com-amazonaws-lambda-awsgirapiservice/src/state/async_completion_delivery.rs.
Pinned by core/router/tests/all/lambda_async_failure_record_names_its_own_event.rs and
lambda_async_destination_bus_rule_keyed_on_the_publisher.rs.

Cause 3 — the notification dispatcher (configuration matching, the destination's own grant,
and the asynchronous invoke) lives in
services/service-com-amazonaws-s3-amazons3/src/state/notifications.rs. Pinned by
core/router/tests/all/s3_notification_invokes_its_lambda_target.rs,
s3_notification_fans_out_across_destination_kinds.rs and
s3_notification_delivery_follows_the_destination_grant.rs.

What was missing was a suite over the JOIN of the three, the shape of this report's
`s3-notification-to-sqs-lambda-esm-drains-queue` trace: added as
core/router/tests/all/s3_notification_to_sqs_lambda_esm_drains_the_queue.rs, which writes
objects into a bucket whose notification configuration names a queue and asserts that each
admitted key reaches the RUNNING handler of the mapping on that queue as a queue-sourced
record carrying the S3 event, that the queue drains, and that a key the configuration's
filter excludes reaches the function not at all.

Surviving replay key, not a divergence on any cause:
`sqs-esm-disabled-mapping-no-invocation-negative|run#14|AssertionError|assert
_matches(calib_reports, calib_marker)` is `moved` — the run gets past the calibration
assertion that key names and ends later, in a `time.sleep(5)`, when the harness's own 60 s
per-check wall-clock budget runs out; the disabled-then-enabled behaviour it calibrates is
pinned by event_source_mapping_pause_and_resume_delivery.rs. Every other key of this report
is fixed.
