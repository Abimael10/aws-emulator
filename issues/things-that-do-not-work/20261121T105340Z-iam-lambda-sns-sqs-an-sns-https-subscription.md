Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__sns__sqs.https-delivery-content-type-defaults-to-text-plain-and-follows-headercontenttype iam__lambda__sns__sqs.unsubscribe-confirmation-posts-only-for-the-anonymous-flow-and-its-token-reconfirms
Issue-Key: iam__lambda__sns__sqs.https-delivery-content-type-defaults-to-text-plain-and-follows-headercontenttype|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Key: iam__lambda__sns__sqs.unsubscribe-confirmation-posts-only-for-the-anonymous-flow-and-its-token-reconfirms|_captured#5|Timeout|resp = sqs.receive_message(QueueUrl=capture_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Commit: 77bfd47bd7799ecf9c5ff0eea8e18d7399a37670

# iam__lambda__sns__sqs: an SNS https subscription is never delivered a SubscriptionConfirmation POST, so no HTTP/S delivery behaviour (Content-Type defaults, DeliveryPolicy headerContentType, UnsubscribeConfirmation) can happen

> _Note: this issue was identified against the code base at commit `77bfd47bd7799ecf9c5ff0eea8e18d7399a37670`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Subscribe

## Description
Cause 1 of 1: `Subscribe` with `Protocol=https` is accepted and returns a subscription ARN, but SNS never POSTs anything to the subscribed endpoint — the `SubscriptionConfirmation` message that AWS sends immediately on subscribe never arrives. Both checks stall on that first step and are cut off by the harness at 60 s, so neither reaches a verdict.

AWS: Smithy (sns): `Subscribe` takes `Protocol` (`https`) and `Endpoint` and, for an unconfirmed https subscription, returns the literal `pending confirmation` unless `ReturnSubscriptionArn=true`. Recorded behaviour: within seconds of `Subscribe`, SNS issues an HTTP POST to the endpoint carrying a JSON body with `Type: "SubscriptionConfirmation"`, `TopicArn`, `Token`, `SubscribeURL`, `MessageId`, `Message`, `Timestamp`, and the `x-amz-sns-message-type` / `x-amz-sns-topic-arn` headers; `ConfirmSubscription` with that `Token` then confirms the subscription. Once confirmed, each `Publish` POSTs a `Type: "Notification"` body, whose `Content-Type` defaults to `text/plain; charset=UTF-8` and is overridden by the subscription's `DeliveryPolicy` `requestPolicy.headerContentType`; and an anonymous GET of a subscription's `UnsubscribeURL` POSTs exactly one `Type: "UnsubscribeConfirmation"` whose `x-amz-sns-*` headers name the subscription and whose `Token` re-confirms it, while a signed owner `Unsubscribe` POSTs none.

Emulator: `Subscribe(Protocol=https, ...)` returns a subscription ARN, but the endpoint receives no POST. Both checks poll the capture endpoint for a `SubscriptionConfirmation` naming the topic (45 s per attempt, re-subscribing up to a 240 s deadline) and make no observable progress; the suite kills each at its 60 s cap with `Check timed out after 60.0s`, before any assertion on content-type or unsubscribe behaviour is reached.

Why: HTTP/S is one of SNS's two push protocols. A client that subscribes an HTTPS endpoint never gets the confirmation token, so the subscription can never be confirmed and the endpoint never receives a notification — the entire webhook fan-out pattern is inert. The downstream contracts (default `text/plain; charset=UTF-8` Content-Type, `headerContentType` override, and the UnsubscribeConfirmation/Token re-confirm flow) are unreachable as a consequence.

Fix: the SNS HTTP/S delivery component — the part of `Subscribe` that enqueues the SubscriptionConfirmation POST for an `https`/`http` endpoint, and the delivery worker that performs the outbound POSTs (Notification and UnsubscribeConfirmation) with their `x-amz-sns-*` headers and Content-Type. Related open issue 20261104T044826Z-lambda-sns-sqs-an-sns-assumable-role-check-never-settles-because.md records SNS delivery not happening under a different service heading (`lambda__sns__sqs`) and does not carry these checks.

Traces: iam__lambda__sns__sqs.https-delivery-content-type-defaults-to-text-plain-and-follows-headercontenttype, iam__lambda__sns__sqs.unsubscribe-confirmation-posts-only-for-the-anonymous-flow-and-its-token-reconfirms.

## Evidence
Both failing checks under `### iam__lambda__sns__sqs` [mismatch]:

    Error: Check timed out after 60.0s

The first graded step of each is the SubscriptionConfirmation POST:
  `sub, conf = _subscribe_and_capture_confirmation(ctx, topic, endpoint, capture)` →
  `sns.subscribe(TopicArn=..., Protocol="https", Endpoint=..., ReturnSubscriptionArn=True)` then
  `_captured(..., lambda r: any(x["body"].get("Type") == "SubscriptionConfirmation" and x["body"].get("TopicArn") == topic_arn for x in r), timeout_s=45)`
  with `assert time.monotonic() < deadline, "no SubscriptionConfirmation for the topic reached the HTTPS endpoint; captured ..."`.

Expected (AWS): a POST with `Type: "SubscriptionConfirmation"` and the topic's ARN reaches the endpoint within seconds; `ConfirmSubscription(Token=...)` then confirms it.
Actual (emulator): nothing is captured; the check makes no progress and is cut off at the 60 s cap, never reaching the Content-Type or UnsubscribeConfirmation assertions.

## Steps to Reproduce
1. CreateTopic.
2. Stand up an HTTPS endpoint that records the POSTs it receives.
3. Subscribe(TopicArn=<topic>, Protocol='https', Endpoint=<that endpoint>, ReturnSubscriptionArn=true).
4. Poll the endpoint's captured requests for a body with Type == 'SubscriptionConfirmation' and TopicArn == <topic>. Expected: present within seconds. Observed: nothing is ever POSTed.
5. (Blocked by 4) ConfirmSubscription with the captured Token, Publish, and assert the Notification POST's Content-Type is 'text/plain; charset=UTF-8', then SetSubscriptionAttributes DeliveryPolicy requestPolicy.headerContentType and assert a later Notification carries the override.
6. (Blocked by 4) GET the subscription's UnsubscribeURL anonymously and assert exactly one UnsubscribeConfirmation POST arrives, with x-amz-sns-* headers naming the subscription, and that its Token re-confirms the subscription; assert a signed owner Unsubscribe POSTs none.
