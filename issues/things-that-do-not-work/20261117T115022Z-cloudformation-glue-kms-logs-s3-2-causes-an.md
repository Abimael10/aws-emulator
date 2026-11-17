Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-invalid-encryption-mode-rejected-and-rolled-back cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-replacement-updates-the-job-it-encrypts-in-place
Issue-Key: cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-invalid-encryption-mode-rejected-and-rolled-back|run#12|AssertionError|assert failed["StackStatus"] == "ROLLBACK_COMPLETE", (
Issue-Key: glue|GetSecurityConfiguration|EntityNotFoundException
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation__glue__kms__logs__s3: 2 causes — an invalid AWS::Glue::SecurityConfiguration EncryptionMode is accepted, and a stack-created security configuration is unknown to GetSecurityConfiguration

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSecurityConfiguration

## Description
Two net-new root causes in the CloudFormation AWS::Glue::SecurityConfiguration handler.

Cause 1 of 2: an invalid EncryptionMode is accepted instead of failing the create.
AWS: recorded behavior only for the CFN handler; the Glue contract is that S3Encryption.S3EncryptionMode / CloudWatchEncryption.CloudWatchEncryptionMode / JobBookmarksEncryption.JobBookmarksEncryptionMode accept only their modeled enum values, and CreateSecurityConfiguration refuses an unknown one, so the resource create fails and the stack ends ROLLBACK_COMPLETE with a resource-handler failure reason.
Emulator: the stack with the invalid encryption mode reaches CREATE_COMPLETE and the StackStatusReason is None — the value is never validated.
Why: a template declaring encryption the service would reject is reported as a successful deployment, so an operator believes a security configuration is in force that AWS would never have created.
Fix: the AWS::Glue::SecurityConfiguration handler's property validation (enum validation of the three *EncryptionMode members) and the create-failure/rollback path.
Failing check: cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-invalid-encryption-mode-rejected-and-rolled-back

Cause 2 of 2: the security configuration the stack created does not exist in Glue, and its not-found error carries no message.
AWS: recorded behavior only — a CREATE_COMPLETE AWS::Glue::SecurityConfiguration is readable with GetSecurityConfiguration by the name the template gave it, returning the SecurityConfiguration with its EncryptionConfiguration; EntityNotFoundException is AWS's answer only for a name that does not exist, and it always carries a message naming the entity.
Emulator: GetSecurityConfiguration raises EntityNotFoundException for the configuration the stack reports created, and the error's Message is EMPTY ('An error occurred (EntityNotFoundException) when calling the GetSecurityConfiguration operation:'), so the replacement-update scenario (a replacing security configuration updating the job it encrypts in place) can never be observed.
Why: a client cannot verify or re-read what the stack provisioned, and an empty error message tells it nothing about which entity was missing.
Fix: the AWS::Glue::SecurityConfiguration handler's create path (it must write into Glue's own state) and Glue's EntityNotFoundException message construction.
Failing check: cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-replacement-updates-the-job-it-encrypts-in-place

## Evidence
Cause 1: [mismatch] cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-invalid-encryption-mode-rejected-and-rolled-back — AssertionError: create ended 'CREATE_COMPLETE': None; expected StackStatus ROLLBACK_COMPLETE.

Cause 2: [error] cloudformation__glue__kms__logs__s3.cfn-glue-security-configuration-replacement-updates-the-job-it-encrypts-in-place — botocore.errorfactory.EntityNotFoundException: An error occurred (EntityNotFoundException) when calling the GetSecurityConfiguration operation: <empty message>; expected the stack-created security configuration to be returned.

## Steps to Reproduce
1. CreateStack with an AWS::Glue::SecurityConfiguration whose S3Encryption/CloudWatchEncryption mode is not a modeled enum value; poll the stack status (it reaches CREATE_COMPLETE instead of ROLLBACK_COMPLETE).
2. CreateStack with a valid AWS::Glue::SecurityConfiguration plus a Glue job referencing it; when the stack is CREATE_COMPLETE, call GetSecurityConfiguration(Name=<the template's name>).
