Issue-Source: trace-sweep-divergence
Issue-Trace: cogid-creds-custom-role-arn-outside-mappings-rejected
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cogid-creds-custom-role-arn-outside-mappings-rejected cogid-creds-developer-authenticated-credentials-envelope cogid-creds-unauth-credentials-envelope
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity|GetCredentialsForIdentity|2xx->2xx|$.Credentials.AccessKeyId
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# cognito-identity: GetCredentialsForIdentity vends a Credentials.AccessKeyId that is not an STS temporary-credential key id

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
One net-new root cause, from the only wire divergence cluster of this corpus (simparity-wire-aws-v1) that the replay report marked neither ALREADY FILED (empty stub_path) nor OWNED (empty owned_by).

=== Cause 1 of 1: the access key id in the vended Credentials envelope is an emulator literal, not an STS temporary-credential key id ===

AWS: Smithy (com.amazonaws.cognitoidentity) — GetCredentialsForIdentity returns GetCredentialsForIdentityResponse{IdentityId, Credentials}; Credentials.AccessKeyId targets AccessKeyString ("The Access Key portion of the credentials"), which carries NO @pattern and NO @length, so the key-id SHAPE is recorded behavior only and the fixture asserts the shape rather than a literal ('<shaped-not-asserted>'). What a real identity pool vends is the credential of an sts:AssumeRoleWithWebIdentity session, and the access key id of ANY AWS temporary/session credential carries the STS temporary-credential key-type prefix 'ASIA' followed by uppercase-alphanumeric ([A-Z0-9]) body characters — never a long-term 'AKIA' key id and never a vendor literal. The status for the whole cluster is HTTP 200 with no error; the requirement is the key-type CLASS of the key id, while the body after the prefix, the secret, the session token, the expiration and the identity id are per-run random values and are NOT requirements.

Emulator: the cluster answers HTTP 200 (expected status matched) but $.Credentials.AccessKeyId is the literal-prefixed value 'EMULATORASKKCYNMYUWG' — fixed leading text 'EMULATOR' plus a random uppercase body — so the response is not a temporary-credential key id of any AWS key-type class. Replay detail: "$.Credentials.AccessKeyId: value_mismatch (expected '<shaped-not-asserted>', got 'EMULATORASKKCYNMYUWG')". The same emulator value shape appears on all four steps of the cluster, across the unauthenticated vend, the developer-authenticated vend, and the vend performed after a CustomRoleArn-outside-RoleMappings request was rejected — i.e. it is the shared minting path, not one code path.

Why: clients classify a credential by the key-type prefix of its access key id. SDK and CLI credential-type detection, audit/compliance tooling, and any harness that must prove a caller is a role session rather than a long-term key all read a Cognito-vended credential as a non-session, non-AWS credential. Code that fails closed on "this is not a temporary credential" breaks against the emulator, and code that asserts the session nature of a Cognito vend passes only by accident. Policy/condition logic keyed on the credential type sees the wrong class.

Fix: the shared vended-role-session minting component that the cognito-identity GetCredentialsForIdentity handler calls after it resolves the role and whose vended access key id is returned verbatim as Credentials.AccessKeyId (the vend in services/service-com-amazonaws-cognitoidentity-awscognitoidentityservice/src/ops/get_credentials_for_identity.rs). It must generate the key id with the STS temporary-credential key-type prefix instead of an emulator literal. No other member of the Credentials envelope or of the response is at fault in this cluster — IdentityId, the envelope's remaining members and the 200 status all matched.

Traces: cogid-creds-custom-role-arn-outside-mappings-rejected#task#7 (representative), plus the cluster's other members cogid-creds-developer-authenticated-credentials-envelope#task#6 and cogid-creds-unauth-credentials-envelope#task#6 — one cluster, one cause, so no additional cause tokens are listed.

Related open issues that do NOT cover this cluster (none carries its replay key; the report marked it neither OWNED nor ALREADY FILED, and these were all raised against the behavioral corpus, not simparity-wire-aws-v1): 20261029T071507Z-cognito-identity-getcredentialsforidentity-under-a-token-role-mapping.md (same symptom on the Token role-mapping path), 20261028T221339Z-cognito-identity-3-net-new-getcredentialsforidentity-root-causes.md (developer-identity and Rules-mapping paths) and 20261028T040721Z-cognito-identity-cognito-idp-iam-getcredentialsforidentity-vends-an.md (the IAM-OIDC-registered-issuer path). A single fix in the shared minting component would close all of them, which is why this is filed as one cause.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (score 97.6%, 12 mismatched steps, bootstrap_signal false, emulator_up true).

Divergence cluster service=cognito-identity|GetCredentialsForIdentity|status=200->200|keys=$.Credentials.AccessKeyId (verdict: mismatch, affected_count 3, stub_path empty, owned_by empty)
  representative: cogid-creds-custom-role-arn-outside-mappings-rejected#task#7
  expected status 200, actual status 200 (status itself matches)
  expected: $.Credentials.AccessKeyId — shape-asserted, not literal ('<shaped-not-asserted>'): an STS temporary-credential access key id, i.e. the fixed key-type prefix 'ASIA' followed by uppercase-alphanumeric body characters. The body after the prefix is a per-run random value and is not part of the requirement.
  actual:   $.Credentials.AccessKeyId = 'EMULATORASKKCYNMYUWG' — fixed literal text 'EMULATOR' followed by a random body; no AWS key-type prefix.

Other members of the same cluster, same expected-vs-actual:
  cogid-creds-developer-authenticated-credentials-envelope#task#6
  cogid-creds-unauth-credentials-envelope#task#6

Controls that isolate the cause: every other asserted member of these three fixtures' credential envelopes passes (486 of 498 steps passed overall), the HTTP status is the recorded 200, no error envelope is involved, and the divergence is identical on the unauthenticated, developer-authenticated and post-CustomRoleArn-rejection vends — so the key-id class is the sole divergence and it comes from the shared minting path.

Not requirements and deliberately excluded from this issue: the key-id body characters, the secret key, the session token, the expiration timestamp, the identity id and the request id, all of which differ on every run.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout.
2. Replay the wire fixture cogid-creds-custom-role-arn-outside-mappings-rejected and inspect step task#7; repeat with cogid-creds-unauth-credentials-envelope step task#6 and cogid-creds-developer-authenticated-credentials-envelope step task#6.
3. Manually: CreateIdentityPool with AllowUnauthenticatedIdentities=true (add a DeveloperProviderName for the developer-authenticated fixture) and SetIdentityPoolRoles with Roles{authenticated, unauthenticated} pointing at roles that trust cognito-identity.amazonaws.com for sts:AssumeRoleWithWebIdentity.
4. GetId (or GetOpenIdTokenForDeveloperIdentity for the developer path), then call GetCredentialsForIdentity with that IdentityId.
5. Read $.Credentials.AccessKeyId from the 200 response: AWS returns an STS temporary-credential key id whose fixed prefix is 'ASIA'; the emulator returns one whose fixed prefix is the literal 'EMULATOR' (observed 'EMULATORASKKCYNMYUWG').
