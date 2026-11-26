Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.graphql-query-validation-rejected-before-execution
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.graphql-query-validation-rejected-before-execution|run#13|AssertionError|assert isinstance(locations, list) and locations, (
Issue-Commit: 0d759a8e8fc83ca814d2defa03332b5c846235e3

# AppSync GraphQL data plane: validation errors in the 200 errors envelope carry no `locations`, so a request error never points back into the document

> _Note: this issue was identified against the code base at commit `0d759a8e8fc83ca814d2defa03332b5c846235e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GraphqlRequest

## Description
One net-new root cause.

Cause 1 of 1: a document that parses but fails GraphQL validation is answered correctly (HTTP 200, `data: null`, correct message text, no `errorType`, no `path`), but the error object contains ONLY `message` — the `locations` array that points back into the submitted query document is absent entirely.

AWS: recorded behavior only — the GraphQL data plane is not described by the AppSync Smithy model (smithy_query on the appsync model exposes only the control plane; there is no modeled shape for the /graphql POST response), so the requirement comes from the recording and from the GraphQL over-HTTP envelope AppSync serves. For a request error raised by validation, AppSync answers HTTP 200 with a body whose `data` key is present and null and whose `errors[0]` carries the `message` plus a non-empty `locations` array, each entry an object with integer `line` and `column` members locating the offending selection/variable in the document text. No `errorType` and no `path` are present on a validation error (both of which this emulator already gets right). The fixed text of the message is AWS's validation-rule wording — e.g. `Validation error of type FieldUndefined: Field '<field>' in type '<Type>' is undefined @ '<path>'`, `Variable '<name>' has coerced Null value for NonNull type '<Type>!'`, `Variable '<name>' has an invalid value ...` — where the field name, type name, variable name and document path are values taken from the request, not literals from any one run; the `line`/`column` values are likewise derived from the position of the offending token in the submitted document.

Emulator: the errors array contains a single object with no `locations` member at all. Replay recorded, for the document `query { nope }` against a schema whose Query type has only `echo(id: ID!)`:
  errors[0] == {'message': "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined @ 'nope'"}
where AWS also supplies `locations: [{line: <int>, column: <int>}]`. The check asserts `isinstance(locations, list) and locations` and fails with `AssertionError: undefined field: a validation error must point at a location in the document`. Because the first validation case aborts the check, the two remaining validation classes (unsupplied non-null variable, non-coercible variable value) and the two front-door parse classes (`Unable to parse GraphQL query.` / `Invalid request, \`query\` can't be empty.` as 400 MalformedHttpRequestException with no `data` key) were never exercised in this run; the same missing-`locations` defect is visible on the resolver-error envelope too, but that check is already owned by 20261116T072149Z-appsync-8-net-new-causes-graphql-data-plane.md.

Why: a client that relies on AWS's behavior cannot localize a bad query. Every standard GraphQL client and IDE integration (Amplify, Apollo, graphql-js consumers) reads `errors[].locations[].line/column` to underline the offending token and to map a server-side validation failure back to source; with the member absent these tools report the error with no position, and code that indexes `errors[0]['locations'][0]['line']` raises KeyError/TypeError against the emulator while working against AWS. Tests that assert the full request-error shape also diverge.

Fix: the AppSync GraphQL data-plane request-error path in the appsync service crate — the validator that converts a validation failure into the response envelope must propagate the source position of the offending AST node (and, for variable-coercion failures, the position of the variable definition in the operation) and emit it as a non-empty `locations` array of {line, column} integers alongside `message`. The same envelope builder is used for resolver/field errors, so the position must be threaded through the shared error-envelope serializer rather than special-cased for one rule.

Traces: appsync.graphql-query-validation-rejected-before-execution.

## Evidence
Check appsync.graphql-query-validation-rejected-before-execution, case "undefined field" (`query { nope }`, schema `type Query { echo(id: ID!): String }`):

Expected (AWS): HTTP 200, body `{"data": null, "errors": [{"message": "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined @ 'nope'", "locations": [{"line": <int>, "column": <int>}]}]}` — no `errorType`, no `path`. Fixed text: `Validation error of type FieldUndefined: Field '<field>' in type '<Type>' is undefined @ '<path>'`; variable parts (`nope`, `Query`) come from the request and the API's active schema; `line`/`column` come from the offending token's position in the submitted document.

Actual (emulator): HTTP 200 with the correct message, correct null `data`, correct absence of `errorType`/`path`, but `errors[0] == {'message': "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined @ 'nope'"}` — the `locations` member is missing, so `errors[0].get('locations')` is None.

Assertion: `AssertionError: undefined field: a validation error must point at a location in the document, got {'message': "Validation error of type FieldUndefined: Field 'nope' in type 'Query' is undefined @ 'nope'"}` (check_appsync_graphql_query_validation_rejected_before_execution.py line 197).

## Steps to Reproduce
1. CreateGraphqlApi with authenticationType=API_KEY; StartSchemaCreation with `schema { query: Query } type Query { echo(id: ID!): String }` and poll GetSchemaCreationStatus to SUCCESS; CreateApiKey.
2. CreateDataSource (type=NONE) and CreateResolver for Query.echo with a request template `{"version":"2018-05-29","payload":$util.toJson("echo-${ctx.args.id}")}` and response template `$util.toJson($ctx.result)`.
3. Confirm the control query works: POST the GRAPHQL uri with header `x-api-key: <key>` and body `{"query":"query Q($id: ID!) { echo(id: $id) }","variables":{"id":"abc"}}` -> 200, `data.echo == "echo-abc"`.
4. POST the same endpoint with body `{"query":"query { nope }"}`.
5. Observe HTTP 200 with `data: null` and one error whose only member is `message`; AWS additionally returns `locations: [{line, column}]` with integer line/column. Repeat with `{"query":"query Q($id: ID!) { echo(id: $id) }","variables":{}}` (unsupplied non-null variable) and `variables={"id":{"not":"an ID"}}` (non-coercible value) — both validation classes must also carry `locations`.
