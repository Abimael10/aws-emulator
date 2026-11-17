Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kms__stepfunctions.cfn-activity-encryptionconfiguration-change-replaces-the-activity
Issue-Key: cloudformation__kms__stepfunctions.cfn-activity-encryptionconfiguration-change-replaces-the-activity|run#15|AssertionError|assert sfn.describe_activity(activityArn=arn)["encryptionConfiguration"] == want_before, (
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# cloudformation__kms__stepfunctions: DescribeActivity omits kmsDataKeyReusePeriodSeconds from the encryptionConfiguration of a CMK-encrypted activity

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeActivity

## Description
Cause 1 of 1: a Step Functions activity created with a customer managed key reads back without its data-key reuse period.
  AWS: recorded behavior only for the CloudFormation wiring; Step Functions' EncryptionConfiguration carries three members — type, kmsKeyId and kmsDataKeyReusePeriodSeconds. An activity created from AWS::StepFunctions::Activity with EncryptionConfiguration {type: CUSTOMER_MANAGED_KMS_KEY, KmsKeyId: <key arn>, KmsDataKeyReusePeriodSeconds: <seconds>} reads back from DescribeActivity with ALL THREE members: {'type': 'CUSTOMER_MANAGED_KMS_KEY', 'kmsKeyId': '<the key arn the template set>', 'kmsDataKeyReusePeriodSeconds': <the seconds the template set>}.
  Emulator: DescribeActivity right after create returns only {'kmsKeyId': '<key arn>', 'type': 'CUSTOMER_MANAGED_KMS_KEY'} — kmsDataKeyReusePeriodSeconds is absent, so the configuration the template declared is not fully represented and the subsequent replacement-on-change assertions cannot be made.
  Why: a client reading an activity's encryption configuration (to verify a deployment, to compute drift, or to decide whether a change requires replacement) cannot see the reuse period it set; code indexing that key raises instead of reading a value.
  Fix: the Step Functions activity state and the DescribeActivity serializer (encryptionConfiguration must persist and render kmsDataKeyReusePeriodSeconds), and the CreateActivity path that stores it.
  Traces: cloudformation__kms__stepfunctions.cfn-activity-encryptionconfiguration-change-replaces-the-activity.

## Evidence
cfn-activity-encryptionconfiguration-change-replaces-the-activity — AssertionError: expected encryptionConfiguration {'type': 'CUSTOMER_MANAGED_KMS_KEY', 'kmsKeyId': 'arn:aws:kms:us-west-2:<acct>:key/<id>', 'kmsDataKeyReusePeriodSeconds': 300} right after create, got {'kmsKeyId': 'arn:aws:kms:us-west-2:<acct>:key/<id>', 'type': 'CUSTOMER_MANAGED_KMS_KEY'}

## Steps to Reproduce
1. Create a CMK in KMS.
2. CreateStack with an AWS::StepFunctions::Activity whose EncryptionConfiguration is {Type: CUSTOMER_MANAGED_KMS_KEY, KmsKeyId: <key arn>, KmsDataKeyReusePeriodSeconds: 300}; wait CREATE_COMPLETE.
3. sfn.describe_activity(activityArn=<the activity arn>) -> expect encryptionConfiguration to carry type, kmsKeyId AND kmsDataKeyReusePeriodSeconds=300.
