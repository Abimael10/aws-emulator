Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration
Issue-Key: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration|run#35|AssertionError|assert headers.get("Content-Type") == "application/json", (
Issue-Commit: 42b42c3bdabf251ec917d999bf0a8b3e884f4f35

# AppConfig-GetConfiguration AWS_PROXY integration serves the configuration body with no Content-Type response header

> _Note: this issue was identified against the code base at commit `42b42c3bdabf251ec917d999bf0a8b3e884f4f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetConfiguration

## Description
Cause 1 of 1: an HTTP API route backed by an AWS_PROXY integration with IntegrationSubtype 'AppConfig-GetConfiguration' returns the deployed configuration's bytes with a 200, but the gateway response carries NO Content-Type header, so the configuration's content type is dropped on the way out.

AWS: Smithy (appconfig GetConfiguration, GET /applications/{Application}/environments/{Environment}/configurations/{Configuration}, success code 200) models the output shape Configuration with Content -> httpPayload (blob, sensitive), ConfigurationVersion -> httpHeader 'Configuration-Version', and ContentType -> httpHeader 'Content-Type'. The ContentType value is the one supplied to CreateHostedConfigurationVersion (here 'application/json'), i.e. the Content-Type header on the GetConfiguration response is the configuration version's own ContentType, not a gateway-chosen default. API Gateway's AWS-service (subtype) integration proxies that service response back to the caller, so the client sees the same payload AND the same Content-Type. The subtype's required request parameters (Application, Environment, Configuration, ClientId) are modelled as the four required GetConfiguration input members (three httpLabel, ClientId httpQuery 'client_id'), which is what the subtype maps. The specific set of headers API Gateway copies from an AWS-service subtype response is not in any Smithy model: for that part this is recorded behavior only, and the recording shows the invoke answering 200 with Content-Type 'application/json' and the body being exactly the deployed configuration content.

Emulator: the refusal leg and the positive leg both reach the right status — the signed GET on the auto-deployed $default stage answers 200 and the body is byte-exact the deployed content ({"marker": <value>} parsed and compared equal) — but the response has no Content-Type header at all. The check fails at `assert headers.get("Content-Type") == "application/json"` with `AssertionError: expected the deployed ContentType to be echoed, got None`. The header is absent (None), not merely a different media type, so the emulator is omitting the member rather than mis-rendering it.

Why: a client of an AppConfig-GetConfiguration-backed route cannot tell how to interpret the bytes it got. Browsers, SDK/HTTP clients and anything that dispatches on media type (json parsers keyed on 'application/json', content-negotiating proxies, CDNs caching by type) treat a body with no Content-Type as opaque or sniff it, so a configuration served through the gateway is handled differently from the same configuration fetched directly from appconfig GetConfiguration — where the modelled ContentType httpHeader is present. It also silently discards the ContentType the operator deliberately set on the hosted configuration version, making that field unobservable through the integration.

Fix: the apigatewayv2 AWS_PROXY IntegrationSubtype response path — the component that turns the invoked AWS service's response into the gateway's HTTP response for the 'AppConfig-GetConfiguration' subtype. It must propagate the appconfig GetConfiguration response's ContentType (and, consistently with the model, Configuration-Version) onto the outgoing response headers instead of emitting only a body. Equivalently, if the subtype invoke is served from appconfig state directly, the serializer must bind the hosted configuration version's ContentType to the Content-Type header per the Configuration output shape's httpHeader traits.

Traces: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration (assertion at the positive-control leg of the check, after the BadRequestException refusal leg passes).

Related (not duplicates, both are about the refusal leg, which now passes its code/status/message assertions): open issues 20261117T135333Z-apigatewayv2-appconfig-iam-sts-createintegration-accepts-an-appconfig-getconfiguration-subtype-with-no.md and 20261118T131019Z-apigatewayv2-appconfig-iam-sts-apigatewayv2-createintegration-subtype-required-parameter-refusal-lists-the.md. Neither mentions the integration's response headers.

## Evidence
Expected (AWS): the signed GET on the deployed $default stage of the HTTP API answers 200 with the deployed configuration bytes AND the response header Content-Type set to the hosted configuration version's ContentType — 'application/json' for a version created with ContentType='application/json'. (Smithy appconfig GetConfiguration output 'Configuration': ContentType -> httpHeader 'Content-Type'; ConfigurationVersion -> httpHeader 'Configuration-Version'; Content -> httpPayload.)
Actual (emulator): HTTP 200 and a byte-exact body (the `returned == {"marker": ...}` assertion passes), but `headers.get("Content-Type")` is None — the header is not emitted.
Harness: check_apigatewayv2__appconfig__iam__sts_appconfig_getconfiguration_proxy_missing_parameter_mapping_refused_else_returns_configuration.py line 192, `AssertionError: expected the deployed ContentType to be echoed, got None`.
Fixed/variable split for the assertion: the required part is that a Content-Type header is present and equals the ContentType stored on the deployed configuration version; 'application/json' is the value this run supplied to CreateHostedConfigurationVersion, the marker value, api id, role arn, stage URL and configuration ids are per-run values and are not part of the requirement.

## Steps to Reproduce
1. appconfig: CreateApplication, CreateEnvironment, CreateConfigurationProfile(LocationUri='hosted').
2. appconfig: CreateHostedConfigurationVersion(Content=b'{"marker":"<any>"}', ContentType='application/json'); note VersionNumber.
3. appconfig: CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100.0, FinalBakeTimeInMinutes=0, ReplicateTo='NONE') and StartDeployment for that profile/version; poll GetDeployment until State='COMPLETE'.
4. iam: CreateRole trusting apigateway.amazonaws.com with an inline policy allowing appconfig:GetConfiguration on the application arn and arn/*; note the role arn.
5. apigatewayv2: CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='AWS_PROXY', IntegrationSubtype='AppConfig-GetConfiguration', PayloadFormatVersion='1.0', CredentialsArn=<role arn>, RequestParameters={Application:<appId>, Environment:<envId>, Configuration:<profileId>, ClientId:'<any>'}); CreateRoute(RouteKey='GET /config', Target='integrations/<integrationId>', AuthorizationType='AWS_IAM'); CreateStage(StageName='$default', AutoDeploy=True).
6. Issue a SigV4-signed (service 'execute-api') GET to <stage url>/config and inspect the response headers.
Observed: 200, correct body, no Content-Type header.
Expected: 200, correct body, Content-Type: application/json (the deployed version's ContentType).
