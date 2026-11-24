Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-schema-creation-introspection
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-schema-creation-introspection
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|GetSchemaCreationStatus|2xx->2xx|$.details
Issue-Commit: f1ef109f453fb378a4dbe7e2cc166b7782add00e

# AppSync GetSchemaCreationStatus reports a static details string instead of AWS's apiId-bearing "No schema creation is ongoing" text

> _Note: this issue was identified against the code base at commit `f1ef109f453fb378a4dbe7e2cc166b7782add00e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSchemaCreationStatus

## Description
Cause 1 of 1: GetSchemaCreationStatus on a GraphQL API that has no schema-creation operation in flight returns the wrong `details` string.

AWS: Smithy model `GetSchemaCreationStatusResponse` (operation GetSchemaCreationStatus, GET /v1/apis/{apiId}/schemacreation, 200) declares `status: SchemaStatus` and `details: String`, documented as "Detailed information about the status of the schema creation operation." The model does not enumerate the string's wording, so for the actual text this is recorded behavior only: the recording shows AWS answering 200 with `details` = "No schema creation is ongoing in GraphQL API <apiId>". The fixed text of that message is "No schema creation is ongoing in GraphQL API "; its single variable part is the API id, taken from the request's `apiId` httpLabel (the recording's `or4hi6rs6bs3tk4cmtwkjj2bor` is one run's value filling that slot, not part of the template). `status` is served correctly and is not part of this divergence.

Emulator: answers the same 200 but with a constant, API-agnostic string, `details` = "No schema creation has been performed for this API." — different fixed text and no apiId interpolation at all.

Why: a client that reads `details` to report or log which API has no schema-creation activity loses the API identity entirely, and any client or test that matches AWS's "No schema creation is ongoing in GraphQL API ..." phrasing (for example to distinguish "never started" from an in-flight/failed creation) does not match against the emulator. The wording is also semantically different: AWS states that no creation is *ongoing*, while the emulator asserts none has ever been *performed* — a claim AWS's message does not make and which is wrong for an API whose creation already completed.

Fix: the GetSchemaCreationStatus handler's response projection for appsync — the branch that builds `details` when the API has no active schema-creation record must emit the AWS fixed text with the request's `apiId` interpolated, rather than a hardcoded constant.

Traces: appsync-schema-creation-introspection#task#3 (the only member of this cluster).

## Evidence
Fixture appsync-schema-creation-introspection, step task#3, operation GetSchemaCreationStatus, service appsync.
Verdict: mismatch. Expected status 200, actual status 200 (status code agrees).
Replay detail:
  $.details: value_mismatch (expected 'No schema creation is ongoing in GraphQL API or4hi6rs6bs3tk4cmtwkjj2bor', got 'No schema creation has been performed for this API.')
affected_count: 1 (cluster has no other members).
Fixed text expected: "No schema creation is ongoing in GraphQL API "; variable part: the `apiId` from the request path. Emulator text is entirely constant.
No existing open issue carries this replay key — the other 22 appsync divergences in this replay are owned by other open issues (e.g. 20261029T200818Z-appsync-source-api-association-projection-and-merge-status.md, 20261122T111346Z-appsync-response-document-fidelity-getintrospectionschema-omits-schema-directives.md); none of them covers GetSchemaCreationStatus.details.

## Steps to Reproduce
1. Replay fixture appsync-schema-creation-introspection against the emulator (trace set simparity-wire-aws-v1); the divergence is at step task#3.
2. Equivalently, by hand: CreateGraphqlApi to obtain an apiId, then without calling StartSchemaCreation issue GET /v1/apis/{apiId}/schemacreation (GetSchemaCreationStatus).
3. Observe the 200 response body's `details` member: the emulator returns the constant 'No schema creation has been performed for this API.' where AWS returns 'No schema creation is ongoing in GraphQL API ' followed by the requested apiId.
