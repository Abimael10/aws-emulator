Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint cognito-idp.set-user-mfa-preference-forbidden-without-authorization
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token|_post#15|EgressRefused|with urllib.request.urlopen(req, timeout=20) as resp:
Issue-Key: cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens|request#18|EgressRefused|with self.opener.open(req, timeout=20) as resp:
Issue-Key: cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint|_post_revoke#22|EgressRefused|with urllib.request.urlopen(req, timeout=20) as resp:
Issue-Key: cognito-idp.set-user-mfa-preference-forbidden-without-authorization|_get_client_credentials_token#12|EgressRefused|with urllib.request.urlopen(req, timeout=15, context=tls) as resp:
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# REOPENS: the pool's hosted-UI/OAuth2 domain is still announced on <domain>.auth.<region>.amazoncognito.com, so /oauth2/token, /oauth2/userInfo and /oauth2/revoke are unreachable

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserPoolDomain

## Description
REOPENS 20261117T112621Z-cognito-idp-8-net-new-causes-cors-preflight.md (its Cause 3 of 8): its fix left the call refused.

Cause 1 of 1: the hosted-UI/OAuth2 address the emulator publishes for a user-pool domain is a real-AWS host the emulator does not serve, so the entire OAuth2 endpoint surface of the pool is unreachable.

AWS: Smithy — CreateUserPoolDomain{Domain, UserPoolId, ManagedLoginVersion?, CustomDomainConfig?} -> {CloudFrontDomain, ManagedLoginVersion, Routing}, and DescribeUserPoolDomain -> DomainDescription (DomainDescriptionType). A prefix domain is reachable and serves the pool's OAuth2/hosted-UI endpoints: POST {domain}/oauth2/token (grant_type=refresh_token returns a new access_token and id_token and, without rotation, no refresh_token; grant_type=client_credentials returns an access_token), GET/POST {domain}/oauth2/userInfo (200 with the user's claims for a valid access token, refused for a bad or revoked one), POST {domain}/oauth2/revoke, and {domain}/login. Reference: the Cognito user-pool "Token endpoint", "UserInfo endpoint" and "Revoke endpoint" documentation; the host form and the endpoint bodies are recorded behavior only — they are not modelled members. For an emulator the requirement is reachability: an address the emulator publishes must be an address the emulator is reached on.

Emulator: the domain is announced as `<domain>.auth.<region>.amazoncognito.com`, a production AWS host, so every request a client builds from it leaves the endpoint under test and is refused at egress: `simparity.egress.EgressRefused: egress refused: '<domain>.auth.<region>.amazoncognito.com' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on` (surfaced as `CapabilityNotOffered: the target serves no address for <domain>.auth.<region>.amazoncognito.com`). No HTTP status, error code or body exists, because no request is sent.

Why: the whole OAuth2 plane of a user pool is unusable against the emulator — a refresh-token grant, the userinfo claims and their refusal for bad tokens, token revocation, and any scope-gated call that needs a client-credentials access token from the token endpoint. A client following the documented path (create a domain, call the endpoints on the host the service reports) cannot reach the emulator at all, and the emulator cannot be distinguished from one with no OAuth2 surface. Related OPEN issues 20261124T120000Z-cognito-idp-browser-origin-core-gate-and-cloudformation-domain-remainders.md, 20261122T211230Z-cognito-idp-hosted-ui-read-paths-serve-incomplete.md and 20261123T064929Z-cognito-idp-uicustomization-cssurl-is-rooted-at-the.md concern the CONTENT of domain/branding reads, not the host the domain is published on; 20261117T112621Z-apigateway-cognito-identity-cognito-idp-the-pool-s.md carries the same refusal under a different service heading.

Fix: hosted-UI domain URL minting in the CreateUserPoolDomain / DescribeUserPoolDomain handlers of the cognito-idp crate, plus the OAuth2 endpoint routing (/oauth2/token, /oauth2/userInfo, /oauth2/revoke, /login) — the announced domain must be an address the emulator serves, and those endpoints must answer on it.

Traces (check ids): cognito-idp.oauth2-token-refresh-grant-returns-new-access-token-without-refresh-token, cognito-idp.oauth2-userinfo-returns-claims-and-rejects-bad-tokens, cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint, cognito-idp.set-user-mfa-preference-forbidden-without-authorization.

## Evidence
All four checks fail at the egress hook (egress.py _hook -> refuse), e.g. cognito-idp.deep-cognito-idp-2-token-rotation-revoke-endpoint:
```
CapabilityNotOffered: the target serves no address for sim2real-behav-rot-3f8060b411ed.auth.us-west-2.amazoncognito.com
File "egress.py", line 338, in _hook
  refuse(host, endpoint_host)
File "egress.py", line 291, in refuse
  raise EgressRefused(
simparity.egress.EgressRefused: egress refused: 'sim2real-behav-rot-3f8060b411ed.auth.us-west-2.amazoncognito.com' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on
```
The identical refusal is reported for the other three, differing only in the per-run domain prefix (sim2real-behav-rtgrant-..., sim2real-behav-userinfo-..., sim2real-mfascope-dom-...). The domain prefix and region are per-run values; the published host FORM (<domain>.auth.<region>.amazoncognito.com) is the requirement violated.
Expected: CreateUserPoolDomain/DescribeUserPoolDomain report a domain the emulator serves, and POST {domain}/oauth2/token with grant_type=refresh_token answers 200 {access_token, id_token, token_type:"Bearer", expires_in} and no refresh_token; GET {domain}/oauth2/userInfo with a valid access token answers 200 with the user's claims and is refused for an invalid/revoked token; POST {domain}/oauth2/revoke answers 200 and invalidates the refresh token; a client-credentials grant answers 200 with an access token carrying the requested scopes.
Actual: nothing is sent — the announced host is outside the endpoint under test, so no status, code or body is produced.
This is the refusal 20261117T112621Z-cognito-idp-8-net-new-causes-cors-preflight.md (status: fixed) claimed to fix as its Cause 3; it carries all four of these check keys and they are refused the same way again.

## Steps to Reproduce
1. CreateUserPool + CreateUserPoolClient (with a secret and/or an allowed OAuth flow and scopes); CreateUserPoolDomain(Domain=<prefix>, UserPoolId=<pool>).
2. Read the domain the service reports (CreateUserPoolDomain.CloudFrontDomain / DescribeUserPoolDomain.DomainDescription).
3. POST https://<reported domain>/oauth2/token with grant_type=refresh_token (and, separately, grant_type=client_credentials), GET https://<reported domain>/oauth2/userInfo with a valid access token and with a tampered one, and POST https://<reported domain>/oauth2/revoke with a refresh token.
   Expected: each is served by the emulator on the address it reported. Actual: the reported host is <prefix>.auth.<region>.amazoncognito.com, which is not the endpoint under test, so the call is refused at egress and never made.
