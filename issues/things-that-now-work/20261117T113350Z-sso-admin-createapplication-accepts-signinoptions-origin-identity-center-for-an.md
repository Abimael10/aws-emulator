Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sso-admin.create-application-oauth-origin-configuration
Issue-Shape-Id: com.amazonaws.ssoadmin#SWBExternalService
Issue-Key: sso-admin.create-application-oauth-origin-configuration|_refusal#15|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 3c9e1d40e4e7a3b63b75dd42df212f92ec4807bb

# CreateApplication accepts SignInOptions.Origin=IDENTITY_CENTER for an OAuth2.0 (OIDC) application provider instead of refusing it with ValidationException

> _Note: this issue was identified against the code base at commit `3c9e1d40e4e7a3b63b75dd42df212f92ec4807bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApplication

## Description
Cause 1 of 1: sso-admin CreateApplication does not validate PortalOptions.SignInOptions.Origin against the federation protocol of the ApplicationProviderArn, so an OAuth2.0/OIDC customer-managed provider is allowed to be created with the SAML-only IDENTITY_CENTER sign-in origin. Failing check: sso-admin.create-application-oauth-origin-configuration.

AWS: Smithy (services/service-com-amazonaws-ssoadmin-swbexternalservice/smithy-model) models CreateApplication with required input InstanceArn, ApplicationProviderArn, Name, optional PortalOptions, and error ValidationException (client, HTTP 400). SignInOptions.Origin is a required enum member of SignInOrigin with values IDENTITY_CENTER | APPLICATION, and its documentation states that IDENTITY_CENTER means "IAM Identity Center uses SAML identity-provider initiated authentication to sign the customer directly into a SAML-based application", while APPLICATION means Identity Center redirects to the configured ApplicationUrl. For a provider whose FederationProtocol is OAUTH (the customer-managed .../applicationProvider/custom provider that CreateApplication accepts), the IDENTITY_CENTER origin is therefore not a legal configuration, and real AWS refuses the create with HTTP 400 ValidationException and the message "SignInOrigin cannot be IDENTITY_CENTER for an OIDC application" (recorded behavior; the model states the SAML-only semantics of the value but not the message text). In that message the whole string is fixed text — it carries no values from the request or from state.

Emulator: the same CreateApplication call succeeds. With InstanceArn naming the account's Identity Center instance, ApplicationProviderArn an OAUTH /custom provider, Status=DISABLED and PortalOptions={Visibility: DISABLED, SignInOptions: {Origin: IDENTITY_CENTER}} (no ApplicationUrl), the emulator returns HTTP 200 with a populated CreateApplicationResponse (ApplicationArn, InstanceArn, IdentityStoreArn) rather than raising ValidationException. The check's _refusal() helper therefore fails with "expected the call to be refused, but it succeeded: {'ApplicationArn': 'arn:aws:sso::<account>:application/ssoins-<id>/apl-<id>', ...}". The accepted application is also persisted, so it becomes visible to ListApplications, which AWS never shows for a refused create. The positive neighbour in the same check (the same provider and instance with Origin=APPLICATION plus ApplicationUrl, round-tripped through DescribeApplication) passes, so the gap is specific to the cross-field Origin/federation-protocol rule and not to the setup.

Why: a client relying on AWS's behavior gets a silently mis-configured application. Code that probes a configuration by attempting the create and catching ValidationException concludes IDENTITY_CENTER is a valid sign-in origin for an OIDC application, and provisioning templates or IaC that would be rejected (and rolled back) against AWS instead commit an application whose portal sign-in can never work, and which additionally has no ApplicationUrl. Negative-path tests written against the emulator pass and then fail against real AWS, and the account is left holding a resource AWS would never have created.

Fix: belongs in the sso-admin CreateApplication handler's input validation — after resolving ApplicationProviderArn to its application provider, reject PortalOptions.SignInOptions.Origin=IDENTITY_CENTER when that provider's FederationProtocol is OAUTH, returning ValidationException (HTTP 400) with the message above, and do so before any application state is written so nothing is persisted and ListApplications is unchanged. The same cross-field rule should be applied by UpdateApplication's validation if it shares the PortalOptions path.

Traces: sso-admin.create-application-oauth-origin-configuration (mismatch; assertion raised in _refusal() after the create returned 200).

## Evidence
Check: sso-admin.create-application-oauth-origin-configuration (verdict: mismatch).

Request (after a passing positive neighbour with Origin=APPLICATION):
  CreateApplication(
    InstanceArn=<the account's Identity Center instance ARN>,
    ApplicationProviderArn=<an OAUTH, customer-managed provider ARN ending in /custom>,
    Name=<unique name>,
    Status="DISABLED",
    PortalOptions={"Visibility": "DISABLED", "SignInOptions": {"Origin": "IDENTITY_CENTER"}})

Expected (AWS): HTTP 400, Error.Code = "ValidationException", Error.Message = "SignInOrigin cannot be IDENTITY_CENTER for an OIDC application" (entire message is fixed text, no request/state-derived parts). No application created; the refused name never appears in ListApplications.

Actual (emulator): HTTP 200 with a created application, e.g.
  {'ApplicationArn': 'arn:aws:sso::000000000000:application/ssoins-7223b1d1ac5e1f0e/apl-af98b87408ecc7f1',
   'InstanceArn': 'arn:aws:sso:::instance/ssoins-7223b1d1ac5e1f0e',
   'IdentityStoreArn': 'arn:aws:identitystore::000000000000:identitystore/d-9067f3a1c2',
   'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
(the ARNs, account id and instance/identity-store ids above are per-run values, not requirements; the divergence is the 200-with-resource instead of the 400 refusal).

Failure text:
  File "check_sso_admin_create_application_oauth_origin_configuration.py", line 136, in _refusal
    raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
  AssertionError: expected the call to be refused, but it succeeded: {...}

Model citations: Smithy SignInOptions.Origin -> SignInOrigin enum [IDENTITY_CENTER, APPLICATION]; Origin=IDENTITY_CENTER documented as "SAML identity-provider initiated authentication ... into a SAML-based application"; CreateApplication errors include ValidationException (httpError 400).

## Steps to Reproduce
1. Boot the emulator and point an sso-admin client at it.
2. Obtain the account's IAM Identity Center instance: list_instances(), or create_instance() and wait for Status=ACTIVE.
3. Pick an OAuth2.0 customer-managed provider: from list_application_providers(), the entry with FederationProtocol == "OAUTH" whose ApplicationProviderArn ends with "/custom".
4. Positive neighbour (should pass): create_application(InstanceArn=..., ApplicationProviderArn=<that provider>, Name=<name-a>, Status="DISABLED", PortalOptions={"Visibility": "DISABLED", "SignInOptions": {"Origin": "APPLICATION", "ApplicationUrl": "https://app.example.com/login"}}) and confirm describe_application round-trips Status/Visibility/SignInOptions.
5. Now call create_application with the SAME instance and provider, Name=<name-b>, Status="DISABLED", PortalOptions={"Visibility": "DISABLED", "SignInOptions": {"Origin": "IDENTITY_CENTER"}}.
6. Observe the emulator returns HTTP 200 with ApplicationArn/InstanceArn/IdentityStoreArn. Expected: ClientError with Error.Code "ValidationException" and Error.Message "SignInOrigin cannot be IDENTITY_CENTER for an OIDC application".
7. Paginate list_applications(InstanceArn=...) and observe <name-b> is present; on AWS the refused create leaves nothing behind, while <name-a> remains listed.
