Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key
Issue-Key: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key|run#24|PublishedUrlRejected|url = f"{ctx.stage_url(api_endpoint, '$default')}/put"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__kinesis: the Kinesis-PutRecord AWS_PROXY subtype cannot be driven — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so no request reaches the Kinesis-PutRecord subtype route.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API: a POST on a Kinesis-PutRecord subtype route lands a record on the stream carrying the mapped Data and PartitionKey, while a subtype created with its required parameters (StreamName, Data, PartitionKey) unmapped is refused at CreateIntegration with 400 BadRequestException naming the missing parameters in the subtype's declared order.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is sent:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: the half of the scenario that proves the integration actually lands the mapped Data and PartitionKey on the stream can never run, because the API has no reachable address.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route that host into the AWS_PROXY subtype dispatcher. Context: fixed issues 20261109T154749Z-apigatewayv2-kinesis-createintegration-accepts-a-kinesis-putrecord-aws.md and 20261118T133240Z-apigatewayv2-kinesis-apigatewayv2-the-kinesis-putrecord-subtype-refusal-lists-the.md covered the CreateIntegration refusal; the data-plane half stays unobservable while the invoke address is undialable.

Traces: apigatewayv2__kinesis.aws-proxy-subtype-kinesis-putrecord-required-parameters-refused-else-lands-mapped-data-and-key.

## Evidence
Expected: CreateIntegration with required parameters unmapped is refused 400 BadRequestException; with them mapped, POST '<ApiEndpoint>/<route path>' lands a record on the stream whose Data and PartitionKey are the mapped values.

Actual: no request is delivered. The check fails with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:34661 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:35637/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values are per-run.)

## Steps to Reproduce
1. CreateStream; CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='AWS_PROXY', IntegrationSubtype='Kinesis-PutRecord') with StreamName/Data/PartitionKey mapped; CreateRoute + CreateStage.
2. Read ApiEndpoint and POST the body to '<ApiEndpoint>/<route path>'.
3. GetRecords on the stream.
4. AWS: the record is present with the mapped Data/PartitionKey. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so nothing is delivered.
