Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-scope-based-method-access-positive
Issue-Key: apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-scope-based-method-access-positive|_http_request#8|EgressRefused|with urllib.request.urlopen(req, timeout=timeout) as resp:
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# apigateway__cognito-identity__cognito-idp: the pool's hosted-UI domain is announced on <domain>.auth.<region>.amazoncognito.com, so the scope-based API Gateway authorizer flow is unreachable

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateUserPoolDomain

## Description
Cause 1 of 1: the user-pool hosted-UI/OAuth2 domain is published on a real-AWS host the emulator does not serve.
AWS: a domain created by CreateUserPoolDomain is reachable and its OAuth2 endpoints (/oauth2/token for the client_credentials grant used to mint scoped access tokens, /oauth2/authorize, /login) answer on the host the service reports for that domain. An emulator must announce a host it itself serves, so a client that follows the announced domain reaches the endpoint under test.
Emulator: the domain is announced as <domain>.auth.us-west-2.amazoncognito.com. The check's request to that host leaves the endpoint under test and is refused at egress — 'CapabilityNotOffered: the target serves no address for <domain>.auth.us-west-2.amazoncognito.com ... an emulator run may reach only the emulator ... the emulator announces an address other than the endpoint it is reached on'.
Why: the scope-based authorizer scenario (mint a scoped token at the pool's token endpoint, then call an API Gateway method whose Cognito authorizer enforces the scope) cannot be run at all: the token endpoint is unreachable, so the positive leg never produces a token and the negative leg never produces a 403.
Fix: hosted-UI domain URL minting in the CreateUserPoolDomain/DescribeUserPoolDomain handlers and the OAuth2 endpoint routing of the cognito-idp crate — announce and serve the endpoint under test. The same mechanism is reported for cognito-idp's own OAuth2 checks in the companion cognito-idp issue filed in this sweep.
Traces: apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-scope-based-method-access-positive.

## Evidence
Error: CapabilityNotOffered: the target serves no address for sim2real-dom-78a5963885e8.auth.us-west-2.amazoncognito.com — simparity.egress.EgressRefused: egress refused: 'sim2real-dom-78a5963885e8.auth.us-west-2.amazoncognito.com' is not the endpoint under test (127.0.0.1). Expected: the announced domain host is served by the emulator and POST /oauth2/token answers a scoped access token.

## Steps to Reproduce
Replay apigateway__cognito-identity__cognito-idp.apigw-cognito-authorizer-scope-based-method-access-positive: create a user pool with a resource server and scopes, CreateUserPoolDomain, create a client_credentials app client, then POST to the announced domain's /oauth2/token to mint a scoped token and call the REST API method guarded by the Cognito authorizer.
