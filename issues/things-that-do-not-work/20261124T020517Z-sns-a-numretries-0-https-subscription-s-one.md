Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns.https-subscription-with-a-zero-retry-delivery-policy-attempts-a-failing-notification-exactly-once
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sns.https-subscription-with-a-zero-retry-delivery-policy-attempts-a-failing-notification-exactly-once|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Commit: f4f47df5aa8b9a19bc159acc32ad01036b03b7fe

# sns: a numRetries=0 HTTPS subscription's one-attempt-then-dead-letter budget cannot be observed — SNS POSTs nothing to the https endpoint, not even the SubscriptionConfirmation

> _Note: this issue was identified against the code base at commit `f4f47df5aa8b9a19bc159acc32ad01036b03b7fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Subscribe

## Description
Cause 1 of 1: SNS makes no HTTP request at all to a subscribed https endpoint, so a subscription's DeliveryPolicy retry budget and its RedrivePolicy dead-lettering are unobservable. The check subscribes an always-500 https endpoint that records every request it is given, and its FIRST blocking wait is the `SubscriptionConfirmation` POST that Subscribe must make (its Token is the only input ConfirmSubscription accepts); nothing is ever captured, so the check is killed at the 60 s cap before a single Publish is attempted. Failing check id: sns.https-subscription-with-a-zero-retry-delivery-policy-attempts-a-failing-notification-exactly-once.

AWS:      Smithy (com.amazonaws.sns#AmazonSimpleNotificationService): `Subscribe` (TopicArn, Protocol, Endpoint, Attributes, ReturnSubscriptionArn) creates an http/https subscription in PendingConfirmation; `ConfirmSubscription` (TopicArn, Token) returns its SubscriptionArn; `SetSubscriptionAttributes` accepts AttributeName `DeliveryPolicy` and `RedrivePolicy`, and `GetSubscriptionAttributes` reports `EffectiveDeliveryPolicy`. The wire shape of the POSTs is recorded behavior only, and the recording shows: on Subscribe, one POST to the endpoint with header `x-amz-sns-message-type: SubscriptionConfirmation` whose JSON body carries Type, MessageId, Token, TopicArn, Message, SubscribeURL, Timestamp, SignatureVersion, Signature and SigningCertURL (an https URL on the SNS host, serving the PEM the signature verifies against); after confirmation, one POST per Publish with `x-amz-sns-message-type: Notification` and `x-amz-sns-message-id: <that Publish's MessageId>`. The retry rule: with `DeliveryPolicy = {"healthyRetryPolicy": {"numRetries": 0, ...}}` an endpoint answering HTTP 500 is POSTed exactly ONCE and the notification is then written to the queue named by `RedrivePolicy.deadLetterTargetArn` (the dead letter carrying that notification's MessageId, exactly once); with `numRetries: 1` the same endpoint is POSTed at least twice before dead-lettering. Attempt counts follow the configured policy; the topic/queue names, message ids and tokens are per-run values.

Emulator: the endpoint is never contacted — no SubscriptionConfirmation, hence no Token, hence no confirmed subscription and no notification attempt to count. Subscribe returns success, so the loss is silent; the only replay output is `Error: Check timed out after 60.0s`, with no assertion from the check body evaluated. The retry-budget contrast the check exists to grade (1 attempt for numRetries=0 across two trials, >= 2 attempts for a numRetries=1 twin on the same endpoint) and the dead-letter write are therefore ungraded, not merely wrong.

Why:      a client that subscribes an https endpoint gets nothing while Publish reports success — total, silent data loss on a first-class SNS protocol. Specifically for this cause: a client cannot verify that its configured retry budget is honoured, so it cannot tell a never-retrying delivery path from a correctly bounded one, and cannot confirm that an exhausted notification lands in its dead-letter queue instead of vanishing. Alerting and webhook integrations built on http/https subscriptions are untestable.

Fix:      the SNS http/https delivery worker and the Subscribe handler's confirmation dispatch, plus the per-subscription delivery-policy retry loop and the dead-letter hand-off that must run after the last failed attempt (EffectiveDeliveryPolicy's numRetries bounding the POST count, RedrivePolicy.deadLetterTargetArn receiving the exhausted notification once). Related OPEN issue that does NOT carry this check's replay key and so does not cover it: 20261115T165530Z-sns-nothing-is-ever-posted-to-an-https.md, filed on the same missing-POST symptom for seven other `sns`-heading checks (it names delivery-policy retries as one of the behaviors hidden behind it but is not marked as owning this check); see also 20261109T163411Z-iam-lambda-sns-sqs-sns-never-delivers-to-an-https-subscription-endpoint.md for the iam__lambda__sns__sqs heading. Fixing the delivery worker alone is not enough for this check: the numRetries=0 / numRetries=1 attempt counts and the dead-letter write must both hold once POSTs begin.

Traces:   sns.https-subscription-with-a-zero-retry-delivery-policy-attempts-a-failing-notification-exactly-once (blocked in `_subscribe_and_capture_confirmation` / `_captured`, killed at 60 s).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] sns.https-subscription-with-a-zero-retry-delivery-policy-attempts-a-failing-notification-exactly-once
  Error: Check timed out after 60.0s
  (no traceback from the check body: the run was killed while draining the capture queue, so no assertion was ever evaluated)

Expected (AWS): after CreateTopic and Subscribe(Protocol="https", Endpoint=<function URL that answers 500 to Notifications and records every request>, ReturnSubscriptionArn=true), the endpoint receives one POST with x-amz-sns-message-type: SubscriptionConfirmation whose body's Token confirms the subscription; then with SetSubscriptionAttributes DeliveryPolicy={"healthyRetryPolicy":{"numRetries":0,"minDelayTarget":1,"maxDelayTarget":1,"numNoDelayRetries":0,"numMinDelayRetries":0,"numMaxDelayRetries":0,"backoffFunction":"linear"}} and RedrivePolicy={"deadLetterTargetArn":<dlq arn>}, a Publish produces exactly ONE POST carrying x-amz-sns-message-id = that Publish's MessageId and exactly one dead letter for that MessageId in the DLQ; the numRetries=1 twin on the same endpoint produces at least TWO such POSTs before its dead letter.
Actual: the endpoint receives no request of any kind. The capture queue stays empty, the check never obtains a confirmation Token, and the harness terminates it at the 60 s cap. Subscribe returns 200 with a SubscriptionArn.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Stand up an https endpoint that records each request (headers + body) and answers HTTP 500 to a Notification and 200 otherwise — e.g. a Lambda function URL (AuthType=NONE) writing each request into an SQS capture queue.
3. sns CreateTopic --name t.
4. sns Subscribe --topic-arn <t> --protocol https --notification-endpoint <url> --return-subscription-arn.
5. Drain the capture queue: AWS has already POSTed a SubscriptionConfirmation (x-amz-sns-message-type header, body with Token/SubscribeURL); the emulator records nothing, so no Token exists and ConfirmSubscription cannot be called through the documented path.
Observed: no request reaches the endpoint; the check blocks here until the 60 s cap.
6. (Blocked by the above) sns SetSubscriptionAttributes DeliveryPolicy with healthyRetryPolicy.numRetries=0 and RedrivePolicy with deadLetterTargetArn=<dlq>; verify GetSubscriptionAttributes EffectiveDeliveryPolicy.healthyRetryPolicy.numRetries == 0; Publish once and count POSTs bearing that MessageId in x-amz-sns-message-id — AWS makes exactly one and writes exactly one dead letter for it; repeat with numRetries=1 on a twin subscription to the same endpoint and expect >= 2 POSTs.
7. Or replay the check id in trace_ids.
