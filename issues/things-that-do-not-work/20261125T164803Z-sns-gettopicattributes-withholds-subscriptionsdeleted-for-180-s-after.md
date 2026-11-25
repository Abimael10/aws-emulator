Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns.topic-attributes-subscription-counters-track-state
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sns.topic-attributes-subscription-counters-track-state|_poll#20|Timeout|time.sleep(interval_s)
Issue-Commit: 5e8364ffd1cc635503e41df160d30e11335e3874

# sns: GetTopicAttributes withholds SubscriptionsDeleted for 180 s after Unsubscribe, so the topic's subscription counters cannot be observed tracking state

> _Note: this issue was identified against the code base at commit `5e8364ffd1cc635503e41df160d30e11335e3874`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTopicAttributes

## Description
One net-new root cause, grounded in the single failing check under `### sns` that the replay marked neither ALREADY FILED nor OWNED (the service's other 10 failures are all OWNED by 20261125T032512Z-sns-the-signed-notification-envelope-announces-signingcerturl-at.md or 20261124T020517Z-sns-a-numretries-0-https-subscription-s-one.md, which concern HTTPS delivery and the signed envelope, not the topic counters). No open issue mentions the GetTopicAttributes subscription counters.

Cause 1 of 1: the `SubscriptionsDeleted` topic attribute is held back behind an emulator-invented 180-second settling span, so after an Unsubscribe no bounded client poll shorter than three minutes ever sees the counter move. The check drives the documented sequence (counters 0/0/0 on a fresh topic -> an auto-confirmed sqs Subscribe -> a confirmation-requiring email Subscribe -> Unsubscribe of the confirmed one -> NotFound on its ARN -> SubscriptionsDeleted >= 1), polling each transition, and is killed at the suite's 60 s cap with no verdict; the one transition in it the emulator cannot produce inside any 60 s budget is the deleted count. Failing check: sns.topic-attributes-subscription-counters-track-state.
AWS:      Smithy: `GetTopicAttributesResponse$Attributes` documents `SubscriptionsConfirmed` – "The number of confirmed subscriptions for the topic", `SubscriptionsPending` – "The number of subscriptions pending confirmation for the topic" and `SubscriptionsDeleted` – "The number of deleted subscriptions for the topic"; `Unsubscribe` deletes the subscription (and `GetSubscriptionAttributes` on the deleted ARN is then refused NotFound / "Subscription does not exist"). The model states no settling period for any of the three counters, so they are read-time counts of the topic's subscription state that a client converges on by polling. AWS's own propagation delay is a lag a polling client sees END; a value a client cannot observe at all inside a normal convergence loop is not a modelled or recorded behaviour.
Emulator: `SubscriptionsDeleted` is deliberately gated behind `SUBSCRIPTION_DELETION_SETTLES_AFTER = Duration::from_secs(180)`, pushed by `TopicRecord::count_deleted_subscription` and read by `TopicRecord::reported_subscriptions_deleted`, which `GetTopicAttributes` reports. Every read inside the first 180 s after the Unsubscribe therefore still reports the pre-Unsubscribe count (0 for a topic whose only deletion just happened), and the check — whose whole budget is 60 s — is killed at the cap ("Check timed out after 60.0s") instead of reaching a verdict.
Why:      a client that unsubscribes and then polls GetTopicAttributes to confirm the deletion was accounted for never converges: it either loops to its own deadline and reports a failure, or concludes the deletion did not happen even though the subscription is already gone from ListSubscriptionsByTopic and GetSubscriptionAttributes is already NotFound. Any dashboard, drift check or test that reads the three counters as one consistent view of a topic sees an internally inconsistent one for three minutes.
Fix:      SNS's subscription-counter state — the deleted-count settling span in the service state (`state.rs`, `SUBSCRIPTION_DELETION_SETTLES_AFTER`, `TopicRecord::count_deleted_subscription`, `TopicRecord::reported_subscriptions_deleted`) and the attribute assembly in the GetTopicAttributes handler that reports it — so that the deleted count follows an Unsubscribe within a span a polling client observes, as the confirmed and pending counts already do.
Traces:   sns.topic-attributes-subscription-counters-track-state; emulator code at services/service-com-amazonaws-sns-amazonsimplenotificationservice/src/state.rs:471 (the 180 s span), :848 (`reported_subscriptions_deleted`), :868 (`count_deleted_subscription`) and src/ops/get_topic_attributes.rs:228-231 (the attribute as reported).

## Evidence
Suite replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 574/632 passed (90.8%), bootstrap_signal false. Under `### sns`: 241/252 (96%), 11 failed, of which this one is neither OWNED nor ALREADY FILED.

  [mismatch] sns.topic-attributes-subscription-counters-track-state
    Error: Check timed out after 60.0s
    (the check polls GetTopicAttributes for each transition with a 2 s interval; its last wait is `SubscriptionsDeleted increments after Unsubscribe`)

Expected (AWS): on a fresh topic GetTopicAttributes reports SubscriptionsConfirmed=0, SubscriptionsPending=0, SubscriptionsDeleted=0; after an sqs Subscribe (auto-confirmed) confirmed=1/pending=0; after an email Subscribe pending=1/confirmed=1; after Unsubscribe of the confirmed subscription confirmed=0 with pending still 1, GetSubscriptionAttributes on the removed ARN is refused NotFound with message "Subscription does not exist", and SubscriptionsDeleted is >= 1 within a client's bounded poll.
Actual: the emulator's own source fixes the deleted count's settling span at 180 s (state.rs:471), three times the check's whole budget, so SubscriptionsDeleted still reports the pre-Unsubscribe value on every read the check can make and the check never reaches a verdict. The topic name, subscription ARNs and the exact instants are per-run values; the requirement is that the three counters are a consistent, observable view of the topic's subscription state after Unsubscribe.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from that checkout, and replay simparity-behavioral-aws-v1.
2. `sns create-topic --name <name>` and read GetTopicAttributes: all three of SubscriptionsConfirmed / SubscriptionsPending / SubscriptionsDeleted must be 0.
3. `sns subscribe --protocol sqs --notification-endpoint arn:aws:sqs:<region>:<account>:<queue>` on the topic; poll GetTopicAttributes until SubscriptionsConfirmed=1 (pending must stay 0, deleted 0).
4. `sns subscribe --protocol email --notification-endpoint someone@example.com`; poll until SubscriptionsPending=1 with SubscriptionsConfirmed still 1.
5. `sns unsubscribe --subscription-arn <the sqs subscription arn>`; poll GetTopicAttributes. Observed: SubscriptionsConfirmed returns to 0 but SubscriptionsDeleted stays at its pre-Unsubscribe value for 180 s, so a 60 s poll never sees it increment. Expected: SubscriptionsDeleted reaches >= 1 within a bounded poll while SubscriptionsPending stays 1.
6. `sns get-subscription-attributes --subscription-arn <that arn>` and confirm it is refused NotFound / "Subscription does not exist".
7. Re-replay sns.topic-attributes-subscription-counters-track-state to confirm the fix.
