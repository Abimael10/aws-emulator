Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__dynamodb__lambda.e2e-serverless-visitor-ip-capture-lifecycle
Issue-Key: apigatewayv2__dynamodb__lambda.e2e-serverless-visitor-ip-capture-lifecycle|phase#6|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__dynamodb__lambda: the serverless visitor-IP-capture lifecycle cannot proceed — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so the multi-phase scenario cannot drive any traffic through the API and aborts in its solve phase.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API: the scenario GETs '<ApiEndpoint>/<route path>', the AWS_PROXY Lambda records the caller's source IP into DynamoDB, and the subsequent phases read the item back.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is sent, and the harness aborts the phase:
  AssertionError: [phase 2/4 solve] endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: a client that follows ApiEndpoint cannot reach the API, so the whole capture lifecycle — invoke, Lambda write, DynamoDB read-back — is unobservable.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route requests addressed to that host into the route/integration pipeline. Context: open issue 20261029T065950Z-apigatewayv2-dynamodb-lambda-the-deployed-execute-api-data.md describes the WebSocket/$default side of this service pair and does not carry this check's key.

Traces: apigatewayv2__dynamodb__lambda.e2e-serverless-visitor-ip-capture-lifecycle.

## Evidence
Expected: GET '<ApiEndpoint>/<route path>' is served 200 by the AWS_PROXY Lambda, which writes the caller's source IP to the DynamoDB table; later phases read that item back.

Actual: phase 2/4 aborts before any request is sent:
  AssertionError: [phase 2/4 solve] endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:42065 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:42321/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values are per-run.)

## Steps to Reproduce
1. Create the DynamoDB table and the Lambda; CreateApi(ProtocolType='HTTP') with an AWS_PROXY integration, route and stage.
2. Read ApiEndpoint and GET '<ApiEndpoint>/<route path>'.
3. AWS: 200 and an item in the table. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so the phase aborts at the dial.
