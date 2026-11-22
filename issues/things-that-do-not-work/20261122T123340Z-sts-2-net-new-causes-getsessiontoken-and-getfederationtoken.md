Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.get-session-token-duration-exceeds-max-rejected sts.get-session-token-minimum-session-token-size-exceeds-max-rejected sts.get-federation-token-tags-exceeding-max-count-rejected
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.get-session-token-duration-exceeds-max-rejected|run#3|AssertionError|assert at_max["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Key: sts.get-session-token-minimum-session-token-size-exceeds-max-rejected|run#3|AssertionError|assert at_max["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Key: sts.get-federation-token-tags-exceeding-max-count-rejected|run#3|AssertionError|assert at_max["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Commit: b2fe2d689da9f6d193550aea820e1c1e26b17eb1

# sts: 2 net-new causes — GetSessionToken and GetFederationToken mint EMULATOR-class AccessKeyIds instead of the ASIA temporary-credential class at the input-boundary values

> _Note: this issue was identified against the code base at commit `b2fe2d689da9f6d193550aea820e1c1e26b17eb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSessionToken

## Description
Two net-new root causes, both in STS's temporary-credential minting, each reached through a DIFFERENT handler and each blocking a boundary-validation check before it can grade the refusal it is about. An already-open issue, 20261115T051545Z-sts-getsessiontoken-mints-an-emul-class-accesskeyid-instead.md, names the GetSessionToken MFA path and 20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md names AssumeRole; the suite reports NEITHER as owning any of the three checks below, and GetFederationToken is a third handler again, so these are filed as their own work items.

Cause 1 of 2 — GetSessionToken returns a Credentials.AccessKeyId outside the ASIA class (failing checks: sts.get-session-token-duration-exceeds-max-rejected, sts.get-session-token-minimum-session-token-size-exceeds-max-rejected).
AWS: Smithy (services/service-com-amazonaws-sts-awssecuritytokenservicev20110615) models GetSessionToken(DurationSeconds @range 900..129600, SerialNumber, TokenCode, MinimumSessionTokenSize @range 0..4096) -> Credentials (+ SessionTokenUtilization, SessionTokenSize); its only modelled error is RegionDisabledException (403). The model puts no pattern on Credentials.AccessKeyId, so for the key id this is recorded behavior only, read with AWS's documented access-key-id classes: a long-term IAM user key id begins AKIA and every STS-minted temporary key id begins with the four characters ASIA. A call at either documented maximum — DurationSeconds=129600, or MinimumSessionTokenSize=4096 — is INSIDE the modelled range, so it succeeds and returns an AccessKeyId whose first four characters are ASIA, together with SecretAccessKey, SessionToken and Expiration.
Emulator: the call succeeds but the AccessKeyId's first four characters are 'EMUL' (observed 'EMULATORASZUP7DM4NET' and 'EMULATORASAM2SMJOI7P' — per-run key values, only the 4-character class is the requirement). 'EMUL' is an emulator-authored class that exists in no AWS partition.
Why: the key-id class is the only thing on the wire that distinguishes a temporary session from a long-term credential. Any client that classifies credentials by prefix — SDK/CLI tooling, credential scanners, GetAccessKeyInfo, test harnesses, and the emulator's own authorizer when it decides whether a request is session-scoped — treats the session as a long-term user key, so session-only policy conditions cannot be evaluated against the right principal class. It also stops both checks at their positive control, leaving the refusal they exist to verify unexercised: one second past the maximum (DurationSeconds=129601) and one byte past it (MinimumSessionTokenSize=4097) must each be refused ValidationError with the message '1 validation error detected: Value '<value>' at '<memberName>' failed to satisfy constraint: Member must have value less than or equal to <max>' — fixed text except <value> (the rejected request value), <memberName> ('durationSeconds', 'minimumSessionTokenSize' — the lower-camel wire member name) and <max> (the member's modelled range max, 129600 / 4096).
Fix: the STS GetSessionToken handler's credential minting — the access-key-id generator in STS's session/credential state must emit an ASIA-classed key id (and the authorizer must accept it); then the handler's input validation for DurationSeconds and MinimumSessionTokenSize must enforce the modelled range maxima with the ValidationError wording above.
Traces: sts.get-session-token-duration-exceeds-max-rejected, sts.get-session-token-minimum-session-token-size-exceeds-max-rejected.

Cause 2 of 2 — GetFederationToken returns a Credentials.AccessKeyId outside the ASIA class (failing check: sts.get-federation-token-tags-exceeding-max-count-rejected).
AWS: Smithy models GetFederationToken(Name required @length 2..32, Policy, PolicyArns, DurationSeconds, Tags @length 0..50, MinimumSessionTokenSize) -> Credentials + FederatedUser + SessionTokenUtilization/SessionTokenSize, with errors MalformedPolicyDocument (400), PackedPolicyTooLarge (400), RegionDisabledException (403). As above, the key-id class is recorded behavior only: a federated session's Credentials.AccessKeyId begins with ASIA. A Tags list of exactly 50 entries is inside the modelled @length max, so it succeeds and mints such credentials.
Emulator: the call succeeds but the AccessKeyId's first four characters are 'EMUL' (observed 'EMULATORASRWBXVV54VV' — a per-run value; the class is the requirement).
Why: same as Cause 1 for any client classifying credentials by prefix, and it stops the check at its positive control so the 51-tag refusal is never graded: a Tags list one entry past the maximum must be refused ValidationError with '1 validation error detected: Value '<renderedTagList>' at 'tags' failed to satisfy constraint: Member must have length less than or equal to 50' — fixed text except <renderedTagList>, the awsQuery rendering of the submitted list as '[Tag(key=<k>, value=<v>), ...]' built from the request's own tags, and the limit 50 taken from the member's modelled @length max.
Fix: the STS GetFederationToken handler's credential minting (the same shared access-key-id generator in STS's session/credential state), then that handler's input validation for the Tags list length.
Traces: sts.get-federation-token-tags-exceeding-max-count-rejected.

## Evidence
Cause 1 — check sts.get-session-token-duration-exceeds-max-rejected, line 36: AssertionError: "expected a temporary (ASIA-prefixed) access key at the boundary duration, got 'EMULATORASZUP7DM4NET'". Call: sts:GetSessionToken(DurationSeconds=129600) with client-side parameter validation disabled. Expected Credentials.AccessKeyId[:4] == 'ASIA'; actual 'EMUL'.
Check sts.get-session-token-minimum-session-token-size-exceeds-max-rejected, line 36: AssertionError: "expected a temporary (ASIA-prefixed) access key at the boundary size, got 'EMULATORASAM2SMJOI7P'". Call: sts:GetSessionToken(MinimumSessionTokenSize=4096). Expected AccessKeyId[:4] == 'ASIA'; actual 'EMUL'.
Both checks then assert, unreached, that DurationSeconds=129601 / MinimumSessionTokenSize=4097 are refused ValidationError with "1 validation error detected: Value '129601' at 'durationSeconds' failed to satisfy constraint: Member must have value less than or equal to 129600" and "1 validation error detected: Value '4097' at 'minimumSessionTokenSize' failed to satisfy constraint: Member must have value less than or equal to 4096".

Cause 2 — check sts.get-federation-token-tags-exceeding-max-count-rejected, line 48: AssertionError: "expected 50 tags to still mint temporary credentials, got 'EMULATORASRWBXVV54VV'". Call: sts:GetFederationToken(Name=<unique name>, Tags=[50 entries]). Expected Credentials.AccessKeyId[:4] == 'ASIA'; actual 'EMUL'. The unreached assertion requires the 51-tag call to be refused ValidationError with "1 validation error detected: Value '[Tag(key=..., value=v), ...]' at 'tags' failed to satisfy constraint: Member must have length less than or equal to 50".

In all three the assertion is on the 4-character key CLASS, not on the per-run key value.

## Steps to Reproduce
1. Build and boot the emulator; create a botocore STS client with parameter_validation=False so the service's own validation answers.
2. Cause 1: call sts:GetSessionToken(DurationSeconds=129600) and sts:GetSessionToken(MinimumSessionTokenSize=4096). Inspect Credentials.AccessKeyId — the emulator returns a key id beginning 'EMUL'; AWS returns one beginning 'ASIA'. Then call GetSessionToken(DurationSeconds=129601) and GetSessionToken(MinimumSessionTokenSize=4097) and compare the error code (ValidationError) and message text to the templates above.
3. Cause 2: call sts:GetFederationToken(Name='sim2real-behav-fed-<suffix>', Tags=[{Key: f'sim2real-tag-{i}', Value: 'v'} for i in range(50)]). Inspect Credentials.AccessKeyId — 'EMUL'-prefixed instead of 'ASIA'. Then repeat with 51 tags and compare code/message to the template above.
