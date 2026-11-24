Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.enable-mfa-device-already-enrolled-rejected
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.enable-mfa-device-already-enrolled-rejected|run#19|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 7148f2b6638ce0f4ccfada01b07f71c83f7419cd

# IAM EnableMFADevice: the EntityAlreadyExists refusal for an already-enrolled device is emulator prose naming the serial number instead of AWS's "MFA Device for user <user> already exists."

> _Note: this issue was identified against the code base at commit `7148f2b6638ce0f4ccfada01b07f71c83f7419cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableMFADevice

## Description
ONE root cause (1 of 1).

Cause 1 of 1: re-enabling a user's already-active virtual MFA device is refused with the correct code EntityAlreadyExists but a message the emulator writes itself, built from the device serial, where AWS's message names only the user.
AWS: Smithy iam#EnableMFADevice takes UserName (required, existingUserNameType, @length 1-128, pattern ^[\w+=,.@-]+$), SerialNumber (required, serialNumberType, @length 9-256, pattern ^[\w+=/:,.@-]+$) and AuthenticationCode1/AuthenticationCode2 (both required, @length 6-6, pattern ^[\d]+$), and models EntityAlreadyExistsException (awsQueryError EntityAlreadyExists, httpResponseCode 409) among its errors. Enabling a device that is already enrolled for that user — the same serial, with two fresh valid consecutive TOTP codes — is refused EntityAlreadyExists 409 with exactly "MFA Device for user <UserName> already exists.": fixed text "MFA Device for user " + " already exists.", with ONE variable part, the UserName taken verbatim from the request. The serial number does not appear in it. (The user name in the recorded line, sim2real-mfa-reenable-c3fdf0d3ab76, is that run's generated value filling the template, not part of it.)
Emulator: the same call is refused with the right code and a different template: "Device <serialNumber ARN> is already enabled for user <UserName>." — fixed text "Device " + " is already enabled for user " + ".", substituting the device's serial (an arn:aws:iam::<account>:mfa/<deviceName> value from state) in a position AWS's message has no slot for. Only the message diverges: the code assertion (EntityAlreadyExists) passed, and the surrounding state assertions are consistent with AWS (exactly one device still enrolled after the refusal).
Why: the message is the only human- and tool-readable discriminator between IAM's several EntityAlreadyExists causes on MFA operations (device already enrolled for this user vs. a name collision on CreateVirtualMFADevice vs. a device assigned elsewhere). A client that matches IAM's already-enrolled sentence, or that parses the user name out of it to point an operator at the right principal, sees text AWS never emits and must fall back to the code alone; an enrolment flow that distinguishes "this user already has MFA" from "this device is taken" cannot classify the refusal.
Fix: the EntityAlreadyExists message rendering in the already-enrolled branch of the EnableMFADevice handler in the iam service, which must emit AWS's template filled with the request's UserName and nothing else. Named so it is not mistaken for coverage: open issue 20261027T214234Z-iam-6-net-new-behavioral-root-causes-enablemfadevice.md diagnosed EnableMFADevice as a 501 scaffold stub reached through iam.entity-temporarily-unmodifiable-enforced; in THIS checkout the operation answers and refuses correctly coded, so this is a different defect on that same operation and that issue does not carry this check's replay key.
Traces: iam.enable-mfa-device-already-enrolled-rejected.

## Evidence
Suite simparity-behavioral-aws-v1, replayed against this checkout (713/1010 passed). The check is listed under `### iam` and is marked neither ALREADY FILED nor OWNED. Service token `iam` is bootstrapped.

Cause 1 — iam.enable-mfa-device-already-enrolled-rejected [mismatch], check line 53:
  AssertionError: expected message 'MFA Device for user sim2real-mfa-reenable-c3fdf0d3ab76 already exists.', got 'Device arn:aws:iam::000000000000:mfa/sim2real-mfa-reenable-dev-f25e02367870 is already enabled for user sim2real-mfa-reenable-c3fdf0d3ab76.'
  Sequence: CreateUser U; CreateVirtualMFADevice D (serial = D's ARN, seed = Base32StringSeed); EnableMFADevice(UserName=U, SerialNumber=D, two fresh consecutive TOTP codes computed against the service clock) succeeds; ListMFADevices(UserName=U) returns exactly that one device (asserted, passes); EnableMFADevice(UserName=U, SerialNumber=D, two NEWER fresh valid codes) is refused.
  The code assertion passed first: code == 'EntityAlreadyExists' (iam#EnableMFADevice's modeled EntityAlreadyExistsException, awsQueryError EntityAlreadyExists / HTTP 409).
  Expected message: 'MFA Device for user <UserName> already exists.' — fixed text with the request's UserName as its only variable part.
  Actual message: 'Device <serial> is already enabled for user <UserName>.' — a template with an extra slot for the serial that AWS's sentence does not have. The account id, device name and user name in the recorded strings are this run's values.
  Assertions after the failing one (still exactly one device enrolled; DeactivateMFADevice clears it; the identical EnableMFADevice then succeeds again) were not graded.

## Steps to Reproduce
Boot the emulator from this checkout and point an `iam` client at it.
1. CreateUser U.
2. CreateVirtualMFADevice(VirtualMFADeviceName=D) and keep SerialNumber and Base32StringSeed.
3. EnableMFADevice(UserName=U, SerialNumber=<serial>, AuthenticationCode1/2 = two consecutive valid TOTP codes from the seed) — succeeds.
4. EnableMFADevice(UserName=U, SerialNumber=<same serial>, AuthenticationCode1/2 = two NEWER consecutive valid TOTP codes).
5. Observe EntityAlreadyExists / HTTP 409 (correct code) with 'Device <serial> is already enabled for user U.' instead of AWS's 'MFA Device for user U already exists.'
   Replay: iam.enable-mfa-device-already-enrolled-rejected
