Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam.cfn-iam-group-managed-policy-swap-in-place-then-groupname-replacement-retained cloudformation__iam.cfn-iam-group-name-collision-with-preexisting-group-rejected-leaving-it-untouched cloudformation__iam.cfn-iam-policy-no-principal-rejected-then-round-trips-attached-to-user cloudformation__iam.cfn-iam-user-login-profile-and-boundary-update-then-username-replacement-retained
Issue-Key: cloudformation__iam|GetGroup|NoSuchEntity
Issue-Key: cloudformation__iam.cfn-iam-group-name-collision-with-preexisting-group-rejected-leaving-it-untouched|run#11|AssertionError|assert status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {status!r}"
Issue-Key: cloudformation__iam.cfn-iam-policy-no-principal-rejected-then-round-trips-attached-to-user|run#10|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {bad_status!r}"
Issue-Key: cloudformation__iam.cfn-iam-user-login-profile-and-boundary-update-then-username-replacement-retained|_update#25|AssertionError|assert stack["StackStatus"] == "UPDATE_COMPLETE", (
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# cloudformation__iam: 4 net-new causes — a group vanishes on in-place update, and name/principal/login-profile violations are not refused

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Four net-new root causes in the AWS::IAM::* resource handlers. (The open issue 20261109T163411Z-cloudformation-iam-4-net-new-causes-capability-iam.md covers other CAPABILITY_IAM causes in this suite; none of the four below is one of them.)

Cause 1 of 4: an in-place update that swaps an AWS::IAM::Group's managed policy destroys the group.
AWS: Smithy (iam GetGroup -> NoSuchEntityException 404 "The group with name <name> cannot be found."). The recording shows a managed-policy swap on an AWS::IAM::Group is an in-place update: the group keeps its name and GetGroup continues to answer; only a GroupName change is a replacement, and the replaced group is retained.
Emulator: after the update, GetGroup on the group the stack still declares answers NoSuchEntity: `An error occurred (NoSuchEntity) when calling the GetGroup operation: The group with name <group> cannot be found.` (the fixed text is AWS's NoSuchEntity wording; the variable part is the group name from the request).
Why: an update that should change only an attachment deletes the principal; everything attached to the group (users, policies) loses its grants and the stack's declared state no longer exists.
Fix: the AWS::IAM::Group resource handler's update path (attachment change = in-place AttachGroupPolicy/DetachGroupPolicy, not delete+create).
Traces: cloudformation__iam.cfn-iam-group-managed-policy-swap-in-place-then-groupname-replacement-retained.

Cause 2 of 4: an AWS::IAM::Group whose GroupName collides with a pre-existing group is created instead of refused.
AWS: Smithy iam CreateGroup declares EntityAlreadyExistsException (409, awsQueryError EntityAlreadyExists); the recording shows the resource create failing on the collision and the stack settling ROLLBACK_COMPLETE with the pre-existing group untouched.
Emulator: the stack reaches `CREATE_COMPLETE` (`AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'`), so a name already in use is silently adopted or duplicated.
Why: two stacks can "own" one identity; a later stack delete removes a group another stack still relies on.
Fix: the AWS::IAM::Group create handler — fail the resource on EntityAlreadyExists so the stack rolls back.
Traces: cloudformation__iam.cfn-iam-group-name-collision-with-preexisting-group-rejected-leaving-it-untouched.

Cause 3 of 4: an AWS::IAM::Policy with no principal attached is accepted.
AWS: recorded behavior only for the resource; the recording shows AWS::IAM::Policy requires at least one of Users, Groups or Roles and the create fails without one, so the stack settles ROLLBACK_COMPLETE; the same template with a user attached round-trips.
Emulator: the no-principal stack reaches `CREATE_COMPLETE` (`AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'`).
Why: a template that grants nothing to anybody deploys as a success, so a client believes a permission is in place that was never attached to any principal.
Fix: the AWS::IAM::Policy resource handler's required-property validation (Users/Groups/Roles).
Traces: cloudformation__iam.cfn-iam-policy-no-principal-rejected-then-round-trips-attached-to-user.

Cause 4 of 4: a stack update that sets an AWS::IAM::User's LoginProfile and PermissionsBoundary fails and rolls the stack back.
AWS: recorded behavior only — the recording shows the update applying in place and settling UPDATE_COMPLETE, after which the login profile and the boundary read back, and a later UserName change is a retained replacement.
Emulator: the User resource reports UPDATE_FAILED and the stack settles `UPDATE_ROLLBACK_COMPLETE` with reason `The following resource(s) failed to update: [User].`; the event trail shows User UPDATE_FAILED followed by a retry that reports UPDATE_COMPLETE before the stack rolls back anyway.
Why: a client cannot add console access or a permissions boundary to a deployed user through CloudFormation at all; the stack is rolled back to its prior state.
Fix: the AWS::IAM::User resource handler's update path (LoginProfile create/update and PermissionsBoundary set on an existing user) and the status it reports to the stack engine.
Traces: cloudformation__iam.cfn-iam-user-login-profile-and-boundary-update-then-username-replacement-retained.

## Evidence
Cause 1: `botocore.errorfactory.NoSuchEntityException: An error occurred (NoSuchEntity) when calling the GetGroup operation: The group with name sim2real-cfnreal-grp-a-1fb4e0aa75d2 cannot be found.` (expected: the group still exists after the in-place managed-policy swap).
Cause 2: `AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'` (AWS::IAM::Group name collides with a pre-existing group; expected EntityAlreadyExists -> rollback).
Cause 3: `AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'` (AWS::IAM::Policy with no Users/Groups/Roles).
Cause 4: `AssertionError: update ended 'UPDATE_ROLLBACK_COMPLETE': 'The following resource(s) failed to update: [User]. '; events [... ('User', 'UPDATE_FAILED'), ('User', 'UPDATE_IN_PROGRESS'), ('User', 'UPDATE_COMPLETE'), ... ('UPDATE_ROLLBACK_COMPLETE')]` (expected UPDATE_COMPLETE).

## Steps to Reproduce
Cause 1: deploy a stack with an AWS::IAM::Group + ManagedPolicyArns; UpdateStack swapping the managed policy ARN; call GetGroup(GroupName=<same name>). Observed NoSuchEntity; expected the group.
Cause 2: CreateGroup out of band, then CreateStack with an AWS::IAM::Group using that same GroupName. Observed CREATE_COMPLETE; expected ROLLBACK_COMPLETE with the original group untouched.
Cause 3: CreateStack with an AWS::IAM::Policy that names no Users, Groups or Roles. Observed CREATE_COMPLETE; expected ROLLBACK_COMPLETE.
Cause 4: deploy a stack with an AWS::IAM::User, then UpdateStack adding LoginProfile and PermissionsBoundary. Observed UPDATE_FAILED on [User] -> UPDATE_ROLLBACK_COMPLETE; expected UPDATE_COMPLETE.
