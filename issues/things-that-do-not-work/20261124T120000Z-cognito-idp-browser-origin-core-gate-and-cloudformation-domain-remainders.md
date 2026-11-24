Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected cognito-idp.browser-origin-sign-up-response-carries-cors-headers cognito-idp.browser-origin-unsigned-admin-create-user-rejected cognito-idp.user-pool-api-options-without-origin-rejected cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery
Issue-Key: cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected|run#7|AssertionError|assert r.status_code == 400, f"SignUp without name answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.browser-origin-sign-up-response-carries-cors-headers|run#9|AssertionError|assert r.status_code == 200, f"unsigned SignUp answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.browser-origin-unsigned-admin-create-user-rejected|run#7|AssertionError|assert r.status_code == 400, f"unsigned AdminCreateUser answered {r.status_code}: {r.text[:300]!r}"
Issue-Key: cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin|run#15|AssertionError|assert status == 200, f"the unsigned InitiateAuth answered {status}, not 200: {body[:300]!r}"
Issue-Key: cognito-idp.user-pool-api-options-without-origin-rejected|run#7|AssertionError|assert body == {
Issue-Key: cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery|run#8|WaiterError|cfn.get_waiter("stack_create_complete").wait(

# cognito-idp: what remains of the browser-origin and hosted-UI surface — an unsigned browser-origin call is refused by the CORE's cross-site gate, and one CloudFormation-declared domain is refused by the owning service

> _Carved out of `20261117T112621Z-cognito-idp-8-net-new-causes-cors-preflight.md`, whose other causes now work. That report's causes 1, 5, 6, 7 and 8 are closed (the CORS preflight is answered, the missing-parameter wording is AWS's, SECRET_HASH is verified, `UpdateProvisionedLimit` refuses what AWS refuses, and an unset `IdpIdentifiers` is absent from a create's answer), and the OAuth2 token / user-info / revocation endpoints of a pool's domain are now served and announced on the endpoint under test. These three remainders are what that change could not close._

## Operation
SignUp

## Description

Cause 1 of 3: an unsigned POST carrying a browser `Origin` is refused by the EMULATOR CORE before any service sees it.
AWS: the user-pool API serves unsigned requests for its anonymous operations from any origin — Smithy marks `SignUp` and `InitiateAuth` `optionalAuth` with an empty auth list. A browser `SignUp` missing a schema-required attribute is answered HTTP 400 `InvalidParameterException` with `Access-Control-Allow-Origin: *` and `Access-Control-Expose-Headers: x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date`; an unsigned `AdminCreateUser` (an operation that does require SigV4) is answered HTTP 400 with `x-amzn-ErrorType: MissingAuthenticationTokenException:` and body `{"__type":"MissingAuthenticationTokenException","message":"Missing Authentication Token"}`, and creates nothing.
Emulator: every request that names a modelled operation and carries an `Origin` is answered HTTP 403 `{"__type":"AccessDenied","message":"Requests a browser sends from another site are refused"}`.
Why this is not a service-crate fix: the refusal is the core's. `vendor/smithy-server-runtime/src/multi_service_router.rs:258` calls `refuse_cross_site_browsers(&parts)` for every request that names a modelled operation, before authentication, with `RemoteOrigins::SameOriginOnly` and `allow_remote = false` hard-coded; `vendor/server/src/listener_guard.rs:87` forbids any request carrying an `Origin` under that posture, and the operator's own `--remote-origin` opt-in is not read there. No seam of a generated service runs after it: `Service::override_operation` runs before, `Service::override_routing_error` only for a request that names NO operation, and `Service::override_refusal` can reword a `Refusal::Unauthenticated` but can neither serve the request nor change its 403 (`vendor/smithy-server-runtime/src/pre_dispatch.rs`, the `json` arm pins 403 for that variant).
Required core change: `refuse_cross_site_browsers` must not refuse a request to an operation the model serves unsigned (its `auth_schemes()` is empty), and the core must let the claiming service state the refusal — status, error class and message — for an unsigned request to a signed-only operation, so a service can answer AWS's own 400 `MissingAuthenticationTokenException` there.
Traces: cognito-idp.browser-origin-sign-up-missing-required-attribute-rejected, cognito-idp.browser-origin-sign-up-response-carries-cors-headers, cognito-idp.browser-origin-unsigned-admin-create-user-rejected, cognito-idp.cors-unsigned-initiate-auth-and-global-sign-out-carry-allow-origin.

Cause 2 of 3: the document an `OPTIONS` request that states no `Origin` is refused with is not the recorded one.
AWS: such a request asks for a CORS permission with no origin for it to be about; the endpoint answers HTTP 400 with a JSON document the check compares whole (`assert body == {...}`).
Emulator: the status is now 400 and the body is this endpoint's own unauthenticated refusal, `{"__type":"MissingAuthenticationTokenException","message":"Missing Authentication Token"}` — which the check does not accept, so the recorded document differs in a way the replay does not disclose (it reports only what the emulator answered).
Fix: `services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/service/cors_preflight.rs`, the `Asked::OriginMissing` arm. What is needed is the recorded body for that one step; the status (400) and the absence of every `Access-Control-*` header on it already match.
Traces: cognito-idp.user-pool-api-options-without-origin-rejected.

Cause 3 of 3: the user-pool domain one recorded CloudFormation template declares is refused by the owning service.
AWS: the stack reaches `CREATE_COMPLETE`, `DescribeUserPoolDomain` then reports `Status: ACTIVE`, and the OIDC discovery document is served.
Emulator: `AWS::Cognito::UserPoolDomain` is now provisioned in the owning service (`services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioning/cognito_user_pool.rs`, `Kind::Domain`) instead of being recorded as created while nothing created it — and for that template the owning service REFUSES `CreateUserPoolDomain`, so the stack rolls back and the `Status` assertion is never reached. The refusal now lands on the resource's own `CREATE_FAILED` event, naming the call, the domain and the owning service's code and message, so a deployer can read which parameter was refused; the replay reports only the waiter's `ROLLBACK_COMPLETE`, so the declaration shape that provokes it is not visible from it. Every declaration form constructed by hand deploys and reaches `ACTIVE` (a prefix domain with and without a branding version, a long prefix, a name CloudFormation computed, a fully-qualified custom domain with a certificate, and a pool+client+domain stack), which is what `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/cognito_user_pool_domain_provisioning.rs` and `.../service-com-amazonaws-cognitoidentityprovider-.../tests/all/user_pool_domain_name_form.rs` pin.
Fix: re-run that check's template against the domain create and read the `CREATE_FAILED` event reason it now carries, then serve the declaration it states.
Traces: cognito-idp.deploy-user-pool-domain-cloudformation-serves-oidc-discovery.

## Evidence
Cause 1: unsigned `SignUp` with `Origin: https://app.example.com` answered 403 `{"__type":"AccessDenied","message":"Requests a browser sends from another site are refused"}` (expected 400 `InvalidParameterException` with the CORS headers); unsigned `AdminCreateUser` answered the same 403 (expected 400 `MissingAuthenticationTokenException`). The refusal is made at `vendor/smithy-server-runtime/src/multi_service_router.rs:258`, before the operation runs and after the service's only seams.
Cause 2: `OPTIONS /` with `Access-Control-Request-Method: POST` and no `Origin` answered 400 `{"__type":"MissingAuthenticationTokenException","message":"Missing Authentication Token"}`; the check's `assert body == {...}` rejects it.
Cause 3: `WaiterError: Waiter StackCreateComplete failed ... matched expected path: "ROLLBACK_COMPLETE"`, where the stack previously reached `CREATE_COMPLETE` with no domain created in the owning service at all.

## Steps to Reproduce
1. Cause 1: POST an unsigned `SignUp` (and an unsigned `AdminCreateUser`) to `/` with `X-Amz-Target` and an `Origin` header.
2. Cause 2: send `OPTIONS /` with `Access-Control-Request-Method: POST` and no `Origin`, and compare the whole JSON body.
3. Cause 3: deploy that check's CloudFormation template and read the `Subject`/domain resource's `CREATE_FAILED` event reason.
