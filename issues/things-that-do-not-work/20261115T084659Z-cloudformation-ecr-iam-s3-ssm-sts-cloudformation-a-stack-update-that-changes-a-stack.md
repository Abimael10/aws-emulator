Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place
Issue-Key: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place|_assert_modern_toolkit#17|AssertionError|assert _user_tags(ssm_tags.get("TagList", [])) == user_tags, (
Issue-Commit: c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9

# cloudformation: a stack update that changes a stack-level tag VALUE does not re-tag already-created resources — the AWS::SSM::Parameter keeps the old tag value

> _Note: this issue was identified against the code base at commit `c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
This issue carries 1 cause.

Cause 1 of 1: an in-place stack update that changes the VALUE of a stack-level tag leaves the already-provisioned resources carrying the previous value.
- AWS: CloudFormation propagates the stack's Tags to every resource type that supports tagging, and re-applies them on each update: after UpdateStack with the same tag key bound to a new value, the resource's own tag listing reports the NEW value, alongside the three CloudFormation-managed tags aws:cloudformation:logical-id, aws:cloudformation:stack-id and aws:cloudformation:stack-name. For the resource exercised here the read-back is SSM ListTagsForResource(ResourceType='Parameter', ResourceId=<parameter name>) — Smithy: ListTagsForResource -> TagList of Tag{Key,Value}; the model states nothing about propagation, so the propagation rule itself is recorded behavior only. The requirement is that the user tag's value after the update equals the value in the updated template/stack tags; the key, the value text, the parameter name and the stack id are all request- and state-derived.
- Emulator: after the update the parameter still lists the pre-update value for that key (user tags read Foo=Bar where the update set Foo=BarBaz), while the aws:cloudformation:* tags are present and correct. So tags are applied at CREATE time and never reconciled on UPDATE.
- Why: tags are load-bearing for cost allocation, ABAC authorization (aws:ResourceTag conditions) and discovery. A client that re-deploys to change a tag value sees the stack reach UPDATE_COMPLETE while the resource keeps the stale value, so an ABAC policy keyed on the new value silently denies, and any tag-driven inventory is wrong with no error anywhere to signal it.
- Fix: the CloudFormation stack-update resource reconciliation path — on an update, re-apply the stack's effective tag set to every already-existing taggable resource (for AWS::SSM::Parameter, via the SSM parameter tag state), not only to resources being created or replaced.
- Traces: cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place.

## Evidence
Behavioral suite replay of this checkout (246/585 passed, bootstrap_signal=false). Service heading 'cloudformation__ecr__iam__s3__ssm__sts': 5/6 passed, 1 failed — the failing check was marked neither 'ALREADY FILED' nor 'OWNED by'.

cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place [mismatch]:
  AssertionError: '/cdk-bootstrap/<qualifier>/version' tags are [{'Key': 'Foo', 'Value': 'Bar'}, {'Key': 'aws:cloudformation:logical-id', 'Value': 'CdkBootstrapVersion'}, {'Key': 'aws:cloudformation:stack-id', 'Value': 'arn:aws:cloudformation:<region>:<account>:stack/<stack-name>/<uuid>'}, {'Key': 'aws:cloudformation:stack-name', 'Value': '<stack-name>'}, {'Key': 'sim2real', 'Value': '<check-name>'}], expected [('Foo', 'BarBaz'), ('sim2real', '<check-name>')]

The re-bootstrap re-deployed the same toolkit stack with the tag Foo changed from 'Bar' to 'BarBaz'. The emulator's parameter still carries Foo=Bar. The qualifier, stack name, stack id/uuid, region and account in that listing are per-run values; the divergence is the stale tag VALUE for the key the update changed. (The aws:cloudformation:* tags are correctly present and are filtered out of the user-tag comparison.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite check cloudformation__ecr__iam__s3__ssm__sts.cdk-bootstrap-re-bootstrap-with-a-new-tag-value-updates-every-toolkit-resource-in-place.
2. Equivalently, by hand: create a stack containing an AWS::SSM::Parameter with stack-level tags {Foo: Bar, <probe-key>: <probe-value>}; wait for CREATE_COMPLETE.
3. Update the SAME stack with the same template but stack tags {Foo: BarBaz, <probe-key>: <probe-value>}; wait for UPDATE_COMPLETE.
4. Call ssm ListTagsForResource(ResourceType='Parameter', ResourceId=<the parameter's name>) and read the tag whose Key is 'Foo'. Expect Value 'BarBaz'; observe 'Bar'.
