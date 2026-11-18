Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.list-policies-granting-service-access iam.op-contract-listGroups-missing-permission-denied iam.op-contract-listInstanceProfiles-missing-permission-denied iam.op-contract-listRoles-missing-permission-denied iam.op-contract-listUsers-missing-permission-denied iam.simulate-custom-policy iam.untag-policy iam.iam-cred-password-policy-violation-and-role-session-change-password-rejected iam.minted-identifier-prefixes-and-arn-paths iam.ssh-public-key-lifecycle
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.list-policies-granting-service-access|run#23|AssertionError|assert message2 == expected_denied, (
Issue-Key: iam.op-contract-listGroups-missing-permission-denied|run#21|AssertionError|assert denial.get("message") == expected_denial, (
Issue-Key: iam.op-contract-listInstanceProfiles-missing-permission-denied|run#17|AssertionError|assert denied[2] == expected_denial, (
Issue-Key: iam.op-contract-listRoles-missing-permission-denied|run#22|AssertionError|assert denial.get("message") == expected_denial, (
Issue-Key: iam.op-contract-listUsers-missing-permission-denied|run#30|AssertionError|assert denial.get("message") == expected_denial, (
Issue-Key: iam.simulate-custom-policy|run#12|AssertionError|assert msg == want, f"AccessDenied message: wanted {want!r}, got {msg!r}"
Issue-Key: iam.untag-policy|run#25|AssertionError|assert message2 == expected2, f"expected message {expected2!r}, got {message2!r}"
Issue-Key: iam.iam-cred-password-policy-violation-and-role-session-change-password-rejected|_client_error#39|AssertionError|raise AssertionError(f"expected this call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: iam.minted-identifier-prefixes-and-arn-paths|_assert_id#35|AssertionError|assert value.startswith(prefix), (
Issue-Key: iam.ssh-public-key-lifecycle|run#21|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 86d32c23846cd48a29a50565dea948b0a247c0e9

# iam: 4 net-new causes — denials append an authorization-details trailer these recordings do not carry, ChangePassword ignores PasswordReusePrevention, access-key ids are EMULATOR-prefixed, and the SSH-public-key NoSuchEntity sentence is wrong

> _Note: this issue was identified against the code base at commit `86d32c23846cd48a29a50565dea948b0a247c0e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListUsers

## Description
Four net-new root causes in the IAM service, each grounded in its own failing behavioral check(s) from simparity-behavioral-aws-v1. All four must be closed before this issue is submitted. None of the listed checks is reported as blocked by an unimplemented stub, and the replay reports none of them owned by an open issue.

Cause 1 of 4: every IAM AccessDenied message is served with a trailing GetRequestAuthorizationDetails pointer sentence appended, which the recordings behind these seven checks do not carry.
AWS: recorded behavior only (the Smithy model carries no message text). For these denials AWS's message is exactly the leading sentence and nothing else — for an implicit deny `User: <principal-arn> is not authorized to perform: <action> on resource: <resource> because no identity-based policy allows the <action> action`, and for an explicit deny `User: <principal-arn> is not authorized to perform: <action> on resource: <resource> with an explicit deny in an identity-based policy`. Fixed text: everything except `<principal-arn>`, `<action>` and `<resource>`, which come from the request and the emulator's state (e.g. resource `arn:aws:iam::<account>:group/`, `.../role/`, `.../instance-profile/`, `.../user/`, `*`, or `policy <policy-arn>`). HTTP status 403 / code AccessDenied are correct in the emulator and are not at issue; only the message body is.
Emulator: the leading sentence is correct, then a second sentence is appended that AWS did not send: `. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`. The trailer appears on implicit denials (iam:ListPoliciesGrantingServiceAccess, iam:SimulateCustomPolicy, iam:UntagPolicy) and on explicit denials (iam:ListGroups, iam:ListInstanceProfiles, iam:ListRoles, iam:ListUsers) alike, i.e. it is appended unconditionally by the denial serializer rather than per denial class.
Why: a client that matches, logs or asserts on AWS's denial message sees text AWS never sent; message-equality and regression assertions against real AWS fail even though status and code match, and operators are pointed at an authorization id that identifies nothing.
Fix: the IAM authorization denial message builder (the shared AccessDenied serializer the IAM operation handlers call) — it must emit the trailer only for the denial classes whose recordings carry it, not for every denial. IMPORTANT: this is the opposite direction from the open issue 20261117T135333Z-iam-6-net-new-causes-denials-omit-the.md (cause 1) and from the fixed issues 20261028T012639Z-iam-the-implicit-deny-accessdenied-message-no-longer.md and 20261029T114202Z-iam-the-implicit-deny-accessdenied-message-for-a.md, all of which are grounded in OTHER checks whose recordings DO carry the trailer (iam.op-contract-list-users-live-enforcement-denied, iam.absent-condition-key-denied-unless-ifexists-forallvalues-or-null, iam.current-time-date-condition-window-denied-outside, iam.policy-arn-condition-scopes-delegated-attach-other-policy-denied). Neither issue covers the seven checks below (the replay reports them owned by nothing), and a fix that simply strips the trailer everywhere will break those. The requirement is to reproduce the recorded message per denial class, not one blanket choice.
Traces: iam.list-policies-granting-service-access, iam.op-contract-listGroups-missing-permission-denied, iam.op-contract-listInstanceProfiles-missing-permission-denied, iam.op-contract-listRoles-missing-permission-denied, iam.op-contract-listUsers-missing-permission-denied, iam.simulate-custom-policy, iam.untag-policy.

Cause 2 of 4: ChangePassword does not enforce the account password policy's PasswordReusePrevention — a new password inside the reuse window is accepted.
AWS: Smithy iam#ChangePassword declares PasswordPolicyViolationException (httpError 400, awsQueryError code PasswordPolicyViolation); the account password policy's PasswordReusePrevention field bounds how many previous passwords may not be reused, and a ChangePassword whose NewPassword is one of them is refused with that error and the fixed message `Password cannot be the same as previously used password` (no variable parts), leaving the live password unchanged. In the same scenario the emulator already enforces MinimumPasswordLength and RequireSymbols correctly on CreateLoginProfile/UpdateLoginProfile, so only the reuse rule on ChangePassword is at issue.
Emulator: with the strict policy re-applied (MinimumPasswordLength=24, PasswordReusePrevention=3) the IAM user's own ChangePassword back to a password used two changes earlier returns HTTP 200 with an empty body instead of the 400 PasswordPolicyViolation (`expected this call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}`).
Why: a client relying on AWS's password hygiene can rotate a user straight back onto a recently used password; the credential state the emulator reaches is not reachable on AWS, and code paths that handle PasswordPolicyViolation are never exercised.
Fix: the ChangePassword handler's password-policy validation — it must consult the account password policy and the user's password history (the login-profile state) for PasswordReusePrevention, not only the length/character-class rules.
Traces: iam.iam-cred-password-policy-violation-and-role-session-change-password-rejected.

Cause 3 of 4: minted IAM access-key ids are not in AWS's access-key-id class — they are EMULATOR-prefixed.
AWS: Smithy iam#CreateAccessKey returns AccessKey (required) whose AccessKeyId targets accessKeyIdType (@length 16-128, @pattern ^[\w]+$). AWS's IAM unique-identifier prefixes are documented and load-bearing: a long-term IAM user access key id is 20 characters and begins with the literal prefix `AKIA` (a temporary STS credential begins `ASIA`). The requirement is the id FORM (AKIA + 16 further characters, 20 total), never any one run's literal id, which is per-run state.
Emulator: the id is `EMULATOR`-prefixed and 20 characters of the wrong shape (one run: 'EMULATORAKINY3UTUEWK'), so it satisfies the Smithy pattern but is not recognisable as an AWS access key id.
Why: credential-classifying tooling (long-term vs temporary, scanners, policy linters, presign helpers that branch on the prefix) misclassifies every key the emulator mints, and a client validating the documented prefix rejects it outright.
Fix: the access-key-id allocator behind the IAM user access-key state (reached by CreateAccessKey and projected by ListAccessKeys), not the serializer. Note: the open wire issue 20261116T105641Z-iam-7-net-new-causes-minted-access-key.md (causes 1-2) and the open behavioral issue 20261111T103332Z-iam-7-net-new-causes-deleteaccountpasswordpolicy-is-unrouted.md describe the same EMULATOR prefix for other recorded units, but the replay reports this check owned by neither, so it is filed here to bind its replay key.
Traces: iam.minted-identifier-prefixes-and-arn-paths.

Cause 4 of 4: the NoSuchEntity refusal for a deleted SSH public key names the wrong entity class in its message.
AWS: Smithy iam#GetSSHPublicKey and iam#UpdateSSHPublicKey both declare NoSuchEntityException (httpError 404, awsQueryError code NoSuchEntity). Recorded behavior for the text: the message is `The Public Key with id <SSHPublicKeyId> cannot be found.` — fixed apart from `<SSHPublicKeyId>`, which is the id from the request (an APKA-prefixed publicKeyIdType value). Both the read (GetSSHPublicKey) and the mutation (UpdateSSHPublicKey) on the dead id carry the same sentence.
Emulator: code NoSuchEntity is correct, but the message is `The SSH Public Key with id <SSHPublicKeyId> cannot be found.` — the emulator inserts `SSH ` into AWS's fixed text.
Why: a client that matches AWS's documented message to distinguish a missing key from a missing user mis-handles the error; the divergence is in the fixed part of the sentence, not a per-run value.
Fix: the NoSuchEntity message construction in the SSH-public-key handlers (GetSSHPublicKey / UpdateSSHPublicKey, and the shared helper the DeleteSSHPublicKey path uses).
Traces: iam.ssh-public-key-lifecycle.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (796/1144 passed; bootstrap_signal false; iam is bootstrapped).

Cause 1 — seven failing checks, same shape, status 403 / code AccessDenied correct, message carries an extra sentence:
  iam.op-contract-listGroups-missing-permission-denied: `AssertionError: the explicit Deny's refusal must carry AWS's exact message 'User: arn:aws:iam::000000000000:user/<user> is not authorized to perform: iam:ListGroups on resource: arn:aws:iam::000000000000:group/ with an explicit deny in an identity-based policy'; got '... with an explicit deny in an identity-based policy. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <id>'`
  iam.op-contract-listInstanceProfiles-missing-permission-denied, iam.op-contract-listRoles-missing-permission-denied, iam.op-contract-listUsers-missing-permission-denied: identical, with resource `arn:aws:iam::000000000000:instance-profile/`, `.../role/`, `.../user/`.
  iam.list-policies-granting-service-access: expected `'User: .../nopriv-<suffix> is not authorized to perform: iam:ListPoliciesGrantingServiceAccess on resource: arn:aws:iam::000000000000:user/<target> because no identity-based policy allows the iam:ListPoliciesGrantingServiceAccess action'`; got the same plus the `. Go to https://...` trailer.
  iam.simulate-custom-policy: expected `'... is not authorized to perform: iam:SimulateCustomPolicy on resource: * because no identity-based policy allows the iam:SimulateCustomPolicy action'`; got the same plus the trailer.
  iam.untag-policy: expected `'... is not authorized to perform: iam:UntagPolicy on resource: policy arn:aws:iam::000000000000:policy/<policy> because no identity-based policy allows the iam:UntagPolicy action'`; got the same plus the trailer.
(The `<id>` inside the trailer is per-run; the defect is the presence of the whole sentence, not its id.)

Cause 2 — iam.iam-cred-password-policy-violation-and-role-session-change-password-rejected:
  `AssertionError: expected this call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'RequestId': ..., 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', ...}, 'RetryAttempts': 0}}` on `change_password(OldPassword=<23-char password>, NewPassword=<password used two changes earlier>)` with the strict policy (MinimumPasswordLength=24, PasswordReusePrevention=3) in force. Expected 400 PasswordPolicyViolation with message `Password cannot be the same as previously used password`. The preceding length/symbol refusals on CreateLoginProfile and UpdateLoginProfile in the same check passed, so only the reuse rule is missing.

Cause 3 — iam.minted-identifier-prefixes-and-arn-paths:
  `AssertionError: access key unique id must carry the documented 'AKIA' prefix, got 'EMULATORAKINY3UTUEWK'` (expected length 20, prefix AKIA).

Cause 4 — iam.ssh-public-key-lifecycle:
  `AssertionError: expected message 'The Public Key with id <APKA...> cannot be found.', got 'The SSH Public Key with id <APKA...> cannot be found.'` after DeleteSSHPublicKey, on GetSSHPublicKey (and asserted again on UpdateSSHPublicKey for the same id). Error code NoSuchEntity matched.

Model grounding — smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json:
- ChangePassword: errors EntityTemporarilyUnmodifiable(409), InvalidUserType(400), LimitExceeded(409), NoSuchEntity(404), PasswordPolicyViolation(400), ServiceFailure(500).
- GetSSHPublicKey: input SSHPublicKeyId -> publicKeyIdType (@length 20-128, @pattern ^[\w]+$); errors NoSuchEntity(404), UnrecognizedPublicKeyEncoding(400).
- CreateAccessKey: output AccessKey (required); AccessKey.AccessKeyId -> accessKeyIdType, @length 16-128, @pattern ^[\w]+$.
- SimulateCustomPolicy / UntagPolicy: no message text modelled; the denial text is recorded behavior only.

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator from the checkout, then replay simparity-behavioral-aws-v1.
2. Cause 1: create an IAM user with (a) no policy allowing iam:ListPoliciesGrantingServiceAccess / iam:SimulateCustomPolicy / iam:UntagPolicy and (b) an identity policy with an explicit Deny for iam:ListGroups / iam:ListInstanceProfiles / iam:ListRoles / iam:ListUsers; call each operation with that user's key and compare the AccessDenied message byte-for-byte: the emulator appends `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <id>` that these recordings do not carry. Re-replay the four checks named in 20261117T135333Z-iam-6-net-new-causes-denials-omit-the.md as well: those recordings DO carry the trailer, so both families must pass.
3. Cause 2: UpdateAccountPasswordPolicy with MinimumPasswordLength=24, PasswordReusePrevention=3, AllowUsersToChangePassword=true and the character-class flags set; CreateLoginProfile with a compliant password, create an access key for the user, then as that user call ChangePassword twice to walk the history and once more back to a password inside the last 3. Observed 200; expected 400 PasswordPolicyViolation, message `Password cannot be the same as previously used password`, with the live password unchanged.
4. Cause 3: CreateAccessKey for a user and inspect AccessKey.AccessKeyId — it must be 20 characters beginning `AKIA`, not `EMULATOR...`. ListAccessKeys must project the same form.
5. Cause 4: UploadSSHPublicKey, DeleteSSHPublicKey, then GetSSHPublicKey and UpdateSSHPublicKey with the deleted SSHPublicKeyId; both must answer NoSuchEntity with message `The Public Key with id <SSHPublicKeyId> cannot be found.`
6. Re-replay the 10 checks listed in trace_ids and confirm they pass.
