Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__transfer.deploy-transfer-service-managed-user-exists-after-create-and-gone-after-stack-delete cloudformation__iam__transfer.deploy-transfer-user-duplicate-username-on-server-rejected
Issue-Key: cloudformation__iam__transfer|DescribeUser|ResourceNotFoundException
Issue-Key: cloudformation__iam__transfer.deploy-transfer-user-duplicate-username-on-server-rejected|run#18|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# AWS::Transfer::User is never created in Transfer state, and a duplicate username on the same server is accepted

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUser

## Description
Cause 1 of 2: a Transfer user deployed by a stack does not exist in Transfer.
AWS: Smithy (transfer DescribeUser -> ResourceNotFoundException for an unknown user; CreateUser writes the user under ServerId). The recording shows that after the stack reaches CREATE_COMPLETE, DescribeUser(ServerId, UserName) answers with the user's Role, HomeDirectory and ARN, and only after DeleteStack does it answer ResourceNotFoundException.
Emulator: DescribeUser on the user the stack just created raises `ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeUser operation: Unknown user` — the resource is reported as created but no user is written to Transfer state.
Why: a client cannot read back, update or use a user CloudFormation says it created; no SFTP identity exists behind the stack's declared resource.
Fix: the AWS::Transfer::User resource handler — it must perform the transfer CreateUser state write so DescribeUser/ListUsers see the user (and delete it on stack delete).
Traces: cloudformation__iam__transfer.deploy-transfer-service-managed-user-exists-after-create-and-gone-after-stack-delete.

Cause 2 of 2: a second AWS::Transfer::User with a UserName already present on the same server is accepted.
AWS: Smithy transfer CreateUser declares ResourceExistsException; a username is unique per ServerId, so the duplicate create fails and the stack settles ROLLBACK_COMPLETE.
Emulator: the stack settles `CREATE_COMPLETE` (`AssertionError: rejected stack ended 'CREATE_COMPLETE'`).
Why: two stacks/resources claim one SFTP identity; the first user's configuration can be overwritten or deleted by the second stack without any warning at deploy time.
Fix: CreateUser's uniqueness check per (ServerId, UserName) and the CFN handler surfacing it as a resource failure.
Traces: cloudformation__iam__transfer.deploy-transfer-user-duplicate-username-on-server-rejected.

## Evidence
Cause 1: `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeUser operation: Unknown user` — raised for the user the stack reported as created (expected: the user is described successfully, and only gone after DeleteStack).
Cause 2: `AssertionError: rejected stack ended 'CREATE_COMPLETE'` (expected ROLLBACK_COMPLETE for a duplicate UserName on the same ServerId).

## Steps to Reproduce
Cause 1: CreateStack with an AWS::Transfer::Server and a service-managed AWS::Transfer::User; after CREATE_COMPLETE call DescribeUser(ServerId=<server>, UserName=<user>). Observed ResourceNotFoundException "Unknown user"; expected the user. Then DeleteStack and confirm it is gone.
Cause 2: with that user in place, CreateStack a second AWS::Transfer::User with the same UserName on the same ServerId. Observed CREATE_COMPLETE; expected the create to be refused and the stack to settle ROLLBACK_COMPLETE.
