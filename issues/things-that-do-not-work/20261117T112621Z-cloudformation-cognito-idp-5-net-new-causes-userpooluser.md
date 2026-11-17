Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cognito-idp.cfn-cognito-resource-server-and-user-stay-create-complete-after-out-of-band-delete cloudformation__cognito-idp.cfn-user-pool-user-custom-named-replacement-rejected-then-succeeds-with-new-username cloudformation__cognito-idp.cfn-user-pool-email-verification-message-and-subject-round-trip cloudformation__cognito-idp.cfn-user-pool-sms-verification-message-round-trip cloudformation__cognito-idp.cfn-cognito-user-pool-group-name-collision-rolls-back cloudformation__cognito-idp.cfn-user-pool-resource-server-duplicate-identifier-rejected-rollback cloudformation__cognito-idp.cfn-user-pool-client-mixed-legacy-and-allow-auth-flows-update-rejected-rollback cloudformation__cognito-idp.cfn-user-pool-verification-message-template-email-message-invalid-pattern-rejected
Issue-Key: cognito-identity-provider|AdminGetUser|UserNotFoundException
Issue-Key: cloudformation__cognito-idp.cfn-user-pool-email-verification-message-and-subject-round-trip|run#12|KeyError|assert pool["EmailVerificationMessage"] == email_message, (
Issue-Key: cloudformation__cognito-idp.cfn-user-pool-sms-verification-message-round-trip|run#11|KeyError|assert pool["SmsVerificationMessage"] == sms_message, (
Issue-Key: cloudformation__cognito-idp.cfn-cognito-user-pool-group-name-collision-rolls-back|run#15|AssertionError|assert len(validation_events) == 1, (
Issue-Key: cloudformation__cognito-idp.cfn-user-pool-resource-server-duplicate-identifier-rejected-rollback|run#13|AssertionError|assert failed == [(duplicate_stack_name, expected_reason)], (
Issue-Key: cloudformation__cognito-idp.cfn-user-pool-client-mixed-legacy-and-allow-auth-flows-update-rejected-rollback|run#17|AssertionError|assert status == "UPDATE_ROLLBACK_COMPLETE", f"mixed-auth-flows update ended {status!r}"
Issue-Key: cloudformation__cognito-idp.cfn-user-pool-verification-message-template-email-message-invalid-pattern-rejected|_assert_invalid_email_message_reason#9|AssertionError|assert re.fullmatch(pattern, reason), (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation__cognito-idp: 5 net-new causes — UserPoolUser never provisioned, verification messages lost, no pre-provision name-conflict validation, mixed auth flows accepted, handler reason omits the service suffix

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Five distinct root causes in CloudFormation's AWS::Cognito::* provisioning. Each cause below must be closed before submitting.

Cause 1 of 5: an AWS::Cognito::UserPoolUser in a CREATE_COMPLETE stack does not exist in the pool.
AWS: a stack that declares AWS::Cognito::UserPoolUser creates the user in the referenced pool; after CREATE_COMPLETE AdminGetUser(UserPoolId=<pool>, Username=<the template's Username>) answers 200 with that Username (Smithy: AdminGetUser -> Username required; UserNotFoundException is a 404 reserved for a user that does not exist).
Emulator: the stack reports CREATE_COMPLETE and the resource reports CREATE_COMPLETE, but the positive-control AdminGetUser raises UserNotFoundException 'User does not exist.' — the user was never written to the pool.
Why: every template-driven user (seed accounts, test fixtures, group memberships) is absent while CloudFormation reports success, so dependent stack resources and later reads fail.
Fix: the AWS::Cognito::UserPoolUser resource handler in the CloudFormation provisioning layer (it must call cognito-idp AdminCreateUser and record the physical id).
Traces: cloudformation__cognito-idp.cfn-cognito-resource-server-and-user-stay-create-complete-after-out-of-band-delete, cloudformation__cognito-idp.cfn-user-pool-user-custom-named-replacement-rejected-then-succeeds-with-new-username.

Cause 2 of 5: verification-message properties set by the template are not readable on the pool.
AWS: Smithy (cognito-idp): UserPool carries EmailVerificationMessage (@length 6..20000, @pattern containing {####}), EmailVerificationSubject (@length 1..140) and SmsVerificationMessage (@length 6..140, @pattern \{####\}). A stack that sets them on AWS::Cognito::UserPool leaves DescribeUserPool reporting each value verbatim.
Emulator: DescribeUserPool's UserPool has no EmailVerificationMessage/EmailVerificationSubject and no SmsVerificationMessage member at all after the stack completes — the checks die with KeyError on those keys.
Why: a client reading back its own configuration (or a template round-trip/drift comparison) finds the property missing, and the user-visible verification text is not what the template declared.
Fix: property pass-through in the AWS::Cognito::UserPool resource handler and/or the DescribeUserPool projection in the cognito-idp crate.
Traces: cloudformation__cognito-idp.cfn-user-pool-email-verification-message-and-subject-round-trip, cloudformation__cognito-idp.cfn-user-pool-sms-verification-message-round-trip.

Cause 3 of 5: there is no pre-provision validation stage, so name conflicts are reported at the wrong level and DescribeEvents carries no VALIDATION_ERROR.
AWS: a stack declaring a resource whose name is already taken is refused before provisioning: DescribeEvents reports exactly one operation event with EventType 'VALIDATION_ERROR', LogicalResourceId of the offending resource, ValidationName 'NAME_CONFLICT_VALIDATION', ValidationStatusReason "Resource of type '<AWS::Cognito::UserPoolGroup>' with identifier '<poolId>|<groupName>' already exists." and an empty PhysicalResourceId; the stack ends ROLLBACK_COMPLETE with no stack resources provisioned, and the only CREATE_FAILED event is on the stack itself with the reason 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.' (fixed text; the error count is the variable part).
Emulator: no VALIDATION_ERROR operation event is emitted at all (0 found for the Group), and the duplicate resource-server stack instead fails at the resource level with CREATE_FAILED on logical id 'ResourceServer' and the reason 'Resource handler returned message: "<identifier> already exists in user pool <poolId>." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'.
Why: a client that reads DescribeEvents to learn why a stack rolled back (the mechanism the stack-level reason points it to) finds nothing, and the failure is attributed to a resource that AWS never attempted.
Fix: the pre-provision validation stage of CreateStack and its operation-event recording in the CloudFormation layer (name-conflict validation for AWS::Cognito::UserPoolGroup and AWS::Cognito::UserPoolResourceServer).
Traces: cloudformation__cognito-idp.cfn-cognito-user-pool-group-name-collision-rolls-back, cloudformation__cognito-idp.cfn-user-pool-resource-server-duplicate-identifier-rejected-rollback.

Cause 4 of 5: an update mixing legacy ExplicitAuthFlows with ALLOW_* flows is accepted.
AWS: cognito-idp refuses an app client whose ExplicitAuthFlows mixes the legacy values (e.g. USER_PASSWORD_AUTH, ADMIN_NO_SRP_AUTH) with the prefixed ALLOW_* values with InvalidParameterException (400); a stack update that declares such a client therefore fails and the stack ends UPDATE_ROLLBACK_COMPLETE with the client left at its previous configuration.
Emulator: the update is accepted and the stack ends 'UPDATE_COMPLETE'.
Why: a template AWS rejects deploys clean here, so the configuration drift is only discovered against real AWS.
Fix: ExplicitAuthFlows validation in the CreateUserPoolClient/UpdateUserPoolClient handlers of the cognito-idp crate (surfaced through the AWS::Cognito::UserPoolClient resource handler).
Traces: cloudformation__cognito-idp.cfn-user-pool-client-mixed-legacy-and-allow-auth-flows-update-rejected-rollback.

Cause 5 of 5: a resource-handler failure reason omits the service exception's own suffix.
AWS: when a Cognito API call fails, the CREATE_FAILED ResourceStatusReason is 'Resource handler returned message: "<the service message> (Service: CognitoIdentityProvider, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'. The fixed parts are the wrapper text, the service name CognitoIdentityProvider, the status code and the HandlerErrorCode; the variable parts are the inner service message (here the constraint report for verificationMessageTemplate.emailMessage), the request id, the attempt count and the request token.
Emulator: the reason is 'Resource handler returned message: "1 validation error detected: Value ... failed to satisfy constraint: Member must satisfy regular expression pattern: ..." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' — the '(Service: CognitoIdentityProvider, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)' segment is missing.
Why: tooling that parses the reason to recover the failing service, status and request id (the documented shape) cannot.
Fix: resource-handler error-reason formatting in the CloudFormation layer.
Traces: cloudformation__cognito-idp.cfn-user-pool-verification-message-template-email-message-invalid-pattern-rejected.

## Evidence
Cause 1: botocore.errorfactory.UserNotFoundException: An error occurred (UserNotFoundException) when calling the AdminGetUser operation: User does not exist. — raised by the positive control right after `assert status == 'CREATE_COMPLETE'`.
Cause 2: KeyError: 'EmailVerificationMessage' and KeyError: 'SmsVerificationMessage' on the DescribeUserPool result.
Cause 3: 'expected exactly one VALIDATION_ERROR event for Group, got 0'; and "expected exactly one CREATE_FAILED event on the stack itself with reason 'Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.', got [('ResourceServer', 'Resource handler returned message: \"<identifier> already exists in user pool <poolId>.\" (RequestToken: ..., HandlerErrorCode: InvalidRequest)')]".
Cause 4: "mixed-auth-flows update ended 'UPDATE_COMPLETE'" (expected 'UPDATE_ROLLBACK_COMPLETE').
Cause 5: reason matched neither the expected shape; the emulator's text lacks ' (Service: CognitoIdentityProvider, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)' inside the quoted handler message.

## Steps to Reproduce
Re-replay the checks in trace_ids. In outline: (1) deploy a stack with AWS::Cognito::UserPool + ::UserPoolResourceServer + ::UserPoolUser, wait for CREATE_COMPLETE and call AdminGetUser for the template's Username; (2) deploy a pool whose EmailVerificationMessage/EmailVerificationSubject/SmsVerificationMessage are set and read DescribeUserPool; (3) create a group out of band, then deploy a stack declaring AWS::Cognito::UserPoolGroup with the same GroupName and read DescribeEvents; deploy a second stack with an already-used resource-server Identifier and read DescribeStackEvents; (4) update a stack's AWS::Cognito::UserPoolClient to ExplicitAuthFlows mixing USER_PASSWORD_AUTH with ALLOW_* values; (5) deploy a pool whose VerificationMessageTemplate.EmailMessage omits {####} and read the CREATE_FAILED ResourceStatusReason.
