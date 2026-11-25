Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-api-without-lambda-permission-rejected apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb
Issue-Key: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-api-without-lambda-permission-rejected|_poll_until#34|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudfront__dynamodb__lambda__s3.e2e-spa-distribution-serves-shell-and-api-writes-dynamodb|_poll_until#40|AssertionError|raise AssertionError(
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# apigateway__cloudfront__dynamodb__lambda__s3: the SPA distribution's edge address refuses every connection, so neither the shell nor the API-permission refusal is ever served

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution publishes an edge address with no listener, so the SPA shell at / is never served and the API path's expected refusal is never reached.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." — a bare DNS host name. After the distribution is Deployed, GET https://<DomainName>/ returns the S3 origin's default root object (200 with the shell's bytes), and a request to the API cache behaviour is proxied to the API Gateway origin: it answers the API's own response, and when the stage has no lambda:InvokeFunction resource policy for apigateway.amazonaws.com the integration fails and the viewer receives API Gateway's 5xx relayed by the edge, not a transport error.

Emulator: the published DomainName is "127.0.0.1/_aws/cloudfront/<DistributionId>" and nothing listens on the 127.0.0.1:443 it implies, while the control plane answers on a different authority. Polling for the shell fails with "timed out waiting for the shell at / after <N>s (last: ConnectionError(MaxRetryError('HTTPSConnectionPool(host=\"127.0.0.1\", port=443): Max retries exceeded with url: /_aws/cloudfront/<Id>/ (Caused by NewConnectionError(\"... [Errno 111] Connection refused\"))')))". No request ever reaches a cache behaviour, so neither the positive shell/DynamoDB-write path nor the negative missing-lambda-permission path is exercised.

Why: a client following AWS's contract (read DomainName, address the viewer there) cannot load the application at all, and cannot tell a correctly refused API call from an absent edge — both appear as a refused TCP connect.

Fix: the CloudFront CreateDistribution/UpdateDistribution handler and distribution state must publish a DomainName that is a host authority the emulator serves, and the edge data plane must route each cache behaviour's requests to its configured S3 or API Gateway origin and answer them, relaying the origin's status and body.

Note: open issue 20261118T115347Z-apigateway-cloudfront-dynamodb-lambda-s3-customerrorresponses-are-never.md covers a different edge defect in this group and does not carry these replay keys.

Traces: the 2 failing checks in trace_ids.

## Evidence
Expected (AWS): GET https://<DomainName>/ answers 200 with the SPA shell's bytes; a POST to the API cache behaviour reaches the API Gateway origin and is answered by it (the API's own 5xx when the stage lacks the lambda:InvokeFunction resource policy).

Actual (emulator): "AssertionError: timed out waiting for the shell at / after <N>s (last: ConnectionError(MaxRetryError('HTTPSConnectionPool(host=\"127.0.0.1\", port=443): Max retries exceeded with url: /_aws/cloudfront/<Id>/ (Caused by NewConnectionError(\"HTTPSConnection(host=\"127.0.0.1\", port=443): Failed to establish a new connection: [Errno 111] Connection refused\"))')))". The host, port, distribution id and elapsed seconds are per-run values; the requirement is that the announced DomainName be served.

## Steps to Reproduce
1. Deploy the SPA estate: an S3 origin holding index.html, a REST API stage, a DynamoDB table and a distribution with an S3 default behaviour plus an API cache behaviour.
2. Read Distribution.DomainName from CreateDistribution.
3. GET https://<DomainName>/ and poll for the shell.
4. Observe every attempt is refused at connect ([Errno 111] Connection refused) and the poll times out.
5. Re-replay the check ids in trace_ids to confirm.
