Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.additional-authentication-providers-limit appsync.create-graphql-api-malformed-cloudwatch-logs-role-rejected appsync.create-api-event-log-config-malformed-role-rejected appsync.create-api-key-description-length-boundary appsync.create-channel-namespace-handler-config-nonexistent-datasource-rejected appsync.pipeline-resolver-functions-limit appsync.tag-resource-channel-namespace-reserved-key-rejected appsync.create-function-code-size-limit
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|UpdateGraphqlApi|BadRequestException
Issue-Key: appsync.create-graphql-api-malformed-cloudwatch-logs-role-rejected|_error#5|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: appsync.create-api-event-log-config-malformed-role-rejected|_error#5|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: appsync.create-api-key-description-length-boundary|_error#11|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: appsync|CreateDataSource|NotFoundException
Issue-Key: appsync.pipeline-resolver-functions-limit|_error#19|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: appsync|TagResource|BadRequestException
Issue-Key: appsync.create-function-code-size-limit|run#14|AssertionError|assert message == f"Code must be {_LIMIT} bytes or less", (
Issue-Commit: 316bc8d4d27618b04e0a3739e3ce26f595ac855b

# appsync: 7 net-new causes — control-plane validation and limit ceilings missing (additional auth providers, cloudWatchLogsRoleArn, api-key description, Event-API data sources, pipeline function ceiling, channel-namespace tag ARNs, code-size message)

> _Note: this issue was identified against the code base at commit `316bc8d4d27618b04e0a3739e3ce26f595ac855b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateGraphqlApi

## Description
Seven distinct, net-new root causes in the appsync emulator's control plane. Every one is grounded in a failing behavioral check that is neither ALREADY FILED nor OWNED by an open issue. A fixer must close all seven before submitting.

Cause 1 of 7: UpdateGraphqlApi treats several OPENID_CONNECT additionalAuthenticationProviders as duplicates of one another, so a list of distinct OIDC providers is refused, and the 50-provider ceiling is therefore never reached.
Failing check id: appsync.additional-authentication-providers-limit
AWS: Smithy: UpdateGraphqlApi.additionalAuthenticationProviders targets the list AdditionalAuthenticationProviders of AdditionalAuthenticationProvider{authenticationType, openIDConnectConfig, userPoolConfig, lambdaAuthorizerConfig} with no modeled list-length bound; errors include BadRequestException(400). Recorded behavior for the quota: AWS accepts up to 50 additional authentication providers on one API, each OPENID_CONNECT entry distinguished by its openIDConnectConfig.issuer — duplicate detection is per provider configuration (issuer / user-pool id / authorizer ARN), NOT per authenticationType, so 50 OIDC entries with 50 distinct issuers is a valid request. Only the 51st entry is refused, with BadRequestException and the message "Maximum number of additional authentication providers is <limit>. You added <count>." — the fixed text is that sentence pair, the variable parts are the limit (50) and the number of providers the request carried. A refused update leaves the previously stored providers in place, and a later same-shape update back at the ceiling succeeds.
Emulator: the FIRST update, carrying 50 OIDC providers with 50 distinct issuers, is refused: HTTP 400 BadRequestException "Authentication type OPENID_CONNECT for additional authentication provider 2 already specified on the API. It can only be specified once." The validator collapses the list by authenticationType, so any second entry of the same type looks like a duplicate; the per-issuer identity is ignored and the real 50-provider ceiling message is unreachable.
Why: a client cannot configure more than one OIDC (or more than one user-pool) additional provider at all — the standard multi-tenant/multi-issuer AppSync setup is rejected outright with a message about a restriction AWS does not have, and code that probes the quota sees the wrong error text at the wrong request size.
Fix: the UpdateGraphqlApi (and CreateGraphqlApi) handler's additionalAuthenticationProviders validation in the appsync service crate — duplicate detection must key on the provider's configuration, not on authenticationType, and the list-length refusal must carry AWS's ceiling message.
Traces: appsync.additional-authentication-providers-limit (error: botocore BadRequestException on the at-limit UpdateGraphqlApi).

Cause 2 of 7: a malformed logConfig.cloudWatchLogsRoleArn / eventConfig.logConfig.cloudWatchLogsRoleArn is accepted, so both CreateGraphqlApi and CreateApi mint a resource AWS refuses.
Failing check ids: appsync.create-graphql-api-malformed-cloudwatch-logs-role-rejected, appsync.create-api-event-log-config-malformed-role-rejected
AWS: Smithy: LogConfig.cloudWatchLogsRoleArn (required, String) on CreateGraphqlApi and EventLogConfig.cloudWatchLogsRoleArn (required, String) on CreateApi; both operations model BadRequestException(400). AWS validates that the value is a well-formed IAM role ARN and refuses a value that is not one with HTTP 400 BadRequestException and the fixed message "The cloudWatchLogsRoleArn is invalid. Please refer to the documentation for more details." (fixed text, no request-derived parts). The refused call creates nothing — the name does not appear in ListGraphqlApis / ListApis — while the same request carrying a real role ARN succeeds and reads back the ARN and log level verbatim.
Emulator: both calls answer HTTP 200 and create the resource with the junk string stored verbatim: CreateGraphqlApi returns graphqlApi.logConfig = {fieldLogLevel: ERROR, cloudWatchLogsRoleArn: "not-a-valid-arn", excludeVerboseContent: false}, and CreateApi returns api.eventConfig.logConfig = {logLevel: ERROR, cloudWatchLogsRoleArn: "not-a-valid-arn"}. No ARN syntax check exists on either path.
Why: a typo'd or unresolved role ARN (a very common CFN/CDK parameter mistake) is silently accepted, so the API appears to be configured for CloudWatch logging while nothing can ever publish. Negative-path tests that assert the 400 pass against AWS and fail here, and the emulator's state afterwards is a resource AWS would never have.
Fix: the CreateGraphqlApi and CreateApi handlers' log-config validation (and the matching Update* paths) in the appsync service crate — reject a cloudWatchLogsRoleArn that is not a well-formed IAM role ARN with the modeled BadRequestException and record no resource.
Traces: appsync.create-graphql-api-malformed-cloudwatch-logs-role-rejected, appsync.create-api-event-log-config-malformed-role-rejected (both: "expected the call to be rejected, but it succeeded" with HTTPStatusCode 200).

Cause 3 of 7: CreateApiKey does not enforce the 256-character description ceiling.
Failing check id: appsync.create-api-key-description-length-boundary
AWS: Smithy: CreateApiKey.description targets String with no modeled length bound, and the operation models BadRequestException(400) — so the bound is recorded behavior only: AWS accepts a 256-character description verbatim and refuses 257 with HTTP 400 BadRequestException and the fixed message "Invalid description given, must be less than 256 characters." (fixed text, no request-derived parts). The refused call mints no key, so ListApiKeys then shows only the accepted one.
Emulator: the 257-character description is accepted with HTTP 200 and an apiKey is minted (id da2-..., description echoed at full length), so a second key exists where AWS has one.
Why: a client's over-long description is stored instead of refused; the key count and ListApiKeys contents diverge from AWS, and any guard rail built on the documented refusal never fires.
Fix: the CreateApiKey handler's description validation (and UpdateApiKey's) in the appsync service crate — refuse >256 characters with the modeled BadRequestException and create no key.
Traces: appsync.create-api-key-description-length-boundary (mismatch: "expected a ClientError, but the call succeeded").

Cause 4 of 7: CreateDataSource cannot see an Event API — an apiId minted by CreateApi is refused NotFoundException "API not found."
Failing check id: appsync.create-channel-namespace-handler-config-nonexistent-datasource-rejected
AWS: Smithy: CreateApi (POST /v2/apis) returns Api$apiId; CreateDataSource (POST /v1/apis/{apiId}/datasources) takes that same apiId and models NotFoundException(404) only for an apiId that does not exist. Event APIs and GraphQL APIs share one apiId namespace, so CreateDataSource(apiId=<Event API id>, type=AWS_LAMBDA, lambdaConfig, serviceRoleArn) succeeds and the data source becomes referenceable from CreateChannelNamespace handlerConfigs. On that foundation AWS refuses a DIRECT handlerConfigs integration naming a data source the API does not have, with HTTP 400 BadRequestException and the fixed message "DataSource not found.", creating no namespace.
Emulator: CreateDataSource against the apiId CreateApi just returned fails with HTTP 404 NotFoundException "API not found." — the data-source handler resolves apiId only against APIs created by CreateGraphqlApi, so Event APIs have no data sources at all and the whole channel-namespace/handler-config surface (including the "DataSource not found." refusal above) is unreachable.
Why: Event API applications cannot attach a Lambda (or any) data source, so no DIRECT/CODE channel-namespace handler can be wired up; a client gets a not-found for a resource it has just created and whose id the emulator itself issued.
Fix: the CreateDataSource handler's apiId lookup (and the sibling Get/Update/Delete/ListDataSources paths) in the appsync service crate — resolve apiId against the single api registry that CreateApi also writes, then add the handlerConfigs data-source existence check that yields BadRequestException "DataSource not found."
Traces: appsync.create-channel-namespace-handler-config-nonexistent-datasource-rejected (error: NotFoundException "API not found." on CreateDataSource).

Cause 5 of 7: UpdateResolver does not enforce the 10-functions-per-PIPELINE-resolver ceiling.
Failing check id: appsync.pipeline-resolver-functions-limit
AWS: Smithy: UpdateResolver.pipelineConfig targets PipelineConfig{functions: FunctionsIds} with no modeled list bound, and the operation models BadRequestException(400) — the ceiling is recorded behavior: a PIPELINE resolver holds at most 10 functions. Growing it to 11 is refused with HTTP 400 BadRequestException and the message "A resolver can only be composed of <limit> functions." (fixed text, the limit being the variable part — 10). The refused update leaves the resolver's prior function list untouched, and a same-size reordering update still succeeds.
Emulator: the 11-function UpdateResolver answers HTTP 200 and stores all 11 ids in resolver.pipelineConfig.functions, so the resolver ends up in a state AWS cannot produce and the documented refusal never happens.
Why: a client over-building a pipeline gets no diagnosis; the resolver is persisted beyond the service quota, so behaviour that depends on the quota (and any negative test asserting the refusal) diverges, and the over-long pipeline then executes differently than it ever could on AWS.
Fix: the UpdateResolver / CreateResolver handlers' pipelineConfig validation in the appsync service crate — refuse more than 10 function ids with the modeled BadRequestException and leave the stored resolver unchanged.
Traces: appsync.pipeline-resolver-functions-limit (mismatch: "expected the call to be rejected, but it succeeded" with a 200 carrying 11 function ids).

Cause 6 of 7: the tagging surface refuses a ChannelNamespace ARN — TagResource answers BadRequestException "Invalid input resource arn" for an ARN the service itself issued.
Failing check id: appsync.tag-resource-channel-namespace-reserved-key-rejected
AWS: Smithy: TagResource/UntagResource/ListTagsForResource bind resourceArn as an @httpLabel typed ResourceArn (@length 70..75, @pattern ^arn:aws:appsync:[A-Za-z0-9_/.-]{0,63}:\d{12}:apis/[0-9A-Za-z_-]{26}$) and model BadRequestException(400). That modeled pattern covers only the api ARN; AWS nonetheless accepts the ChannelNamespace ARN returned in ChannelNamespace$channelNamespaceArn (recorded behavior — the pattern is not enforced as the authority on the server for this resource), so TagResource(resourceArn=<channelNamespaceArn>, tags={<ordinary key>: <value>}) succeeds, and the tag is then visible through ListTagsForResource, GetChannelNamespace and ListChannelNamespaces alike. Only a reserved key is refused: a tag key beginning with "aws:" (case-insensitive) yields HTTP 400 BadRequestException with the fixed message "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)" and leaves the previously applied tags untouched.
Emulator: the positive control already fails — TagResource with an ordinary key on the channelNamespaceArn is refused HTTP 400 BadRequestException "Invalid input resource arn". The handler validates resourceArn against the api-only shape pattern, so no channel-namespace tag can ever be set, read or removed, and the reserved-key refusal (with its own distinct message) is unreachable.
Why: channel namespaces cannot be tagged at all on the emulator, so tag-based discovery, cost allocation and the tag-propagation contract of CloudFormation-managed namespaces all break; the client is told its ARN is invalid even though the emulator minted that exact ARN.
Fix: the TagResource / UntagResource / ListTagsForResource handlers' resourceArn parsing and the tag state they read in the appsync service crate — accept every appsync resource ARN the service issues (api, channel namespace), keep the tags with the resource, and add the reserved-"aws:"-prefix key refusal with AWS's message.
Related-but-not-covering: 20261027T222245Z-appsync-tagging-operations-are-not-gated-by-identity.md is about IAM gating of the same operations and carries neither this check's replay key nor this ARN-parsing cause.
Traces: appsync.tag-resource-channel-namespace-reserved-key-rejected (error: BadRequestException "Invalid input resource arn" on the positive-control TagResource).

Cause 7 of 7: the CreateFunction code-size refusal carries a generic framework constraint message instead of AppSync's.
Failing check id: appsync.create-function-code-size-limit
AWS: Smithy: CreateFunction.code targets Code with @length min 1 max 32768, and the operation models BadRequestException(400). AWS accepts APPSYNC_JS code of exactly 32,768 bytes and refuses 32,769 with HTTP 400 BadRequestException whose message is "Code must be <limit> bytes or less" — fixed text with the limit (32768) as its only variable part. It does not echo the submitted code and does not use the "N validation error(s) detected" framework phrasing. The refused call creates no function, so ListFunctions shows only the accepted one.
Emulator: the status and error code are right (400 BadRequestException), but the message is the generic smithy constraint report: "1 validation error detected: Value '<the entire submitted code, elided> ... [32769 bytes]' at 'code' failed to satisfy constraint: Member must have length less than or equal to 32768". The @length trait is being rendered by the generic constraint reporter rather than by the operation's own validation.
Why: clients and tooling match on the message text to tell a size refusal apart from other BadRequestExceptions; the generic text also echoes the whole payload back into the error, which AWS never does (log/PII amplification), so error handling and log assertions written against AWS diverge.
Fix: the CreateFunction (and UpdateFunction / CreateResolver / UpdateResolver `code`) validation in the appsync service crate — produce AppSync's own ceiling message for the code length check instead of the generic constraint-violation renderer.
Traces: appsync.create-function-code-size-limit (mismatch: "expected AWS's exact ceiling message naming the 32768-byte limit").

## Evidence
Cause 1 — appsync.additional-authentication-providers-limit
Expected (AWS): UpdateGraphqlApi(apiId, name, authenticationType=API_KEY, additionalAuthenticationProviders=[50 x {authenticationType: OPENID_CONNECT, openIDConnectConfig:{issuer: https://<prefix>-<i>.example.com}}]) -> 200, and GetGraphqlApi returns exactly those 50 issuers; the same call with 51 providers -> 400 BadRequestException, Message "Maximum number of additional authentication providers is 50. You added 51.", with the stored 50 unchanged.
Actual (emulator): the 50-provider call itself fails — botocore.errorfactory.BadRequestException: An error occurred (BadRequestException) when calling the UpdateGraphqlApi operation: Authentication type OPENID_CONNECT for additional authentication provider 2 already specified on the API. It can only be specified once.

Cause 2 — appsync.create-graphql-api-malformed-cloudwatch-logs-role-rejected / appsync.create-api-event-log-config-malformed-role-rejected
Expected (AWS): CreateGraphqlApi(name, authenticationType=API_KEY, logConfig={fieldLogLevel: ERROR, cloudWatchLogsRoleArn: "not-a-valid-arn"}) -> 400 BadRequestException "The cloudWatchLogsRoleArn is invalid. Please refer to the documentation for more details." and the name absent from ListGraphqlApis; likewise CreateApi(name, eventConfig={... logConfig:{logLevel: ERROR, cloudWatchLogsRoleArn: "not-a-valid-arn"}}) -> the same 400 and the name absent from ListApis.
Actual (emulator): both answer HTTP 200 and create the resource — graphqlApi.logConfig.cloudWatchLogsRoleArn == 'not-a-valid-arn' (apiId abt6kt2dmzphuocjrfe3zefb35 in the replay) and api.eventConfig.logConfig.cloudWatchLogsRoleArn == 'not-a-valid-arn' (apiId jbwet3w4b3ccipwe6cz23qqrqk).

Cause 3 — appsync.create-api-key-description-length-boundary
Expected (AWS): CreateApiKey(apiId, description='d'*256) -> 200 with the description echoed; CreateApiKey(apiId, description='d'*257) -> 400 BadRequestException "Invalid description given, must be less than 256 characters."; ListApiKeys then returns only the accepted key id.
Actual (emulator): the 257-character call returns HTTP 200 with apiKey {'id': 'da2-...', 'description': 'ddd…' (257 chars)}.

Cause 4 — appsync.create-channel-namespace-handler-config-nonexistent-datasource-rejected
Expected (AWS): CreateApi(eventConfig=API_KEY modes) -> api.apiId; CreateDataSource(apiId=<that id>, name=<ds>, type=AWS_LAMBDA, lambdaConfig={lambdaFunctionArn}, serviceRoleArn=<appsync-trusted role>) -> 200; CreateChannelNamespace(apiId, name, handlerConfigs={onPublish:{behavior: DIRECT, integration:{dataSourceName: <nonexistent>}}}) -> 400 BadRequestException "DataSource not found." with no namespace created (GetChannelNamespace -> NotFoundException), and the same shape naming the real data source -> 200.
Actual (emulator): CreateDataSource fails first — botocore.errorfactory.NotFoundException: An error occurred (NotFoundException) when calling the CreateDataSource operation: API not found.

Cause 5 — appsync.pipeline-resolver-functions-limit
Expected (AWS): UpdateResolver(kind=PIPELINE, pipelineConfig={functions: [11 function ids]}) -> 400 BadRequestException "A resolver can only be composed of 10 functions."; GetResolver still shows the prior 10 ids.
Actual (emulator): HTTP 200 with resolver.pipelineConfig.functions holding all 11 ids (e.g. ['lsjnr5uvuqn3vccxjdkpipscvg', …, 'cx2hhgi6dtgqp'] — 11 entries).

Cause 6 — appsync.tag-resource-channel-namespace-reserved-key-rejected
Expected (AWS): CreateChannelNamespace(..., tags={'Team':'checkout'}) -> channelNamespaceArn; TagResource(resourceArn=<channelNamespaceArn>, tags={'Env':'staging'}) -> 200; TagResource(resourceArn=<same>, tags={'aws:reserved':'x'}) -> 400 BadRequestException "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)"; ListTagsForResource / GetChannelNamespace / ListChannelNamespaces all then report {'Team':'checkout','Env':'staging'}.
Actual (emulator): the ordinary-key TagResource is already refused — botocore.errorfactory.BadRequestException: An error occurred (BadRequestException) when calling the TagResource operation: Invalid input resource arn.

Cause 7 — appsync.create-function-code-size-limit
Expected (AWS): CreateFunction(..., runtime={APPSYNC_JS,1.0.0}, code=<32769 bytes>) -> 400 BadRequestException, Message exactly "Code must be 32768 bytes or less".
Actual (emulator): 400 BadRequestException with Message "1 validation error detected: Value 'export function request(ctx) { return {}; }\nexport function response(ctx) { return ctx.result; }\n// aaaa… [32769 bytes]' at 'code' failed to satisfy constraint: Member must have length less than or equal to 32768".

## Steps to Reproduce
Cause 1:
1. appsync.CreateGraphqlApi(name=<unique>, authenticationType=API_KEY).
2. UpdateGraphqlApi(apiId, name, authenticationType=API_KEY, additionalAuthenticationProviders=[{authenticationType: OPENID_CONNECT, openIDConnectConfig:{issuer: https://a-<i>.example.com}} for i in 0..49]) — AWS answers 200; the emulator refuses with the "already specified on the API" message.
3. (unreachable) repeat with 51 distinct issuers and observe AWS's "Maximum number of additional authentication providers is 50. You added 51."

Cause 2:
1. CreateGraphqlApi(name=<unique>, authenticationType=API_KEY, logConfig={fieldLogLevel: ERROR, cloudWatchLogsRoleArn: 'not-a-valid-arn'}) -> expect 400 BadRequestException; the emulator returns 200.
2. CreateApi(name=<unique>, eventConfig={authProviders/connectionAuthModes/defaultPublishAuthModes/defaultSubscribeAuthModes = [{authType: API_KEY}], logConfig={logLevel: ERROR, cloudWatchLogsRoleArn: 'not-a-valid-arn'}}) -> expect the same 400; the emulator returns 200.
3. ListGraphqlApis / ListApis — on AWS neither name is present; on the emulator both are.

Cause 3:
1. CreateGraphqlApi(API_KEY). 2. CreateApiKey(apiId, description='d'*256) -> 200. 3. CreateApiKey(apiId, description='d'*257) -> expect 400 BadRequestException "Invalid description given, must be less than 256 characters."; the emulator mints a second key. 4. ListApiKeys shows two keys instead of one.

Cause 4:
1. CreateApi(name=<unique>, eventConfig=<API_KEY modes>) and keep api.apiId.
2. iam.CreateRole trusting appsync.amazonaws.com.
3. CreateDataSource(apiId=<the Event API id>, name=<ds>, type=AWS_LAMBDA, lambdaConfig={lambdaFunctionArn: arn:aws:lambda:<region>:<account>:function:<ds>}, serviceRoleArn=<role arn>) -> expect 200; the emulator answers 404 NotFoundException "API not found."
4. (unreachable) CreateChannelNamespace with handlerConfigs.onPublish DIRECT naming a nonexistent data source -> 400 "DataSource not found."

Cause 5:
1. CreateGraphqlApi(API_KEY); StartSchemaCreation('schema { query: Query } type Query { widget: String }') and poll to SUCCESS.
2. CreateDataSource(type=NONE); CreateFunction x11 against it.
3. CreateResolver(typeName=Query, fieldName=widget, kind=PIPELINE, pipelineConfig={functions: first 10 ids}).
4. UpdateResolver(same, pipelineConfig={functions: 11 ids}) -> expect 400 BadRequestException "A resolver can only be composed of 10 functions."; the emulator answers 200 and stores 11.

Cause 6:
1. CreateApi(eventConfig=<API_KEY modes>).
2. CreateChannelNamespace(apiId, name=<unique>, subscribeAuthModes/publishAuthModes=[{authType: API_KEY}], tags={'Team':'checkout'}) and keep channelNamespaceArn.
3. TagResource(resourceArn=<channelNamespaceArn>, tags={'Env':'staging'}) -> expect 200; the emulator answers 400 "Invalid input resource arn".
4. (unreachable) TagResource(..., tags={'aws:reserved':'x'}) -> 400 "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)", with ListTagsForResource/GetChannelNamespace/ListChannelNamespaces all still reporting {'Team':'checkout','Env':'staging'}.

Cause 7:
1. CreateGraphqlApi(API_KEY); CreateDataSource(type=NONE).
2. CreateFunction(apiId, name, dataSourceName, runtime={APPSYNC_JS,1.0.0}, code=<exactly 32768 bytes>) -> 200.
3. CreateFunction(... code=<32769 bytes>) -> expect 400 BadRequestException "Code must be 32768 bytes or less"; the emulator returns the generic "1 validation error detected: … at 'code' failed to satisfy constraint …" text.
4. ListFunctions must contain only the accepted function.
