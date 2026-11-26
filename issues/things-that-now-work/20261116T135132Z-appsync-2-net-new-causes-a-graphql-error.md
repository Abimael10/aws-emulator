Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.graphql-resolver-util-error-returns-graphql-error appsync.domain-api-association-lifecycle
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.graphql-resolver-util-error-returns-graphql-error|run#14|KeyError|assert errors[0]["errorType"] == "CustomError", errors[0]
Issue-Key: appsync.domain-api-association-lifecycle|_create_domain_name_expect_rejection#8|Timeout|time.sleep(interval_s)
Issue-Commit: b3a98b26c8bf2cb2bdcd9c0a5c1e0e4aad254ca8

# appsync: 2 net-new causes — a GraphQL error raised by util.error(msg, type) carries no errorType, and the custom-domain / API-association lifecycle never answers (hangs to the 60 s cap)

> _Note: this issue was identified against the code base at commit `b3a98b26c8bf2cb2bdcd9c0a5c1e0e4aad254ca8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomainName

## Description
Two distinct, net-new root causes in the appsync emulator, both grounded in behavioral checks that are neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 2: a GraphQL error raised by `util.error(message, errorType)` in an APPSYNC_JS resolver is serialized WITHOUT the `errorType` member.
Failing check id: appsync.graphql-resolver-util-error-returns-graphql-error
AWS: recorded behavior only for the data-plane envelope (the GraphQL response body is not a Smithy shape; Smithy: CreateResolver's runtime APPSYNC_JS/1.0.0 and `code` are the modeled surface). AppSync's documented utility `util.error(message, errorType)` aborts the resolver and appends one entry to the response's top-level `errors` array; HTTP status stays 200, the field's value in `data` is null, and that entry carries, in addition to `message`, `path` and `locations`, an `errorType` member whose value is EXACTLY the second argument given to util.error — the fixed text of the entry is the member names, the variable parts (`message`, `errorType`) are the values the resolver passed. In this check's shape the mutation `Mutation.fail(msg)` calls util.error(ctx.args.msg, 'CustomError'), so `errors[0].errorType` must be the string the resolver supplied ('CustomError' here) and `errors[0].message` the message it supplied.
Emulator: the response is otherwise right — status 200, `data.fail` is null, and `errors[0].message` equals the message the resolver passed (that assertion passed) — but `errors[0]` has NO `errorType` key at all, so indexing it raises `KeyError: 'errorType'`. The second argument of util.error is dropped on the floor.
Why: clients and SDK/Amplify error handling branch on `errorType` (it is how an application distinguishes an authorization denial, a conflict, and a resolver-authored custom error from one another). With the member absent, every util.error-raised failure looks untyped: retry/认 classification code sees `undefined`, custom error taxonomies collapse into one bucket, and code that reads error.errorType throws on the emulator where it works against AWS.
Fix: the GraphQL data plane's error-envelope serializer for the APPSYNC_JS resolver runtime — the path that converts a sandbox `util.error(message, errorType[, data, errorInfo])` abort into an `errors[]` entry must carry errorType (and the optional data/errorInfo members) through, not just message.
Related-but-not-covering: 20261114T022837Z-appsync-cloudwatch-appsync-graphql-errors-omit-errortype-and.md records the same omission under the appsync__cloudwatch service token bundled with missing AWS/AppSync metric series; it does not carry this check's replay key and is a different service token, so this cause is filed here.
Traces: appsync.graphql-resolver-util-error-returns-graphql-error (error: KeyError: 'errorType' at `assert errors[0]["errorType"] == "CustomError"`).

Cause 2 of 2: the custom-domain-name / API-association lifecycle never produces a response — the whole leg hangs until the harness's 60 s cap.
Failing check id: appsync.domain-api-association-lifecycle
AWS: Smithy: CreateDomainName (POST /v1/domainnames, required domainName @length 1..253 @pattern ^(\*[\w\d-]*\.)?([\w\d-]+\.)+[\w\d-]+$, required certificateArn) models errors AccessDeniedException(403), BadRequestException(400), InternalFailureException(500); GetDomainName (GET /v1/domainnames/{domainName}) and AssociateApi (POST /v1/domainnames/{domainName}/apiassociation) additionally model NotFoundException(404). AWS refuses a certificate that is not chained to a publicly trusted CA (an imported leaf signed by a throwaway root) with 400 BadRequestException and the fixed message "Certificate is invalid." — the text is fixed, with no request-derived parts. Because the domain was never created, GetDomainName on it then answers 404 NotFoundException with message "Domain name not found", the domain does not appear in ListDomainNames' domainNameConfigs, and AssociateApi on it answers 404 NotFoundException with message "Domain name not found." (note AWS's wording differs between the two: GetDomainName has no trailing period, AssociateApi does). Every one of those is a prompt refusal, not a long-running operation.
Emulator: no response is produced for the lifecycle at all — the check reaches the harness's 60 s cap and is reported "Check timed out after 60.0s", so none of the three refusals above can be observed. The check's own retry window for a transient certificate/throttling rejection is 80 s, i.e. the emulator either answers nothing or keeps answering a retryable-looking certificate rejection forever instead of settling on the terminal 400/404 pair; either way the call never completes.
Why: a client doing custom-domain setup gets a hang instead of an answer. Error handling that depends on a bounded refusal (create → inspect → associate) never runs, SDK retry/timeouts fire with no server verdict, and infrastructure tooling (CDK/CFN custom-domain wiring) stalls rather than failing fast with the certificate diagnosis a user can act on.
Fix: the CreateDomainName handler and its certificate validation, plus the GetDomainName / ListDomainNames / AssociateApi handlers and the domain-name state they read, in the appsync service crate — the create path must terminate with the modeled BadRequestException and must not record a domain, and the read/associate paths must then refuse with the modeled NotFoundException.
Related-but-not-covering: 20261025T182150Z-appsync-createdomainname-answers-501-notimplemented-instead-of-rejecting.md describes this operation answering 501 NotImplemented, and 20261111T065707Z-appsync-3-net-new-causes-graphqlapi-uris-dns.md bundles a CreateDomainName acceptance cause; neither carries this check's replay key and the observed behavior is now a hang rather than a 501, so it is filed here.
Traces: appsync.domain-api-association-lifecycle (mismatch: "Check timed out after 60.0s").

## Evidence
Cause 1 — appsync.graphql-resolver-util-error-returns-graphql-error
Expected (AWS): HTTP 200; body data.fail == null; len(errors) == 1; errors[0].message == the message util.error was given; errors[0].errorType == the errorType util.error was given (the check uses 'CustomError').
Actual (emulator): HTTP 200 with data.fail == null and errors[0].message correct, but errors[0] has no 'errorType' member — the check fails with:
  File "check_appsync_graphql_resolver_util_error_returns_graphql_error.py", line 58, in run
    assert errors[0]["errorType"] == "CustomError", errors[0]
  KeyError: 'errorType'

Cause 2 — appsync.domain-api-association-lifecycle
Expected (AWS): CreateDomainName(domainName=<made-up .com name>, certificateArn=<imported leaf signed by a throwaway root CA, in us-east-1>) -> 400 BadRequestException, Message exactly "Certificate is invalid."; then GetDomainName(domainName=<same>) -> 404 NotFoundException, Message "Domain name not found"; ListDomainNames' domainNameConfigs does NOT contain that domainName; AssociateApi(domainName=<same>, apiId=<a freshly created API_KEY GraphQL API>) -> 404 NotFoundException, Message "Domain name not found."
Actual (emulator): no verdict is ever returned — the replay reports "Error: Check timed out after 60.0s", so none of the four expectations is reachable. The positive control (CreateGraphqlApi returning an apiId) is part of the same body, so the hang is in the domain-name leg.

## Steps to Reproduce
Cause 1:
1. appsync.CreateGraphqlApi(authenticationType=API_KEY); StartSchemaCreation with `type Query { noop: String } type Mutation { fail(msg: String): String } schema { query: Query mutation: Mutation }` and poll GetSchemaCreationStatus to SUCCESS/ACTIVE.
2. CreateDataSource(name="noneDs", type="NONE").
3. CreateResolver(typeName="Mutation", fieldName="fail", kind="UNIT", dataSourceName="noneDs", runtime={APPSYNC_JS,1.0.0}, code=`export function request(ctx){ util.error(ctx.args.msg, 'CustomError'); return {}; } export function response(ctx){ return ctx.result; }`).
4. CreateApiKey, then POST to uris.GRAPHQL with header x-api-key: `mutation Fail($m: String) { fail(msg: $m) }`, variables {"m": "boom-custom"}.
5. Observe the 200 body: errors[0].message is "boom-custom" but there is no errors[0].errorType (AWS returns errorType == "CustomError").

Cause 2:
1. Import into ACM (us-east-1) a leaf certificate for a made-up <token>.com name, signed by a throwaway root CA.
2. appsync.CreateGraphqlApi(authenticationType=API_KEY) as a positive control (returns an apiId).
3. appsync.CreateDomainName(domainName=<token>.com, certificateArn=<the imported cert>) — AWS answers 400 BadRequestException "Certificate is invalid."; the emulator never settles and the check is killed at the 60 s cap.
4. (unreachable on the emulator) GetDomainName and AssociateApi on that domain must each answer 404 NotFoundException ("Domain name not found" / "Domain name not found."), and ListDomainNames must not list it.
