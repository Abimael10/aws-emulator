Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket
Issue-Key: iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket|run#15|AssertionError|assert payload["access_key_prefix"] == "ASIA", (
Issue-Commit: 5bda3f4959ddaaecd7545612991d55d37567ef10

# sts AssumeRole called inside a function still vends an EMUL-class AccessKeyId instead of the ASIA class, and the class cannot be changed from a service crate

> _Split out of `issues/things-that-now-work/20261115T051545Z-iam-lambda-s3-sts-2-net-new-causes.md`, whose cause 1 (UpdateAssumeRolePolicy / CreateRole accepting a trust document naming an unresolvable `AWS` principal) is fixed. This report carries the replay key that does NOT pass, re-filed here so it is not recorded as working. Everything below is what that fix established about it; a later fixer starts from here rather than from scratch._

## Operation
AssumeRole

## Description
One cause, and it is BLOCKED on a file the merge gate refuses to let a fix edit rather than
unimplemented.

AWS: every set of temporary security credentials STS vends carries an `AccessKeyId` whose
leading four-character class token is `ASIA`, while a long-term IAM user key begins `AKIA`
(AWS IAM User Guide, "IAM identifiers" -> unique identifier prefixes). The Smithy model puts
no pattern on `Credentials$AccessKeyId` beyond `accessKeyIdType` (length 16..128, pattern
`^[\w]*$`), so the class token is recorded behaviour plus AWS's documented prefixes; only the
leading four characters are the requirement, and the rest is a per-mint random id that is NOT
part of it. Clients branch on that class: tooling decides from it whether a credential needs
an accompanying session token, credential scanners and policy linters read it, and this
emulator's own reading of the class (`crate::state::ACCESS_KEY_ID_PREFIXES` in
`services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/src/state.rs`, which
`GetAccessKeyInfo` serves) expects it.

Emulator: a `sts:AssumeRole` made with an in-handler boto3 client inside a Lambda answers an
`AccessKeyId` whose first four characters are `EMUL`.

Why it is BLOCKED, and on what: a service crate cannot choose the class. Every credential this
emulator vends is minted by the core credential store, and the kinds are SEALED:

* `vendor/server/src/credential_store.rs:17` — `pub trait CredentialKind: Send + Sync +
  sealed::Sealed`, with `sealed` a `pub(crate)` module, so no crate under `services/` can
  define a credential kind of its own, and `CredentialStore::create` (:33) takes only a
  `&'static dyn CredentialKind`;
* `vendor/server/src/aws_credentials.rs:47,57` — the only two kinds, `AwsAccessKey` and
  `AwsSession`, mint ids prefixed `EMULATORAK` and `EMULATORAS`; the comment at :12-16 states
  that `EMULATOR` class is deliberate, "so secret scanners never mistake one for an AWS key";
* `vendor/server/src/request_authentication.rs:138-142` — SigV4 verification resolves the key
  id a request presents only through `CredentialStore::secret_for`, so an id a handler made up
  is not a credential the emulator can verify. The failing check SIGNS an S3 read with the
  vended credential, so fabricating the prefix in the handler would break the very call the
  assertion is about, and `AGENTS.md` forbids a handler constructing a credential id or secret
  in any case.

So the honest exit for this cause is `mark_wont_fix(vendor_blocker)` naming
`vendor/server/src/aws_credentials.rs`, unless the core library changes. The required core
change: `AwsSession::mint` (and `AwsAccessKey::mint` with it) must mint AWS's published classes
— `ASIA` for a temporary credential, `AKIA` for a long-term key — or `CredentialKind` must be
opened so the service that mints a session names the class, after which
`crate::state::mint_session` in the STS crate needs no change at all and every vending
operation (`AssumeRole`, `AssumeRoleWith*`, `GetSessionToken`, `GetFederationToken`) is fixed
at once. The project's own threat model already describes STS sessions as `ASIA…`
(SR-SESSION-01), so the core change does not widen anything the design did not intend; a
scanner-safe class token that is still `ASIA`-classed is the only open question for whoever
makes it.

Related open reports of the same family, all waiting on the same core change:
`issues/things-that-do-not-work/20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md`,
`issues/things-that-do-not-work/20261027T130805Z-sts-5-net-new-behavioral-root-causes-temporary.md`,
`issues/things-that-do-not-work/20261103T090000Z-sts-a-temporary-credentials-class-is-not-read-anywhere.md`
and
`issues/things-that-do-not-work/20261115T051545Z-sts-getsessiontoken-mints-an-emul-class-accesskeyid-instead.md`.
One core change closes all of them.

Traces: iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket.

## Evidence
Check `iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket`, line 468 —
AssertionError: "temporary credentials must use the ASIA access-key prefix, got 'EMUL'". The
handler read `Credentials.AccessKeyId[:4]` from its own `sts:AssumeRole` response.
Expected prefix: `ASIA`. Actual prefix: `EMUL` (the rest of the observed id is a per-run value
and is not part of the requirement).

Replay after the parent fix: this unit still fails on this key; the parent's other unit
(`assume-role-denied-when-trust-policy-omits-lambda-role`) passes, and `other_causes` was
empty, so the parent fix caused nothing here.

## Steps to Reproduce
1. Create a Lambda execution role and a second role whose trust policy names that execution
   role's ARN, and a `python3.12` function whose handler calls `boto3` `sts:AssumeRole` on the
   second role and returns `Credentials.AccessKeyId[:4]`.
2. Invoke the function and read that value: the emulator yields `EMUL`; AWS yields `ASIA`.
3. The same holds for `GetSessionToken` and `GetFederationToken`, and for a direct
   `sts:AssumeRole` on the socket.
