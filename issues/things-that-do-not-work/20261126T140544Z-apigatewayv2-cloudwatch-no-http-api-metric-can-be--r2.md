Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudwatch.http-api-count-and-4xx-metrics-carry-the-apiid-stage-dimensions apigatewayv2__cloudwatch.http-api-missing-invoke-permission-rejected-as-5xx-metric apigatewayv2__cloudwatch.http-api-route-throttle-zero-rejected-429-counted-in-4xx apigatewayv2__cloudwatch.obs-apigatewayv2-2-http-api-detailed-metrics-opt-in-adds-method-resource-series apigatewayv2__cloudwatch.obs-apigatewayv2-3-http-api-5xx-alarm-actions-disabled-notification-blocked apigatewayv2__cloudwatch.obs-apigatewayv2-3-http-api-5xx-alarm-notifies-sqs-via-sns
Issue-Key: apigatewayv2__cloudwatch.http-api-count-and-4xx-metrics-carry-the-apiid-stage-dimensions|_v2_invoke_url#14|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cloudwatch.http-api-missing-invoke-permission-rejected-as-5xx-metric|_v2_invoke_url#11|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cloudwatch.http-api-route-throttle-zero-rejected-429-counted-in-4xx|_v2_invoke_url#14|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cloudwatch.obs-apigatewayv2-2-http-api-detailed-metrics-opt-in-adds-method-resource-series|_v2_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cloudwatch.obs-apigatewayv2-3-http-api-5xx-alarm-actions-disabled-notification-blocked|_v2_invoke_url#23|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Key: apigatewayv2__cloudwatch.obs-apigatewayv2-3-http-api-5xx-alarm-notifies-sqs-via-sns|_v2_invoke_url#22|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{path.lstrip('/')}"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__cloudwatch: no HTTP API metric can be produced — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host, so no traffic reaches the stage (6 checks)

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so no request can be driven through the stage and no AWS/ApiGateway datapoint can be emitted.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API; the recordings dial it, the stage answers, and CloudWatch then reports AWS/ApiGateway Count/4xx/5xx (and, with DetailedMetricsEnabled, the per-route Method/Resource series) under the {ApiId, Stage} dimensions, with alarms firing from those datapoints.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is sent:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: every observability behaviour that depends on real API traffic is unobservable — the {ApiId, Stage} Count/4xx/5xx series, the 5xx produced by a missing lambda:InvokeFunction grant, the 429 produced by a zero-rate route throttle being counted as 4xx, the opt-in detailed per-method/resource series, and the 5xx alarm's notification path (including the ActionsEnabled=false suppression).

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route requests addressed to that host so the metric emitter on the stage's request path is reached. Context: fixed issues 20261109T154749Z-apigatewayv2-cloudwatch-no-http-api-metric-can-be.md and 20261110T162302Z-apigatewayv2-cloudwatch-apigatewayv2-http-apis-publish-no-aws-apigateway-metrics.md covered the AWS-host form and the missing metrics; the host is now '.localhost' and still unserved.

Traces: the 6 failing checks in trace_ids, all failing at the first dial of the published invoke address.

## Evidence
Expected: a burst of GETs on '<ApiEndpoint>[/<stage>]/<route path>' is served, and GetMetricData/GetMetricStatistics then report AWS/ApiGateway Count/4xx/5xx under {ApiId=<api id>, Stage=<stage>} (plus Method/Resource dimensions when DetailedMetricsEnabled=true), with the 5xx alarm transitioning to ALARM and notifying its action unless ActionsEnabled=false.

Actual (all 6 checks under '### apigatewayv2__cloudwatch'): no request is delivered, so no datapoint exists. Each fails with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:43359 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:42263/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values differ per check and per run.)

## Steps to Reproduce
1. CreateApi(ProtocolType='HTTP') with an AWS_PROXY Lambda integration, a route and a stage (optionally DetailedMetricsEnabled=true, or a route throttle of 0, or no lambda:InvokeFunction grant).
2. Read ApiEndpoint and drive GETs at '<ApiEndpoint>[/<stage>]/<route path>'.
3. Poll CloudWatch for AWS/ApiGateway Count/4xx/5xx under {ApiId, Stage}.
4. AWS: the stage answers and the datapoints appear. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so step 2 delivers nothing and step 3 has no data.
