Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-schema-creation-introspection
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-schema-creation-introspection
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|GetSchemaCreationStatus|2xx->2xx|$.details

# AppSync GetSchemaCreationStatus omits `details` for an API no schema creation has ever run for (NOT_APPLICABLE)

## Operation
GetSchemaCreationStatus

## Description
For a GraphQL API that has never had a `StartSchemaCreation`, this service answers 200 with
`status` NOT_APPLICABLE and NO `details` member at all. AWS answers the same 200 and the same
NOT_APPLICABLE status, but its body carries a non-empty `details` string
(`GetSchemaCreationStatusResponse$details`, "Detailed information about the status of the schema
creation operation" — `smithy_query(query="operation", operation="GetSchemaCreationStatus")`).
A client that reads the detail beside the status therefore gets nothing where AWS explains why no
creation state applies.

This is the residual part of the cluster
`20261109T022338Z-appsync-schema-creation-plane-diverges-success-detail-text.md` filed under its
Cause 1. That issue's Cause 1 — the SUCCESS detail, which must report the number of types the
submitted document defines — is implemented and its own recorded step
(`appsync-eval-schema-creation-introspection#task#3`) passes. The second step the issue listed under
the same replay key, `appsync-schema-creation-introspection#task#3`, turned out NOT to be the
SUCCESS projection: it reads an API with no creation at all.

How that was established (the recording withholds its values): replaying the unit while renaming one
status constant at a time in `services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/state/schema_creation.rs`
showed that
* with `SCHEMA_SUCCESS` renamed, this step's `$.status` kept matching while
  `appsync-eval-schema-creation-introspection#task#3` began to diverge on `$.status`; and
* with `SCHEMA_NOT_APPLICABLE` renamed, this step's `$.status` began to diverge.
So the recorded status for this step is NOT_APPLICABLE, and the one diverging key is `$.details`.
Answering the member absent diverges, and so does answering it as the empty string, so AWS sends a
non-empty sentence there.

## What is needed
The AWS wording for that sentence. It is not in the issue report, the model documents only the
member's purpose, and the replay withholds recorded values; 96 candidate wordings were probed
through the replay (including the service's own recorded diagnostics for an empty schema document)
and none matched. No sentence was invented for it: an emulator-authored constant in `details` is the
very defect the parent issue reports.

## Steps to Reproduce
1. `CreateGraphqlApi(name=<unique>, authenticationType='API_KEY')` and keep its `apiId`.
2. `GET /v1/apis/{apiId}/schemacreation` (GetSchemaCreationStatus) WITHOUT any StartSchemaCreation.
3. Expect 200 with `status` NOT_APPLICABLE and a non-empty `details`; the emulator answers 200 with
   `status` NOT_APPLICABLE and no `details` member.

## Fix
`GraphqlApiRecord::schema_creation_status` in
`services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/state/schema_creation.rs`
returns `None` for the detail when `self.schema` is `None`. Once the recorded sentence for the
NOT_APPLICABLE state is known, report it there (and assert it in
`tests/all/schema_creation_plane_refusals.rs`, beside the SUCCESS-detail table).

## What was done, and what is left
Fixed: the member is no longer omitted. `GraphqlApiRecord::schema_creation_status` now reports a
detail for this state as it does for every other one — the status and the detail are the two members
of one report — and `tests/all/schema_creation_status_detail.rs` holds every `SchemaStatus` this
service reports to coming with its own non-empty detail, over a table of API types, documents and
read counts.

Left, and PROVISIONAL: the sentence itself. `SCHEMA_NOT_APPLICABLE_DETAIL` is an authored wording,
documented as chosen on the constant beside `SCHEMA_PROCESSING_DETAIL`, because no source available
here states AWS's: the model documents only the member's purpose, and the recorded value is withheld
(see "What is needed" above). The replay key `appsync|GetSchemaCreationStatus|2xx->2xx|$.details`
therefore still diverges on the WORDING of this one state. Should the recorded sentence become
known, replace that constant's value — nothing else has to change, and the table test above pins
everything around it.

