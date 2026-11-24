Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.user-pool-api-cors-preflight-echoes-requested-method-and-headers cognito-idp.user-pool-api-options-without-origin-rejected cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected cognito-idp.browser-origin-sign-up-response-carries-cors-headers cognito-idp.browser-origin-unsigned-admin-create-user-rejected cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint cognito-idp.set-user-mfa-preference-forbidden-without-authorization cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery cognito-idp.initiate-auth-user-password-auth-missing-password-param-rejected cognito-idp.user-password-auth-with-the-app-secret-hash-issues-usable-tokens cognito-idp.op-contract-update-provisioned-limit cognito-idp.provisioned-limit-large-numeric-value-precision-preserved cognito-idp.provisioned-limit-numeric-value-precision-round-trip cognito-idp.provisioned-limit-update-concurrent-modification-conflict cognito-idp.provisioned-limit-update-reflected cognito-idp.update-provisioned-limit-concurrent-writes-conflict cognito-idp.update-provisioned-limit-exceeds-quota-rejected cognito-idp.update-provisioned-limit-excessive-value-rejected cognito-idp.identity-provider-defaults-survive-update-deleted-rejects-access
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.user-pool-api-cors-preflight-echoes-requested-method-and-headers|run#3|AssertionError|assert r.status_code == 200, f"preflight answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.user-pool-api-options-without-origin-rejected|run#2|AssertionError|assert ok.status_code == 200, f"preflight with Origin answered {ok.status_code}: {ok.text[:300]!r}"
Issue-Key: cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin|run#10|AssertionError|assert status == 200, f"the preflight answered {status}, not 200: {body[:300]!r}"
Issue-Key: cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected|run#7|AssertionError|assert r.status_code == 400, f"SignUp without name answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.browser-origin-sign-up-response-carries-cors-headers|run#9|AssertionError|assert r.status_code == 200, f"unsigned SignUp answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.browser-origin-unsigned-admin-create-user-rejected|run#7|AssertionError|assert r.status_code == 400, f"unsigned AdminCreateUser answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token|_post#15|EgressRefused|with urllib.request.urlopen(req, timeout=20) as resp:
Issue-Key: cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens|request#18|EgressRefused|with self.opener.open(req, timeout=20) as resp:
Issue-Key: cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint|_post_revoke#22|EgressRefused|with urllib.request.urlopen(req, timeout=20) as resp:
Issue-Key: cognito-idp.set-user-mfa-preference-forbidden-without-authorization|_get_client_credentials_token#12|EgressRefused|with urllib.request.urlopen(req, timeout=15, context=tls) as resp:
Issue-Key: cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery|run#12|AssertionError|assert domain_desc.get("Status") == "ACTIVE"
Issue-Key: cognito-idp.initiate-auth-user-password-auth-missing-password-param-rejected|run#13|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cognito-idp.user-password-auth-with-the-app-secret-hash-issues-usable-tokens|_denial#12|AssertionError|raise AssertionError(f"a denial was required but the call was accepted: {answer!r}")
Issue-Key: cognito-idp.op-contract-update-provisioned-limit|_error#8|AssertionError|raise AssertionError(
Issue-Key: cognito-idp.provisioned-limit-large-numeric-value-precision-preserved|_error#9|AssertionError|raise AssertionError(f"expected UpdateProvisionedLimit to be rejected, but it succeeded: {result!r}")
Issue-Key: cognito-idp.provisioned-limit-numeric-value-precision-round-trip|run#7|AssertionError|raise AssertionError(
Issue-Key: cognito-idp.provisioned-limit-update-concurrent-modification-conflict|run#12|AssertionError|raise AssertionError(
Issue-Key: cognito-idp.provisioned-limit-update-reflected|run#10|AssertionError|raise AssertionError(
Issue-Key: cognito-idp.update-provisioned-limit-concurrent-writes-conflict|run#17|AssertionError|raise AssertionError(f"re-requesting the provisioned {free_value} was accepted, not refused as unchanged")
Issue-Key: cognito-idp.update-provisioned-limit-exceeds-quota-rejected|_error#15|AssertionError|raise AssertionError(f"expected UpdateProvisionedLimit to be rejected, but it succeeded: {result!r}")
Issue-Key: cognito-idp.update-provisioned-limit-excessive-value-rejected|_error#13|AssertionError|raise AssertionError(
Issue-Key: cognito-idp.identity-provider-defaults-survive-update-deleted-rejects-access|run#8|AssertionError|assert "IdpIdentifiers" not in created, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp: 8 net-new causes — CORS preflight unrouted, browser-origin requests refused 403 AccessDenied, hosted-UI domain unserved, and UpdateProvisionedLimit accepts everything

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InitiateAuth

## Description
Eight distinct root causes on the cognito-idp endpoint/browser surface, the hosted-UI domain, and the provisioned-limit plane. Each cause below must be closed before submitting.

Cause 1 of 8: the user-pool API endpoint does not answer a CORS preflight.
AWS: an OPTIONS request to https://cognito-idp.<region>.amazonaws.com/ carrying Origin and Access-Control-Request-Method is answered HTTP 200 with an empty body, Access-Control-Allow-Origin: *, Access-Control-Allow-Methods echoing the requested method, Access-Control-Allow-Headers echoing Access-Control-Request-Headers verbatim (and absent when none was requested), Access-Control-Expose-Headers: x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date and Access-Control-Max-Age: 172800. (Recorded behavior of the public user-pool endpoint; the operations involved are anonymous — Smithy marks SignUp and InitiateAuth optionalAuth with no auth schemes.)
Emulator: the preflight is answered HTTP 404 with body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"} — the router cannot place an OPTIONS request that carries no X-Amz-Target.
Why: no browser-hosted client (Amplify, the JS SDK) can call the pool at all: the preflight fails before any operation is attempted.
Fix: the request router / CORS layer in front of the cognito-idp service (preflight handling that does not depend on X-Amz-Target or a SigV4 scope).
Traces: cognito-idp.user-pool-api-cors-preflight-echoes-requested-method-and-headers, cognito-idp.user-pool-api-options-without-origin-rejected, cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin.

Cause 2 of 8: an unsigned POST carrying a browser Origin is blanket-refused 403 AccessDenied.
AWS: the user-pool API serves unsigned requests for its anonymous operations from any origin — Smithy: SignUp and InitiateAuth carry optionalAuth with an empty auth list. A browser SignUp missing a schema-required attribute is answered HTTP 400 (InvalidParameterException) with Access-Control-Allow-Origin: * and Access-Control-Expose-Headers: x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date; an unsigned AdminCreateUser (an operation that does require SigV4) is answered HTTP 400 with x-amzn-ErrorType: MissingAuthenticationTokenException: and body {"__type":"MissingAuthenticationTokenException","message":"Missing Authentication Token"} and creates nothing.
Emulator: every such request is answered HTTP 403 with body {"__type":"AccessDenied","message":"Requests a browser sends from another site are refused"} — a refusal class AWS does not have, applied to anonymous operations too.
Why: browser sign-up/sign-in is impossible, and the error a page does receive names a non-existent condition instead of the operation's own validation or auth failure.
Fix: the same endpoint-level origin/auth gate in front of the cognito-idp service: anonymous operations must be served unsigned, and an unsigned signed-only operation must get MissingAuthenticationTokenException.
Traces: cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected, cognito-idp.browser-origin-sign-up-response-carries-cors-headers, cognito-idp.browser-origin-unsigned-admin-create-user-rejected.

Cause 3 of 8: the pool's hosted-UI/OAuth2 domain is announced on a real-AWS host the emulator does not serve.
AWS: a pool domain created by CreateUserPoolDomain is reachable, and its OAuth2 endpoints (/oauth2/token, /oauth2/userInfo, /oauth2/revoke, /login) answer on the domain the service reports. In an emulator the announced host must be an address the emulator itself serves.
Emulator: the domain is announced as <domain>.auth.<region>.amazoncognito.com, so every call to the hosted-UI/OAuth2 endpoints leaves the endpoint under test and is refused at egress: 'CapabilityNotOffered: the target serves no address for <domain>.auth.us-west-2.amazoncognito.com ... the emulator announces an address other than the endpoint it is reached on'.
Why: the whole OAuth2 token/userinfo/revoke surface is unreachable — refresh-token grants, userinfo claims and token revocation cannot be exercised at all.
Fix: hosted-UI domain URL minting in the CreateUserPoolDomain/DescribeUserPoolDomain handlers and the OAuth2 endpoint routing in the cognito-idp crate (announce and serve the endpoint under test).
Traces: cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token, cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens, cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint, cognito-idp.set-user-mfa-preference-forbidden-without-authorization.

Cause 4 of 8: a user-pool domain never reaches Status ACTIVE.
AWS: Smithy: DescribeUserPoolDomain -> DomainDescription (DomainDescriptionType). After the domain is created (here by a CloudFormation stack that reports CREATE_COMPLETE) DescribeUserPoolDomain reports Status 'ACTIVE', and the OIDC discovery document is then served.
Emulator: DomainDescription.Status is not ACTIVE after the stack completes, so the check fails on `domain_desc.get("Status") == "ACTIVE"` and discovery is never reached.
Why: clients that wait for ACTIVE before using the domain (the documented readiness signal) wait forever.
Fix: domain lifecycle/status in the CreateUserPoolDomain/DescribeUserPoolDomain handlers in the cognito-idp crate.
Traces: cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery.

Cause 5 of 8: InitiateAuth's missing-parameter message is re-authored.
AWS: InitiateAuth with AuthFlow=USER_PASSWORD_AUTH and no PASSWORD in AuthParameters is refused InvalidParameterException (modeled, 400) with the message 'Missing required parameter PASSWORD' — fixed text whose only variable part is the parameter name.
Emulator: answers 'Missing required parameter PASSWORD in AuthParameters for AuthFlow USER_PASSWORD_AUTH'.
Why: clients matching AWS's wording do not recognise the refusal.
Fix: parameter validation wording in the InitiateAuth handler in the cognito-idp crate.
Traces: cognito-idp.initiate-auth-user-password-auth-missing-password-param-rejected.

Cause 6 of 8: USER_PASSWORD_AUTH is accepted without SECRET_HASH on a client that has a secret.
AWS: for an app client created with GenerateSecret=true, InitiateAuth without a SECRET_HASH auth parameter is refused NotAuthorizedException (modeled, 403) with the message 'Client <clientId> is configured with secret but SECRET_HASH was not received' — fixed text plus the request's ClientId; with the recomputed SECRET_HASH the identical call signs the user in.
Emulator: the hashless InitiateAuth succeeds and returns an AuthenticationResult with access/refresh tokens.
Why: the client secret stops being a credential — any party knowing only the client id can authenticate users of a confidential client.
Fix: SECRET_HASH verification in the InitiateAuth (and RespondToAuthChallenge) handlers in the cognito-idp crate. Related open issue 20261029T182116Z-cognito-idp-mfa-challenge-state-machine-mfa-setup.md covers other secret-hash checks but not this one.
Traces: cognito-idp.user-password-auth-with-the-app-secret-hash-issues-usable-tokens.

Cause 7 of 8: UpdateProvisionedLimit accepts every request — no same-value, floor or ceiling refusal, and the write is not applied.
AWS: Smithy: UpdateProvisionedLimit {LimitDefinition, RequestedLimitValue} -> Limit (required); errors InvalidParameterException (400) and ServiceQuotaExceededException (400). Re-requesting the value already provisioned is refused InvalidParameterException with the fixed text 'Requested value is the same as the current provisioned limit'; a value below the category's FreeLimitValue is refused InvalidParameterException; a value beyond the account's Service Quotas ceiling (e.g. 2,000,000,000) is refused ServiceQuotaExceededException; none of the refusals moves the value, and an accepted request echoes the requested value in Limit.ProvisionedLimitValue and GetProvisionedLimit reads it back.
Emulator: every UpdateProvisionedLimit answers HTTP 200 — the unchanged value, a below-free value and an absurd 2,000,000,000 request all succeed — and the response echoes the category's current/free value rather than the requested one (e.g. a request of 2,000,000,000 returned ProvisionedLimitValue 50 with FreeLimitValue 50).
Why: a client cannot distinguish an applied capacity change from a rejected one; the no-op guard and the quota ceiling AWS enforces are both absent, and the echoed value misreports what was provisioned.
Fix: the UpdateProvisionedLimit handler and the account-level provisioned-limit state in the cognito-idp crate. Related open issue 20261026T235252Z-cognito-idp-five-causes-declared-custom-attribute-constraints.md carries one same-value check; the ones below are not covered by it.
Traces: cognito-idp.op-contract-update-provisioned-limit, cognito-idp.provisioned-limit-large-numeric-value-precision-preserved, cognito-idp.provisioned-limit-numeric-value-precision-round-trip, cognito-idp.provisioned-limit-update-concurrent-modification-conflict, cognito-idp.provisioned-limit-update-reflected, cognito-idp.update-provisioned-limit-concurrent-writes-conflict, cognito-idp.update-provisioned-limit-exceeds-quota-rejected, cognito-idp.update-provisioned-limit-excessive-value-rejected.

Cause 8 of 8: CreateIdentityProvider emits IdpIdentifiers as an empty list when the request supplied none.
AWS: Smithy: CreateIdentityProvider.IdpIdentifiers (IdpIdentifiersListType, @length 0..50, optional) and the IdentityProviderType in the response. A create that supplies no IdpIdentifiers answers an IdentityProvider with no IdpIdentifiers member at all; the key is absent, not [].
Emulator: the response carries IdpIdentifiers: [].
Why: a client distinguishing 'never set' from 'set to empty' (e.g. before an update that adds identifiers) reads the wrong state.
Fix: output serialization of the CreateIdentityProvider/DescribeIdentityProvider handlers in the cognito-idp crate (omit absent optional members).
Traces: cognito-idp.identity-provider-defaults-survive-update-deleted-rejects-access.

## Evidence
Cause 1: OPTIONS / with Origin + Access-Control-Request-Method answered 404 b'{"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}'; expected 200 with Allow-Origin '*', Allow-Methods echoing POST/PUT, Allow-Headers echoing the requested list, Expose-Headers 'x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date', Max-Age '172800' and an empty body.
Cause 2: unsigned SignUp answered 403 '{"__type":"AccessDenied","message":"Requests a browser sends from another site are refused"}' (expected 400 InvalidParameterException with CORS headers); unsigned AdminCreateUser answered the same 403 (expected 400, x-amzn-ErrorType 'MissingAuthenticationTokenException:', body {'__type': 'MissingAuthenticationTokenException', 'message': 'Missing Authentication Token'}).
Cause 3: simparity.egress.EgressRefused: egress refused: 'sim2real-behav-userinfo-18c034c950b7.auth.us-west-2.amazoncognito.com' is not the endpoint under test (127.0.0.1) — same refusal for the token, revoke and MFA-scope domains.
Cause 4: AssertionError at `assert domain_desc.get("Status") == "ACTIVE"` after the stack reached CREATE_COMPLETE.
Cause 5: expected message 'Missing required parameter PASSWORD', got 'Missing required parameter PASSWORD in AuthParameters for AuthFlow USER_PASSWORD_AUTH'.
Cause 6: 'a denial was required but the call was accepted' — InitiateAuth(USER_PASSWORD_AUTH, no SECRET_HASH) returned AuthenticationResult with AccessToken/RefreshToken; expected ('NotAuthorizedException', 'Client <clientId> is configured with secret but SECRET_HASH was not received').
Cause 7: 'expected UpdateProvisionedLimit to be rejected, but it succeeded: {Limit: {LimitDefinition: {LimitClass: API_CATEGORY, Attributes: {Category: UserCreation}}, ProvisionedLimitValue: 50, FreeLimitValue: 50}}' for RequestedLimitValue=2,000,000,000; and 'a same-value UpdateProvisionedLimit request ... unexpectedly succeeded; AWS rejects it with InvalidParameterException (same as current)'.
Cause 8: 'expected no IdpIdentifiers key on a create that supplied none, got []'.

## Steps to Reproduce
Re-replay the checks in trace_ids. In outline: (1) send OPTIONS / to the cognito-idp endpoint with Origin and Access-Control-Request-Method: POST (and a variant with Access-Control-Request-Headers); (2) POST an unsigned SignUp and an unsigned AdminCreateUser with an Origin header and X-Amz-Target; (3) CreateUserPoolDomain then call /oauth2/token, /oauth2/userInfo and /oauth2/revoke on the host the service reports; (4) deploy a pool domain by CloudFormation and poll DescribeUserPoolDomain for Status; (5) InitiateAuth USER_PASSWORD_AUTH omitting PASSWORD; (6) InitiateAuth USER_PASSWORD_AUTH on a GenerateSecret=true client without SECRET_HASH; (7) GetProvisionedLimit then UpdateProvisionedLimit with the same value, with FreeLimitValue-1 and with 2000000000; (8) CreateIdentityProvider without IdpIdentifiers and inspect the response keys.
