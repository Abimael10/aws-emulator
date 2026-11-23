Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sns__sqs.iam-trust-sns-wrong-principal-negative
Issue-Key: lambda__sns__sqs.iam-trust-sns-wrong-principal-negative|_set_feedback_role_rejected#19|Timeout|time.sleep(interval_s)
Issue-Commit: f9dc389755539ec737bd6c84507d1519aa5a1660

# lambda__sns__sqs: SNS SetTopicAttributes on an SQS delivery-status feedback role never settles, so a wrongly-trusted role is neither accepted nor refused

> _Note: this issue was identified against the code base at commit `f9dc389755539ec737bd6c84507d1519aa5a1660`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetTopicAttributes

## Description
Cause 1 of 1: setting SQSSuccessFeedbackRoleArn on a topic reaches no decision — neither the accept the correctly-trusted role must get nor the refusal the wrongly-trusted role must get is observable, so the check runs out its budget with no verdict. Failing check id: lambda__sns__sqs.iam-trust-sns-wrong-principal-negative.

AWS: SetTopicAttributes with AttributeName=SQSSuccessFeedbackRoleArn and a role whose trust policy allows sts:AssumeRole for the service principal sns.amazonaws.com is accepted (HTTP 200, empty body) once the IAM trust-policy write has propagated, and GetTopicAttributes then reports Attributes.SQSSuccessFeedbackRoleArn equal to that role ARN. A role whose trust policy names a different service principal (ec2.amazonaws.com) is never assumable by SNS, so SNS refuses the call on every attempt — with AuthorizationError (HTTP 403) or InvalidParameter (HTTP 400) — and the attribute never appears in GetTopicAttributes. Smithy: SetTopicAttributes input {TopicArn required, AttributeName required, AttributeValue}; modelled errors AuthorizationErrorException (awsQueryError code AuthorizationError, httpResponseCode 403), InvalidParameterException (code InvalidParameter, 400), InvalidSecurityException (403), NotFoundException (404), InternalError (500). The error code and status are the requirement; the message's variable part is the role ARN and the topic ARN from the request.

Emulator: the check never reaches a verdict — the replay reports `Check timed out after 60.0s`. The only waits that can consume that budget are the two same-length (90 s) SetTopicAttributes retry windows: the positive control's loop, which retries only while the emulator answers AuthorizationError for the correctly-trusted sns.amazonaws.com role, and the negative leg's window, which retries only while the emulator keeps refusing. One of them never terminates, so the emulator never produced a settled answer for the feedback-role attribute: no accepted 200 that GetTopicAttributes reads back, and no decided refusal for the wrong principal. Hypothesis: SetTopicAttributes does not resolve the referenced role's trust policy when the *FeedbackRoleArn attributes are set, so the accept/refuse decision is never made.

Why: a client cannot tell a usable delivery-status feedback role from an unusable one. Code that configures SQSSuccessFeedbackRoleArn gets no 200 to confirm the attribute took hold and no AuthorizationError/InvalidParameter to tell it the role is wrongly trusted — delivery-status logging is silently unconfigurable, and a wrongly-trusted role may be left on the topic.

Fix: SNS's SetTopicAttributes handler and its validation of the delivery-status attributes (SQSSuccessFeedbackRoleArn / SQSFailureFeedbackRoleArn and their sample-rate siblings) — the role's trust policy must be resolved against the sns.amazonaws.com service principal to decide accept vs AuthorizationError/InvalidParameter — and the topic-attribute state GetTopicAttributes reads back.

Traces: lambda__sns__sqs.iam-trust-sns-wrong-principal-negative.

Related but not covering this: 20261107T054154Z-lambda-sns-sqs-3-net-new-root-causes.md is open on other lambda/sns/sqs delivery causes; this check is not listed as owned by it.

## Evidence
Expected (AWS): SetTopicAttributes(TopicArn=<topic>, AttributeName="SQSSuccessFeedbackRoleArn", AttributeValue=<role trusting sns.amazonaws.com>) -> HTTP 200, and GetTopicAttributes(TopicArn=<topic>).Attributes["SQSSuccessFeedbackRoleArn"] == that role ARN. The same call with a role trusting only ec2.amazonaws.com -> refused on every attempt with error code AuthorizationError (HTTP 403) or InvalidParameter (HTTP 400), and GetTopicAttributes never reports that role ARN.
Actual (emulator): no decision at all — replay reports `Check timed out after 60.0s`; one of the two 90 s SetTopicAttributes retry windows never terminates, so neither the accepted 200 (read back by GetTopicAttributes) nor a settled AuthorizationError/InvalidParameter refusal is observable.

## Steps to Reproduce
1. iam CreateRole roleGood with a trust policy allowing sts:AssumeRole for Service sns.amazonaws.com, and roleBad allowing only ec2.amazonaws.com; attach a logs inline policy to each.
2. sns CreateTopic topicGood and topicBad; subscribe an SQS queue to each.
3. sns SetTopicAttributes(topicGood, "SQSSuccessFeedbackRoleArn", roleGood) then (topicGood, "SQSSuccessFeedbackSampleRate", "100") -> expect 200; GetTopicAttributes(topicGood) must show SQSSuccessFeedbackRoleArn == roleGood.
4. sns SetTopicAttributes(topicBad, "SQSSuccessFeedbackRoleArn", roleBad) -> expect AuthorizationError (403) or InvalidParameter (400) on every retry; GetTopicAttributes(topicBad) must never show roleBad.
5. Replay: lambda__sns__sqs.iam-trust-sns-wrong-principal-negative.
