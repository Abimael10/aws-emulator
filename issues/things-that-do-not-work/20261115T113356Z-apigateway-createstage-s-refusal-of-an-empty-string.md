Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.stage-variables-empty-vs-absent-preserved
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.stage-variables-empty-vs-absent-preserved|run#7|AssertionError|assert message == (
Issue-Commit: 38e24911c56fad04803701729ab45a93a1394fb2

# apigateway: CreateStage's refusal of an empty-string stage-variable VALUE carries the stage-variable NAME-constraint sentence instead of AWS's "Invalid stage variable value of key <key>." message

> _Note: this issue was identified against the code base at commit `38e24911c56fad04803701729ab45a93a1394fb2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStage

## Description
Cause 1 of 1: CreateStage correctly refuses a stage variable whose value is the empty string with BadRequestException, but the message it serves is the wrong one for that error class — it is the stage-variable NAME/pattern sentence rather than AWS's value-rejection sentence, and it names no key.

AWS: Smithy (service-com-amazonaws-apigateway-backplanecontrolservice) models CreateStage POST /restapis/{restApiId}/stages -> 201 Stage, with BadRequestException (client, HTTP 400) among its errors; `variables` targets MapOfStringToString and the model states no value constraint, so the refusal text is recorded behavior only. API Gateway forbids an empty-string stage variable value and refuses the call 400 BadRequestException with the message template: `Invalid stage variable value of key <key>.  Please use values with alphanumeric characters and the symbols ' ', -', '.', '_', ':', '/', '?', '&', '=', and ','.` In that template the FIXED text is `Invalid stage variable value of key `, then `.`, two spaces, and `Please use values with alphanumeric characters and the symbols ' ', -', '.', '_', ':', '/', '?', '&', '=', and ','.` (note AWS's own typo `-'` and the double space after the period, both part of the fixed text). The only VARIABLE part is `<key>` — the offending map key taken from the request's `variables` (in the replay that key happened to be `emptyVar`; the key is a request value, not part of the template). The refused call also leaves no partial stage: a following GetStage for that stage name answers 404 NotFoundException with the message `Invalid stage identifier specified`.

Emulator: the call is refused with the right code (BadRequestException) but the message is `Variable names can have alphanumeric and underscore characters, and the values must match [A-Za-z0-9-._~:/?#&=,]+` — the stage-variable NAME-validation sentence. It is a different error class's text: it describes the key-name rule and the regex for values, omits the `Invalid stage variable value of key <key>.` lead entirely, and therefore never names which key was rejected.

Why: a client that parses or asserts on API Gateway's refusal text (CLI/CDK/IaC error surfacing, retry-or-fail branching, tests pinned to AWS wording) sees text AWS never emits, and an operator debugging a stage with several variables cannot tell WHICH variable was rejected because the key is absent from the message. The symptom is also actively misleading: the emulator's wording blames the variable NAME while the request's names were valid and only the value was empty.

Fix: the CreateStage handler's stage-variable validation in the apigateway service — the empty/invalid-VALUE branch must emit the value-rejection message template above (with the offending key interpolated) rather than reusing the name/pattern message; the code (BadRequestException/400) and the no-partial-state behavior are already correct and must not regress.

Traces: apigateway.stage-variables-empty-vs-absent-preserved (the one failing check for this cause).

## Evidence
Check apigateway.stage-variables-empty-vs-absent-preserved, assertion at check_apigateway_stage_variables_empty_vs_absent_preserved.py line 37.

Request: CreateStage(restApiId=<api>, stageName='prod', deploymentId=<deployment>, variables={'emptyVar': ''}) against a REST API with a MOCK GET on root and one deployment.

Expected (AWS): HTTP 400, Error.Code = 'BadRequestException', Error.Message = "Invalid stage variable value of key emptyVar.  Please use values with alphanumeric characters and the symbols ' ', -', '.', '_', ':', '/', '?', '&', '=', and ','." — i.e. the fixed template with the request's map key interpolated.

Actual (replay): Error.Code = 'BadRequestException' (correct), Error.Message = 'Variable names can have alphanumeric and underscore characters, and the values must match [A-Za-z0-9-._~:/?#&=,]+'.

Replay output:
  AssertionError: unexpected message: 'Variable names can have alphanumeric and underscore characters, and the values must match [A-Za-z0-9-._~:/?#&=,]+'

The rest of the check (NotFoundException + 'Invalid stage identifier specified' for the uncreated stage, and the positive control where 'presentVar' round-trips and a never-set key is absent from GetStage.variables) was not reached because the message assertion fails first.

## Steps to Reproduce
1. CreateRestApi(name=<any>); read the root resource id via GetResources.
2. PutMethod(GET, authorizationType='NONE'), PutIntegration(type='MOCK', requestTemplates={'application/json': '{"statusCode": 200}'}), PutMethodResponse(statusCode='200'), PutIntegrationResponse(statusCode='200', selectionPattern='').
3. CreateDeployment(restApiId=<api>) and keep the deployment id.
4. Call CreateStage(restApiId=<api>, stageName='prod', deploymentId=<deployment>, variables={'emptyVar': ''}).
5. Observe the error: code is BadRequestException (correct) but the message is the stage-variable NAME/pattern sentence. Expected the value-rejection template "Invalid stage variable value of key emptyVar.  Please use values with alphanumeric characters and the symbols ' ', -', '.', '_', ':', '/', '?', '&', '=', and ','." with the offending key from the request.
6. (Already correct, must not regress) GetStage(restApiId=<api>, stageName='prod') -> 404 NotFoundException 'Invalid stage identifier specified'; then CreateStage with variables={'presentVar':'hello'} succeeds and GetStage shows presentVar='hello' with no entry for a never-set key.
