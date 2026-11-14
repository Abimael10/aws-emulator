Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.deploy-api-key-cfn-grants-data-plane-access appsync.deploy-data-source-resolver-cfn-executes-query appsync.deploy-function-cfn-pipeline-resolver-invocable appsync.deploy-resolver-cfn-insufficient-iam-permission-rejected appsync.deploy-graphql-api-cfn-behavioral appsync.deploy-appsync-2-cfn-update-replace-rollback-rejected appsync.deploy-cfn-invalid-schema-stack-rolls-back-rejected appsync.deploy-domain-name-cfn-association-resolves
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.deploy-api-key-cfn-grants-data-plane-access|run#11|AssertionError|assert "errors" not in payload, f"unexpected errors with valid key: {payload}"
Issue-Key: appsync.deploy-data-source-resolver-cfn-executes-query|run#17|AssertionError|assert status == 200 and "errors" not in payload, f"putItem failed: {payload}"
Issue-Key: appsync.deploy-function-cfn-pipeline-resolver-invocable|run#17|AssertionError|assert status == 200 and "errors" not in payload, f"pipeline query failed: {payload}"
Issue-Key: appsync.deploy-resolver-cfn-insufficient-iam-permission-rejected|run#18|AssertionError|assert status == 200 and "errors" not in payload, f"positive control (permitted read) failed: {payload}"
Issue-Key: appsync.deploy-graphql-api-cfn-behavioral|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: appsync.deploy-appsync-2-cfn-update-replace-rollback-rejected|run#26|AssertionError|assert errs and any("version" in (e.get("message") or "") for e in errs), (
Issue-Key: appsync.deploy-cfn-invalid-schema-stack-rolls-back-rejected|run#14|AssertionError|assert err == ("NotFoundException", "API not found."), (
Issue-Key: appsync.deploy-domain-name-cfn-association-resolves|run#10|AssertionError|raise AssertionError(
Issue-Commit: 91290edf36da0a835077a90e85db4b27bf00fabc

# appsync: 4 net-new causes — CloudFormation-deployed VTL resolvers never execute, query validation does not reject undeclared fields, a rolled-back API's id is refused BadRequest instead of NotFound, and CreateDomainName accepts an untrusted certificate

> _Note: this issue was identified against the code base at commit `91290edf36da0a835077a90e85db4b27bf00fabc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetGraphqlApi

## Description
Four distinct root causes found replaying the CloudFormation-deploy behavioral checks of the appsync suite. All four are net-new: none of the failing checks below is marked ALREADY FILED or OWNED in the replay report. Related open issues are named per cause; none of them carries these checks' replay keys.

Cause 1 of 4: the GraphQL data plane refuses to execute a resolver defined by VTL mapping templates.
AWS: Smithy/recorded behavior — AWS::AppSync::Resolver and AWS::AppSync::FunctionConfiguration accept RequestMappingTemplate/ResponseMappingTemplate (no Runtime/Code), and AppSync's GraphQL endpoint executes those VTL templates: a query/mutation on such a field answers HTTP 200 with the resolved value under `data` and NO `errors` member (e.g. `$util.toJson("...")` in the response template is returned as the field value). The appsync Smithy model models Resolver$requestMappingTemplate/responseMappingTemplate as first-class resolver definitions, so a mapping-template resolver is a supported, executable resolver, not a degraded one.
Emulator: every such field answers HTTP 200 with the field null and a GraphQL error whose message is emulator-authored prose: "This field's resolver is defined by mapping templates, which this endpoint does not run; define it with APPSYNC_JS code." (pipeline stages prefix it with the function name, e.g. "AppendProcessed: This field's resolver ..."). AWS never emits that message or that errorType. The polling variant of the same cause (deploy-graphql-api-cfn-behavioral) therefore never sees a non-null value and is cut off at its 60 s budget.
Why: a client that deploys the documented CloudFormation AppSync stack (GraphQLApi + NONE/DynamoDB DataSource + UNIT or PIPELINE Resolver with mapping templates) gets a null field and an error envelope where AWS returns data, so no VTL-based application — the dominant AppSync resolver style — can run, and negative tests that depend on a working positive control (e.g. the IAM-permission check's permitted GetItem) cannot be distinguished from a real denial.
Fix: the GraphQL data-plane resolver executor of the appsync service (the VTL request/response mapping-template evaluation path and its $util helpers), not the control plane — CreateResolver already stores and returns the templates.
Related: open issue 20261114T140817Z-appsync-7-net-new-causes-the-graphql-data.md describes the same refusal for directly created resolvers; these CloudFormation-deployed checks are not in its replay set.
Traces: deploy-api-key-cfn-grants-data-plane-access, deploy-data-source-resolver-cfn-executes-query, deploy-function-cfn-pipeline-resolver-invocable, deploy-resolver-cfn-insufficient-iam-permission-rejected, deploy-graphql-api-cfn-behavioral.

Cause 2 of 4: a query that selects a field the active schema does not declare is executed instead of rejected.
AWS: recorded behavior only — AppSync validates the incoming GraphQL document against the API's active schema before execution. Selecting a field that the active schema's type does not declare is a validation failure: the response carries an `errors` entry naming the offending field (AWS's validation text names the field, e.g. that the field is not defined on the type) and no successful data for it. After a stack update that rolls back the v2 schema, the API serves v1 again, so the v2-only field must be refused.
Emulator: the request is accepted and answered HTTP 200 `{'data': {'echo': {'msg': 'x', 'version': None}}}` — the undeclared field is silently resolved to null with no `errors` member at all.
Why: a client cannot tell a rolled-back/old schema from the new one, and schema-drift bugs that AWS surfaces as a validation error are reported as a successful query with null data; any client asserting on error presence for an unknown field sees success.
Fix: the GraphQL data-plane request pipeline's document-validation stage of the appsync service (validate the parsed query against the API's active schema before resolver dispatch).
Traces: deploy-appsync-2-cfn-update-replace-rollback-rejected.

Cause 3 of 4: an AppSync API identifier that the emulator itself published is refused as malformed instead of being resolved (or reported absent).
AWS: Smithy — GetGraphqlApi (GET /v1/apis/{apiId}) models both BadRequestException (400) and NotFoundException (404). For an API that no longer exists, AWS answers 404 NotFoundException with the message "API not found." (fixed text, no variable part). BadRequestException is for an identifier AWS cannot parse at all. The identifier read back here is the one CloudFormation recorded as the Api resource's PhysicalResourceId after an invalid-schema stack rolled back, so AWS's answer for it is the not-found refusal.
Emulator: GetGraphqlApi answers 400 BadRequestException with the message "Invalid API ID <identifier>" — fixed text "Invalid API ID " plus the identifier taken from the request — for the very value its own CloudFormation resource recorded (an appsync API ARN of the form arn:<partition>:appsync:<region>:<account>:apis/<apiId>). Hypothesis: the emulator records the AppSync API's CFN PhysicalResourceId in ARN form while GetGraphqlApi accepts only the bare api id, so the rolled-back/absent-API probe is answered 400 instead of 404.
Why: a client that reads a stack resource's PhysicalResourceId (or an ARN from any AppSync response) and passes it to GetGraphqlApi is refused as if it sent garbage; the rollback/teardown invariant "the API is gone" (404 NotFoundException "API not found.") is unobservable, and error-code-driven cleanup logic takes the wrong branch.
Fix: the appsync GetGraphqlApi handler's apiId resolution/refusal classification, together with the AWS::AppSync::GraphQLApi physical-id the CloudFormation resource provider records — one of the two must change so the published identifier is accepted and an absent API yields 404 NotFoundException "API not found.".
Traces: deploy-cfn-invalid-schema-stack-rolls-back-rejected.

Cause 4 of 4: CreateDomainName accepts a certificate AWS refuses, so the stack succeeds where AWS rolls back.
AWS: Smithy — CreateDomainName models BadRequestException (400); certificateArn must be an ACM/IAM certificate ARN. AWS additionally requires the certificate to chain to a publicly trusted CA for the domain: a self-signed leaf or a private-CA-signed leaf is refused with BadRequestException and the message "Certificate is invalid" (fixed text; CloudFormation wraps it in the resource failure reason together with that call's Request ID/RequestToken, which are per-run values). Consequently the AWS::AppSync::DomainName resource is CREATE_FAILED, the stack ends ROLLBACK_COMPLETE, and afterwards GetDomainName answers NotFoundException "Domain name not found" and ListDomainNames does not list it.
Emulator: the stack containing that DomainName reaches CREATE_COMPLETE — the certificate is accepted with no trust-chain validation, so no CREATE_FAILED event, no rollback, and the domain exists.
Why: a client validating its certificate wiring gets a green deploy for a configuration AWS rejects; the domain appears usable and the failure only materialises on real AWS.
Fix: the appsync CreateDomainName handler's certificate validation (reject a certificate whose chain is not publicly trusted with BadRequestException "Certificate is invalid").
Related open issues describing the same validation gap on the direct control-plane path: 20261025T182150Z-appsync-createdomainname-answers-501-notimplemented-instead-of-rejecting.md, 20261027T212657Z-appsync-custom-domain-names-createdomainname-performs-no-certificate.md, 20261111T065707Z-appsync-3-net-new-causes-graphqlapi-uris-dns.md — none covers this CloudFormation-deploy check.
Traces: deploy-domain-name-cfn-association-resolves.

## Evidence
Cause 1 — deploy-api-key-cfn-grants-data-plane-access:
  expected: HTTP 200, no `errors` member, the templated value under data
  actual:   AssertionError: unexpected errors with valid key: {'data': {'ping': None}, 'errors': [{'path': ['ping'], 'message': "This field's resolver is defined by mapping templates, which this endpoint does not run; define it with APPSYNC_JS code."}]}
  deploy-data-source-resolver-cfn-executes-query: putItem failed: {'data': {'putItem': None}, 'errors': [{'path': ['putItem'], 'message': "This field's resolver is defined by mapping templates, ..."}]}
  deploy-function-cfn-pipeline-resolver-invocable: pipeline query failed: {'data': {'pipelineEcho': None}, 'errors': [{'path': ['pipelineEcho'], 'message': "AppendProcessed: This field's resolver is defined by mapping templates, ..."}]}
  deploy-resolver-cfn-insufficient-iam-permission-rejected: positive control (permitted read) failed: {'data': {'getItem': None}, 'errors': [{'path': ['getItem'], 'message': "This field's resolver is defined by mapping templates, ..."}]}
  deploy-graphql-api-cfn-behavioral: Check timed out after 60.0s polling for data.behavHello != null (template response `$util.toJson("sim2real-cfn-templated-response")`).

Cause 2 — deploy-appsync-2-cfn-update-replace-rollback-rejected:
  expected: an `errors` entry naming the undeclared field (the v2-only field) — the v1 schema is active after the rollback
  actual:   AssertionError: v1 schema accepted the not-yet-declared field 'version': HTTP 200 {'data': {'echo': {'msg': 'x', 'version': None}}}

Cause 3 — deploy-cfn-invalid-schema-stack-rolls-back-rejected:
  expected: ('NotFoundException', 'API not found.')  [HTTP 404]
  actual:   ('BadRequestException', 'Invalid API ID arn:aws:appsync:<region>:<account>:apis/<apiId>')  [HTTP 400]
  (the identifier is the PhysicalResourceId the emulator's own AWS::AppSync::GraphQLApi resource recorded; the ARN/region/account shown are per-run values, the diverging parts are the status 400 vs 404, the code BadRequestException vs NotFoundException and the fixed message text)

Cause 4 — deploy-domain-name-cfn-association-resolves:
  expected: DomainNameRes CREATE_FAILED with a reason containing "Certificate is invalid", stack ROLLBACK_COMPLETE, then GetDomainName -> NotFoundException "Domain name not found"
  actual:   AssertionError: stack <name> reached CREATE_COMPLETE -- expected DomainNameRes to be rejected for a certificate this account cannot get domain-validated by ACM

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (service appsync).
2. Cause 1: create a stack with AWS::AppSync::GraphQLApi (API_KEY) + AWS::AppSync::GraphQLSchema + AWS::AppSync::DataSource (NONE or DYNAMODB) + AWS::AppSync::Resolver carrying RequestMappingTemplate/ResponseMappingTemplate (and, for the pipeline case, AWS::AppSync::FunctionConfiguration). After CREATE_COMPLETE, POST the query to GetAtt GraphQLUrl with the x-api-key header: the field comes back null with the "defined by mapping templates" error instead of the templated value.
3. Cause 2: deploy a schema v1, update the stack to a v2 schema that adds a field, let the update roll back, then query the v2-only field against the API: it is answered 200 with null instead of a validation error.
4. Cause 3: create a stack whose AWS::AppSync::GraphQLSchema Definition is invalid SDL; wait for ROLLBACK_COMPLETE; take the Api resource's PhysicalResourceId from DescribeStackResources and call GetGraphqlApi with it.
5. Cause 4: import an ACM certificate signed by a throwaway private CA, then create a stack with AWS::AppSync::GraphQLApi + AWS::AppSync::DomainName using that certificateArn: the stack reaches CREATE_COMPLETE instead of failing the DomainName with "Certificate is invalid" and rolling back.
