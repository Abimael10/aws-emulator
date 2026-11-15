Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns.authenticate-on-unsubscribe-refuses-the-anonymous-unsubscribe-url sns.confirm-subscription-token-confirms-only-its-own-subscription-whatever-the-topic-arn sns.confirm-subscription-token-is-reusable-until-unsubscribe-then-not-found sns.https-endpoint-failing-with-500-is-retried-then-dead-lettered sns.https-raw-message-delivery-posts-the-bare-body-with-the-rawdelivery-header sns.https-subscription-receives-nothing-until-confirmed-then-gets-the-notification sns.raw-message-delivery-drops-message-attributes-for-an-https-subscriber-but-keeps-them-for-sqs
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sns.authenticate-on-unsubscribe-refuses-the-anonymous-unsubscribe-url|_captured#6|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.confirm-subscription-token-confirms-only-its-own-subscription-whatever-the-topic-arn|_captured#6|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.confirm-subscription-token-is-reusable-until-unsubscribe-then-not-found|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.https-endpoint-failing-with-500-is-retried-then-dead-lettered|_captured#7|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.https-raw-message-delivery-posts-the-bare-body-with-the-rawdelivery-header|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.https-subscription-receives-nothing-until-confirmed-then-gets-the-notification|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: sns.raw-message-delivery-drops-message-attributes-for-an-https-subscriber-but-keeps-them-for-sqs|_captured#7|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# sns: nothing is ever POSTed to an https subscription endpoint — no SubscriptionConfirmation, no Notification, no UnsubscribeConfirmation

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Subscribe

## Description
Cause 1 of 1: SNS's HTTP/HTTPS delivery path makes no request to a subscribed https endpoint at all. Every one of the seven failing checks subscribes an https endpoint that records what it receives and then blocks waiting for SNS's first POST; each is killed at the 60 s check cap having captured nothing. The first unmet wait in six of them is the `SubscriptionConfirmation` that Subscribe must send (its Token is the only way the check can confirm the subscription), so every behavior layered on top — confirmation-token semantics, pending-vs-confirmed delivery, notification envelope and x-amz-sns-* headers, RawMessageDelivery, delivery-policy retries and dead-lettering, and the AuthenticateOnUnsubscribe 403 on the anonymous UnsubscribeURL — is unobservable behind the same missing POST.

AWS:      Smithy (com.amazonaws.sns): `Subscribe` with Protocol `https` creates a subscription in PendingConfirmation and SNS immediately POSTs a message to the endpoint; `ConfirmSubscription` takes the Token from that message and returns the subscription's ARN. The wire shape of those POSTs is recorded behavior only, and the recordings show: a POST with header `x-amz-sns-message-type: SubscriptionConfirmation` whose JSON body carries Type, MessageId, Token, TopicArn, Message, SubscribeURL and Timestamp (SubscribeURL being a ConfirmSubscription URL for exactly that topic and token); after confirmation, a POST per Publish with `x-amz-sns-message-type: Notification`, the message id in `x-amz-sns-message-id`, the subscription ARN header and an UnsubscribeURL in the body (with `x-amz-sns-rawdelivery: true` and the bare published body when RawMessageDelivery=true); and an `UnsubscribeConfirmation` POST for the anonymous unsubscribe flow.

Emulator: the endpoint receives no request whatsoever — not the confirmation, not any notification. Subscribe returns successfully and Publish reports success, so the loss is silent; the checks simply time out ("Check timed out after 60.0s").

Why:      an https subscriber gets no messages at all while Publish reports success — silent, total data loss on a first-class SNS delivery protocol. Clients cannot complete the standard confirmation handshake (no Token ever arrives, so the subscription can never leave PendingConfirmation through the documented path), cannot observe retry/dead-letter behavior, and cannot reverse an accidental anonymous unsubscribe.

Fix:      the SNS http/https delivery worker and the Subscribe handler's confirmation dispatch — SNS must POST the SubscriptionConfirmation on Subscribe for a pending http/https endpoint, POST notifications for confirmed ones with the x-amz-sns-* headers and the raw/envelope body rules, apply the subscription's DeliveryPolicy retries and dead-letter the exhausted message, and emit UnsubscribeConfirmation for the anonymous unsubscribe flow. The identical symptom is already filed for the `iam__lambda__sns__sqs` heading as 20261109T163411Z-iam-lambda-sns-sqs-sns-never-delivers-to-an-https-subscription-endpoint.md, which carries only that heading's two checks; these seven `sns`-heading checks are not covered by it and are filed here. Note the endpoint these checks use is a Lambda function URL that forwards captures to SQS, so if the egress/function-URL plumbing is what never gets called, the fix lands in the same delivery worker's outbound path.

Traces:   sns.authenticate-on-unsubscribe-refuses-the-anonymous-unsubscribe-url, sns.confirm-subscription-token-confirms-only-its-own-subscription-whatever-the-topic-arn, sns.confirm-subscription-token-is-reusable-until-unsubscribe-then-not-found, sns.https-endpoint-failing-with-500-is-retried-then-dead-lettered, sns.https-raw-message-delivery-posts-the-bare-body-with-the-rawdelivery-header, sns.https-subscription-receives-nothing-until-confirmed-then-gets-the-notification, sns.raw-message-delivery-drops-message-attributes-for-an-https-subscriber-but-keeps-them-for-sqs.

## Evidence
All seven checks: "Error: Check timed out after 60.0s" with no capture recorded.

Expected (AWS): after `sns subscribe --protocol https --notification-endpoint <url>`, the endpoint receives a POST with header x-amz-sns-message-type: SubscriptionConfirmation and a body containing Type/MessageId/Token/TopicArn/Message/SubscribeURL/Timestamp; after ConfirmSubscription, each Publish produces a POST with x-amz-sns-message-type: Notification, x-amz-sns-message-id: <that publish's MessageId> and an UnsubscribeURL (bare body plus x-amz-sns-rawdelivery: true when RawMessageDelivery=true); an anonymous Unsubscribe produces an UnsubscribeConfirmation POST.
Actual: the endpoint is never contacted, so the checks block on their first wait (`_subscribe_and_capture_confirmation` / `_captured`) until the harness kills them. Subscribe and Publish both return success.

## Steps to Reproduce
1. Stand up an https endpoint that records the requests it receives (e.g. a Lambda function URL writing each request to a queue).
2. aws sns create-topic --name t; aws sns subscribe --topic-arn <t> --protocol https --notification-endpoint <url> --return-subscription-arn
3. Inspect the endpoint's captures: no SubscriptionConfirmation POST ever arrives (AWS sends one immediately), so no Token is available for ConfirmSubscription.
4. Publish to the topic and inspect again: no Notification POST arrives either.
5. Repeat with RawMessageDelivery=true, with a DeliveryPolicy numRetries plus a dead-letter RedrivePolicy against an always-500 endpoint, and with an anonymous GET of an UnsubscribeURL — none of these produce any request to the endpoint.
