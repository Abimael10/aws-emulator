Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudcontrol__cloudformation.cfn-apigatewayv2-identity-readonly-ignored-writeonly-body-hidden-from-cloudcontrol
Issue-Key: apigatewayv2__cloudcontrol__cloudformation.cfn-apigatewayv2-identity-readonly-ignored-writeonly-body-hidden-from-cloudcontrol|run#22|AssertionError|assert labels == (api_id, "execute-api", ctx.region), (
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# apigatewayv2 publishes GetApi/Fn::GetAtt ApiEndpoint as a path-style 127.0.0.1 URL, so the endpoint's host does not carry {api-id}.execute-api.{region} as its leading labels

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetApi

## Description
Cause 1 of 1: the ApiEndpoint apigatewayv2 computes for an HTTP API (returned by GetApi/CreateApi and surfaced through CloudFormation's read-only Fn::GetAtt ApiEndpoint) is a path-style URL on the emulator's own loopback listener, with the api id carried in the PATH instead of in the host. Its host therefore has no {api-id}, no `execute-api` label and no region label at all.

AWS: Smithy apigatewayv2 Api.ApiEndpoint is documented as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." So the published URI's hostname is a DNS name whose three leading labels are, in order, the API's own id (the same value Ref/ApiId returns), the literal label `execute-api`, and the API's region; the stage/route path is appended after the host. The DNS suffix after those three labels, and the port, are environment values and are not part of the requirement — the three leading host labels are. The same value is what CloudFormation returns for the read-only Fn::GetAtt ApiEndpoint on AWS::ApiGatewayV2::Api (and what Cloud Control reports as the readable ApiEndpoint property), so all three views must agree on that form.

Emulator: GetApi returns ApiEndpoint as `https://<emulator-host>:<port>/_aws/execute-api/<api-id>` (one run: 'https://127.0.0.1:37491/_aws/execute-api/mp67t4dbd4'; the host, port and api id are per-run values — the URL FORM is the defect). Splitting that URL's hostname on '.' yields the leading labels ('127','0','0') instead of (<api-id>, 'execute-api', <region>): the api id has been moved into a `/_aws/execute-api/<api-id>` path prefix and the `execute-api` and region labels are gone. Fn::GetAtt ApiEndpoint carries the same path-style value.

This is the mirror image of the earlier refusals filed against the pure-AWS form (open 20261114T171242Z-apigateway-apigatewayv2-reopens-an-http-api-s-published-apiendpoint-is.md, and fixed 20261109T152222Z-apigateway-apigatewayv2-an-http-api-s-apiendpoint-is.md / 20261110T024230Z-apigatewayv2-lambda-apigatewayv2-createapi-getapi-publish-apiendpoint-as-https-api.md, which demanded a reachable host): the endpoint is now reachable but has lost the documented host form. Those issues do not cover this cause and it is not a refusal of the operation — GetApi answered 200. Both requirements have to hold at once: the published host must be served by the emulator AND its leading labels must be <api-id>.execute-api.<region> (e.g. a locally-resolvable suffix under which the emulator listens), with the stage/route path appended after it.

Why: a client that follows AWS's documented contract parses ApiEndpoint to recover the API's identity and region (SigV4 signing of execute-api requests, per-API host routing, logging/metric dimensions, and any code that builds `<ApiEndpoint>/<stage>/<route>`) reads garbage — it extracts '127' as the api id and has no region. CDK/CFN consumers that hand the Fn::GetAtt value to such a client inherit the same breakage, and host-based virtual routing of the execute-api data plane cannot be exercised at all because every API shares one hostname.

Fix: the apigatewayv2 ApiEndpoint derivation used by CreateApi/GetApi/GetApis (and reused by the CloudFormation AWS::ApiGatewayV2::Api read-only attribute and the Cloud Control resource model) must build the endpoint as a host whose leading labels are the api id, `execute-api` and the region, on a suffix/port the emulator's execute-api data plane actually serves, rather than embedding the api id in a `/_aws/execute-api/<id>` path on the service listener.

Traces: apigatewayv2__cloudcontrol__cloudformation.cfn-apigatewayv2-identity-readonly-ignored-writeonly-body-hidden-from-cloudcontrol. The check stops at this assertion, so the rest of that scenario (Fn::GetAtt ExecuteApiArn form `arn:aws:execute-api:<region>:<account>:<api-id>`, the write-only Body creating the route `GET /probe`, Cloud Control's model exposing ApiId/Name/ApiEndpoint while hiding Body, and the post-delete NotFoundException message "Invalid API identifier specified <account>:<api-id>") is unverified rather than passing.

## Evidence
Replay (apigatewayv2__cloudcontrol__cloudformation.cfn-apigatewayv2-identity-readonly-ignored-writeonly-body-hidden-from-cloudcontrol):
```
File "check_apigatewayv2__cloudcontrol__cloudformation_cfn_apigatewayv2_identity_readonly_ignored_writeonly_body_hidden_from_cloudcontrol.py", line 121, in run
  assert labels == (api_id, "execute-api", ctx.region), (
AssertionError: GetApi ApiEndpoint 'https://127.0.0.1:37491/_aws/execute-api/mp67t4dbd4' does not carry the api id, execute-api and the region as its leading host labels: ('127', '0', '0')
```
Expected: urlsplit(GetApi(ApiId=<id>)["ApiEndpoint"]).hostname.split('.')[:3] == (<id>, 'execute-api', <region>) — the documented {api-id}.execute-api.{region}... host form, on a suffix the emulator serves; Fn::GetAtt ApiEndpoint equals that same value and is never the template's override.
Actual: ApiEndpoint is a path-style URL on the service listener, '<scheme>://<emulator-host>:<port>/_aws/execute-api/<api-id>', whose leading host labels are the octets of the loopback address. The api id 'mp67t4dbd4', host 127.0.0.1 and port 37491 are per-run values; the defect is the URL form.

## Steps to Reproduce
1. CloudFormation CreateStack with a single AWS::ApiGatewayV2::Api whose Properties are an inline OpenAPI `Body` (one GET /probe MOCK integration) plus a bogus read-only `ApiEndpoint` override, and Outputs {Ref: Api}, {Fn::GetAtt: [Api, ApiEndpoint]}, {Fn::GetAtt: [Api, ExecuteApiArn]}.
2. Poll DescribeStacks until CREATE_COMPLETE and read the outputs; api_id = the Ref output.
3. Call apigatewayv2 GetApi(ApiId=api_id) and inspect ApiEndpoint.
Expected: urlsplit(ApiEndpoint).hostname's first three labels are (api_id, 'execute-api', <region>), and the Fn::GetAtt ApiEndpoint output equals GetApi's value.
Actual: ApiEndpoint is 'https://127.0.0.1:<port>/_aws/execute-api/<api_id>' — the host is the loopback address, so the leading labels are ('127','0','0') and neither 'execute-api' nor the region appears in the host.
