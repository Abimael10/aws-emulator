Issue-Source: trace-sweep-divergence
Issue-Trace: iam-mfa-device-lifecycle
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-mfa-device-lifecycle iam-mfa-device-lifecycle-batch7
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|DeactivateMFADevice|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 051d1ca879f4768e0cde48b6e9d78ea8677353f9

# iam DeactivateMFADevice refuses with the MFA-device-not-found message when the user itself does not exist

> _Note: this issue was identified against the code base at commit `051d1ca879f4768e0cde48b6e9d78ea8677353f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeactivateMFADevice

## Description
1 cause. DeactivateMFADevice called for a UserName that no longer exists returns the right error class but the wrong refusal: it reports the MFA device as not found instead of the user.

AWS: Smithy DeactivateMFADevice models NoSuchEntityException with awsQueryError code 'NoSuchEntity' and httpError 404 (input: UserName -> existingUserNameType, SerialNumber required). When the named user does not exist, IAM resolves the user first and refuses with Error.Code 'NoSuchEntity', HTTP 404 and Error.Message "The user with name <UserName> cannot be found." - fixed text "The user with name " ... " cannot be found." with the UserName taken verbatim from the request. The device-scoped message "MFA Device <serial> cannot be found for user <UserName>." is AWS's wording for the different case where the user exists but has no such device enrolled; AWS checks the user before the device, so for a deleted user the user message is the one that is returned. Status and code are confirmed by the recording (404 / NoSuchEntity matched); the message did not.
Emulator: HTTP 404, Error.Code NoSuchEntity (both correct), Error.Message is the device-scoped sentence - "MFA Device arn:aws:iam::<account>:mfa/<device> cannot be found for user <user>." - i.e. the handler evaluates device lookup before user existence, so the deleted user is reported as a missing device.
Why: the two messages mean different things to a client. A caller cleaning up MFA enrolment reads the device message as "the user is still there, the device is already gone" and proceeds as if the user survived; AWS's message says the user itself is gone. Any error-handling path or test that matches AWS's user-not-found wording (or that distinguishes the two NoSuchEntity cases, which share code and status) takes the wrong branch, because the message is the only discriminator.
Fix: the DeactivateMFADevice handler in the iam service crate must resolve UserName before the serial number and emit AWS's user-not-found NoSuchEntity message when the user is absent, keeping the device-scoped message for the case where the user exists but the serial is not enrolled.
Traces: iam-mfa-device-lifecycle#task#10, and the same cluster member iam-mfa-device-lifecycle-batch7#task#8.

## Evidence
Replayed corpus simparity-wire-aws-v1, divergence cluster service=iam|DeactivateMFADevice|status=404->404, verdict=mismatch, affected_count 2 (iam-mfa-device-lifecycle#task#10, iam-mfa-device-lifecycle-batch7#task#8).

expected_status 404, actual_status 404 (Error.Code NoSuchEntity matched).
  $.ErrorResponse.Error.Message: value_mismatch
    expected 'The user with name <UserName> cannot be found.'   (recorded with the probe user name substituted)
    got      'MFA Device arn:aws:iam::<account>:mfa/<device-name> cannot be found for user <UserName>.'

Message anatomy - AWS's fixed text is "The user with name " + <UserName> + " cannot be found."; the only variable part is UserName, taken verbatim from the request. The emulator's string instead uses the device template "MFA Device " + <serial/ARN> + " cannot be found for user " + <UserName> + ".", whose variable parts are the request's SerialNumber and UserName.

Model reference: smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json - operation DeactivateMFADevice: input {UserName -> existingUserNameType @length 1-128 @pattern ^[\w+=,.@-]+$, SerialNumber required}, errors include NoSuchEntityException (errorKind client, httpError 404, awsQueryError code NoSuchEntity).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture iam-mfa-device-lifecycle through step task#10 (equivalently iam-mfa-device-lifecycle-batch7 step task#8): the MFA device and then the user are removed, and DeactivateMFADevice is then called again with that UserName and SerialNumber.
3. Observe the 404 NoSuchEntity body: Error.Message names the MFA device where AWS names the user ("The user with name <UserName> cannot be found.").
