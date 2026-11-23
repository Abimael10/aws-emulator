Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration
Issue-Key: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration|_error#23|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# CreateIntegration accepts an AppConfig-GetConfiguration subtype with no required request-parameter mappings

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Cause 1 of 1: an AWS_PROXY integration with an IntegrationSubtype is created even though the subtype's required parameter mappings are absent.

AWS: Smithy (apigatewayv2 CreateIntegration) models `IntegrationSubtype` and `RequestParameters` and declares BadRequestException (400) among its errors; AWS documents that for an AWS_PROXY integration with a subtype, the subtype's required request parameters must be supplied in RequestParameters. The recording shows the call refused with a 400 client error naming the missing mapping, and only an integration whose mappings are complete returns the configuration through the proxy.

Emulator: the call is accepted. It answers HTTP 201 with the integration (`IntegrationSubtype: 'AppConfig-GetConfiguration'`, `IntegrationType: 'AWS_PROXY'`, `RequestParameters: {}`, `IntegrationId: 'ur7ayid'`) instead of refusing it: `AssertionError: expected the call to be rejected, but it succeeded: {...'RequestParameters': {}...}`.

Why: the client's misconfiguration is discovered only at invoke time (or never), the broken integration is attached to a route, and a template/IaC run that should fail fast appears to succeed.

Fix: CreateIntegration's input validation — when IntegrationSubtype is set, require the subtype's mandatory RequestParameters mapping keys and refuse with a 400 BadRequestException naming the missing parameter.

Traces: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration.

## Evidence
Expected: 400 BadRequestException naming the missing required request-parameter mapping for IntegrationSubtype AppConfig-GetConfiguration (the check asserts the call is refused).
Actual: HTTP 201 and an integration object:
  `AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'ConnectionType': 'INTERNET', 'IntegrationId': 'ur7ayid', 'IntegrationSubtype': 'AppConfig-GetConfiguration', 'IntegrationType': 'AWS_PROXY', 'PayloadFormatVersion': '1.0', 'RequestParameters': {}, ...}`

## Steps to Reproduce
1. Create an HTTP API and a role API Gateway can assume.
2. CreateIntegration with IntegrationType=AWS_PROXY, IntegrationSubtype=AppConfig-GetConfiguration, CredentialsArn=<role>, and RequestParameters={} (no required mappings).
Observed: 201 with the integration created. Expected: 400 BadRequestException naming the missing required mapping; nothing created.
