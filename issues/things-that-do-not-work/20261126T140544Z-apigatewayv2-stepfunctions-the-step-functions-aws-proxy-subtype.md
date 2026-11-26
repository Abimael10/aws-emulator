Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__stepfunctions.aws-proxy-subtype-stepfunctions-stopexecution-required-executionarn-refused-else-aborts-the-run apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startexecution-subtype-execution-input-is-the-posted-body apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startexecution-without-states-permission-denied-starts-nothing apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startsyncexecution-subtype-returns-the-state-machine-output
Issue-Key: apigatewayv2__stepfunctions.aws-proxy-subtype-stepfunctions-stopexecution-required-executionarn-refused-else-aborts-the-run|run#24|PublishedUrlRejected|url = (f"{ctx.stage_url(api['ApiEndpoint'], '$default')}"
Issue-Key: apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startexecution-subtype-execution-input-is-the-posted-body|_v2_invoke_url#6|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startexecution-without-states-permission-denied-starts-nothing|_v2_invoke_url#9|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Key: apigatewayv2__stepfunctions.xsvc-apigatewayv2-1-startsyncexecution-subtype-returns-the-state-machine-output|_v2_invoke_url#5|PublishedUrlRejected|return f"{ctx.stage_url(api_endpoint, stage or '$default')}/{p}"
Issue-Commit: ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a

# apigatewayv2__stepfunctions: the Step Functions AWS_PROXY subtype data plane cannot be reached — the published ApiEndpoint is an unserved '<api-id>.execute-api.<region>.localhost:<port>' host (4 checks)

> _Note: this issue was identified against the code base at commit `ec22e48eafa5e4290d9bdfafda3d7e5b1d68564a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApi

## Description
Cause 1 of 1: the invoke address published for the HTTP API is not an address the emulator answers on, so no request reaches a StepFunctions AWS_PROXY subtype route.

AWS: Smithy (apigatewayv2) documents Api$ApiEndpoint as "The URI of the API, of the form {api-id}.execute-api.{region}.amazonaws.com. The stage name is typically appended to this URI to form a complete path to a deployed API stage." On AWS that host serves the API: a POST on a StepFunctions-StartExecution subtype route starts an execution whose Input is the posted body; a StartSyncExecution subtype route returns the state machine's output; a StopExecution subtype route aborts the named run; a route whose credentials carry no states:StartExecution grant starts nothing and the gateway answers 5xx; and a subtype created with a required parameter (StateMachineArn / ExecutionArn) unmapped is refused 400 BadRequestException at CreateIntegration.

Emulator: it publishes 'https://<api-id>.execute-api.<region>.localhost:<ephemeral-port>' while listening only on 127.0.0.1:<core-port>. The dial is refused before any request is sent:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://<api-id>.execute-api.<region>.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<core-port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
The api-id, region label and ports are per-run values; the requirement is that the published address is one this emulator answers on.

Why: the half of each scenario that proves the integration actually drives Step Functions (an execution started with the posted body, a sync output returned, a run aborted, nothing started when the grant is absent) can never run, because the API has no reachable address.

Fix: the ApiEndpoint publication in the apigatewayv2 Api response serializers must announce an address the emulator serves, and the execute-api front door must route that host into the AWS_PROXY subtype dispatcher. Context: fixed issue 20261109T154749Z-apigatewayv2-stepfunctions-2-net-new-causes-createintegration-accepts.md is the same family; the published host is now '.localhost' and still unserved.

Traces: the 4 failing checks in trace_ids.

## Evidence
Expected: POST '<ApiEndpoint>/<route path>' on each StepFunctions AWS_PROXY subtype route drives Step Functions — StartExecution starts a run whose Input equals the posted body, StartSyncExecution returns the state machine's output, StopExecution aborts the named execution, and a route without a states:StartExecution grant starts no execution.

Actual (all 4 checks under '### apigatewayv2__stepfunctions'): no request is delivered. Each fails with
  simparity.published.PublishedUrlRejected: endpoint_url was given https://mp67t4dbd4.execute-api.us-west-2.localhost:44437 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:38031/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
(host/port values differ per check and per run.)

## Steps to Reproduce
1. CreateStateMachine; CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='AWS_PROXY', IntegrationSubtype='StepFunctions-StartExecution' | 'StepFunctions-StartSyncExecution' | 'StepFunctions-StopExecution') with required RequestParameters mapped; CreateRoute + CreateStage.
2. Read ApiEndpoint and POST the scenario's body to '<ApiEndpoint>/<route path>'.
3. AWS: the execution is started/returned/aborted. Emulator: the published host '<api-id>.execute-api.<region>.localhost:<port>' is not served, so nothing is delivered.
