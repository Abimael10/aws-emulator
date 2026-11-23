Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration
Issue-Key: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration|_poll_http#31|TimeoutError|raise TimeoutError(
Issue-Commit: bea5cdf57d487a31229ef2924227deee2fe47ff2

# apigatewayv2: an AWS_PROXY AppConfig-GetConfiguration subtype integration answers 500 "Internal Server Error" instead of the deployed configuration content

> _Note: this issue was identified against the code base at commit `bea5cdf57d487a31229ef2924227deee2fe47ff2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Cause 1 of 1: a deployed HTTP API route whose target is an AWS_PROXY integration with IntegrationSubtype 'AppConfig-GetConfiguration' and all four required request parameters mapped (Application, Environment, Configuration, ClientId) never performs the AppConfig GetConfiguration call. Every signed GET on the $default stage path is answered 500 with the gateway's generic body, so the subtype integration's success path is unservable.

AWS: Smithy (apigatewayv2 CreateIntegration, POST /v2/apis/{ApiId}/integrations, success 201) models IntegrationType=AWS_PROXY together with IntegrationSubtype (StringWithLengthBetween1And128), RequestParameters (IntegrationParameters) and CredentialsArn (Arn); the AWS-service-integration reference for the HTTP-API AWS_PROXY subtypes declares AppConfig-GetConfiguration's required parameters as Application, Environment, Configuration, ClientId and defines the integration as invoking appconfig:GetConfiguration under the CredentialsArn role. Smithy (appconfig GetConfiguration, GET /applications/{Application}/environments/{Environment}/configurations/{Configuration}, success 200) returns Content as the @httpPayload blob with ContentType bound to the Content-Type response header and ConfigurationVersion bound to Configuration-Version. For the gateway's framing of a subtype success this is recorded behavior only: the recording shows the stage answering HTTP 200 with the deployed configuration version's bytes as the entire response body and the deployed ContentType (here application/json) echoed as the response Content-Type.

Emulator: the control plane accepts the fully-mapped CreateIntegration, the route (GET /config, AuthorizationType=AWS_IAM) and the auto-deployed $default stage, but every signed request to the stage path answers HTTP 500 with body {"message":"Internal Server Error"} — the harness retries for 50 s and never sees any other status, so the integration is never dispatched to AppConfig at all. Failure: TimeoutError: GET /config reaching the AppConfig-GetConfiguration integration never reached an accepted status within 50s. Last: 500 '{"message":"Internal Server Error"}'. In that generic body, {"message":"Internal Server Error"} is the gateway's own fixed text (not an AppConfig error); AWS would instead send 200 and the configuration bytes, whose value comes from the deployed hosted configuration version in state, with Content-Type taken from that version's stored ContentType.

Why: a client that fronts AppConfig with an HTTP API — the documented, credential-free way for a browser or device to read a deployed configuration — gets a 500 for a correctly configured integration. It cannot distinguish a misconfiguration from a service fault, cannot read any configuration through the gateway, and any assertion that the proxied body equals the deployed content or that Content-Type is the deployed ContentType fails. The 500 also masks the AppConfig-side error classes (400 BadRequestException / 404 ResourceNotFoundException) a client would otherwise key on.

Fix: the apigatewayv2 execute-api data-plane integration dispatcher for AWS_PROXY integrations carrying an IntegrationSubtype — the AppConfig-GetConfiguration subtype has no handler that resolves the mapped Application/Environment/Configuration/ClientId parameters, assumes CredentialsArn, calls appconfig GetConfiguration and serializes its Content blob as the response body with the returned ContentType as Content-Type; the request currently falls through to the generic 500 path. Note the control-plane refusal leg of the same check is covered elsewhere and is NOT this cause.

Related (not duplicates, do not close against them): open issue 20261117T135333Z-apigatewayv2-appconfig-iam-sts-createintegration-accepts-an-appconfig-getconfiguration-subtype-with-no.md and open issue 20261118T131019Z-apigatewayv2-appconfig-iam-sts-apigatewayv2-createintegration-subtype-required-parameter-refusal-lists-the.md both concern the CreateIntegration required-parameter refusal (the check now passes that leg); open issue 20261029T182452Z-cloudwatch-apigatewayv2-http-api-returns-its-own-generic-message-internal.md concerns passing an integration's own 5xx through, whereas here no integration is invoked at all.

Traces: apigatewayv2__appconfig__iam__sts.appconfig-getconfiguration-proxy-missing-parameter-mapping-refused-else-returns-configuration (positive-control leg).

## Evidence
Expected (AWS, from the recording): a signed GET on the auto-deployed $default stage path routed to the AWS_PROXY / AppConfig-GetConfiguration integration answers HTTP 200; the body is exactly the deployed hosted configuration version's bytes (the JSON document deployed in that run) and the response Content-Type is the ContentType stored with that version ('application/json').
Actual (emulator): HTTP 500 on every attempt for 50 s, body {"message":"Internal Server Error"} — the gateway's own generic error text, with no AppConfig call made and no Content-Type of the deployed version.
Harness: check_apigatewayv2__appconfig__iam__sts_appconfig_getconfiguration_proxy_missing_parameter_mapping_refused_else_returns_configuration.py, run() -> _poll_http(accept=lambda s: s == 200) -> `TimeoutError: GET /config reaching the AppConfig-GetConfiguration integration (<stage-url>/config) never reached an accepted status within 50s. Last: 500 '{"message":"Internal Server Error"}'`. The preceding CreateIntegration BadRequestException assertions in the same check passed, so the failure is isolated to the data plane.

## Steps to Reproduce
1. AppConfig: CreateApplication, CreateEnvironment, CreateConfigurationProfile(LocationUri='hosted'), CreateHostedConfigurationVersion(Content=b'{"marker":"<value>"}', ContentType='application/json'), CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo='NONE'), StartDeployment and wait for the deployment State to reach COMPLETE.
2. IAM: CreateRole trusting apigateway.amazonaws.com with an inline policy allowing appconfig:GetConfiguration on the application ARN and its children; note the role ARN.
3. apigatewayv2: CreateApi(ProtocolType='HTTP'); CreateIntegration(IntegrationType='AWS_PROXY', IntegrationSubtype='AppConfig-GetConfiguration', PayloadFormatVersion='1.0', CredentialsArn=<role arn>, RequestParameters={'Application': <appId>, 'Environment': <envId>, 'Configuration': <profileId>, 'ClientId': '<any client id>'}); CreateRoute(RouteKey='GET /config', Target='integrations/<integrationId>', AuthorizationType='AWS_IAM'); CreateStage(StageName='$default', AutoDeploy=True).
4. Send a SigV4-signed (service execute-api) GET to <stage url>/config.
Observed: 500 with body {"message":"Internal Server Error"}, repeatedly, for at least 50 s.
Expected: 200 whose body is exactly the deployed configuration content and whose Content-Type is the deployed version's ContentType.
