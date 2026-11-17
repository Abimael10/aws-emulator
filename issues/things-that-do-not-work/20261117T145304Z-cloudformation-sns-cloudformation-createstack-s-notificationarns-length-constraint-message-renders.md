Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns.create-stack-notification-arns-over-five-rejected
Issue-Key: cloudformation__sns.create-stack-notification-arns-over-five-rejected|run#17|AssertionError|assert error.get("Message") == expected_message, (
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# cloudformation CreateStack's NotificationARNs length-constraint message renders the value as "6 items" instead of the bracketed list of submitted ARNs

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
ONE net-new root cause, grounded in the only failing check of the suite's `cloudformation__sns` heading that the replay report marks neither ALREADY FILED nor OWNED (every other failure under that heading is OWNED).

Cause 1 of 1: the rendering of the offending value in CreateStack's constraint-violation message for NotificationARNs.
Failing check id: cloudformation__sns.create-stack-notification-arns-over-five-rejected

AWS: Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) models com.amazonaws.cloudformation#NotificationARNs as a list with @length min 0, max 5. A CreateStack carrying 6 NotificationARNs is refused with HTTP 400, error Type Sender, Code ValidationError and the standard AWS input-validation sentence: `1 validation error detected: Value '[<arn1>, <arn2>, ...]' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5`. Anatomy: `1 validation error detected: Value '`, `' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5` is FIXED text (including the lower-cased wire member name `notificationARNs` and the modelled bound 5); the variable part is the REJECTED VALUE ITSELF, rendered as a bracketed, comma-and-space separated list of exactly the ARNs the request submitted, in request order. No stack is created. Recorded behavior for the sentence template; the bound and member name come from the model.

Emulator: answers the right status, Type and Code, but substitutes a summary of the value instead of the value: `1 validation error detected: Value '6 items' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5`.

Why: AWS's validation message is machine-consumed — it is the only place the rejected value is echoed, and clients, CLI users and golden-output tests read it to see WHICH values were submitted. A count placeholder breaks any match against AWS's text and removes the diagnostic content, so a caller that passed the wrong ARNs (and not merely too many) learns nothing from the refusal.

Fix: CloudFormation's request-validation error formatter (the component that renders `1 validation error detected: Value '<value>' at '<member>' failed to satisfy constraint: ...`) — render list values as the bracketed element list rather than an `<n> items` summary. The constraint check itself and the ValidationError classification are already correct.

Traces: cloudformation__sns.create-stack-notification-arns-over-five-rejected (the 6-ARN leg; the check's positive control with 5 topics reaching CREATE_COMPLETE is not what failed).

## Evidence
Failing check (suite service heading `### cloudformation__sns`): cloudformation__sns.create-stack-notification-arns-over-five-rejected

AssertionError:
  expected "1 validation error detected: Value '[arn:aws:sns:<region>:<account>:<topic-0>, arn:aws:sns:<region>:<account>:<topic-1>, arn:aws:sns:<region>:<account>:<topic-2>, arn:aws:sns:<region>:<account>:<topic-3>, arn:aws:sns:<region>:<account>:<topic-4>, arn:aws:sns:<region>:<account>:<topic-5>]' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5"
  got {'Type': 'Sender', 'Code': 'ValidationError', 'Message': "1 validation error detected: Value '6 items' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5"}

Type=Sender and Code=ValidationError match; only the `Value '...'` rendering diverges — AWS echoes the submitted ARN list, the emulator writes the element count.
Model evidence: NotificationARNs = list of NotificationARN, @length {min 0, max 5}.

## Steps to Reproduce
1. sns:CreateTopic six times -> six real topic ARNs.
2. cloudformation:CreateStack(StackName=<name>, TemplateBody=<any valid template>, NotificationARNs=[all six ARNs]).
   EXPECT HTTP 400, Type Sender, Code ValidationError, Message "1 validation error detected: Value '[<arn0>, <arn1>, <arn2>, <arn3>, <arn4>, <arn5>]' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5".
   OBSERVE the same status/Type/Code but Message "1 validation error detected: Value '6 items' at 'notificationARNs' failed to satisfy constraint: Member must have length less than or equal to 5".
3. Confirm no stack was created, then CreateStack the identical template with only 5 of the ARNs — it reaches CREATE_COMPLETE and echoes all 5 NotificationARNs.
Replay: cloudformation__sns.create-stack-notification-arns-over-five-rejected
