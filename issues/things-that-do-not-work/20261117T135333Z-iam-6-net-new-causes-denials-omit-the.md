Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.absent-condition-key-denied-unless-ifexists-forallvalues-or-null iam.current-time-date-condition-window-denied-outside iam.op-contract-list-users-live-enforcement-denied iam.policy-arn-condition-scopes-delegated-attach-other-policy-denied iam.create-role-permissions-boundary-nonexistent-arn-rejected iam.create-user-permissions-boundary-nonexistent-arn-rejected iam.deactivate-mfa-device-not-enrolled-rejected iam.mfa-devices-per-user-quota-exceeded-rejected iam.saml-provider-assertion-encryption-required-without-a-key-rejected iam.saml-provider-identity-providers-quota-exceeded-rejected iam.saml-provider-keys-quota-exceeded-rejected
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.absent-condition-key-denied-unless-ifexists-forallvalues-or-null|_assert_stably_denied#18|AssertionError|assert msg == _expected_denial(msg, expected), f"{action}: denial message {msg!r}"
Issue-Key: iam.current-time-date-condition-window-denied-outside|_assert_stably_denied#23|AssertionError|assert msg == want, f"{action}: expected message {want!r}, got {msg!r}"
Issue-Key: iam.op-contract-list-users-live-enforcement-denied|run#20|AssertionError|assert denied_message == expected, f"expected message {expected!r}, got {denied_message!r}"
Issue-Key: iam.policy-arn-condition-scopes-delegated-attach-other-policy-denied|_assert_stably_denied#26|AssertionError|assert msg == want, f"{action}: expected message {want!r}, got {msg!r}"
Issue-Key: iam.create-role-permissions-boundary-nonexistent-arn-rejected|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: iam.create-user-permissions-boundary-nonexistent-arn-rejected|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: iam.deactivate-mfa-device-not-enrolled-rejected|run#13|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: iam.mfa-devices-per-user-quota-exceeded-rejected|_error#13|AssertionError|raise AssertionError(f"expected this call to be refused, but it succeeded: {result!r}")
Issue-Key: iam.saml-provider-assertion-encryption-required-without-a-key-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: iam.saml-provider-identity-providers-quota-exceeded-rejected|_error#14|AssertionError|raise AssertionError(f"expected this call to be refused, but it succeeded: {result!r}")
Issue-Key: iam.saml-provider-keys-quota-exceeded-rejected|_error#14|AssertionError|raise AssertionError(f"expected this call to be refused, but it succeeded: {result!r}")
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# iam: 6 net-new causes — denials omit the authorization-details pointer, permissions-boundary/MFA/SAML refusals missing, wrong MFA message

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRoles

## Description
Six net-new root causes in IAM. (The open issue 20261109T163411Z-iam-3-net-new-causes-denials-omit-the.md describes the same missing authorization-details pointer for other checks; cause 1 here is grounded in four further failing checks it does not carry.)

Cause 1 of 6: an AccessDenied message omits the trailing GetRequestAuthorizationDetails pointer.
AWS: recorded behavior only for the tail (the model carries no message text). Every IAM denial ends with `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` — fixed text apart from the region and the per-request authorization id, which appears twice and must be the same value in both places. The leading sentence (`User: <principal-arn> is not authorized to perform: <action> on resource: <resource> because no identity-based policy allows the <action> action` / `... with an explicit deny in an identity-based policy`) is correct in the emulator.
Emulator: the message stops after the leading sentence, with no `. Go to ...` tail at all, for implicit denials (iam:ListRoles, iam:AttachUserPolicy) and explicit denials (iam:ListUsers) alike.
Why: operators and tooling resolve a denial through that authorization id; without it a client cannot look up why the request was denied, and message-equality assertions against real AWS fail.
Fix: the IAM authorization denial serializer — mint a per-request authorization id and append the pointer sentence to every AccessDenied message.
Traces: iam.absent-condition-key-denied-unless-ifexists-forallvalues-or-null, iam.current-time-date-condition-window-denied-outside, iam.op-contract-list-users-live-enforcement-denied, iam.policy-arn-condition-scopes-delegated-attach-other-policy-denied.

Cause 2 of 6: CreateRole/CreateUser accept a PermissionsBoundary ARN that names no policy.
AWS: Smithy CreateRole and CreateUser take `PermissionsBoundary` (arnType) and declare NoSuchEntityException (404, awsQueryError NoSuchEntity) / InvalidInputException (400); AWS resolves the boundary policy and refuses when it does not exist, creating nothing.
Emulator: both calls return 200 and the principal is created carrying the dangling boundary — the response even echoes `PermissionsBoundary: {'PermissionsBoundaryType': 'Policy', 'PermissionsBoundaryArn': 'arn:aws:iam::000000000000:policy/sim2real-missing-...'}`.
Why: a principal exists with a boundary that cannot be evaluated; a client believes its blast radius is bounded when nothing bounds it.
Fix: CreateRole/CreateUser input validation — resolve PermissionsBoundary against stored policies and refuse when absent.
Traces: iam.create-role-permissions-boundary-nonexistent-arn-rejected, iam.create-user-permissions-boundary-nonexistent-arn-rejected.

Cause 3 of 6: DeactivateMFADevice's refusal message for a device that is not enrolled is not AWS's.
AWS: Smithy DeactivateMFADevice declares NoSuchEntityException; the recorded message is the fixed sentence `MFA Device invalid for user.` (no request-derived parts).
Emulator: `MFA Device arn:aws:iam::000000000000:mfa/<serial> cannot be found for user <user>.` — a bespoke sentence that leaks the serial and user name.
Why: clients matching the documented message mis-handle the error, and the emulator's text implies a different (not-found) classification.
Fix: the DeactivateMFADevice handler's NoSuchEntity message.
Traces: iam.deactivate-mfa-device-not-enrolled-rejected.

Cause 4 of 6: the per-user MFA device quota is not enforced.
AWS: Smithy EnableMFADevice declares LimitExceededException (409, awsQueryError LimitExceeded); enrolling past the per-user device quota is refused.
Emulator: the over-quota call succeeds (`AssertionError: expected this call to be refused, but it succeeded: None` — a 200 with an empty body).
Why: a client can exceed a quota AWS enforces, so quota-handling code is never exercised and emulator state diverges from anything reproducible on AWS.
Fix: EnableMFADevice's quota check against the user's enrolled devices.
Traces: iam.mfa-devices-per-user-quota-exceeded-rejected.

Cause 5 of 6: CreateSAMLProvider accepts AssertionEncryptionMode=Required with no private key on file.
AWS: Smithy CreateSAMLProvider models `AssertionEncryptionMode` and `AddPrivateKey` and declares InvalidInputException (400); requiring encrypted assertions with no decryption key is refused and no provider is created.
Emulator: the call returns 200 with a SAMLProviderArn.
Why: an identity provider that can never decrypt an assertion is created; the misconfiguration surfaces only at federation time.
Fix: CreateSAMLProvider's cross-field validation (AssertionEncryptionMode Required implies AddPrivateKey).
Traces: iam.saml-provider-assertion-encryption-required-without-a-key-rejected.

Cause 6 of 6: the SAML identity-provider count quota and the per-provider private-key quota are not enforced.
AWS: Smithy CreateSAMLProvider/AddClientIDToOpenIDConnectProvider-class operations declare LimitExceededException (409, awsQueryError LimitExceeded); the account's SAML provider quota and a provider's private-key quota are both enforced, and the over-quota call is refused with nothing created.
Emulator: both over-quota calls return 200 with a SAMLProviderArn (`expected this call to be refused, but it succeeded: {'SAMLProviderArn': ...}`).
Why: quota exhaustion — a real operational failure mode — cannot be reproduced, and clients that handle LimitExceeded never see it.
Fix: the SAML provider store's quota enforcement on create and on key addition.
Traces: iam.saml-provider-identity-providers-quota-exceeded-rejected, iam.saml-provider-keys-quota-exceeded-rejected.

## Evidence
Cause 1: `AssertionError: iam:ListRoles: expected message 'User: arn:aws:iam::000000000000:user/<user> is not authorized to perform: iam:ListRoles on resource: arn:aws:iam::000000000000:role/ because no identity-based policy allows the iam:ListRoles action. Go to <authorization-details pointer>', got 'User: ... because no identity-based policy allows the iam:ListRoles action'`; same tail missing for iam:AttachUserPolicy and for the explicit-deny iam:ListUsers message.
Cause 2: `AssertionError: expected the call to be rejected, but it succeeded: {'Role': {... 'PermissionsBoundary': {'PermissionsBoundaryType': 'Policy', 'PermissionsBoundaryArn': 'arn:aws:iam::000000000000:policy/sim2real-missing-...'} ...}, 'HTTPStatusCode': 200}` (and the CreateUser twin).
Cause 3: `AssertionError: expected message 'MFA Device invalid for user.', got 'MFA Device arn:aws:iam::000000000000:mfa/<serial> cannot be found for user <user>.'`
Cause 4: `AssertionError: expected this call to be refused, but it succeeded: None`
Cause 5: `AssertionError: expected the call to be rejected, but it succeeded: {'SAMLProviderArn': 'arn:aws:iam::000000000000:saml-provider/<name>', ... 'HTTPStatusCode': 200}`
Cause 6: same success shape for the provider-count and private-key quota calls.

## Steps to Reproduce
Cause 1: create a user with a policy that allows nothing for iam:ListRoles (or an explicit Deny for iam:ListUsers) and call the operation with that user's key. Compare the AccessDenied message tail — the `. Go to https://...#/authorization-details/<id> ... authorization id: <id>` sentence is absent.
Cause 2: CreateRole / CreateUser with PermissionsBoundary=arn:aws:iam::<account>:policy/<name-that-does-not-exist>. Observed 200; expected a refusal.
Cause 3: CreateUser, then DeactivateMFADevice with a SerialNumber never enrolled for that user. Compare the message to "MFA Device invalid for user."
Cause 4: EnableMFADevice repeatedly for one user past the per-user quota. Observed success; expected LimitExceeded.
Cause 5: CreateSAMLProvider with AssertionEncryptionMode=Required and no AddPrivateKey. Observed 200; expected InvalidInput.
Cause 6: CreateSAMLProvider past the account's provider quota, and add private keys past a provider's key quota. Observed 200; expected LimitExceeded.
