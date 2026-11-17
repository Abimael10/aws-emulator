Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.create-deployment-no-routes-rejected apigatewayv2.create-authorizer-ttl-out-of-range-rejected apigatewayv2.export-api-invalid-output-type-rejected
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.create-deployment-no-routes-rejected|_error#4|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: apigatewayv2.create-authorizer-ttl-out-of-range-rejected|run#11|AssertionError|assert message == expected_message, (
Issue-Key: apigatewayv2.export-api-invalid-output-type-rejected|run#8|AssertionError|assert message == (
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# apigatewayv2: 3 net-new refusal-contract causes — CreateDeployment never refuses a route-less HTTP API, and the CreateAuthorizer TTL and ExportApi OutputType refusals carry emulator-authored messages

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeployment

## Description
Three net-new root causes in the API Gateway V2 control plane, all in the refusal contract of a single operation each. None is covered by an open issue (the nearest, 20261102T095936Z-apigatewayv2-2-net-new-refusal-contract-divergences-createdomainname.md, is about CreateDomainName/CreateApi; 20261115T131901Z-apigateway-exportapi-document-and-invalid-specification-refusal-diverge-from.md is the REST `apigateway` GetExport/ExportApi surface, a different service).

Cause 1 of 3: CreateDeployment on an HTTP API that has no routes is accepted instead of refused, and mints a deployment.
AWS: CreateDeployment freezes an API's routes for a stage to serve, so an API with no valid route has nothing to deploy and the call is refused. Smithy: CreateDeployment declares BadRequestException (HTTP 400) and the operation's success code is 201. Recorded behavior: HTTP 400, Error.Code `BadRequestException`, Error.Message `Unable to deploy API because no valid routes exist in this API` — the whole sentence is fixed text for this error class, with no request- or state-derived parts — and GetDeployments afterwards lists no deployment. Adding one route makes the identical call succeed.
Emulator: the call answers 201 with a minted DeploymentId and DeploymentStatus `DEPLOYED`, so no refusal happens at all and a deployment is persisted for an API that can serve nothing.
Why: a client that relies on the refusal to detect an incompletely built API gets a deployment id and a `DEPLOYED` status instead of a 400, and a stage released to that deployment serves no route while reporting a healthy deployment — the error branch of every deploy script is dead code and the failure surfaces only later at the data plane.
Fix: the CreateDeployment handler's precondition checks — it weighs a WEBSOCKET route with an empty target but has no "no valid routes" gate for any protocol; the gate must be weighed before anything is minted so the refused call leaves the API's deployments and stages unchanged.
Traces: check apigatewayv2.create-deployment-no-routes-rejected. Handler: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/create_deployment.rs lines 212-239 (the only precondition before minting) and 261-282 (the mint).

Cause 2 of 3: CreateAuthorizer's out-of-range AuthorizerResultTtlInSeconds refusal carries a generic Smithy constraint-violation sentence instead of API Gateway V2's message.
AWS: Smithy: CreateAuthorizer$AuthorizerResultTtlInSeconds targets IntegerWithLengthBetween0And3600 with @range min 0 / max 3600; the operation declares BadRequestException (HTTP 400). Recorded behavior: a value above the ceiling is refused HTTP 400, Error.Code `BadRequestException`, Error.Message `Authorizer result TTL outside allowable range. TTL must be between 0 and 3600 seconds.` — the whole sentence is this error class's fixed text; the bounds it names are the member's modeled range (0 and 3600), and the rejected value is NOT echoed. The ceiling value 3600 itself is accepted and round-trips on GetAuthorizer.
Emulator: status and code are right, but the message is the framework's constraint envelope: `1 validation error detected: Value '3601' at 'authorizerResultTtlInSeconds' failed to satisfy constraint: Member must have value less than or equal to 3600`.
Why: a client or test that matches API Gateway V2's documented TTL sentence, or that parses it to report the allowable range, sees an unrecognized message; the generic envelope also leaks the wire member spelling and the submitted value, which AWS does not include for this error.
Fix: the CreateAuthorizer handler's validation of AuthorizerResultTtlInSeconds — the range must be enforced in the operation with the service's own message rather than being left to the generic request-walk constraint reporter.
Traces: check apigatewayv2.create-authorizer-ttl-out-of-range-rejected.

Cause 3 of 3: ExportApi's invalid-OutputType refusal carries an emulator-authored message.
AWS: Smithy: ExportApi$OutputType is a required httpQuery `outputType` (plain __string, so the model lists no enum values — the valid set JSON|YAML is behavior, not a modeled constraint); the operation declares BadRequestException (HTTP 400). Recorded behavior: an OutputType outside JSON|YAML is refused HTTP 400, Error.Code `BadRequestException`, Error.Message `Output type is XML. Output type should be one of following : JSON, YAML.` — the template is fixed text `Output type is <value>. Output type should be one of following : JSON, YAML.`, where `<value>` is the OutputType taken verbatim from the request (unquoted) and the trailing list is fixed, including the space before the colon. The same call with OutputType=JSON succeeds and returns a non-empty document.
Emulator: status and code are right, but the message is `Invalid value for OutputType: 'XML'. Valid values are: JSON, YAML.` — a different sentence, with the request value quoted and the member named in PascalCase.
Why: a client matching AWS's sentence (or a UI surfacing it) sees unrecognized prose for the only refusal class ExportApi raises for a bad output type.
Fix: the ExportApi handler's OutputType validation / its BadRequestException message construction.
Traces: check apigatewayv2.export-api-invalid-output-type-rejected.

## Evidence
Cause 1 — CreateDeployment on a freshly created HTTP API with no routes.
Expected (AWS): HTTP 400, BadRequestException, Message "Unable to deploy API because no valid routes exist in this API"; GetDeployments Items == [].
Actual (emulator): HTTP 201, body {'AutoDeployed': False, 'CreatedDate': <ts>, 'DeploymentId': '<id>', 'DeploymentStatus': 'DEPLOYED'} — the check's _error() helper raised "expected the call to be rejected, but it succeeded".

Cause 2 — CreateAuthorizer(AuthorizerType=REQUEST, AuthorizerResultTtlInSeconds=3601).
Expected (AWS): HTTP 400 BadRequestException, Message "Authorizer result TTL outside allowable range. TTL must be between 0 and 3600 seconds."
Actual (emulator): HTTP 400 BadRequestException, Message "1 validation error detected: Value '3601' at 'authorizerResultTtlInSeconds' failed to satisfy constraint: Member must have value less than or equal to 3600"
(assertion: unexpected message ... at check line 143; the code assertion for BadRequestException passed.)

Cause 3 — ExportApi(Specification=OAS30, OutputType=XML).
Expected (AWS): HTTP 400 BadRequestException, Message "Output type is XML. Output type should be one of following : JSON, YAML."
Actual (emulator): HTTP 400 BadRequestException, Message "Invalid value for OutputType: 'XML'. Valid values are: JSON, YAML."
(assertion: unexpected message ... at check line 50; the code assertion passed and the positive control OutputType=JSON returned a non-empty body.)

## Steps to Reproduce
All three against the emulator's apigatewayv2 endpoint, region/credentials as the harness configures them.

Cause 1: agw.create_api(Name=<unique>, ProtocolType='HTTP') -> ApiId. Call agw.create_deployment(ApiId=ApiId) with no routes created. AWS: ClientError BadRequestException "Unable to deploy API because no valid routes exist in this API", and agw.get_deployments(ApiId=ApiId)['Items'] == []. Emulator: 201 with a DeploymentId and DeploymentStatus DEPLOYED. Positive control: create an HTTP_PROXY integration and a 'GET /probe' route targeting it, then the identical create_deployment must succeed and read back via get_deployment.

Cause 2: create an HTTP API and a REQUEST-authorizer Lambda; call agw.create_authorizer(ApiId=..., Name=<unique>, AuthorizerType='REQUEST', AuthorizerUri=<lambda invoke arn uri>, IdentitySource=['$request.header.Authorization'], AuthorizerPayloadFormatVersion='2.0', AuthorizerResultTtlInSeconds=3601) and read Error.Message. Positive control: the same call with 3600 is accepted and GetAuthorizer reports AuthorizerResultTtlInSeconds == 3600; also confirm the refused call minted no authorizer (GetAuthorizers count unchanged).

Cause 3: create an HTTP API with one HTTP_PROXY integration and a 'GET /probe' route; call agw.export_api(ApiId=..., Specification='OAS30', OutputType='XML') and read Error.Message. Positive control: OutputType='JSON' returns a non-empty body.
