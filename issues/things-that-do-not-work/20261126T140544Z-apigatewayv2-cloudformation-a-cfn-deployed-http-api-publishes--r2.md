Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-disable-execute-api-endpoint-blocks-default-domain apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-inline-body-openapi-proxy-serves-its-routes
Issue-Key: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-disable-execute-api-endpoint-blocks-default-domain|run#12|PublishedUrlRejected|enabled_endpoint = ctx.stage_url(enabled["ApiEndpoint"], "$default")
Issue-Key: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-inline-body-openapi-proxy-serves-its-routes|run#11|PublishedUrlRejected|api_endpoint = ctx.stage_url(got_api["ApiEndpoint"], "$default")
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__cloudformation: a CFN-deployed HTTP API publishes an unserved '<api-id>.execute-api.<region>.localhost:<port>' ApiEndpoint, so neither the inline-OpenAPI routes nor the DisableExecuteApiEndpoint gate can be observed

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the ApiEndpoint a CloudFormation-deployed AWS::ApiGatewayV2::Api publishes (and that Fn::GetAtt ApiEndpoint returns) is not an address the emulator answers on, so the stack's data plane is undialable.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." CloudFormation surfaces exactly that value through Fn::GetAtt <Api>.ApiEndpoint, and on AWS that host serves the API: the recordings dial it and get the inline-OpenAPI proxy routes' responses, and with DisableExecuteApiEndpoint=true the default execute-api domain answers 404 with body {"message":"Not Found"}.

Emulator: the stack's published ApiEndpoint is 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' (observed e.g. https://e77k44w5b0.execute-api.us-west-2.localhost:32807 and https://mp67t4dbd4.execute-api.us-west-2.localhost:37217) while the emulator listens only on 127.0.0.1:<core-port>. The dial is refused before any request is made:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: a CloudFormation consumer takes the invoke URL from Fn::GetAtt ApiEndpoint (usually as a stack Output) and cannot reach the deployed API, so neither the served routes of an inline Body OpenAPI document nor the DisableExecuteApiEndpoint 404 gate can be verified.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers, and the CloudFormation AWS::ApiGatewayV2::Api attribute reader that reuses it, must announce an address the emulator serves; the execute-api front door must route that host (and honour DisableExecuteApiEndpoint on it). Context: fixed issues 20261109T154749Z-apigatewayv2-cloudformation-a-cfn-deployed-http-api-publishes.md and 20261113T163227Z-apigatewayv2-cloudformation-an-http-api-deployed-with-disableexecuteapiendpoint-true-refuses.md covered the AWS-host form and the 403-vs-404 gate; the host is now '.localhost' and still unserved.

Traces: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-disable-execute-api-endpoint-blocks-default-domain, apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-inline-body-openapi-proxy-serves-its-routes.

## Evidence
Expected: after CREATE_COMPLETE, a GET on Fn::GetAtt ApiEndpoint + route path is served by the stack's stage (200 with the OpenAPI-declared proxy route's body), and with DisableExecuteApiEndpoint=true the same default execute-api URL answers 404 {"message":"Not Found"}.

Actual: no request is delivered. Both checks fail with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://e77k44w5b0.execute-api.us-west-2.localhost:32807 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:32979/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:37217 ... needs http(s)://127.0.0.1:46389/<resource path> ...
(host/port values are per-run.)

## Steps to Reproduce
1. CreateStack with an AWS::ApiGatewayV2::Api (ProtocolType HTTP, inline Body OpenAPI document; and in the second scenario DisableExecuteApiEndpoint=true) plus its stage.
2. Wait for CREATE_COMPLETE and read Fn::GetAtt <Api>.ApiEndpoint.
3. GET '<ApiEndpoint>/<route path>'.
4. AWS: 200 from the declared route (or 404 {"message":"Not Found"} when the default endpoint is disabled). Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so the request cannot be delivered.
