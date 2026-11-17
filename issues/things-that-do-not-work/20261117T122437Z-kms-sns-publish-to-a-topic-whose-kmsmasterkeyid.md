Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__sns.publish-with-a-nonexistent-kms-alias-rejected-kmsnotfound
Issue-Key: kms__sns.publish-with-a-nonexistent-kms-alias-rejected-kmsnotfound|run#13|AssertionError|raise AssertionError(
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__sns: Publish to a topic whose KmsMasterKeyId is a nonexistent alias succeeds instead of being refused KMSNotFound

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
Cause 1 of 1: SNS does not resolve the topic's KMS key at publish time.
  AWS: recorded behavior only — SetTopicAttributes stores the KmsMasterKeyId as given (an alias that does not exist is accepted and echoed by GetTopicAttributes), but Publish must encrypt with it and is therefore refused with the error code 'KMSNotFound' whose message wraps KMS's own refusal: 'Alias arn:<partition>:kms:<region>:<account>:<alias> is not found. (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: <id>; Proxy: null)' — fixed text with the alias ARN built from the topic's stored key id and only the request id varying per call.
  Emulator: Publish SUCCEEDS on the topic configured with the nonexistent alias, so a message is accepted that AWS could not encrypt.
  Why: a client depending on Publish failing fast when its topic key is gone (a deleted alias, a wrong region) believes the message was accepted and encrypted; the KMSNotFound path its retry/alerting is written against never runs.
  Fix: the SNS Publish handler must resolve the topic's KmsMasterKeyId in KMS state (alias resolution included) and map KMS's NotFoundException to SNS's KMSNotFound with the wrapped message.
  Traces: kms__sns.publish-with-a-nonexistent-kms-alias-rejected-kmsnotfound.

## Evidence
publish-with-a-nonexistent-kms-alias-rejected-kmsnotfound — AssertionError: Publish under the nonexistent alias 'alias/sim2real-noalias-<id>' unexpectedly succeeded. Preconditions in the same check hold: GetTopicAttributes echoes the bogus alias and kms.describe_key(KeyId=<alias>) is ('NotFoundException', 'Alias arn:aws:kms:<region>:<acct>:alias/... is not found.'). Expected ('KMSNotFound', 'Alias arn:<partition>:kms:<region>:<account>:<alias> is not found. (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: <id>; Proxy: null)').

## Steps to Reproduce
1. Create an SNS topic; sns.set_topic_attributes(TopicArn=<t>, AttributeName='KmsMasterKeyId', AttributeValue='alias/<a name no alias uses>').
2. Confirm GetTopicAttributes echoes it and kms.describe_key(KeyId=<that alias>) raises NotFoundException.
3. sns.publish(TopicArn=<t>, Message='probe') -> expect the KMSNotFound refusal with the wrapped KMS message, not a MessageId.
