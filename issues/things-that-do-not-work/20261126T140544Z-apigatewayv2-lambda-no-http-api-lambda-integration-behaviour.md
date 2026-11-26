Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__lambda.http-api-cors-signed-iam-get-granted-unsigned-get-refused apigatewayv2__lambda.http-api-lambda-authorizer-raises-unauthorized-message-rejected-401 apigatewayv2__lambda.http-api-node-esm-handler-shapes-statuscode-headers-body-and-base64-response apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for apigatewayv2__lambda.lambda-proxy-status-keyed-response-mapping-rewrites-500-to-403 apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-integration-timeout-50ms-rejected-503 apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-lambda-handler-raises-rejected-500 apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-lambda-without-invoke-permission-rejected-500 apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-malformed-proxy-response-rejected-500-not-502
Issue-Key: apigatewayv2__lambda.http-api-cors-signed-iam-get-granted-unsigned-get-refused|run#10|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/items"
Issue-Key: apigatewayv2__lambda.http-api-lambda-authorizer-raises-unauthorized-message-rejected-401|_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__lambda.http-api-node-esm-handler-shapes-statuscode-headers-body-and-base64-response|run#33|PublishedUrlRejected|base_url = f"{ctx.stage_url(api_endpoint, '$default')}/echo"
Issue-Key: apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for|_invoke_url#12|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2__lambda.lambda-proxy-status-keyed-response-mapping-rewrites-500-to-403|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-integration-timeout-50ms-rejected-503|_invoke_url#14|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{p}"
Issue-Key: apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-lambda-handler-raises-rejected-500|_invoke_url#11|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{p}"
Issue-Key: apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-lambda-without-invoke-permission-rejected-500|_invoke_url#13|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{p}"
Issue-Key: apigatewayv2__lambda.xsvc-apigatewayv2-3-http-api-malformed-proxy-response-rejected-500-not-502|_invoke_url#12|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{p}"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__lambda: no HTTP-API Lambda integration behaviour can be observed — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host (9 checks)

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so no request reaches a route and none of the integration/response behaviours can be exercised.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API, and the gateway's own responses on it are the contract under test: a Lambda handler that raises is answered 500 {"message":"Internal Server Error"}, a route whose function carries no lambda:InvokeFunction grant is answered 500, an integration that exceeds its TimeoutInMillis is answered 503, a malformed proxy response is answered 500, an authorizer that raises Unauthorized is answered 401 {"message":"Unauthorized"}, a handler's statusCode/headers/body/isBase64Encoded shape the served response, status-keyed response parameter mapping rewrites the status, an AWS_IAM route serves a SigV4-signed GET and refuses an unsigned one 403, and an HTTP_PROXY integration overwrites Via and X-Forwarded-For at the origin.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is sent:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: a client that follows ApiEndpoint cannot reach the API at all, so every status/code/body the gateway owes for these nine scenarios is unobservable rather than merely wrong.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route requests addressed to that host and stage prefix into the route/integration pipeline. Context: fixed issues 20261109T154749Z-apigatewayv2-lambda-2-net-new-causes-the-http.md, 20261110T024230Z-apigatewayv2-lambda-apigatewayv2-createapi-getapi-publish-apiendpoint-as-https-api.md and 20261110T170829Z-apigatewayv2-lambda-7-net-new-causes-aws-iam.md are the same family; the host is now '.localhost' and still unserved. Open issue 20261124T063020Z-apigatewayv2-lambda-the-execute-api-front-door-refuses.md concerns the CORS refusal on that front door and carries none of these check keys.

Traces: the 9 failing checks in trace_ids.

## Evidence
Expected, on '<ApiEndpoint>[/<stage>]/<route path>': a raising handler → 500 {"message":"Internal Server Error"}; no lambda:InvokeFunction grant → 500; TimeoutInMillis exceeded → 503; malformed proxy response → 500 (not 502); authorizer raising Unauthorized → 401 {"message":"Unauthorized"}; a Node ESM handler's statusCode/headers/body/isBase64Encoded reflected in the served response; status-keyed response mapping rewriting 500→403; an AWS_IAM route serving a signed GET 200 and refusing an unsigned GET 403; an HTTP_PROXY integration presenting the gateway's own Via and X-Forwarded-For at the origin.

Actual (all 9 checks under '### apigatewayv2__lambda'): no request is delivered. Each fails with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:40377 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:38809/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values differ per check and per run.)

## Steps to Reproduce
1. CreateApi(ProtocolType='HTTP') with the scenario's integration (AWS_PROXY Lambda or HTTP_PROXY), route, authorizer/authorization type, response-parameter mapping and stage.
2. Read ApiEndpoint from the CreateApi/GetApi response.
3. Send the scenario's request to '<ApiEndpoint>[/<stage>]/<route path>'.
4. AWS: the gateway answers with the status/body listed in the evidence. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so the request cannot be delivered.
