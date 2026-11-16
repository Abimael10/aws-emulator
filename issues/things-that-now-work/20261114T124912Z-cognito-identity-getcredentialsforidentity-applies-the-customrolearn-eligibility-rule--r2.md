Issue-Source: trace-sweep-divergence
Issue-Trace: cogid-creds-developer-authenticated-credentials-envelope
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cogid-creds-developer-authenticated-credentials-envelope cogid-merge-getid-blank-absorbed-by-registered-developer-login
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity|GetCredentialsForIdentity|4xx->4xx|$headers.x-amzn-errortype
Issue-Related: 20261030T212445Z-cognito-identity-developer-identity-token-and-credential-refusals.md
Issue-Commit: 9a1cab38181c81438a46747e76c041451acccd46

# cognito-identity: GetCredentialsForIdentity applies the CustomRoleArn eligibility rule before validating the login token, so a non-Cognito login token is refused as InvalidParameterException instead of NotAuthorizedException

> _Note: this issue was identified against the code base at commit `9a1cab38181c81438a46747e76c041451acccd46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
One root cause, on the GetCredentialsForIdentity refusal path.

Cause 1 of 1: GetCredentialsForIdentity is called with a Logins entry whose value is NOT a Cognito-issued OpenID token (e.g. a raw developer-provider login value rather than the Token returned by GetOpenIdTokenForDeveloperIdentity under the cognito-identity.amazonaws.com key), AND with a CustomRoleArn that the identity pool's configuration does not permit (no SAML provider and no RoleMappings for that provider).

AWS: the login token is validated BEFORE the CustomRoleArn eligibility rule, so the refusal returned is the token refusal, not the role-ARN one. The recorded AWS response is HTTP 400 with body member `__type` = `NotAuthorizedException`, response header `x-amzn-errortype: NotAuthorizedException:`, and message `Invalid login token. Not a Cognito token.` That message is entirely FIXED text — it has no variable parts drawn from the request or from state (no provider name, no identity id, no ARN is interpolated). Smithy (com.amazonaws.cognitoidentity#AWSCognitoIdentityService, awsJson1_1): GetCredentialsForIdentity models both NotAuthorizedException (@httpError 403, documented "Thrown when a user is not authorized to access the requested resource.") and InvalidParameterException (@httpError 400); CustomRoleArn targets ARNString with @length min 20 max 2048; Logins targets LoginsMap with @length min 0 max 10. The 400 status actually observed on the wire for NotAuthorizedException, and the ORDERING of the token check ahead of the CustomRoleArn check, are recorded behavior only — the model does not express either.

Emulator: answers HTTP 400 (status matches) but with the wrong error class and the wrong message — body `__type` = `InvalidParameterException`, header `x-amzn-errortype: InvalidParameterException:`, message `Only SAML providers and providers with RoleMappings support custom role ARN.` It evaluates the CustomRoleArn eligibility rule first and therefore never reaches the login-token validation, so the token problem is never reported. (The emulator's custom-role-ARN message itself is the right refusal for a request whose logins ARE valid; the defect is that it is reached first.)

Why: a client that branches on the modeled error class gets the wrong branch. NotAuthorizedException is the signal to re-authenticate the federated login and retry with a fresh Cognito token; InvalidParameterException is the signal that a request parameter was malformed. A caller that sent a stale/foreign login token is told instead that the CustomRoleArn it supplied is unsupported — a parameter it did not get wrong — so it either retries forever with the same invalid token, strips a legitimate CustomRoleArn in a futile workaround, or surfaces a misleading error to the end user. The classification is wrong in both the body `__type` and the `x-amzn-errortype` header, so neither SDK error-shape dispatch nor header-based dispatch recovers.

Fix: the validation ordering and error selection in the GetCredentialsForIdentity handler — validate every Logins entry's token (raising NotAuthorizedException with AWS's fixed message for a login value that is not a Cognito token) BEFORE applying the CustomRoleArn eligibility rule, and emit the matching x-amzn-errortype header.

Traces: cogid-creds-developer-authenticated-credentials-envelope#task#7 (cluster representative) and cogid-merge-getid-blank-absorbed-by-registered-developer-login#task#11.

Open issues that do NOT own this replay key (the replay reports this cluster as unowned, so it is filed here): '20261030T212445Z-cognito-identity-developer-identity-token-and-credential-refusals.md' describes a related ordering fault but does not carry this cluster's replay key (its Issue-Key entries cover only $.__type and $.message, not the $headers.x-amzn-errortype divergence this cluster now also reports); '20261029T230931Z-cognito-identity-vended-tokens-and-credentials-are-not.md' and '20261114T065108Z-cognito-identity-9-net-new-root-causes-developer.md' are different faults.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator (emulator_up=true, 498 steps, 450 passed, 48 mismatched, score 90.4%).

Cluster service=cognito-identity|GetCredentialsForIdentity|status=400->400|keys=$.__type,$.message,$headers.x-amzn-errortype
Representative: cogid-creds-developer-authenticated-credentials-envelope#task#7; also affected: cogid-merge-getid-blank-absorbed-by-registered-developer-login#task#11 (affected_count=2); stub_path empty, owned_by empty.

  status:                      expected 400, got 400 (matches)
  $.__type:                    expected 'NotAuthorizedException', got 'InvalidParameterException'
  $.message:                   expected 'Invalid login token. Not a Cognito token.', got 'Only SAML providers and providers with RoleMappings support custom role ARN.'
  $headers.x-amzn-errortype:   got 'InvalidParameterException:' (expected the NotAuthorizedException form; recorded value shape-not-asserted)

Message decomposition: the expected message 'Invalid login token. Not a Cognito token.' is 100% fixed text for this error class — no request value, identity id, provider name, ARN, timestamp or request id appears in it, so it is reproducible verbatim. The emulator's substituted message is likewise fixed text, but belongs to a different (later) check.

Smithy evidence (services/service-com-amazonaws-cognitoidentity-awscognitoidentityservice/smithy-model/com-amazonaws-cognitoidentity-awscognitoidentityservice.json): GetCredentialsForIdentity input { IdentityId (required, @length 1..55, pattern ^[\w-]+:[0-9a-f-]+$), Logins (LoginsMap, @length 0..10), CustomRoleArn (ARNString, @length 20..2048) }; errors = ExternalServiceException(400), InvalidIdentityPoolConfigurationException(400), InvalidParameterException(400), NotAuthorizedException(403), ResourceConflictException(409), ResourceNotFoundException(404), TooManyRequestsException(429), InternalErrorException(500). Protocol awsJson1_1, so the error class is carried both in the body member __type and in the x-amzn-errortype header. The check ordering is recorded behavior only.

## Steps to Reproduce
1. Replay fixture cogid-creds-developer-authenticated-credentials-envelope, step task#7, against the emulator: GetCredentialsForIdentity with a Logins entry whose value is not a Cognito-issued OpenID token, together with a CustomRoleArn the identity pool's configuration does not permit (no SAML provider, no RoleMappings for that provider).
2. Compare the HTTP status, the body's __type and message members, and the x-amzn-errortype response header against the recording. Expected: 400, __type 'NotAuthorizedException', x-amzn-errortype 'NotAuthorizedException:', message 'Invalid login token. Not a Cognito token.'. Observed: 400, __type 'InvalidParameterException', x-amzn-errortype 'InvalidParameterException:', message 'Only SAML providers and providers with RoleMappings support custom role ARN.'.
3. Replay fixture cogid-merge-getid-blank-absorbed-by-registered-developer-login, step task#11, which reproduces the same cluster.
4. Confirm the ordering is the cause: re-issue the same request WITHOUT CustomRoleArn — the login-token refusal (NotAuthorizedException) must then be the one raised, showing the token check exists but is reached only after the CustomRoleArn rule.
