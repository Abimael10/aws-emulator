Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.mfa-conditioned-role-assumable-only-with-a-real-virtual-mfa-device-token
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.mfa-conditioned-role-assumable-only-with-a-real-virtual-mfa-device-token|run#73|AssertionError|assert session["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Commit: 5bda3f4959ddaaecd7545612991d55d37567ef10

# sts: GetSessionToken mints an EMUL-class AccessKeyId instead of an STS temporary ASIA key id

> _Note: this issue was identified against the code base at commit `5bda3f4959ddaaecd7545612991d55d37567ef10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSessionToken

## Description
Cause 1 of 1 — GetSessionToken's Credentials.AccessKeyId is not in the ASIA temporary-credential class (failing check: sts.mfa-conditioned-role-assumable-only-with-a-real-virtual-mfa-device-token).
AWS: Smithy (service-com-amazonaws-sts-awssecuritytokenservicev20110615): GetSessionToken(DurationSeconds, SerialNumber, TokenCode) returns Credentials; the model puts no pattern on Credentials.AccessKeyId, so this is recorded behavior only, read together with AWS's documented access-key-id prefixes — a long-term IAM user key id begins AKIA, every STS-minted temporary key id begins with the four characters ASIA. A GetSessionToken call carrying SerialNumber/TokenCode for an enrolled virtual MFA device therefore returns an AccessKeyId whose first four characters are ASIA, with SecretAccessKey, SessionToken and Expiration.
Emulator: the AccessKeyId's first four characters are 'EMUL' — an emulator-authored class that exists in no AWS partition. The key class is the only thing distinguishing a temporary session from a long-term credential on the wire.
Why: any client that classifies credentials by key-id prefix (SDK and CLI tooling, credential scanners, test harnesses, and the emulator's own authorizer when it decides whether a request is session-scoped) treats the MFA session as a long-term key. Conditions that depend on the session being temporary — here aws:MultiFactorAuthPresent on the role being assumed — then cannot be evaluated against the right principal class, and MFA-gated access cannot be exercised at all.
Related already-open issues name the sibling paths, not this one: 20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md (AssumeRole) and 20261027T130805Z-sts-5-net-new-behavioral-root-causes-temporary.md; the suite does not report either as owning this check, and GetSessionToken is a separate handler.
Fix: the STS GetSessionToken handler's credential minting — the access-key-id generator in STS's session/credential state must emit an ASIA-classed key id (and the authorizer must accept it), for the MFA-bearing and the plain call alike.
Traces: sts.mfa-conditioned-role-assumable-only-with-a-real-virtual-mfa-device-token.

## Evidence
check sts.mfa-conditioned-role-assumable-only-with-a-real-virtual-mfa-device-token, line 400 — AssertionError: "an MFA-bearing GetSessionToken must return a temporary key in the ASIA class; got prefix 'EMUL'". Call: sts:GetSessionToken with SerialNumber = the enrolled virtual MFA device's ARN and a valid 6-digit TokenCode. Expected Credentials.AccessKeyId[:4] == 'ASIA'; actual 'EMUL'. (The assertion is on the key CLASS, the 4-character prefix, not on the per-run key value.)

## Steps to Reproduce
Create an IAM user, enrol a virtual MFA device for it and call sts:GetSessionToken with that device's SerialNumber and a current TOTP TokenCode (long-term user credentials). Inspect Credentials.AccessKeyId: the emulator returns a key id beginning 'EMUL'; AWS returns one beginning 'ASIA'. The same check then uses that session to assume a role conditioned on aws:MultiFactorAuthPresent.
