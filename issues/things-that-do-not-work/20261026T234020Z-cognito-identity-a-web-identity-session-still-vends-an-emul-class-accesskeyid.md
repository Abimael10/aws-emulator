Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity.classic-flow-open-id-token-assume-role-with-web-identity
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity.classic-flow-open-id-token-assume-role-with-web-identity|run#21|AssertionError|assert creds["AccessKeyId"].startswith("ASIA"), (
Issue-Commit: da7bc09c8c1c199d36cb9fc39c5c5796d768dc3a

# A session minted from a Cognito OpenID token still carries an EMUL-class AccessKeyId instead of the ASIA class, and the class cannot be changed from a service crate

> _Split out of `issues/things-that-now-work/20261026T234020Z-cognito-identity-4-net-new-root-causes-web.md` (its cause 1 of 4), whose other three causes are fixed. This report carries the replay key that does NOT pass, re-filed here so it is not recorded as working. A later fixer starts from here rather than from scratch._

## Operation
AssumeRoleWithWebIdentity (reached through the Amazon Cognito Identity basic/classic flow:
GetId -> GetOpenIdToken -> sts:AssumeRoleWithWebIdentity)

## Description
One cause, and it is BLOCKED on a file the merge gate refuses to let a fix edit rather than
unimplemented.

AWS: every set of temporary security credentials STS vends carries an `AccessKeyId` whose
leading four-character class token is `ASIA`, while a long-term IAM user key begins `AKIA`
(AWS IAM User Guide, "IAM identifiers" -> unique identifier prefixes). The STS model puts no
prefix on `Credentials$AccessKeyId` beyond `accessKeyIdType` (length 16..128, pattern
`^[\w]*$`), so the class token is AWS's documented behaviour rather than a model constraint;
only the leading four characters are the requirement, and the characters after them are a
per-mint random value that is NOT part of it.

Emulator: the session the exchange vends carries an `AccessKeyId` beginning with the literal
`EMULATOR` (`EMULATORAS…`).

Why it is BLOCKED, and on what: a service crate cannot choose the class. The STS crate's
`crate::state::mint_session`
(`services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state.rs`) mints
through the core credential store, and the kinds are SEALED:

* `vendor/server/src/credential_store.rs:17` — `pub trait CredentialKind: Send + Sync +
  sealed::Sealed`, with `sealed` a module private to the core, so no crate under `services/`
  can define a credential kind of its own, and `CredentialStore::create` (:33) takes only a
  `&'static dyn CredentialKind`;
* `vendor/server/src/aws_credentials.rs:47,57` — the only two kinds, `AwsAccessKey` and
  `AwsSession`, mint ids prefixed `EMULATORAK` and `EMULATORAS`; the comment at :12-16 states
  that the `EMULATOR` class is deliberate, "so secret scanners never mistake one for an AWS
  key";
* `AGENTS.md` forbids a handler constructing a credential id or a secret of its own, and the
  emulator verifies a signed request only against the id the credential store minted — so
  fabricating the prefix in the handler would make the vended credential unusable for the
  signed calls these checks go on to make.

The required core change: `AwsSession::mint` (and `AwsAccessKey::mint` with it) must mint AWS's
published classes — `ASIA` for a temporary credential, `AKIA` for a long-term key — or
`CredentialKind` must be opened so the service that mints a session names the class. After it,
`mint_session` needs no change at all and every vending path is fixed at once:
`AssumeRole`, `AssumeRoleWithWebIdentity`, `AssumeRoleWithSAML`, `GetSessionToken`,
`GetFederationToken`, and the Amazon Cognito Identity `GetCredentialsForIdentity` that vends
through them.

Related open reports of the same family, all waiting on the same core change — one core change
closes all of them:
`issues/things-that-do-not-work/20261115T051545Z-sts-assumerole-inside-a-function-still-vends-an-emul-class-accesskeyid.md`
(which carries the fullest analysis),
`issues/things-that-do-not-work/20261115T051545Z-sts-getsessiontoken-mints-an-emul-class-accesskeyid-instead.md`,
`issues/things-that-do-not-work/20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md`,
`issues/things-that-do-not-work/20261027T130805Z-sts-5-net-new-behavioral-root-causes-temporary.md`,
`issues/things-that-do-not-work/20261103T090000Z-sts-a-temporary-credentials-class-is-not-read-anywhere.md`
and
`issues/things-that-do-not-work/20261028T040721Z-cognito-identity-cognito-idp-iam-getcredentialsforidentity-vends-an.md`.

Traces: cognito-identity.classic-flow-open-id-token-assume-role-with-web-identity.

## Evidence
Check `cognito-identity.classic-flow-open-id-token-assume-role-with-web-identity` —
AssertionError: "STS session key must start with ASIA, got EMULATORAS…". The check read
`Credentials.AccessKeyId` from its own `sts:AssumeRoleWithWebIdentity` response.
Expected: the leading four characters `ASIA`. Actual: `EMUL` (the characters after the class
token in any observed id are per-run values and are not part of the requirement).

Replay after the parent fix: this unit still fails on this key; the parent's other three units
(`cloudtrail-logs-pool-management-but-not-identity-data-events`,
`e2e-principal-tags-federation-to-sts-session-tags` and
`server-side-token-check-refuses-a-globally-signed-out-user`) pass, and `other_causes` was
empty, so the parent fix caused nothing here.

## Steps to Reproduce
1. CreateIdentityPool with `AllowUnauthenticatedIdentities=true`; create a role whose trust
   policy federates `cognito-identity.amazonaws.com` for `sts:AssumeRoleWithWebIdentity`;
   SetIdentityPoolRoles for it.
2. GetId, then GetOpenIdToken for that identity.
3. sts:AssumeRoleWithWebIdentity with the returned `Token` and inspect
   `Credentials.AccessKeyId[:4]`: the emulator yields `EMUL`; AWS yields `ASIA`.
4. The same holds for every other vending path (`AssumeRole`, `GetSessionToken`,
   `GetFederationToken`, `GetCredentialsForIdentity`), which is why the fix belongs in the one
   place that mints them.
