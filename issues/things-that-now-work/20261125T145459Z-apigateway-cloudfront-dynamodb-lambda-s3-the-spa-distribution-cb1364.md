Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-api-without-lambda-permission-rejected apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb
Issue-Key: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-api-without-lambda-permission-rejected|run#32|PublishedUrlRejected|edge = ctx.endpoint_url(dist["DomainName"])
Issue-Key: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb|run#38|PublishedUrlRejected|edge = ctx.endpoint_url(dist["DomainName"])
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# apigateway__cloudfront__dynamodb__lambda__s3: the SPA distribution's published DomainName is not an emulator-served address, so neither the shell nor the API-permission check reaches the edge

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution created over the S3 shell origin and the API Gateway origin publishes a <id>.cloudfront.net DomainName the emulator does not serve, so the single-page-app scenario can never issue a viewer request.

AWS: Smithy (cloudfront 2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net"; once GetDistribution reports Deployed, a viewer GET https://<DomainName>/ returns the S3 origin's index.html (after the viewer-request function rewrites extensionless paths to /index.html), and POST https://<DomainName>/aws-blocks/api reaches the API Gateway origin, which either returns the Lambda's JSON body or — with no lambda:InvokeFunction permission for apigateway.amazonaws.com — the 500 the integration produces. Both outcomes are observable only at that address.

Emulator: the published DomainName (e.g. dvbb9jkpe6kwe0.cloudfront.net) is not one of the emulator's own hosts and no per-distribution listener is announced, so the check's first viewer call is refused before it is sent: "simparity.published.PublishedUrlRejected: endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". Both checks in this service group stop there.

Why: a client following AWS's documented flow (create distribution, wait for Deployed, fetch the published DomainName) cannot reach its own site or API through CloudFront at all, even though the control plane reports a Deployed distribution — the announced address is not the address the emulator is reached on.

Fix: the CloudFront distribution's published DomainName and its viewer listener (distribution create/read serializer plus the edge data plane that relays to the S3 and API Gateway origins).

Traces: the 2 failing checks below. The open issue 20261125T052443Z-apigateway-cloudfront-dynamodb-lambda-s3-the-spa-distribution.md describes the same scenario as an edge address that refuses connections, but does not carry these checks' replay keys.

## Evidence
Expected (AWS): GET https://<DomainName>/ → 200 with the S3 shell body; POST https://<DomainName>/aws-blocks/api → the API Gateway origin's response (200 from the Lambda, or the integration's 500 when the lambda:InvokeFunction permission is absent).

Actual (emulator): no viewer request is issued. "simparity.published.PublishedUrlRejected: endpoint_url was given https://dvbb9jkpe6kwe0.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:39013/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The distribution id/host label and loopback port vary per run; the defect is that the announced DomainName is not an address the emulator serves.

## Steps to Reproduce
1. Create an S3 bucket with index.html, a REST API stage over a Lambda integration, a CloudFront function for the SPA rewrite, and CreateDistribution with both the S3 origin and the API Gateway origin.
2. Wait until GetDistribution reports Status=Deployed.
3. GET Distribution.DomainName and POST <DomainName>/aws-blocks/api.
4. Observe the published host is not served by the emulator, so neither viewer call can be made.
