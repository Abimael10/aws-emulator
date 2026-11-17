Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__iot__lambda.cfn-iot-authorizer-invalid-status-value-rejected cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-disabled-replacement-removed-by-delete-stack cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-enabled-missing-token-key-rejected
Issue-Key: cloudformation__iam__iot__lambda.cfn-iot-authorizer-invalid-status-value-rejected|_split_handler_rejection#13|AssertionError|assert message == expected_message, f"expected handler message {expected_message!r}, got {message!r}"
Issue-Key: cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-disabled-replacement-removed-by-delete-stack|run#23|AssertionError|assert updated["StackStatus"] == "UPDATE_COMPLETE", f"update ended in {updated['StackStatus']!r}"
Issue-Key: cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-enabled-missing-token-key-rejected|run#13|AssertionError|assert stack["StackStatus"] == "ROLLBACK_COMPLETE", (
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 3 causes in CloudFormation's AWS::IoT::Authorizer handler: the enum refusal uses the service's API wording, a signing-disabled replacement update rolls back, and a missing TokenKeyName is accepted

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three distinct root causes in the CloudFormation resource handler for AWS::IoT::Authorizer.

Cause 1 of 3: an illegal `Status` value is refused with the IoT API's own parameter-validation message instead of CloudFormation's resource-schema validation message.
AWS: CloudFormation validates a resource's properties against its registered resource schema BEFORE calling the service, so a `Status` outside the schema's enum is refused by the handler with the schema wording: fixed text `Model validation failed (#/Status: #: only 1 subschema matches out of 2)` + newline + `#/Status: failed validation constraint for keyword [enum] (#/Status)`, whose only variable part is the property pointer (`#/Status`). The service's own `1 validation error detected: ...` wording belongs to a direct iot CreateAuthorizer call, not to a stack operation. (Smithy: iot CreateAuthorizer input member `status` targets AuthorizerStatus (enum ACTIVE|INACTIVE); the CloudFormation resource-schema validation stage is documented behavior — recorded behavior only for the message template.)
Emulator: the handler message is the service's parameter-validation wording, with the rejected value and member name taken from the request.
Why: a client reading a stack event's failure reason cannot tell schema rejection from service rejection, and tooling that matches CloudFormation's `Model validation failed` prefix to classify a template error does not fire.
Fix: the AWS::IoT::Authorizer resource handler's property-validation stage — enum/property validation must run against the resource schema and produce the `Model validation failed (...)` message before the iot client is called.
Traces: cloudformation__iam__iot__lambda.cfn-iot-authorizer-invalid-status-value-rejected

Cause 2 of 3: a stack update that replaces an AWS::IoT::Authorizer with a signing-disabled one ends UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE.
AWS: toggling `SigningDisabled` on AWS::IoT::Authorizer is a replacement update; CloudFormation creates the replacement, repoints references and deletes the old authorizer, and the stack reaches `UPDATE_COMPLETE`. (Smithy: cloudformation DescribeStacks output member `Stacks[].StackStatus`, enum StackStatus values UPDATE_COMPLETE / UPDATE_ROLLBACK_COMPLETE; iot CreateAuthorizer input member `signingDisabled`.)
Emulator: the update ends `'UPDATE_ROLLBACK_COMPLETE'` — the replacement could not be completed, so the stack is rolled back to the previous authorizer.
Why: a client cannot change an authorizer's signing mode through a stack update at all; the update appears to fail for a template real CloudFormation applies, and the subsequent delete-stack assertions (the replacement being removed with the stack) can never be reached.
Fix: the AWS::IoT::Authorizer resource handler's update/replacement path in the cloudformation service, and whichever iot call in it is failing for a signing-disabled create.
Traces: cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-disabled-replacement-removed-by-delete-stack

Cause 3 of 3: an AWS::IoT::Authorizer with signing enabled but no `TokenKeyName` is accepted — the stack reaches CREATE_COMPLETE instead of rolling back.
AWS: with signing enabled (`SigningDisabled` absent or false) `TokenKeyName` and `TokenSigningPublicKeys` are required; omitting `TokenKeyName` makes the create fail, so the stack ends `ROLLBACK_COMPLETE` and no authorizer exists. (Smithy: iot CreateAuthorizer input members `tokenKeyName`, `tokenSigningPublicKeys`, `signingDisabled`; error InvalidRequestException, httpError 400. The required-together rule is documented behavior — recorded behavior only for the pairing.)
Emulator: the stack status is `'CREATE_COMPLETE'`.
Why: a template that would be refused by AWS deploys an authorizer that cannot verify any token's signature. A client validating its IoT custom-authorizer configuration gets a green deploy and a silently unusable authorizer.
Fix: the AWS::IoT::Authorizer resource handler's create-time validation (and/or the iot CreateAuthorizer handler's required-member check) for the signing-enabled/TokenKeyName pairing.
Traces: cloudformation__iam__iot__lambda.cfn-iot-authorizer-signing-enabled-missing-token-key-rejected

## Evidence
Cause 1 — actual, verbatim:
  AssertionError: expected handler message 'Model validation failed (#/Status: #: only 1 subschema matches out of 2)\n#/Status: failed validation constraint for keyword [enum] (#/Status)', got "1 validation error detected: Value 'ENABLED' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [ACTIVE, INACTIVE]"
(The expected text is entirely fixed; in the actual message, 'ENABLED' is the value from the request and 'status' the member name.)
Cause 2 — actual, verbatim:
  AssertionError: update ended in 'UPDATE_ROLLBACK_COMPLETE'
(expected StackStatus 'UPDATE_COMPLETE').
Cause 3 — actual, verbatim:
  AssertionError: expected the missing-TokenKeyName create to roll back, got 'CREATE_COMPLETE'

## Steps to Reproduce
Cause 1: CreateStack with an AWS::IoT::Authorizer whose `Status: ENABLED` (not in the enum). Read the resource's failure reason from DescribeStackEvents. Observed the iot API's `1 validation error detected: ...` wording; expected CloudFormation's `Model validation failed (#/Status: #: only 1 subschema matches out of 2)\n#/Status: failed validation constraint for keyword [enum] (#/Status)`.
Cause 2: CreateStack with an AWS::IoT::Authorizer, then UpdateStack flipping it to a signing-disabled authorizer. Observed StackStatus 'UPDATE_ROLLBACK_COMPLETE'; expected 'UPDATE_COMPLETE', then DeleteStack removing the replacement.
Cause 3: CreateStack with an AWS::IoT::Authorizer that has signing enabled and no `TokenKeyName`. Observed StackStatus 'CREATE_COMPLETE'; expected 'ROLLBACK_COMPLETE' with no authorizer created.
