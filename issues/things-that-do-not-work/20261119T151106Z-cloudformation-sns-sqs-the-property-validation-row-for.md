Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.cfn-subscription-raw-message-delivery-non-boolean-rejected-creates-nothing
Issue-Key: cloudformation__sns__sqs.cfn-subscription-raw-message-delivery-non-boolean-rejected-creates-nothing|run#12|AssertionError|assert found == _PROPERTY_ROWS, (
Issue-Commit: a0a325cd7a8f100e0b15b3c52772a25e70aa00e8

# cloudformation__sns__sqs: the PROPERTY_VALIDATION row for a property whose value has the wrong JSON type is worded by the emulator ('Property validation failure: [Value of property {/X} does not match type {T}]') instead of AWS's 'Property [X] expected type: T, found: <found type>'

> _Note: this issue was identified against the code base at commit `a0a325cd7a8f100e0b15b3c52772a25e70aa00e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
ONE net-new root cause of service `cloudformation__sns__sqs`, grounded in the single failing check in trace_ids. The check is not marked ALREADY FILED or OWNED in the replay report, and no open issue of this service carries it.

=== Cause 1 of 1: the type-mismatch property-validation reason uses emulator wording ===
AWS: recorded behavior only (CloudFormation does not model stack/operation event text in Smithy). When a template supplies a property value whose JSON type does not match the resource type's schema — here an AWS::SNS::Subscription with RawMessageDelivery given as the string "not-a-boolean" where the schema declares Boolean — the stack is rejected by pre-deployment property validation (ROLLBACK_COMPLETE, nothing provisioned: the template's queue does not exist and SQS GetQueueUrl refuses AWS.SimpleQueueService.NonExistentQueue 'The specified queue does not exist.'), and DescribeEvents reports exactly one VALIDATION_ERROR event for the offending resource: LogicalResourceId = the resource's logical id, ValidationName = PROPERTY_VALIDATION, ValidationStatus = FAILED, ValidationFailureMode = FAIL, ValidationPath = /Resources/<logical id>/Properties/<property name>, and ValidationStatusReason = `Property [<property name>] expected type: <schema type>, found: <JSON type of the supplied value>`. The fixed text is `Property [` … `] expected type: ` … `, found: ` …; the variable parts are the property's name and schema type (from the resource type's schema) and the JSON type actually supplied (String here).
Emulator: every graded field of that row matches EXCEPT the reason, which reads `Property validation failure: [Value of property {/RawMessageDelivery} does not match type {Boolean}]` — emulator-authored prose that names the property by a leading-slash pointer, does not state the type that was found, and is not AWS's sentence for this error class. The stack status and the stack-level 'Validation failed with 1 error(s). …' summary assertions passed immediately before, so only this reason text diverges.
Why: a client that reads DescribeEvents to tell a user which property is wrong and what type it expected (a deploy wrapper, a CI gate, any tooling written against CloudFormation's property-validation text) cannot parse the emulator's sentence and loses the 'found' type entirely, so a template authored against AWS's diagnostics gets a different, unparseable diagnosis for the same mistake.
Fix: the CloudFormation stack-provisioning path's pre-deployment property validation — the message builder that renders a schema type mismatch into the resource's PROPERTY_VALIDATION event reason must emit AWS's `Property [<name>] expected type: <schema type>, found: <found type>` form (the ValidationPath it already sets is correct). Related OPEN issues of this service describe different causes and do NOT carry this check: 20261113T225447Z-cloudformation-sns-sqs-a-stack-rejected-by-pre.md (the stack-level CREATE_FAILED summary vs the resource-level text) and 20261109T185037Z-cloudformation-sns-sqs-6-net-new-causes-custom-resource-responseurl-points.md (no stack-level PROPERTY_VALIDATION reporting at all).
Traces: cloudformation__sns__sqs.cfn-subscription-raw-message-delivery-non-boolean-rejected-creates-nothing.

## Evidence
cloudformation__sns__sqs.cfn-subscription-raw-message-delivery-non-boolean-rejected-creates-nothing [mismatch]:
  AssertionError: VALIDATION_ERROR rows expected [('Subscription', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'Property [RawMessageDelivery] expected type: Boolean, found: String', '/Resources/Subscription/Properties/RawMessageDelivery')], got [('Subscription', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'Property validation failure: [Value of property {/RawMessageDelivery} does not match type {Boolean}]', '/Resources/Subscription/Properties/RawMessageDelivery')]
  The row's LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode and ValidationPath all match; only ValidationStatusReason diverges. The preceding assertions passed: the stack settled ROLLBACK_COMPLETE and its events contained the subsequence ('CREATE_FAILED', 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.') then ('ROLLBACK_COMPLETE', None).

## Steps to Reproduce
1. CreateStack with a template containing an AWS::SNS::Topic, an AWS::SQS::Queue (QueueName=<name>), an AWS::SQS::QueuePolicy granting sns.amazonaws.com sqs:SendMessage on that queue, and an AWS::SNS::Subscription (DependsOn the queue policy) whose Properties set TopicArn, Protocol='sqs', Endpoint={Fn::GetAtt Queue.Arn} and RawMessageDelivery='not-a-boolean' (a String where the schema declares Boolean).
2. Wait for the terminal status: ROLLBACK_COMPLETE.
3. Page DescribeEvents(StackName=<stack id>) and collect every event with EventType=VALIDATION_ERROR as (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath). AWS reports the single row ('Subscription', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'Property [RawMessageDelivery] expected type: Boolean, found: String', '/Resources/Subscription/Properties/RawMessageDelivery'); the emulator reports the same row with reason 'Property validation failure: [Value of property {/RawMessageDelivery} does not match type {Boolean}]'.
4. Confirm the rejected stack created nothing (SQS GetQueueUrl(QueueName=<name>) refuses AWS.SimpleQueueService.NonExistentQueue 'The specified queue does not exist.') and that the same template with RawMessageDelivery='true' reaches CREATE_COMPLETE with the subscription's RawMessageDelivery attribute 'true'.
5. Re-replay cloudformation__sns__sqs.cfn-subscription-raw-message-delivery-non-boolean-rejected-creates-nothing.
