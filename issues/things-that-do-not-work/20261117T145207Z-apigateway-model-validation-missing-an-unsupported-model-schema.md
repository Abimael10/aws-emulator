Issue-Source: trace-sweep-divergence
Issue-Trace: apigw-model-definitions-ref-and-template-generation
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigw-model-definitions-ref-and-template-generation
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|GetModelTemplate|4xx->2xx|$.message
Issue-Key: apigateway|GetModelTemplate|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: apigateway|GetModelTemplate|4xx->2xx|status
Issue-Key: apigateway|CreateModel|4xx->2xx|$.message
Issue-Key: apigateway|CreateModel|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: apigateway|CreateModel|4xx->2xx|status
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# apigateway model validation missing: an unsupported model schema is accepted by CreateModel and GetModelTemplate

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetModelTemplate

## Description
2 net-new root causes in the apigateway control plane's MODEL validation: a model schema AWS refuses is accepted, both when a mapping template is generated from it and when it is created. Neither cluster carries a stub_path or an owned_by.

Cause 1 of 2: GetModelTemplate answers 200 with a generated template for a model whose top-level schema type cannot be mapped, where AWS refuses the read with 400 BadRequestException.
AWS: Smithy services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json models GetModelTemplate as GET /restapis/{restApiId}/models/{modelName}/default_template, success 200 (output Template{value}), with modeled errors BadRequestException (400), NotFoundException (404), TooManyRequestsException (429), UnauthorizedException (401). The recorded refusal is HTTP 400 with message `Unsupported data type for mapping template: string`. Message anatomy: the fixed text is `Unsupported data type for mapping template: ` and the trailing token is a value taken from the request's model — the JSON Schema `type` of the model being templated (here `string`); it is not a literal part of the requirement.
Emulator: the same GET is answered 200 and a template body is returned; no message and no x-amzn-errortype header are emitted.
Why: a client that generates a request/response mapping template from a model relies on this refusal to learn the model is not templatable. Against the emulator it receives a template AWS would never have produced, and the error-handling path that AWS exercises is dead code.
Fix: GetModelTemplate's validation, ahead of the template generator in the operation's handler — refuse a non-templatable top-level schema type with 400 BadRequestException and that message, instead of generating a template.
Traces: apigw-model-definitions-ref-and-template-generation#task#4 (cluster also covers #task#6).

Cause 2 of 2: CreateModel answers 201 and creates a model whose schema AWS refuses with 400 BadRequestException.
AWS: Smithy models CreateModel as POST /restapis/{restApiId}/models, success 201 (output Model{id,name,description,schema,contentType}), required input members name and contentType, with modeled errors BadRequestException (400), ConflictException (409), LimitExceededException (429), NotFoundException (404), TooManyRequestsException (429), UnauthorizedException (401). The recorded refusal is HTTP 400 with message `Invalid model specified: Validation Result: warnings : [], errors : [Invalid model specified: Weight]`. Message anatomy: the fixed text is `Invalid model specified: Validation Result: warnings : [], errors : [Invalid model specified: ` + `]`, and the token inside the errors list is a value from the request — the name of the model whose schema failed validation (here `Weight`); the empty `warnings` list and the errors-list framing are fixed.
Emulator: the same POST is answered 201, the model is created, and no message or x-amzn-errortype header is emitted.
Why: an invalid model enters the API's model collection and can then be attached to a method or a request validator, so a client's schema mistake surfaces much later (or never) instead of at create time, and the recorded refusal's wording — which clients and tests match on — is never produced.
Fix: CreateModel's schema validation in the operation's handler, before the model is written to state.
Traces: apigw-model-definitions-ref-and-template-generation#task#10.

Note on values: the rest api id and the model name in these recordings are per-run values; `Weight` and `string` above are the request-derived variable parts of the two messages, not literals to hard-code.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 2023 steps, 1938 passed, 85 mismatched. Both clusters report verdict=mismatch, stub_path="", owned_by=[].

Cause 1 — GetModelTemplate, fixture apigw-model-definitions-ref-and-template-generation, step task#4 (cluster also covers task#6):
  expected_status 400 / actual_status 200
  detail: status: expected 400, got 200; $.message: missing (expected 'Unsupported data type for mapping template: string', got None); $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  Expected (recorded AWS): 400 BadRequestException, message `Unsupported data type for mapping template: string`, x-amzn-errortype header present.
  Actual (emulator): 200 with a generated template body, no message, no error-type header.

Cause 2 — CreateModel, same fixture, step task#10:
  expected_status 400 / actual_status 201
  detail: status: expected 400, got 201; $.message: missing (expected 'Invalid model specified: Validation Result: warnings : [], errors : [Invalid model specified: Weight]', got None); $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  Expected (recorded AWS): 400 BadRequestException with that message. Actual (emulator): 201 with the created Model body.

Model evidence: smithy_query query=operation (GetModelTemplate, CreateModel) on services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json — GetModelTemplate: GET /restapis/{restApiId}/models/{modelName}/default_template, code 200, errors include BadRequestException httpError 400; CreateModel: POST /restapis/{restApiId}/models, code 201, errors include BadRequestException httpError 400.

## Steps to Reproduce
1. Point a boto3 `apigateway` client at the emulator and `create_rest_api(name=..., endpointConfiguration={'types':['REGIONAL']})`.
2. Cause 1: create a model whose top-level JSON Schema `type` is a scalar (e.g. `{"$schema":"http://json-schema.org/draft-04/schema#","title":"X","type":"string"}`), then call `get_model_template(restApiId=api_id, modelName='X')`. Observe HTTP 200 with a `value` template; AWS refuses with 400 BadRequestException and message `Unsupported data type for mapping template: string`.
3. Cause 2: call `create_model(restApiId=api_id, name='Weight', contentType='application/json', schema=<a schema AWS's model validator rejects, e.g. one whose $ref cannot be resolved within the API's models>)`. Observe HTTP 201 and the model present in `get_models`; AWS refuses with 400 BadRequestException and message `Invalid model specified: Validation Result: warnings : [], errors : [Invalid model specified: Weight]`.
4. Full scenarios: replay fixture apigw-model-definitions-ref-and-template-generation steps task#4 (and task#6) and task#10.
