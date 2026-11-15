Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for
Issue-Key: apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for|run#29|AssertionError|assert proxied_headers.get("via") == "HTTP/1.1 AmazonAPIGateway", (
Issue-Commit: 65202ee683bcb66d82ebd52a38b6336bbaa34d8b

# apigatewayv2 HTTP_PROXY integration forwards the client's Via and X-Forwarded-For to the origin instead of overwriting them with the gateway's own values

> _Note: this issue was identified against the code base at commit `65202ee683bcb66d82ebd52a38b6336bbaa34d8b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
One net-new root cause in the apigatewayv2 execute-api data plane, grounded in a failing behavioral check of this replay. No open issue describes it (the nearest hop-header findings are for other services, e.g. 20261110T092303Z-apigateway-cloudfront-s3-5-net-new-causes-at.md for the CloudFront edge, and carry none of this check's replay keys).

Cause 1 of 1: the HTTP_PROXY hop does not rewrite the hop-identifying request headers.
AWS: recorded behavior only for the data plane (execute-api has no Smithy model); the configuration is modeled - apigatewayv2 CreateIntegration POST /v2/apis/{ApiId}/integrations with IntegrationType=HTTP_PROXY, IntegrationUri, IntegrationMethod, PayloadFormatVersion. Recorded behavior: when a request crosses an HTTP_PROXY integration, API Gateway builds the upstream request with its OWN hop headers and discards whatever the client claimed - the origin sees Via exactly 'HTTP/1.1 AmazonAPIGateway' (fixed text, no variable part) and X-Forwarded-For replaced by the single peer address the gateway itself observed (one IP address literal taken from the connection, never a chain and never containing the client-supplied chain). The contrast leg is recorded too and PASSES on the emulator: the same client headers sent directly to the AWS_PROXY origin, with no HTTP_PROXY hop in between, arrive unmodified (Via verbatim, X-Forwarded-For = the client's chain with the real peer appended), so the only variable between the two legs is the proxy hop.
Emulator: through the HTTP_PROXY hop the origin sees via='2.0 client-claimed-hop, 1.1 another-claimed-hop' - the client's value forwarded verbatim - where 'HTTP/1.1 AmazonAPIGateway' is required. The check stops at that first assertion, so the X-Forwarded-For requirements on the same hop (the origin must see an X-Forwarded-For, it must NOT contain the client-claimed chain, and it must be a single IP address literal rather than a chain) are part of this same cause and must be fixed with it. Both header values the gateway must emit are fixed-form: the Via token 'HTTP/1.1 AmazonAPIGateway' interpolates nothing, and X-Forwarded-For is exactly one value taken from emulator state - the peer address of the connection the gateway accepted (never a value from the request).
Why: a backend behind an HTTP_PROXY integration identifies its immediate peer from Via and X-Forwarded-For. Because the emulator passes the client's values through, a client can spoof both: an application that trusts X-Forwarded-For for rate limiting, geo/IP allowlisting or audit logging sees attacker-controlled addresses locally and the gateway's own address on AWS, and middleware that detects the proxy hop by Via='HTTP/1.1 AmazonAPIGateway' never fires. The defect inverts a security-relevant guarantee rather than merely omitting a header.
Fix: the request-header construction of the HTTP_PROXY integration in the apigatewayv2 execute-api data plane - the hop that builds the upstream request must SET Via to its own 'HTTP/1.1 AmazonAPIGateway' and X-Forwarded-For to the observed peer address, overwriting any client-supplied value, while the direct AWS_PROXY path keeps forwarding the client's headers as it already does.
Traces: apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for (the whole setup - Lambda origin, AWS_PROXY origin API, HTTP_PROXY front API, both stages - succeeds, the direct-hop assertions pass, and only the proxied-hop header assertions fail).

## Evidence
Check apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for [mismatch]:

  AssertionError: the HTTP_PROXY hop must overwrite Via with its own 'HTTP/1.1 AmazonAPIGateway', discarding the client-claimed '2.0 client-claimed-hop, 1.1 another-claimed-hop'; origin saw via='2.0 client-claimed-hop, 1.1 another-claimed-hop'

Request: GET <front-api $default stage>/front with headers Via: '2.0 client-claimed-hop, 1.1 another-claimed-hop' and X-Forwarded-For: '203.0.113.9, 198.51.100.7'; the front API's only route 'GET /front' targets an HTTP_PROXY integration whose IntegrationUri is the invoke URL of a second (AWS_PROXY, Lambda) API that echoes the headers it received. No parameter mapping is configured on either API, so any header change is the gateway's own unconditional behaviour.

Expected at the origin, through the HTTP_PROXY hop: via == 'HTTP/1.1 AmazonAPIGateway'; x-forwarded-for present, not containing '203.0.113.9, 198.51.100.7', and a single IP address literal.
Actual at the origin: via == '2.0 client-claimed-hop, 1.1 another-claimed-hop' (the client's value), i.e. no rewrite at all; the x-forwarded-for assertions are not reached.

Positive control that PASSES on the emulator (same client headers, same origin, one hop, no HTTP_PROXY): the AWS_PROXY origin sees via == the client's value verbatim and x-forwarded-for == the client's chain with the real peer appended - so the divergence is attributable to the proxy hop only.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an SDK at it (region/account per the environment).
2. Create a Lambda function that returns {'statusCode': 200, 'headers': {'Content-Type': 'application/json'}, 'body': json.dumps({'headers': event.get('headers') or {}})}.
3. Origin API: apigatewayv2 CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='AWS_PROXY', IntegrationUri=<function arn>, IntegrationMethod='POST', PayloadFormatVersion='2.0'); CreateRoute(RouteKey='GET /echo', Target='integrations/<id>'); lambda AddPermission for execute-api; CreateStage('$default', AutoDeploy=True).
4. Front API: CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='HTTP_PROXY', IntegrationUri=<origin invoke url>/echo, IntegrationMethod='GET', PayloadFormatVersion='1.0'); CreateRoute(RouteKey='GET /front', Target='integrations/<id>'); CreateStage('$default', AutoDeploy=True). Configure NO parameter mapping.
5. GET <origin url>/echo with headers Via: '2.0 client-claimed-hop, 1.1 another-claimed-hop' and X-Forwarded-For: '203.0.113.9, 198.51.100.7' -> the echoed headers must show via unchanged and x-forwarded-for starting with '203.0.113.9, 198.51.100.7, ' (this leg already passes).
6. GET <front url>/front with the SAME two headers -> the echoed headers must show via == 'HTTP/1.1 AmazonAPIGateway' and x-forwarded-for a single IP address literal that does not contain '203.0.113.9, 198.51.100.7'. Observed: via is the client's value verbatim.
7. Replay apigatewayv2__lambda.http-proxy-integration-overwrites-via-and-x-forwarded-for to confirm.
