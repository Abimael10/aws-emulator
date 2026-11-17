Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__iam__sns.create-user-pool-sms-role-without-sns-publish-rejected
Issue-Key: cognito-idp__iam__sns.create-user-pool-sms-role-without-sns-publish-rejected|_create_pool_tolerating_trust_propagation#6|Timeout|time.sleep(interval_s)
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp__iam__sns: CreateUserPool with an SMS role never settles — the SMS-role binding keeps answering InvalidSmsRoleTrustRelationshipException and no sns:Publish check is ever reached

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserPool

## Description
Cause 1 of 1: binding an SMS role at CreateUserPool never reaches either of AWS's two outcomes.
AWS: Smithy: CreateUserPool.SmsConfiguration {SnsCallerArn, ExternalId}; errors InvalidSmsRoleTrustRelationshipException (400) and InvalidSmsRoleAccessPolicyException (400). AWS probes the role before accepting it: a role whose trust policy lets cognito-idp.amazonaws.com assume it with the given ExternalId is accepted (DescribeUserPool then echoes SnsCallerArn), and a role that trusts Cognito but grants no sns:Publish is refused InvalidSmsRoleAccessPolicyException with the fixed message 'Role does not have permission to publish with SNS', leaving no pool behind (ListUserPools never shows the refused name). InvalidSmsRoleTrustRelationshipException is reserved for a role Cognito cannot assume, and only lags briefly behind IAM propagation.
Emulator: the check makes no progress and is killed at the harness's 60 s budget. Its only retry path is InvalidSmsRoleTrustRelationshipException, so CreateUserPool keeps answering that refusal for a role whose trust policy does name cognito-idp with the matching ExternalId; the access-policy leg (InvalidSmsRoleAccessPolicyException for the role with no sns:Publish) is therefore never reached. Hypothesis: the SMS-role probe does not evaluate the role's trust policy against IAM and refuses unconditionally with the trust-relationship error.
Why: no user pool can be created with SMS enabled, so every SMS/MFA path that depends on an SNS caller role is unreachable, and the one refusal class AWS uses for a permission-less role never appears.
Fix: SmsConfiguration role validation in the CreateUserPool/UpdateUserPool handlers of the cognito-idp crate (resolve the role in IAM, check the trust relationship and the sns:Publish grant, and map the two failures to their distinct exceptions).
Traces: cognito-idp__iam__sns.create-user-pool-sms-role-without-sns-publish-rejected.

## Evidence
Error: Check timed out after 60.0s. The check retries CreateUserPool only while the error code is 'InvalidSmsRoleTrustRelationshipException' (see its _create_pool_tolerating_trust_propagation), so the loop it hung in was that refusal; the assertions it never reached require code 'InvalidSmsRoleAccessPolicyException' with message 'Role does not have permission to publish with SNS' for the role created without an sns:Publish grant, and a successful create plus DescribeUserPool echoing SnsCallerArn for the role that has it.

## Steps to Reproduce
Replay cognito-idp__iam__sns.create-user-pool-sms-role-without-sns-publish-rejected: mint an IAM role trusting cognito-idp.amazonaws.com with an ExternalId and an sns:Publish grant, call CreateUserPool with SmsConfiguration {SnsCallerArn, ExternalId} (expect success and DescribeUserPool echoing the ARN), then mint an identical role without the sns:Publish grant and call CreateUserPool again (expect InvalidSmsRoleAccessPolicyException 'Role does not have permission to publish with SNS' and no pool created).
