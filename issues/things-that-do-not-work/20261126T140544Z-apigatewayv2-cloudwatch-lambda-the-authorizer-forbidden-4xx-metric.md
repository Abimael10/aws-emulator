Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudwatch__lambda.metrics-elbv2-apigw-http-api-authorizer-forbidden-403-counts-lowercase-4xx-under-apiid
Issue-Key: apigatewayv2__cloudwatch__lambda.metrics-elbv2-apigw-http-api-authorizer-forbidden-403-counts-lowercase-4xx-under-apiid|run#15|PublishedUrlRejected|base = ctx.stage_url(http_api["ApiEndpoint"], "live")
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__cloudwatch__lambda: the authorizer-forbidden 4xx metric cannot be driven — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so the authorizer-forbidden request that should be counted is never delivered.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API: a request a Lambda authorizer denies is answered 403 by the gateway and counted in AWS/ApiGateway Count and 4xx under {ApiId, Stage}.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is made:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: a client cannot reach the API at its only published address, so the 403 the authorizer should produce and its 4xx/Count datapoints under {ApiId, Stage} cannot be observed at all.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route that host so the authorizer and the stage metric emitter are reached. Context: fixed issue 20261110T162302Z-apigatewayv2-cloudwatch-lambda-apigatewayv2-http-api-authorizer-forbidden-traffic-is-never.md covered the missing counting itself; it cannot be re-verified while the invoke address is undialable.

Traces: apigatewayv2__cloudwatch__lambda.metrics-elbv2-apigw-http-api-authorizer-forbidden-403-counts-lowercase-4xx-under-apiid.

## Evidence
Expected: a request the Lambda authorizer denies is answered 403 on '<ApiEndpoint>/<route path>', and AWS/ApiGateway Count and 4xx under {ApiId=<api id>, Stage=<stage>} each report that request.

Actual: no request is delivered. The check fails with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:36625 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34553/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values are per-run.)

## Steps to Reproduce
1. CreateApi(ProtocolType='HTTP') with a REQUEST Lambda authorizer that denies, a route bound to it and a stage.
2. Read ApiEndpoint and GET '<ApiEndpoint>/<route path>' without the identity source the authorizer allows.
3. Poll CloudWatch for AWS/ApiGateway 4xx under {ApiId, Stage}.
4. AWS: 403 and a 4xx datapoint. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so nothing is delivered or counted.
