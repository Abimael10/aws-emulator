Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam.request-spot-fleet-overrides-instance-type-and-requirements-conflict-rejected
Issue-Key: ec2|RequestSpotFleet|InvalidParameterCombinationException
Issue-Commit: 07a12ce4140be4c807d532cfc03706c508ac8640

# ec2 RequestSpotFleet: the InstanceType/InstanceRequirements conflict message re-words AWS's fixed text ("InstanceType" instead of "Instance Type")

> _Note: this issue was identified against the code base at commit `07a12ce4140be4c807d532cfc03706c508ac8640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RequestSpotFleet

## Description
Cause 1 of 1: RequestSpotFleet refuses a LaunchTemplateConfigs.Overrides entry that carries both InstanceType and InstanceRequirements with the right HTTP status and the right error code, but its message text is not AWS's.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2 model) documents the mutual exclusion on LaunchTemplateOverrides.InstanceRequirements: "If you specify InstanceRequirements, you can't specify InstanceType." RequestSpotFleet has no modeled errors (ec2Query), so the status/code/message are recorded behavior only: the recording shows an HTTP 400 with error code `InvalidParameterCombinationException` and the message "You can specify either InstanceRequirements or InstanceType, but not both. Remove either InstanceRequirements or Instance Type, and try again." Every word of that message is fixed text — it interpolates nothing from the request or from state (no member name, id, name or limit is substituted), so the whole string is the requirement.

Emulator: same status and same error code, but the message's second sentence is "Remove either InstanceRequirements or InstanceType, and try again." — the emulator writes the second mention as the API member spelling `InstanceType` where AWS writes the two-word prose form `Instance Type`. Replay diff: expected ('InvalidParameterCombinationException', '... Remove either InstanceRequirements or Instance Type, and try again.'), got ('InvalidParameterCombinationException', '... Remove either InstanceRequirements or InstanceType, and try again.').

Why: an error message is part of the contract. A client (or a test, a log assertion, a support runbook, a message-matching retry rule) that compares the refusal against AWS's wording sees an unknown message and cannot classify the refusal, even though the code is right. Nothing in the diverging part is per-run, so the divergence is deterministic.

Fix: the RequestSpotFleet handler's request validation — the branch that rejects an Overrides entry carrying both InstanceType and InstanceRequirements must emit AWS's fixed sentence verbatim, including the two-word "Instance Type" in the remediation clause.

Traces: ec2__iam.request-spot-fleet-overrides-instance-type-and-requirements-conflict-rejected (mismatch).

## Evidence
Check: ec2__iam.request-spot-fleet-overrides-instance-type-and-requirements-conflict-rejected

AssertionError at expect_error(lambda: ec2.request_spot_fleet(SpotFleetRequestConfig=bad_config), *_CONFLICT_ERR):

Expected (AWS): ('InvalidParameterCombinationException', 'You can specify either InstanceRequirements or InstanceType, but not both. Remove either InstanceRequirements or Instance Type, and try again.')
Actual (emulator): ('InvalidParameterCombinationException', 'You can specify either InstanceRequirements or InstanceType, but not both. Remove either InstanceRequirements or InstanceType, and try again.')

Fixed text differing: "Remove either InstanceRequirements or Instance Type, and try again." (AWS) vs "Remove either InstanceRequirements or InstanceType, and try again." (emulator). No part of this message is taken from the request or from emulator state, so there is no per-run value involved.

Model citation: Smithy LaunchTemplateOverrides.InstanceRequirements — "If you specify InstanceRequirements, you can't specify InstanceType." RequestSpotFleet declares no modeled errors, so code/message come from the recording.

## Steps to Reproduce
1. Create an IAM spot-fleet role (spotfleet.amazonaws.com trust) and a launch template with a valid AL2023 ImageId.
2. Call ec2:RequestSpotFleet with SpotFleetRequestConfig.LaunchTemplateConfigs[0].Overrides[0] carrying BOTH InstanceType ("t3.micro") and InstanceRequirements ({VCpuCount:{Min:1,Max:2}, MemoryMiB:{Min:512,Max:2048}}).
3. Observe the refusal: the code is InvalidParameterCombinationException (correct) but the message's remediation clause says "Remove either InstanceRequirements or InstanceType, and try again." instead of AWS's "Remove either InstanceRequirements or Instance Type, and try again."
