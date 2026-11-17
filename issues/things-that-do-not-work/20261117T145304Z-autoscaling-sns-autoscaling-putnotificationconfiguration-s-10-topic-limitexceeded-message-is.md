Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected
Issue-Key: autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected|run#16|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# autoscaling PutNotificationConfiguration's 10-topic LimitExceeded message is emulator-authored prose instead of AWS's "Your AutoScaling Group:<name> can't have more than 10 topics"

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutNotificationConfiguration

## Description
ONE net-new root cause, grounded in the single failing check of the suite's `autoscaling__sns` service heading; the replay report marks it neither ALREADY FILED nor OWNED.

Cause 1 of 1: the refusal message for the per-group SNS-topic limit on PutNotificationConfiguration.
Failing check id: autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected

AWS: an Auto Scaling group may carry notification configurations for at most 10 distinct SNS topics. An 11th PutNotificationConfiguration is refused with Auto Scaling's limit fault (error code LimitExceeded, HTTP 400, Query-protocol Sender error) and the message `Your AutoScaling Group:<group-name> can't have more than 10 topics`. Anatomy of that message: `Your AutoScaling Group:` and ` can't have more than 10 topics` are FIXED text (note there is no space after the colon and no trailing period); the only variable part is the group name taken from the request's AutoScalingGroupName. The limit 10 is the service quota, not a per-run value. Recorded behavior for the exact sentence (the Smithy model documents no message text); the check's preceding error-code assertion passed, so only the message diverges.

Emulator: answers the correct code but an emulator-authored sentence that names neither the group nor AWS's phrasing: `The notification configuration exceeds the limit of 10 SNS topics per Auto Scaling group`.

Why: the message is part of the refusal contract. A client, SDK wrapper, runbook or test that matches AWS's text — or an operator reading the failure — gets an unrecognised sentence, and because the emulator's wording omits the group name the caller cannot tell WHICH group hit the quota when the call is made in a loop over several groups.

Fix: the PutNotificationConfiguration handler's limit validation in the autoscaling service crate — emit AWS's fixed sentence with the request's group name substituted, instead of the hand-written one.

Traces: autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected. The positive control in the same check (freeing one slot lets an 11th topic succeed) is not what fails; only this message does. Related OPEN issues on autoscaling refusal wording, named for context only and not covering this check: 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md, 20261116T221312Z-autoscaling-refusal-messages-diverge-from-aws-s-fixed.md.

## Evidence
Failing check (suite service heading `### autoscaling__sns`): autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected

AssertionError: expected message "Your AutoScaling Group:<group-name> can't have more than 10 topics", got 'The notification configuration exceeds the limit of 10 SNS topics per Auto Scaling group'

Expected (AWS): message == "Your AutoScaling Group:" + <AutoScalingGroupName from the request> + " can't have more than 10 topics" — fixed text "Your AutoScaling Group:" and " can't have more than 10 topics"; variable text: the group name only.
Actual (emulator): "The notification configuration exceeds the limit of 10 SNS topics per Auto Scaling group" — different wording, group name absent.
The error code the check asserted before this line matched, so the status/code classification is right and only the text diverges.

## Steps to Reproduce
1. autoscaling:CreateAutoScalingGroup with MinSize/MaxSize/DesiredCapacity 0 and a launch template.
2. sns:CreateTopic 10 times; call autoscaling:PutNotificationConfiguration(AutoScalingGroupName=<group>, TopicARN=<topic i>, NotificationTypes=["autoscaling:TEST_NOTIFICATION"]) once per topic — all 10 succeed.
3. Create an 11th topic and call PutNotificationConfiguration for it. EXPECT the limit refusal with message "Your AutoScaling Group:<group> can't have more than 10 topics"; OBSERVE "The notification configuration exceeds the limit of 10 SNS topics per Auto Scaling group".
4. Positive control: DeleteNotificationConfiguration for one of the 10, then retry the 11th — it succeeds.
Replay: autoscaling__sns.put-notification-configuration-topic-limit-exceeded-rejected
