Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.launch-under-http-tokens-enforced-requires-required
Issue-Key: ec2__ssm.launch-under-http-tokens-enforced-requires-required|_legs#5|AssertionError|assert (err.get("Code"), err.get("Message")) == (_ERR_UNSUPPORTED, _MSG_UNSUPPORTED), (
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# ec2__ssm: RunInstances refuses an IMDSv1 launch under httpTokensEnforced with a one-line emulator-authored message instead of AWS's httpTokensEnforced sentence

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Cause 1 of 1: RunInstances correctly refuses `MetadataOptions.HttpTokens=optional` while the account's instance-metadata defaults set `HttpTokensEnforced=enabled`, and it correctly uses `UnsupportedOperation`, but the Error.Message is the emulator's own short sentence rather than the message AWS emits for that refusal class.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2) declares NO modelled errors for RunInstances (ec2Query — `errors: []`), so the refusal code and message are recorded behavior only; `MetadataOptions` targets `InstanceMetadataOptionsRequest` (HttpTokens) and the account-level gate is set through ModifyInstanceMetadataDefaults / read back through GetInstanceMetadataDefaults as `AccountLevel.HttpTokensEnforced`. Recorded behavior: the launch is refused 400 `UnsupportedOperation` with the fixed sentence "You can't launch instances with IMDSv1 because httpTokensEnforced is enabled for this account. Either launch the instance with httpTokens=required or contact your account owner to disable httpTokensEnforced using the ModifyInstanceMetadataDefaults API or the account settings in the EC2 console." That text is entirely fixed — it interpolates nothing from the request or from state — and it is the message that tells the caller both remedies (relaunch with httpTokens=required, or clear the account default).

Emulator: the same request is refused `UnsupportedOperation` with Message `IMDSv1 is disabled for this account.` — a sentence AWS does not emit. The code and the refusal itself are right; only the message diverges.

Why: a client that keys on the message — operators reading it, and the many tools that surface the refusal text verbatim — is told IMDSv1 is "disabled" with no indication that the cause is the account-level `httpTokensEnforced` default, no mention of `httpTokens=required`, and no mention of ModifyInstanceMetadataDefaults. The caller is left with a dead end on a refusal AWS makes self-service, and any assertion or log scrape matching AWS's wording fails.

Fix: the RunInstances handler's metadata-options precondition in the ec2 service crate (the branch that compares the request's `MetadataOptions.HttpTokens` against the account-level `HttpTokensEnforced` default) — it must emit AWS's fixed httpTokensEnforced sentence as the `UnsupportedOperation` message.

Traces: check ec2__ssm.launch-under-http-tokens-enforced-requires-required.

## Evidence
Expected: with ModifyInstanceMetadataDefaults setting HttpTokensEnforced=enabled, `RunInstances(..., MetadataOptions={"HttpTokens": "optional"})` is refused HTTP 400 with Code `UnsupportedOperation` and Message exactly "You can't launch instances with IMDSv1 because httpTokensEnforced is enabled for this account. Either launch the instance with httpTokens=required or contact your account owner to disable httpTokensEnforced using the ModifyInstanceMetadataDefaults API or the account settings in the EC2 console." (fixed text, no interpolated values), nothing is launched, and the identical request with HttpTokens=required succeeds.

Actual: AssertionError — "RunInstances with HttpTokens=optional under HttpTokensEnforced=enabled must be refused with 'UnsupportedOperation' and the exact IMDSv1-disabled message, got ('UnsupportedOperation', 'IMDSv1 is disabled for this account.')". Verdict: mismatch.

## Steps to Reproduce
1. GetInstanceMetadataDefaults to record the account's current AccountLevel values (and restore them afterwards).
2. ModifyInstanceMetadataDefaults with HttpTokensEnforced=enabled.
3. Build a VPC/subnet and resolve an AMI from the SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64.
4. Call RunInstances(ImageId=<that AMI>, InstanceType=t3.micro, SubnetId=<subnet>, MinCount=1, MaxCount=1, MetadataOptions={"HttpTokens": "optional"}).
5. Compare the Error.Code and Error.Message against AWS's httpTokensEnforced sentence; observe the message is `IMDSv1 is disabled for this account.`
