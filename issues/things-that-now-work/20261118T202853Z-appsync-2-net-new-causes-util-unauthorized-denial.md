Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.resolver-util-unauthorized-denies-field-but-serves-sibling appsync.schema-replacement-swaps-the-served-schema-atomically
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.resolver-util-unauthorized-denies-field-but-serves-sibling|run#30|AssertionError|assert err.get("message") == "Not Authorized to access secret on type String", (
Issue-Key: appsync.schema-replacement-swaps-the-served-schema-atomically|run#28|AssertionError|assert gone.get("data", "missing") is None, (
Issue-Commit: 499595d2d8b363c80b553c7cad46d3a24d68494f

# appsync: 2 net-new causes — util.unauthorized() denial message is "Unauthorized" instead of "Not Authorized to access <field> on type <Type>", and a field withdrawn by a schema replacement is still executed to null instead of failing document validation

> _Note: this issue was identified against the code base at commit `499595d2d8b363c80b553c7cad46d3a24d68494f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaCreation

## Description
Two net-new root causes in the AppSync GraphQL data plane, both surviving the suite's already-filed/owned markings.

Cause 1 of 2: a resolver calling util.unauthorized() surfaces the wrong error message.
AWS: recorded behavior only (the GraphQL data plane is not described by the Smithy model; smithy_query on the appsync model covers only the control-plane operations such as CreateResolver/StartSchemaCreation, and carries nothing about the GraphQL error envelope). An APPSYNC_JS UNIT resolver whose request() calls util.unauthorized() denies ONLY its field inside an HTTP 200 GraphQL envelope: the field is present and null, its response() handler never runs, a sibling field in the same document still resolves, and errors[] carries exactly one entry with errorType "Unauthorized", path ["<fieldName>"] and message "Not Authorized to access <fieldName> on type <typeName>". In that message the fixed text is "Not Authorized to access " and " on type "; the variable parts are the denied field's name, taken from the request document, and the type name, taken from the served schema (in the check's schema that renders as field `secret` and type `String`).
Emulator: status, errorType, path, the nulled field and the sibling field are all correct, but the single error's message is the bare string "Unauthorized" — no field name and no type name. Replay output: `AssertionError: unexpected denial message 'Unauthorized'`.
Why: a client that matches or logs AppSync's denial text (error-mapping middleware, log scrapers, UI copy derived from the GraphQL error message) cannot tell WHICH field was denied, and any assertion on AWS's wording fails even though the shape is right.
Fix: the APPSYNC_JS runtime's util.unauthorized() implementation / the GraphQL data-plane error serializer that builds the errors[] entry — it must compose the message from the resolved field and its type rather than emitting the errorType as the message.
Traces: appsync.resolver-util-unauthorized-denies-field-but-serves-sibling.

Cause 2 of 2: after a schema replacement, a field the new schema removed is still executed and nulled instead of failing document validation.
AWS: recorded behavior only (data plane, not modeled). A second StartSchemaCreation replaces the served schema atomically. A query naming a field that exists only in the OLD schema is then rejected in the validation phase, before any execution: HTTP 200, the body carries "data" present and set to null (a validation error resolves NOTHING), and errors[] carries a validation error whose message begins "Validation error of type FieldUndefined" and names the removed field — AWS's wording is "Validation error of type FieldUndefined: Field '<fieldName>' in type '<TypeName>' is undefined @ '<path>'", where the fixed text is "Validation error of type FieldUndefined: Field '", "' in type '", "' is undefined @ '" and the variable parts are the field name, its parent type name and the response path, all taken from the request document and the served schema.
Emulator: the same query answers HTTP 200 with `{"data": {"alpha": null}}` — the withdrawn field is admitted, executed to null, and NO errors array is produced. Replay output: `AssertionError: a validation error resolves NOTHING, so data must be present and null: got {'alpha': None} in {'data': {'alpha': None}}`. The added field resolves, so the swap itself reached the endpoint; what is missing is validation of the parsed document against the currently served schema.
Why: a client cannot distinguish "this field was removed from the schema" from "this field resolved to null". Schema-withdrawal is invisible: generated clients and codegen tests keep querying a field the API no longer declares and silently read nulls instead of the FieldUndefined validation error AWS returns, and partial-failure handling that keys off errors[] never fires.
Fix: the GraphQL data-plane request pipeline — run document validation against the schema currently served by the API (as replaced by the latest StartSchemaCreation) before execution, emit data:null plus the FieldUndefined validation error, and resolve nothing. The two open issues 20261116T072149Z-appsync-8-net-new-causes-graphql-data-plane.md ("data plane does not ... validate ...") and 20261114T191235Z-appsync-4-net-new-causes-cloudformation-deployed-vtl.md ("query validation does not reject undeclared fields") describe nearby validation gaps, but neither carries this check's replay key and neither covers the post-replacement schema state exercised here.
Traces: appsync.schema-replacement-swaps-the-served-schema-atomically.

## Evidence
Cause 1 — check appsync.resolver-util-unauthorized-denies-field-but-serves-sibling:
  Expected (AWS): HTTP 200, data={"secret": null, "open": "open-ok"}, errors=[{errorType: "Unauthorized", path: ["secret"], message: "Not Authorized to access secret on type String"}] (message template: "Not Authorized to access <fieldName> on type <typeName>").
  Actual (emulator): HTTP 200 with the correct errorType/path/nulling, but message == "Unauthorized".
  Replay detail: `check_appsync_resolver_util_unauthorized_denies_field_but_serves_sibling.py line 111: AssertionError: unexpected denial message 'Unauthorized'`.

Cause 2 — check appsync.schema-replacement-swaps-the-served-schema-atomically:
  Expected (AWS) for `query Q($m: String!) { alpha(msg: $m) }` after the v2 schema (which drops `alpha`) is created: HTTP 200, body `{"data": null, "errors": [{"message": "Validation error of type FieldUndefined: Field 'alpha' in type 'Query' is undefined @ 'alpha'", ...}]}` (names the removed field; nothing resolved).
  Actual (emulator): HTTP 200, body `{"data": {"alpha": null}}` — field admitted and nulled, no errors array.
  Replay detail: `check_appsync_schema_replacement_swaps_the_served_schema_atomically.py line 192: AssertionError: a validation error resolves NOTHING, so data must be present and null: got {'alpha': None} in {'data': {'alpha': None}}`.
  Earlier legs of the same check passed: the v1 field resolved, GetSchemaCreationStatus stayed terminal across three reads, and the v2-only field `beta` resolved — so the swap reached the endpoint and only validation of the withdrawn field diverges.

## Steps to Reproduce
Cause 1:
1. CreateGraphqlApi (authenticationType=API_KEY); StartSchemaCreation with `schema { query: Query } type Query { secret: String open: String }` and poll GetSchemaCreationStatus to SUCCESS.
2. CreateDataSource type=NONE. CreateResolver for Query.secret (kind=UNIT, runtime APPSYNC_JS 1.0.0) whose code is `export function request(ctx) { util.unauthorized(); }` / `export function response(ctx) { return 'leaked'; }`, and CreateResolver for Query.open returning a value from response().
3. CreateApiKey, then POST `{"query":"query { secret open }"}` with x-api-key to uris.GRAPHQL.
4. Inspect errors[0].message: emulator gives "Unauthorized"; AWS gives "Not Authorized to access secret on type String".

Cause 2:
1. CreateGraphqlApi (API_KEY); StartSchemaCreation with v1 `type Query { keep(msg: String!): String  alpha(msg: String!): String }`; poll to SUCCESS.
2. CreateDataSource type=NONE; CreateResolver for Query.keep and Query.alpha (APPSYNC_JS, runtime.earlyReturn(ctx.args.msg)); CreateApiKey; confirm `alpha` resolves over uris.GRAPHQL.
3. DeleteResolver Query.alpha, then StartSchemaCreation with v2 `type Query { keep(msg: String!): String  beta(msg: String!): String }`; poll to SUCCESS; CreateResolver for Query.beta; confirm `beta` resolves (proves v2 is served).
4. POST `query Q($m: String!) { alpha(msg: $m) }`: emulator answers 200 `{"data":{"alpha":null}}`; AWS answers 200 with data:null and a "Validation error of type FieldUndefined" error naming `alpha`.
