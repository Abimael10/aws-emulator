Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: imagebuilder.create-infrastructure-configuration-invalid-instance-profile-rejected
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder.create-infrastructure-configuration-invalid-instance-profile-rejected|run#7|AssertionError|assert msg == expected, f"expected message {expected!r}, got {msg!r}"
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# Image Builder CreateInfrastructureConfiguration's unknown-instance-profile refusal re-words AWS's "Invalid parameter value for <member>: ..." message (regression of a fixed issue)

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInfrastructureConfiguration

## Description
One net-new root cause in the imagebuilder emulator's refusal-message formatting.

Note on prior art: this exact divergence was previously filed and marked FIXED as 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md (its "Cause 2 of 2"), and the instance-profile wording was also touched by the fixed issue 20261117T002936Z-imagebuilder-image-builder-getimage-omits-modeled-image-fields-and.md. The behavioral replay shows the emulator still emits the emulator-authored prefix, so the fix did not close this cause (or has regressed). No OPEN issue carries this check id: the open imagebuilder refusal-wording issues 20261118T174922Z-imagebuilder-image-builder-distribution-configuration-not-found-refusal-uses.md and 20261125T024626Z-ec2-iam-imagebuilder-image-builder-getdistributionconfiguration-not-found-message-now-uses.md are about the distribution-configuration NOT-FOUND template, not this parameter-validation message, and neither carries the replay key below. The refusal itself is correct AWS behavior here (AWS also refuses this call), so this is a message-template defect, not a call left refused.

Cause 1 of 1: CreateInfrastructureConfiguration with an instanceProfileName that matches no existing IAM instance profile is refused with the right error code and status but the wrong message prefix.
AWS: recorded behavior only for the message text. Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): CreateInfrastructureConfiguration PUT /CreateInfrastructureConfiguration, input member instanceProfileName is @required, target InstanceProfileNameType, @length min 1 max 256, @pattern ^[\w+=,.@-]+$; the model defines com.amazonaws.imagebuilder#InvalidParameterValueException as a client error with httpError 400, single member `message`, doc "The value that you provided for the specified parameter is invalid." (the model does not bind it to this operation, so the binding is recorded behavior). The recorded refusal is HTTP 400, error code InvalidParameterValueException, message: "Invalid parameter value for instanceProfileName: The provided instance profile does not exist. Please specify a different instance profile and try again." Fixed text of the template: "Invalid parameter value for " + <input member name> + ": The provided instance profile does not exist. Please specify a different instance profile and try again." The ONLY variable part is the input member name (here `instanceProfileName`); the profile name the caller supplied does not appear in AWS's message, and no id, ARN or timestamp appears in it.
Emulator: same status and same error code InvalidParameterValueException, but the message is "The value supplied for parameter 'instanceProfileName' is not valid. The provided instance profile does not exist. Please specify a different instance profile and try again." — the leading clause uses the emulator's own "The value supplied for parameter '<member>' is not valid." prefix in place of AWS's "Invalid parameter value for <member>:" prefix. The trailing sentence ("The provided instance profile does not exist. Please specify a different instance profile and try again.") already matches.
Why: the error message is a requirement. A client, SDK retry/branching layer, harness or operator that matches or logs AWS's fixed text sees a mismatch on an otherwise-correct refusal, and emulator-authored prose is not portable back to real AWS; code-plus-message assertions written against AWS fail only against the emulator.
Fix: the CreateInfrastructureConfiguration handler's instance-profile existence validation in the imagebuilder service crate — specifically the message template it passes when raising InvalidParameterValueException (and the shared invalid-parameter message formatter it borrows, which supplies the "The value supplied for parameter '<member>' is not valid." prefix). The error code, the HTTP status, the effect-absent behaviour (the rejected name is not created) and the positive control all already match and need no change.
Traces: imagebuilder.create-infrastructure-configuration-invalid-instance-profile-rejected.

## Evidence
Replay of the simparity-behavioral-aws-v1 behavioral suite against the emulator built from this checkout.

[mismatch] imagebuilder.create-infrastructure-configuration-invalid-instance-profile-rejected:
CreateInfrastructureConfiguration(name=<unique infra name>, instanceProfileName=<a profile name no IAM instance profile matches>, clientToken=<uuid>) is refused with code InvalidParameterValueException (assertion on the code PASSED), then the message assertion fails:

  AssertionError: expected message 'Invalid parameter value for instanceProfileName: The provided instance profile does not exist. Please specify a different instance profile and try again.', got "The value supplied for parameter 'instanceProfileName' is not valid. The provided instance profile does not exist. Please specify a different instance profile and try again."

Expected (AWS): HTTP 400 / InvalidParameterValueException / "Invalid parameter value for <member>: The provided instance profile does not exist. Please specify a different instance profile and try again." — fixed text apart from the member name taken from the request's input member (`instanceProfileName`).
Actual (emulator): HTTP 400 / InvalidParameterValueException / "The value supplied for parameter '<member>' is not valid. The provided instance profile does not exist. Please specify a different instance profile and try again."

The remainder of the check behaved correctly: the effect-absent probe (ListInfrastructureConfigurations filtered on the rejected name returned []) and the positive control (a real IAM instance profile is accepted and GetInfrastructureConfiguration returns the created ARN) both passed, which isolates the divergence to the message template alone.

## Steps to Reproduce
1. Build the emulator from this checkout and boot it.
2. Replay the simparity-behavioral-aws-v1 behavioral suite.
3. Run check imagebuilder.create-infrastructure-configuration-invalid-instance-profile-rejected: call CreateInfrastructureConfiguration with a unique name, a clientToken, and an instanceProfileName that no IAM instance profile matches (e.g. a freshly generated unique name that was never created).
4. Inspect the refusal: the code is InvalidParameterValueException and the status 400 (both correct); compare the Message against AWS's template "Invalid parameter value for instanceProfileName: The provided instance profile does not exist. Please specify a different instance profile and try again."
5. Re-replay that check id to confirm the cause is closed.
