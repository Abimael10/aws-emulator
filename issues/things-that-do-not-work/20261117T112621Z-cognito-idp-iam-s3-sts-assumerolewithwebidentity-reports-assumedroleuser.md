Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__iam__s3__sts.direct-oidc-assume-role-with-web-identity-grants-scoped-s3-read
Issue-Key: cognito-idp__iam__s3__sts.direct-oidc-assume-role-with-web-identity-grants-scoped-s3-read|run#34|AssertionError|assert assumed["Provider"] == issuer_key, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp__iam__s3__sts: AssumeRoleWithWebIdentity reports AssumedRoleUser Provider with the 'https://' scheme instead of the bare issuer host/path

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeRoleWithWebIdentity

## Description
Cause 1 of 1: the web-identity Provider echoed by STS carries the token issuer's URL scheme.
AWS: AssumeRoleWithWebIdentity's response reports the OIDC provider as the identifier IAM registers for it — the issuer URL with the scheme stripped, i.e. '<host>[:<port>]/<path>' (this is the same form as the IAM OIDC provider ARN's trailing segment, which is why the check compares it to the issuer key it registered). The scheme 'https://' is never part of it.
Emulator: the response reports 'https://<host>:<port>/<poolId>' — the raw iss claim of the id token, scheme included.
Why: a client correlating the assumed-role response with the IAM OIDC provider it created (string-comparing the provider key, or building the provider ARN from it) does not match, so provider-scoped logic and policy conditions keyed on the provider fail.
Fix: the Provider projection in the AssumeRoleWithWebIdentity handler (strip the scheme from the token issuer when deriving the provider identifier) in the sts crate.
Traces: cognito-idp__iam__s3__sts.direct-oidc-assume-role-with-web-identity-grants-scoped-s3-read.

## Evidence
AssertionError: Provider: expected '127.0.0.1:45815/us-west-2_23b1e5060', got 'https://127.0.0.1:45815/us-west-2_23b1e5060' (host/port are environment values; the requirement is the scheme-less form).

## Steps to Reproduce
Replay cognito-idp__iam__s3__sts.direct-oidc-assume-role-with-web-identity-grants-scoped-s3-read: register the user pool's issuer as an IAM OIDC provider, sign a user in to obtain an id token, call sts:AssumeRoleWithWebIdentity with it, and compare the response's Provider against the registered issuer key.
