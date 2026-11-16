Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.flush-api-cache-clears-cache appsync.flush-api-cache-immediate-clear appsync.graphql-append-error-partial-data-and-nonnull-propagation appsync.graphql-field-selection-alias-and-fragment-shaping appsync.graphql-pipeline-stash-prev-result-and-before-return appsync.graphql-query-depth-and-resolver-count-limits appsync.graphql-field-level-auth-directive-denies-other-provider appsync.graphql-query-validation-rejected-before-execution appsync.graphql-resolver-error-returns-http-200-with-errors-envelope
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.flush-api-cache-clears-cache|_poll_cache_hit_persists#17|AssertionError|assert last == expected_value, (
Issue-Key: appsync.flush-api-cache-immediate-clear|run#26|AssertionError|assert id1 == id2, (
Issue-Key: appsync.graphql-append-error-partial-data-and-nonnull-propagation|run#22|AssertionError|assert body["data"].get("widget", "MISSING") is None, (
Issue-Key: appsync.graphql-field-selection-alias-and-fragment-shaping|run#15|AssertionError|assert data["widget"] == {"__typename": "Widget", "id": "w-2"}, (
Issue-Key: appsync.graphql-pipeline-stash-prev-result-and-before-return|_compose#18|AssertionError|assert not body.get("errors"), f"unexpected GraphQL errors: {body.get('errors')!r}"
Issue-Key: appsync.graphql-query-depth-and-resolver-count-limits|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: appsync.graphql-field-level-auth-directive-denies-other-provider|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: appsync.graphql-query-validation-rejected-before-execution|run#13|AssertionError|assert isinstance(errors, list) and errors, (
Issue-Key: appsync.graphql-resolver-error-returns-http-200-with-errors-envelope|run#21|AssertionError|assert isinstance(locations, list) and locations, (
Issue-Commit: 4b5ad12ea538b6995117aae35ef3d6dcdbc48855

# appsync: 8 net-new causes — GraphQL data plane does not cache, validate, apply non-null propagation, __typename, pipeline stash/prev.result, depth/resolver limits, field-level @aws_* directives, or error locations

> _Note: this issue was identified against the code base at commit `4b5ad12ea538b6995117aae35ef3d6dcdbc48855`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GraphQL (data-plane POST /graphql)

## Description
Eight independent defects in the emulator's AppSync GraphQL data plane, all reached through GraphqlApi$uris.GRAPHQL on APIs whose schema reached SUCCESS and whose UNIT/PIPELINE resolvers were created over a NONE data source. Each cause is separate and must be closed separately. Related open issues describe neighbouring defects but do NOT carry these checks' replay keys: 20261114T140817Z-appsync-4-net-new-causes-the-graphql-endpoint.md (mentions field-level @aws_* auth being ignored) and 20261114T140817Z-appsync-7-net-new-causes-the-graphql-data.md (VTL execution gaps).

Cause 1 of 8: the API cache never serves a cached response.
AWS: Smithy ApiCache$apiCachingBehavior FULL_REQUEST_CACHING — "All requests from the same user are cached" — with ApiCache$ttl valid 1–3600 s; a second identical query inside the ttl is answered from the cache (the resolver is not re-executed) until FlushApiCache, after which the next identical query re-executes the resolver. Recorded behavior: with a response template of $util.toJson($util.time.nowISO8601()) two identical queries return the IDENTICAL value, and the value changes only after a flush.
Emulator: every request re-executes the resolver. Two identical queries milliseconds apart returned different values ('2026-11-16T07:09:12.954467Z' then '...960661Z'), and a second identical query returned a freshly generated id ('9a06b92e-…' vs 'cf7f2a9c-…'). Nothing is ever cached, so a flush is indistinguishable from no flush.
Why: a client measuring cache effectiveness, or any test that an identical query is served from cache and that FlushApiCache invalidates it, sees uncached behavior; caching configuration becomes a no-op the client cannot detect.
Fix: the GraphQL data-plane request path must consult the API cache record of the API (api-cache state plus the per-resolver cachingConfig) before dispatching a resolver, and FlushApiCache must drop those entries.
Traces: appsync.flush-api-cache-clears-cache, appsync.flush-api-cache-immediate-clear.

Cause 2 of 8: a null from a failed non-null field does not propagate to the nearest nullable parent.
AWS: Smithy UpdateGraphqlApiRequest$queryDepthLimit documents AppSync's result-coercion rule explicitly — "Note that fields can still be set to nullable or non-nullable. If a non-nullable field produces an error, the error will be thrown upwards to the first nullable field available." So for type Widget { id: ID! mandatory: String! } where Widget.mandatory's resolver calls $util.error("mandatory-boom", "MandatoryBoomType"), the 200 envelope carries data.widget == null and exactly one error whose errorType is the resolver's own type and whose path is the full response path of the failing field (['widget','mandatory']).
Emulator: HTTP 200 with data = {'widget': {'id': 'w-1', 'mandatory': None}} — a null was left INSIDE a non-null field and the parent still resolved.
Why: typed clients and codegen'd SDKs treat a non-null field as never null; a null there breaks deserialization in the client rather than surfacing as a nulled parent object.
Fix: result coercion in the GraphQL executor's error handling (propagate the null up to the nearest nullable ancestor while keeping the single error entry).
Traces: appsync.graphql-append-error-partial-data-and-nonnull-propagation.

Cause 3 of 8: __typename resolves to null.
AWS: __typename is a GraphQL meta-field required on every object type and is not part of introspection; 'query { widget(id: "w-2") { __typename id } }' answers data.widget == {"__typename": "Widget", "id": "w-2"} — the concrete schema type name of the object being selected.
Emulator: returned {'__typename': None, 'id': 'w-2'} — the meta-field is included in the shaped response but never resolved.
Why: clients that discriminate unions/interfaces or cache by typename (Apollo and AWS Amplify both do) get a null discriminator and cannot normalize or route results.
Fix: meta-field resolution in the GraphQL executor's selection-set shaping — __typename must be answered from the schema type of the current object, independently of resolvers and of introspectionConfig.
Traces: appsync.graphql-field-selection-alias-and-fragment-shaping.

Cause 4 of 8: a PIPELINE resolver whose function template assigns $ctx.stash fails to render at all.
AWS: a PIPELINE resolver runs the resolver's before template, then each function (a function request template may contain '#set($ctx.stash.tenant = $ctx.args.tenant)' followed by the JSON payload document, and a later function may read '$ctx.prev.result'), then the resolver's after template, which also sees $ctx.stash. The recorded run answers 200 with data.compose == "fn2-saw-one|acme" and no errors; '#return("short-circuited")' in the before template returns that value directly without running either function or the after template.
Emulator: HTTP 200 with data.compose null and one error [{'path': ['compose'], 'message': 'The request mapping template did not render a JSON value: expected value at line 1 column 1'}] — the pipeline never executes. Hypothesis: the VTL renderer does not handle a leading '#set' directive assigning into $ctx.stash (or the resolver's '{}' before template), so the rendered document is empty and then fails JSON parsing.
Why: every pipeline resolver that carries state between functions through $ctx.stash or chains $ctx.prev.result — the normal way to compose functions — fails with a render error instead of resolving, and #return short-circuiting cannot be exercised at all.
Fix: VTL evaluation for pipeline resolvers: #set assignment into the $ctx.stash map, $ctx.prev.result binding between stages, the before/after template split, and #return short-circuit handling.
Traces: appsync.graphql-pipeline-stash-prev-result-and-before-return.

Cause 5 of 8: queryDepthLimit and resolverCountLimit are stored but never enforced on the data plane.
AWS: Smithy UpdateGraphqlApiRequest$queryDepthLimit — "The maximum depth a query can have in a single request… The default value is 0 (or unspecified), which indicates there's no depth limit… This field will produce a limit error if the operation falls out of bounds"; resolverCountLimit bounds resolver invocations the same way. Recorded behavior: the violation is reported INSIDE a 200 GraphQL envelope with exactly one typed error — errorType 'QueryDepthLimitReached' with message 'Query depth limit reached.' (depth) or errorType 'ResolverExecutionLimitReached' with message 'Resolver invocation limit reached.' (count), path [] in both cases (the error is raised against the operation, not a field) — alongside PARTIAL data in which only the field beyond the limit is null and its ancestors still resolved. Both message strings are fixed text; nothing of the request is interpolated.
Emulator: with queryDepthLimit=2 persisted on the API, 'query { top { nested { leaf } } }' resolves fully with no errors array; the check polled for an errors array until the suite killed it at 60 s. resolverCountLimit=1 is likewise ignored.
Why: a client relying on these limits as a guard against expensive queries sees no limit error and no cut result, so the configured protection silently does not exist.
Fix: the data-plane executor must read the API's queryDepthLimit/resolverCountLimit before execution and emit the typed limit error plus partial data.
Traces: appsync.graphql-query-depth-and-resolver-count-limits.

Cause 6 of 8: field-level @aws_api_key / @aws_iam directives are not enforced.
AWS: on an API whose primary auth is AWS_IAM with API_KEY as an additional provider, a field marked @aws_iam is denied to an API-key caller and vice versa — per FIELD, inside a 200 envelope: the caller's own field resolves, the other field is present-and-null, and exactly one error is returned with errorType 'Unauthorized', path naming only the denied field, and a message that names that field. (An unknown API key remains a transport-level 401 — the two refusals must not collapse into each other.)
Emulator: the query 'query { keyOnly iamOnly }' never produced an errors array for either caller; the check polled for the partial data+errors envelope until the suite killed it at 60 s, i.e. both fields are served to whichever provider authenticated.
Why: data AWS denies is handed to the caller — an API-key client reads @aws_iam-only fields and an IAM client reads @aws_api_key-only fields, so per-field authorization is absent.
Fix: field-level auth directives must be compiled from the SDL and evaluated per field against the request's authentication provider in the executor, emitting the Unauthorized field error.
Traces: appsync.graphql-field-level-auth-directive-denies-other-provider.

Cause 7 of 8: documents that fail GraphQL validation are executed instead of refused.
AWS: validation runs BEFORE execution. A parsed-but-invalid document is answered HTTP 200 with the data key PRESENT and null, nothing resolved, and a non-empty errors array whose first entry has no 'path', no 'errorType' (a validation error is not a typed AppSync data-plane error) and a 'locations' list of integer line/column pairs pointing into the document; the message identifies the rule and names the offending symbol — e.g. "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined" for an undefined field, a "coerced Null value for NonNull type 'ID!'" message for an unsupplied non-null variable, and a "has an invalid value" message for a variable whose value cannot coerce. The fixed text is the rule phrase; the symbol names ('nope', 'id', 'ID!') come from the request document.
Emulator: 'query { nope }' was answered 200 with {'data': {'nope': None}} and NO errors array — an undefined field was executed to null instead of refused by validation.
Why: a client gets a successful-looking partial result for a document AWS rejects, so schema drift and client bugs go unreported and the data key cannot be used to tell validation failure from a resolved null.
Fix: a validation pass over the parsed document against the API's compiled schema, ahead of execution, emitting the document-located request errors described above.
Traces: appsync.graphql-query-validation-rejected-before-execution.

Cause 8 of 8: resolver errors carry no 'locations'.
AWS: a resolver that calls $util.error("resolver-boom", "ResolverBoomType") is reported in a 200 envelope with the message and errorType surfaced verbatim, path == the failing response path (['failing']), the sibling field still resolved, AND a 'locations' list whose entries carry integer line and column pointing back into the query document at the failing field.
Emulator: errors[0].locations was absent (None) while the rest of the entry was right.
Why: clients and tooling map a GraphQL error back to the offending selection through locations; with it missing an error cannot be attributed to a position in the document.
Fix: the executor must carry each selection's document position into the error entries it serializes.
Traces: appsync.graphql-resolver-error-returns-http-200-with-errors-envelope.

## Evidence
Cause 1 — expected: two identical queries inside the cache ttl return the identical value until FlushApiCache. Actual: AssertionError "cached query result changed unexpectedly before any flush (expected '2026-11-16T07:09:12.954467Z', got '2026-11-16T07:09:12.960661Z')"; and AssertionError "expected the second identical query to be served from cache (same id), got '9a06b92e-d168-42de-910e-f33283ea2ed5' vs 'cf7f2a9c-7c87-4acb-85ed-b85fc1d4c62a'" (the timestamps/ids are per-run values; the requirement is that the two values are equal).
Cause 2 — expected data.widget == null with errors[0].path == ['widget','mandatory'] and errorType 'MandatoryBoomType'. Actual: AssertionError "expected the null from the failed non-null field to propagate up to the nullable parent (data.widget == null), got {'widget': {'id': 'w-1', 'mandatory': None}}".
Cause 3 — expected data.widget == {'__typename': 'Widget', 'id': 'w-2'}. Actual: AssertionError "expected __typename to resolve to the schema type name alongside the selected field, got {'__typename': None, 'id': 'w-2'}".
Cause 4 — expected 200, no errors, data.compose == 'fn2-saw-one|acme'. Actual: AssertionError "unexpected GraphQL errors: [{'path': ['compose'], 'message': 'The request mapping template did not render a JSON value: expected value at line 1 column 1'}]".
Cause 5 — expected 200 + one error {errorType: 'QueryDepthLimitReached', message: 'Query depth limit reached.', path: []} with data {'top': {'nested': {'leaf': None}}} under queryDepthLimit=2 (and 'ResolverExecutionLimitReached' / 'Resolver invocation limit reached.' under resolverCountLimit=1). Actual: the deep query keeps resolving with no errors; the check polled for an errors array and the suite reported "Check timed out after 60.0s".
Cause 6 — expected 200 with data {'keyOnly': 'key-only-value', 'iamOnly': None} plus one error {errorType: 'Unauthorized', path: ['iamOnly'], message naming iamOnly} for the API-key caller, and the mirror for the SigV4 caller. Actual: no errors envelope is ever produced; "Check timed out after 60.0s" while polling for partial data + errors.
Cause 7 — expected 200, data present and null, non-empty errors with locations and "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined". Actual: AssertionError "undefined field: expected a non-empty errors array, got {'data': {'nope': None}}".
Cause 8 — expected errors[0].locations == [{line: <int>, column: <int>}] pointing at the failing field. Actual: AssertionError "expected errors[0].locations to point back into the query document, got None".

## Steps to Reproduce
For every cause: CreateGraphqlApi (API_KEY unless noted), StartSchemaCreation with the check's SDL, poll GetSchemaCreationStatus to SUCCESS, CreateApiKey, CreateDataSource type=NONE, create the resolvers below, then POST {"query": ...} with header x-api-key to GraphqlApi$uris.GRAPHQL.
1. Resolver Query.ping with responseMappingTemplate '$util.toJson($util.time.nowISO8601())', CreateApiCache (FULL_REQUEST_CACHING, ttl>=1) and wait AVAILABLE; issue the same query twice -> the two values must be equal; FlushApiCache -> the next value must differ. Also: issue one query twice and compare the generated id.
2. SDL 'type Widget { id: ID! mandatory: String! }'; Query.widget payload {"id":"w-1"}; Widget.mandatory response template '$util.error("mandatory-boom", "MandatoryBoomType")'; query '{ widget { id mandatory } }' -> data.widget must be null, errors[0].path == ['widget','mandatory'].
3. Query.widget payload {id,name,colour}; query '{ widget(id: "w-2") { __typename id } }' -> {'__typename': 'Widget', 'id': 'w-2'}.
4. CreateFunction #1 requestMappingTemplate '#set($ctx.stash.tenant = $ctx.args.tenant)\n{"version":"2018-05-29","payload":{}}' response '$util.toJson("one")'; CreateFunction #2 response '$util.toJson("fn2-saw-${ctx.prev.result}")'; PIPELINE resolver Mutation.compose with before template '{}' and after template '$util.toJson("${ctx.prev.result}|${ctx.stash.tenant}")'; run 'mutation M($t: String!) { compose(tenant: $t) }' with t=acme -> data.compose == 'fn2-saw-one|acme'. Then UpdateResolver before template '#return("short-circuited")' -> data.compose == 'short-circuited'.
5. UpdateGraphqlApi(queryDepthLimit=2, resolverCountLimit=0) then query '{ top { nested { leaf } } }'; then (0,1) and '{ top { name } sibling }'.
6. CreateGraphqlApi authenticationType=AWS_IAM with additionalAuthenticationProviders=[{API_KEY}]; SDL 'type Query { keyOnly: String @aws_api_key iamOnly: String @aws_iam }'; resolvers for both fields; query '{ keyOnly iamOnly }' once with x-api-key and once SigV4-signed for service 'appsync'.
7. SDL 'type Query { echo(id: ID!): String }'; POST 'query { nope }' (also 'query Q($id: ID!) { echo(id: $id) }' with variables {} and with {"id": {"not": "an ID"}}).
8. Resolvers Query.ok (payload 'ok-value') and Query.failing (response '$util.error("resolver-boom", "ResolverBoomType")'); query '{ ok failing }' -> errors[0].locations must carry integer line/column.
