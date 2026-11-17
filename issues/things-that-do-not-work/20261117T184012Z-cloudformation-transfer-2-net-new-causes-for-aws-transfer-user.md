Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__transfer.deploy-transfer-user-nonexistent-server-id-rolled-back-and-never-readable cloudformation__transfer.deploy-transfer-user-role-and-home-directory-round-trip-and-delete cloudformation__transfer.deploy-transfer-user-update-adds-key-and-tags-then-rename-replaces-it cloudformation__transfer.deploy-transfer-user-duplicate-username-on-one-server-rejected-and-rolled-back
Issue-Key: cloudformation__transfer|DescribeUser|ResourceNotFoundException
Issue-Key: cloudformation__transfer.deploy-transfer-user-duplicate-username-on-one-server-rejected-and-rolled-back|run#6|AssertionError|assert status == "ROLLBACK_COMPLETE", f"stack ended {status!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 2 net-new causes for AWS::Transfer::User: the user is never created in Transfer (DescribeUser answers "Unknown user"), and a duplicate UserName on one server is accepted

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeUser

## Description
Two distinct defects in the CloudFormation AWS::Transfer::User resource handler. Each cause has its own failing check ids.

Cause 1 of 2: a user a CloudFormation stack reports as created does not exist in AWS Transfer.
AWS: recorded behavior only. A stack declaring an AWS::Transfer::User creates the user on the named server, so transfer DescribeUser(ServerId, UserName) answers with the user's Role, HomeDirectory, SshPublicKeys and Tags, and ListUsers includes it; only an absent user is refused ResourceNotFoundException. Note that AWS distinguishes the two not-found classes: DescribeUser against a ServerId that does not exist is refused ResourceNotFoundException with the message `Unknown server`, not `Unknown user`.
Emulator: DescribeUser raises ResourceNotFoundException `Unknown user` while the owning stack is CREATE_COMPLETE — the handler reports success without creating the user — and it also answers `Unknown user` where AWS answers `Unknown server` for a nonexistent ServerId, so the two classes cannot be told apart.
Why: a client that deploys an SFTP user through CloudFormation and then configures or authenticates it cannot find the user the stack claims to own; updates, renames (replacement) and tag changes on it are all unreachable, and the wrong not-found message makes a bad ServerId look like a bad UserName. The open issue 20261117T135333Z-cloudformation-iam-transfer-aws-transfer-user-is-never-created-in-transfer.md reports the same handler under the cloudformation__iam__transfer heading; it does not carry these checks.
Fix: the AWS::Transfer::User resource handler's Create/Update/Delete (it must call transfer CreateUser and record the user), and the transfer DescribeUser not-found message selection (`Unknown server` for an absent ServerId).
Traces: cloudformation__transfer.deploy-transfer-user-nonexistent-server-id-rolled-back-and-never-readable, cloudformation__transfer.deploy-transfer-user-role-and-home-directory-round-trip-and-delete, cloudformation__transfer.deploy-transfer-user-update-adds-key-and-tags-then-rename-replaces-it

Cause 2 of 2: two AWS::Transfer::User resources with the same UserName on one server are accepted.
AWS: recorded behavior only. A UserName is unique per server, so the second user's create is refused, the resource fails and the stack settles at ROLLBACK_COMPLETE; after the rollback DescribeUser for that username on that server is refused.
Emulator: the stack reaches CREATE_COMPLETE, so a duplicate username is accepted and the uniqueness rule the server enforces is lost.
Why: a template AWS refuses deploys here, and the server ends up with an ambiguous user set whose later behaviour (authentication, home directory mapping) cannot match AWS.
Fix: the AWS::Transfer::User resource handler's Create (uniqueness of UserName within a ServerId) and the transfer CreateUser state that must enforce it.
Traces: cloudformation__transfer.deploy-transfer-user-duplicate-username-on-one-server-rejected-and-rolled-back

## Evidence
Cause 1: three checks fail with `botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeUser operation: Unknown user` on a user whose stack is CREATE_COMPLETE; the nonexistent-server check additionally expects ('ResourceNotFoundException', 'Unknown server') for DescribeUser against ServerId 's-00000000000000000'.
Cause 2: AssertionError: stack ended 'CREATE_COMPLETE' (expected ROLLBACK_COMPLETE) for a template declaring two AWS::Transfer::User resources with the same UserName on one server.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. cloudformation__transfer.deploy-transfer-user-nonexistent-server-id-rolled-back-and-never-readable
2. cloudformation__transfer.deploy-transfer-user-role-and-home-directory-round-trip-and-delete
3. cloudformation__transfer.deploy-transfer-user-update-adds-key-and-tags-then-rename-replaces-it
4. cloudformation__transfer.deploy-transfer-user-duplicate-username-on-one-server-rejected-and-rolled-back
