Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.create-stage-and-deployment apigatewayv2.custom-domain-mapping-key-routes-to-stage-routes apigatewayv2.deep-apigatewayv2-2-enforce-aws-iam-route-unsigned-forbidden-signed-served apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore apigatewayv2.deep-apigatewayv2-2-enforce-integration-timeout-503-while-twin-route-serves apigatewayv2.deep-apigatewayv2-2-enforce-payload-10mib-limit-413-not-delivered apigatewayv2.deep-apigatewayv2-3-stagesem-manual-stage-serves-deployment-snapshot apigatewayv2.deep-apigatewayv2-3-stagesem-request-authorizer-cache-expires-after-ttl apigatewayv2.deep-apigatewayv2-3-stagesem-stage-variable-selects-lambda-backend apigatewayv2.deep-apigatewayv2-3-stagesem-stage-variable-target-without-invoke-permission-rejected apigatewayv2.delete-route-stops-delivery-on-autodeploy apigatewayv2.delete-stage-stops-serving-requests apigatewayv2.deploy-apigatewayv2-1-http-api-cors-removed-in-place-then-api-replaced apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-stripped-lambda-permission-invoke-rejected apigatewayv2.e2e-custom-domain-mapping-invoke apigatewayv2.e2e-vpc-link-private-integration-flow apigatewayv2.e2e-vpc-link-private-integration-invoke apigatewayv2.http-api-cors-configuration-overrides-backend-cors-headers apigatewayv2.http-api-cors-preflight-caught-by-any-route-is-authorized apigatewayv2.http-api-cors-preflight-origin-allowlist-and-delete apigatewayv2.http-api-named-stage-rawpath-includes-stage-prefix apigatewayv2.http-api-path-without-stage-prefix-not-delivered apigatewayv2.http-api-quick-create-synthesizes-integration-route-stage apigatewayv2.http-api-route-key-precedence-and-path-parameters apigatewayv2.http-api-stage-requires-deployment-before-serving apigatewayv2.http-api-unmatched-route-key-not-delivered apigatewayv2.http-api-v2-binary-body-base64-encoded-both-ways apigatewayv2.http-proxy-stage-variable-selects-backend-host apigatewayv2.import-api-payload-format-version-extension-honored apigatewayv2.import-api-then-reimport-changes-routing apigatewayv2.invoke-http-api-httpproxy apigatewayv2.invoke-http-api-lambda-proxy apigatewayv2.lambda-authorizer-array-context-value-passes-through-unchanged apigatewayv2.lambda-authorizer-context-propagates-to-integration-event apigatewayv2.lambda-authorizer-payload-format-1-0-iam-policy-response apigatewayv2.lambda-authorizer-simple-response-explicit-deny-forbidden apigatewayv2.lambda-payload-format-1-0-and-2-0-envelopes-differ apigatewayv2.lambda-payload-format-2-0-response-inference-and-cookies apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response apigatewayv2.quick-create-lambda-target-invoked-directly-delivers-v2-event apigatewayv2.request-authorizer-cache-is-keyed-per-identity-source-value apigatewayv2.request-authorizer-missing-one-of-two-identity-sources-unauthorized apigatewayv2.reset-authorizers-cache-affects-invoke apigatewayv2.reset-authorizers-cache-server-side apigatewayv2.route-throttling-returns-429-while-twin-route-serves apigatewayv2.simple-response-authorizer-under-payload-format-1-0-fails-500 apigatewayv2.update-route-target-switch-takes-effect-on-autodeploy
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.create-stage-and-deployment|_invoke_url#15|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{p}"
Issue-Key: apigatewayv2.custom-domain-mapping-key-routes-to-stage-routes|run#15|PublishedUrlRejected|invoke_url = ctx.stage_url(api_endpoint, "prod")
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-aws-iam-route-unsigned-forbidden-signed-served|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore|_invoke_url#7|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-integration-timeout-503-while-twin-route-serves|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-payload-10mib-limit-413-not-delivered|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-3-stagesem-manual-stage-serves-deployment-snapshot|_invoke_url#23|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-3-stagesem-request-authorizer-cache-expires-after-ttl|_invoke_url#24|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-3-stagesem-stage-variable-selects-lambda-backend|_invoke_url#28|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.deep-apigatewayv2-3-stagesem-stage-variable-target-without-invoke-permission-rejected|_invoke_url#24|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.delete-route-stops-delivery-on-autodeploy|run#10|PublishedUrlRejected|base = ctx.stage_url(api["ApiEndpoint"], "$default")
Issue-Key: apigatewayv2.delete-stage-stops-serving-requests|_invoke_url#9|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.deploy-apigatewayv2-1-http-api-cors-removed-in-place-then-api-replaced|_v2_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, '$default')}/{p}"
Issue-Key: apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys|_http#37|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-stripped-lambda-permission-invoke-rejected|_http#18|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: apigatewayv2.e2e-custom-domain-mapping-invoke|run#7|PublishedUrlRejected|default_url, route = ctx.endpoint_route(f"{api['ApiEndpoint']}/{backend['stage']}/hello")
Issue-Key: apigatewayv2.e2e-vpc-link-private-integration-flow|_fetch_body#17|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: apigatewayv2.e2e-vpc-link-private-integration-invoke|run#19|PublishedUrlRejected|proxy_url, route = ctx.endpoint_route(f"{api_endpoint}/proxy")
Issue-Key: apigatewayv2.http-api-cors-configuration-overrides-backend-cors-headers|_url#9|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2.http-api-cors-preflight-caught-by-any-route-is-authorized|run#10|PublishedUrlRejected|url = f"{ctx.stage_url(api['ApiEndpoint'], '$default')}/orders"
Issue-Key: apigatewayv2.http-api-cors-preflight-origin-allowlist-and-delete|_invoke_url#12|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.http-api-named-stage-rawpath-includes-stage-prefix|_url#8|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2.http-api-path-without-stage-prefix-not-delivered|_url#12|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2.http-api-quick-create-synthesizes-integration-route-stage|run#6|AssertionError|assert not (p := ctx.published_url_mismatch(created.get("ApiEndpoint", ""))), p
Issue-Key: apigatewayv2.http-api-route-key-precedence-and-path-parameters|_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.http-api-stage-requires-deployment-before-serving|_invoke_url#9|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.http-api-unmatched-route-key-not-delivered|_invoke_url#11|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.http-api-v2-binary-body-base64-encoded-both-ways|_url#9|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Key: apigatewayv2.http-proxy-stage-variable-selects-backend-host|run#6|PublishedUrlRejected|backend_urls[label] = ctx.stage_url(api_endpoint, "$default")
Issue-Key: apigatewayv2.import-api-payload-format-version-extension-honored|run#14|PublishedUrlRejected|status, body = _poll_http(f"{ctx.stage_url(imported['ApiEndpoint'], '$default')}/echo-v1", 200)
Issue-Key: apigatewayv2.import-api-then-reimport-changes-routing|run#18|PublishedUrlRejected|url = f"{ctx.stage_url(imported['ApiEndpoint'], '$default')}/widgets"
Issue-Key: apigatewayv2.invoke-http-api-httpproxy|_invoke_url#11|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.invoke-http-api-lambda-proxy|_invoke_url#10|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.lambda-authorizer-array-context-value-passes-through-unchanged|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.lambda-authorizer-context-propagates-to-integration-event|_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.lambda-authorizer-payload-format-1-0-iam-policy-response|_invoke_url#1|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.lambda-authorizer-simple-response-explicit-deny-forbidden|_invoke_url#1|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.lambda-payload-format-1-0-and-2-0-envelopes-differ|_invoke_url#14|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.lambda-payload-format-2-0-response-inference-and-cookies|_invoke_url#13|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response|_invoke_url#13|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.quick-create-lambda-target-invoked-directly-delivers-v2-event|_invoke_url#17|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.request-authorizer-cache-is-keyed-per-identity-source-value|_invoke_url#18|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.request-authorizer-missing-one-of-two-identity-sources-unauthorized|_invoke_url#16|PublishedUrlRejected|base = f"{ctx.stage_url(api_endpoint, stg)}/{p}"
Issue-Key: apigatewayv2.reset-authorizers-cache-affects-invoke|_invoke#24|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: apigatewayv2.reset-authorizers-cache-server-side|_invoke#46|PublishedUrlRejected|_url, _route = ctx.endpoint_route(url)
Issue-Key: apigatewayv2.route-throttling-returns-429-while-twin-route-serves|_invoke_url#12|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.simple-response-authorizer-under-payload-format-1-0-fails-500|_invoke_url#16|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2.update-route-target-switch-takes-effect-on-autodeploy|_url#10|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage)}/{path.lstrip('/')}"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2: the published ApiEndpoint names an unserved '<api-id>.execute-api.<region>.localhost:<port>' host, so the whole HTTP-API execute-api data plane is undialable (48 checks)

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address the emulator publishes for an HTTP API is not an address the emulator answers on, so no request ever reaches the execute-api data plane and every behaviour behind the invoke address is ungradeable.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that published host is the SAME host that serves the API's traffic: a client reads ApiEndpoint out of CreateApi / ImportApi / ReimportApi / GetApi / GetApis / UpdateApi (or Fn::GetAtt ApiEndpoint) and dials it directly, appending the stage prefix (for a named stage) and the route path, and gets the stage's response. Recorded behavior: the recordings dial the published address and are served by the API.

Emulator: it publishes an address on a host it does not serve. In this replay it published 'https://<api-id>.execute-api.us-west-2.localhost:<ephemeral-port>' (per-check examples: https://mp67t4dbd4.execute-api.us-west-2.localhost:44413 for create-stage-and-deployment, .../prod/hello for e2e-custom-domain-mapping-invoke) while the emulator itself is reachable only at 127.0.0.1:<core-port>. Dialling it is refused before any request is made:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The same cause surfaces as a plain assertion where the check inspects the published value itself: apigatewayv2.http-api-quick-create-synthesizes-integration-route-stage fails with "the emulator published https://<api-id>.execute-api.<region>.localhost:<port>: its host is not one of the emulator's own hosts". The api-id, the region label, the ephemeral port and the core port are per-run values and are not part of the requirement; the requirement is that the published ApiEndpoint is an address this emulator answers on, and that the execute-api front door routes requests addressed to it.

Why: a client that follows ApiEndpoint — which is the only address AWS gives it — cannot reach any deployed stage or route at all. Nothing downstream of the invoke address can be exercised: route-key precedence and path parameters, stage/deployment semantics, autodeploy, CORS preflight and header overrides, Lambda and REQUEST authorizers and their caches, payload format 1.0/2.0 envelopes and response inference, parameter mapping, per-route throttling, binary bodies, HTTP_PROXY and VPC-link integrations, and custom-domain mappings.

Fix: the ApiEndpoint publication for the Api resource (the CreateApi/ImportApi/ReimportApi/GetApi/GetApis/UpdateApi response serializers, and the CloudFormation Fn::GetAtt ApiEndpoint reader that reuses it) must announce an address the emulator serves, and the execute-api front door must accept and route requests addressed to that host, including the stage prefix. Context: fixed issue 20261118T125928Z-apigatewayv2-cloudcontrol-cloudformation-apigatewayv2-publishes-getapi-fn-getatt-apiendpoint-as-a.md required ApiEndpoint's host to carry '{api-id}.execute-api.{region}' rather than a path-style loopback URL; the current value satisfies that shape but moved the host off every listener the emulator owns, so both requirements have to hold at once. Open issues 20261109T143515Z-apigatewayv2-lambda-logs-apigatewayv2-the-api-endpoint-is-published-as-an.md and 20261121T045409Z-apigatewayv2-iam-lambda-apigatewayv2-http-api-the-invoke-address-published-for.md describe neighbouring symptoms but carry none of the 48 check keys below.

Traces: the 48 failing checks listed in trace_ids, all failing at the first dial of the published invoke address.

## Evidence
Expected: ApiEndpoint is an address the emulator serves, and a GET on '<ApiEndpoint>[/<stage>]/<route path>' is answered by the stage (200 with the integration's body, or the gateway's modelled 4xx/5xx for the scenario under test).

Actual (48 checks under '### apigatewayv2'): the emulator published https://<api-id>.execute-api.us-west-2.localhost:<ephemeral-port> and the dial is refused with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:44413 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:43313/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(the host/port values differ per check and per run; the host label '<api-id>.execute-api.<region>.localhost' and the fact that it is not a host the emulator serves are the invariant).

Variant on the same cause, where the check asserts on the published value directly:
  apigatewayv2.http-api-quick-create-synthesizes-integration-route-stage →
  AssertionError: the emulator published https://mp67t4dbd4.execute-api.us-west-2.localhost:33021: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:33527/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

No HTTP status or error code is produced by the emulator for these checks, because no request is ever delivered to it.

## Steps to Reproduce
1. CreateApi(Name=<name>, ProtocolType='HTTP', Target=<lambda arn> or an explicit Integration+Route+Stage).
2. Read ApiEndpoint from the CreateApi (or GetApi) response.
3. GET '<ApiEndpoint>/<route path>' (or '<ApiEndpoint>/<stage>/<route path>' for a named stage).
4. AWS: the host resolves and the stage answers. Emulator: the published host is '<api-id>.execute-api.<region>.localhost:<port>', which is not a host/port the emulator listens on, so the request cannot be delivered at all.
5. Replay any check in trace_ids to observe the refusal.
