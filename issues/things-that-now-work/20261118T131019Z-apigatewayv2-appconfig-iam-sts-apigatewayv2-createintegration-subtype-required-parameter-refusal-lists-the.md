Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration
Issue-Key: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration|run#25|AssertionError|assert message == (
Issue-Commit: 1f604c3d80924acff5038a505405bc1871ebd9db

# apigatewayv2 CreateIntegration subtype required-parameter refusal lists the missing parameters alphabetically instead of in the subtype's declared order

> _Note: this issue was identified against the code base at commit `1f604c3d80924acff5038a505405bc1871ebd9db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Cause 1 of 1: the BadRequestException message that refuses an AWS_PROXY integration whose IntegrationSubtype has unmapped required request parameters renders the parameter list in alphabetical order rather than in the subtype's documented parameter order, so the message text diverges from AWS's.

AWS: Smithy (apigatewayv2 CreateIntegration, POST /v2/apis/{ApiId}/integrations, success 201) models IntegrationSubtype (StringWithLengthBetween1And128) and RequestParameters (IntegrationParameters) and declares BadRequestException with httpError 400 among its errors; the model says nothing about the message text, so for the wording this is recorded behavior only. The recording shows HTTP 400 with Error.Code 'BadRequestException' and Error.Message:
  "Operation: <IntegrationSubtype> requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [<p1>, <p2>, ...]"
Fixed text: "Operation: ", " requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [", ", " between names, and the closing "]". Variable parts taken from the request and from the AWS service-integration reference: the IntegrationSubtype from the request, and the subtype's required parameter names IN THE ORDER THE SUBTYPE DECLARES THEM (for AppConfig-GetConfiguration: Application, Environment, Configuration, ClientId — the order published in the HTTP-API AWS-service integration reference), not sorted. No per-run value (api id, integration id, role arn) appears in the message.

Emulator: the refusal itself now happens with the right status and code, but the list is sorted alphabetically. Observed message: 'Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Application, ClientId, Configuration, Environment]' where AWS answers '... [Application, Environment, Configuration, ClientId]'. The check fails on `assert message == (...)` with `AssertionError: unexpected message 'Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Application, ClientId, Configuration, Environment]'`.

Why: a client (or an IaC/test harness) that matches or diffs the refusal message — the normal way this class of misconfiguration is surfaced to a developer, since the message is the only place the missing parameters are named — does not match AWS's text, and the message no longer tells the reader the subtype's parameter order. Any assertion or log-scrape keyed to AWS's wording breaks against the emulator.

Fix: the apigatewayv2 CreateIntegration validation for IntegrationSubtype required-parameter mappings — the required-parameter table must preserve the subtype's declared parameter order and the message builder must emit it in that order instead of sorting the set of missing/required keys.

Related (not a duplicate): open issue 20261117T135333Z-apigatewayv2-appconfig-iam-sts-createintegration-accepts-an-appconfig-getconfiguration-subtype-with-no.md reported that this call was ACCEPTED (201). The refusal now exists; what remains diverging is only the message's parameter ordering described above.

Traces: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration.

## Evidence
Expected (AWS): HTTP 400, Error.Code 'BadRequestException', Error.Message 'Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Application, Environment, Configuration, ClientId]'.
Actual (emulator): HTTP 400, Error.Code 'BadRequestException' (code and status correct), Error.Message 'Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [Application, ClientId, Configuration, Environment]' — the four required parameter names sorted alphabetically instead of in the subtype's declared order.
Harness: check_apigatewayv2__appconfig__iam__sts_appconfig_getconfiguration_proxy_missing_parameter_mapping_refused_else_returns_configuration.py line 147, `AssertionError: unexpected message '...[Application, ClientId, Configuration, Environment]'`.

## Steps to Reproduce
1. Create an HTTP API: apigatewayv2 CreateApi(ProtocolType='HTTP').
2. Create an IAM role API Gateway can assume (trust apigateway.amazonaws.com) and note its arn.
3. Call CreateIntegration(ApiId=<api>, IntegrationType='AWS_PROXY', IntegrationSubtype='AppConfig-GetConfiguration', PayloadFormatVersion='1.0', CredentialsArn=<role arn>, RequestParameters={}).
4. Read Error.Code and Error.Message off the resulting ClientError.
Observed: BadRequestException with the required parameters listed as [Application, ClientId, Configuration, Environment].
Expected: BadRequestException with them listed in the subtype's declared order, [Application, Environment, Configuration, ClientId].
