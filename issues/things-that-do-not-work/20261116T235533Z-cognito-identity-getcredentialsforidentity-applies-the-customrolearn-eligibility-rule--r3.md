Issue-Source: trace-sweep-divergence
Issue-Trace: cogid-creds-developer-authenticated-credentials-envelope
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cogid-creds-developer-authenticated-credentials-envelope cogid-merge-getid-blank-absorbed-by-registered-developer-login
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity|GetCredentialsForIdentity|4xx->4xx|$headers.x-amzn-errortype
Issue-Related: 20261030T212445Z-cognito-identity-developer-identity-token-and-credential-refusals.md
Issue-Commit: bc9fe25dd989616dac13be04840d271782f8fab6

# cognito-identity: GetCredentialsForIdentity applies the CustomRoleArn eligibility rule before validating the Logins token, so a non-Cognito login token is refused InvalidParameterException instead of NotAuthorizedException

> _Note: this issue was identified against the code base at commit `bc9fe25dd989616dac13be04840d271782f8fab6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
One root cause, on the GetCredentialsForIdentity credential-vending path. The emulator evaluates the CustomRoleArn eligibility rule before it validates the login tokens in the Logins map, so a request that is wrong in BOTH ways is refused with the wrong modeled error class, the wrong error-type header and a re-worded message.

Cause 1 of 1: GetCredentialsForIdentity called with a Logins entry whose login token is not a Cognito-issued OpenID token, together with a CustomRoleArn the pool's role configuration does not permit.

AWS: the login token is validated first, so the refusal is the token refusal, not the role-ARN one. The response is HTTP 400 with body member `__type` = `NotAuthorizedException` and response header `x-amzn-errortype: NotAuthorizedException:` (awsJson1_1 carries the error class in both places, so a wrong class is wrong twice). The message is entirely FIXED text with no variable parts — no name, id, limit or member name is interpolated into it: `Invalid login token. Not a Cognito token.` Smithy (com.amazonaws.cognitoidentity#AWSCognitoIdentityService, awsJson1_1): GetCredentialsForIdentity errors include NotAuthorizedException (client, @httpError 403) and InvalidParameterException (client, @httpError 400); input member CustomRoleArn targets ARNString @length min 20 max 2048, Logins targets LoginsMap @length min 0 max 10. The model does not express the ordering between the token check and the CustomRoleArn eligibility check, and the recorded AWS status for this refusal is 400 rather than NotAuthorizedException's modeled 403 — both are recorded behavior only.

Emulator: HTTP 400 (the status itself matches, so this is not an error-status-mapping defect) but `__type` = `InvalidParameterException`, header `x-amzn-errortype: InvalidParameterException:`, and the message `Only SAML providers and providers with RoleMappings support custom role ARN.` — the generic role-mapping/custom-role-ARN refusal, whose own fixed text is correct for the case where ONLY the role ARN is ineligible, but which here fires before the token is ever looked at.

Why: a client branching on the error class to decide what to do takes the wrong branch. NotAuthorizedException means 're-authenticate: this federated login token is stale, forged or from another issuer', and SDK/application code reacts by refreshing the login and retrying. InvalidParameterException means 'the request parameters are malformed', and the message points at a CustomRoleArn the caller must stop sending. So a caller holding an expired or non-Cognito token either retries the same bad token forever or removes a CustomRoleArn that was never the real problem, and never learns that its login needs refreshing. A negative test written against AWS that asserts NotAuthorizedException fails only on the emulator.

Fix: the input-validation ordering and error selection in the GetCredentialsForIdentity handler — validate every Logins entry's token (issuer, shape, signature, pool binding) BEFORE applying the CustomRoleArn eligibility rule, and raise NotAuthorizedException with AWS's fixed message for a non-Cognito token. The x-amzn-errortype header follows from the selected error class in the awsJson1_1 error serializer, so it needs no separate change once the class is right.

Related open issue that the replay report does NOT record as owning this replay key: '20261030T212445Z-cognito-identity-developer-identity-token-and-credential-refusals.md' describes this same validation-ordering fault, but its recorded replay keys cover only the `$.__type` and `$.message` body members and not the `$headers.x-amzn-errortype` key this cluster also diverges on, so this cluster is reported unowned and is filed here with the complete key set. Other open cognito-identity issues that do NOT cover it (different faults): '20261029T230931Z-cognito-identity-vended-tokens-and-credentials-are-not.md' (vended key/token shape), '20261113T131245Z-cognito-identity-developer-identity-ownership-refusals-are-mis.md' (LookupDeveloperIdentity ownership refusals).

Traces: cogid-creds-developer-authenticated-credentials-envelope#task#7 (representative) and cogid-merge-getid-blank-absorbed-by-registered-developer-login#task#11 — both members of the one cluster for this cause.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator up; 57 fixtures, 498 steps, 457 passed, 41 mismatched, 12 divergence clusters). Cluster divergence_key `service=cognito-identity|GetCredentialsForIdentity|status=400->400|keys=$.__type,$.message,$headers.x-amzn-errortype`, affected_count 2:
- status: expected 400, actual 400 (no status divergence).
- $.__type: expected 'NotAuthorizedException', got 'InvalidParameterException'.
- $.message: expected 'Invalid login token. Not a Cognito token.' (fixed text, no variable parts), got 'Only SAML providers and providers with RoleMappings support custom role ARN.'
- $headers.x-amzn-errortype: expected the NotAuthorizedException error-type header ('NotAuthorizedException:'), got 'InvalidParameterException:'.
Failing steps: cogid-creds-developer-authenticated-credentials-envelope#task#7, cogid-merge-getid-blank-absorbed-by-registered-developer-login#task#11.
Smithy: GetCredentialsForIdentity errors = ExternalServiceException(400), InvalidIdentityPoolConfigurationException(400), InvalidParameterException(400), NotAuthorizedException(403), ResourceConflictException(409), ResourceNotFoundException(404), TooManyRequestsException(429), InternalErrorException(500); NotAuthorizedException = client error, @httpError 403, single `message` member, doc 'Thrown when a user is not authorized to access the requested resource.'; InvalidParameterException = client error, @httpError 400, doc 'Thrown for missing or bad input parameter(s).'; input CustomRoleArn = ARNString @length 20..2048, Logins = LoginsMap @length 0..10. Recorded wire status for this refusal is 400.

## Steps to Reproduce
1. Create an identity pool and obtain an identity id (the fixture's earlier steps do this via GetId / GetOpenIdTokenForDeveloperIdentity).
2. Call GetCredentialsForIdentity with that IdentityId, a Logins map whose single entry's value is NOT a Cognito-issued OpenID token, and a CustomRoleArn that the pool's role configuration does not permit (no SAML provider and no RoleMappings entry for that provider).
3. Observe the response: AWS answers HTTP 400 with __type 'NotAuthorizedException', header 'x-amzn-errortype: NotAuthorizedException:' and the message 'Invalid login token. Not a Cognito token.'; the emulator answers HTTP 400 with __type 'InvalidParameterException', header 'x-amzn-errortype: InvalidParameterException:' and the message 'Only SAML providers and providers with RoleMappings support custom role ARN.'
4. Replay to confirm: fixture cogid-creds-developer-authenticated-credentials-envelope step task#7, and fixture cogid-merge-getid-blank-absorbed-by-registered-developer-login step task#11. Compare HTTP status, body __type, body message and the x-amzn-errortype header against the recorded responses.
