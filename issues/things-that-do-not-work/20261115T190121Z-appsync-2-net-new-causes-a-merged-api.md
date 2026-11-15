Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.deploy-appsync-3-cfn-merged-api-auto-merge-serves-source-update appsync.deploy-appsync-3-cfn-merged-api-manual-merge-withholds-update-until-merge appsync.deploy-appsync-3-cfn-merged-api-disassociation-withdraws-source-fields appsync.deploy-appsync-3-cfn-merged-endpoint-source-api-key-rejected appsync.deploy-appsync-3-cfn-association-to-non-merged-api-rejected appsync.deploy-appsync-3-cfn-merged-role-without-source-graphql-denied
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.deploy-appsync-3-cfn-merged-api-auto-merge-serves-source-update|run#22|AssertionError|assert body["data"]["greeting"] == first_literal, (
Issue-Key: appsync.deploy-appsync-3-cfn-merged-api-manual-merge-withholds-update-until-merge|run#18|AssertionError|assert body["data"]["greeting"] == first_literal, f"merged endpoint returned {body!r}"
Issue-Key: appsync.deploy-appsync-3-cfn-merged-api-disassociation-withdraws-source-fields|run#15|AssertionError|assert body["data"]["greeting"] == first_literal, f"merged endpoint returned {body!r}"
Issue-Key: appsync.deploy-appsync-3-cfn-merged-endpoint-source-api-key-rejected|run#15|AssertionError|assert body["data"]["greeting"] == first_literal, f"merged endpoint returned {body!r}"
Issue-Key: appsync.deploy-appsync-3-cfn-association-to-non-merged-api-rejected|run#34|AssertionError|assert body["data"] == {"greeting": first_literal}, (
Issue-Key: appsync.deploy-appsync-3-cfn-merged-role-without-source-graphql-denied|run#20|AssertionError|assert len(errors) == 1 and errors[0].get("errorType") == "Unauthorized", (
Issue-Commit: 0a5e4e1aa59af41a3d3c27e0082d824b0afa7ca4

# appsync: 2 net-new causes — a MERGED API's GraphQL endpoint resolves merged source fields to null without running the source resolver, and the merged execution role is never checked for appsync:SourceGraphQL

> _Note: this issue was identified against the code base at commit `0a5e4e1aa59af41a3d3c27e0082d824b0afa7ca4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSchemaMerge

## Description
Two distinct net-new root causes found replaying the CloudFormation-deployed merged-API (SourceApiAssociation) behavioral checks of the appsync suite. Neither of the six failing checks below is marked ALREADY FILED or OWNED in the replay report, and no open issue carries their replay keys. Related open issues are named per cause.

Cause 1 of 2: the GraphQL endpoint of a MERGED API never executes the resolver of an associated source API — the merged field answers null with no error at all.
AWS: Smithy — GraphqlApi$apiType MERGED, and GraphqlApi$mergedApiExecutionRoleArn is documented as "The Identity and Access Management service role ARN for a merged API. The AppSync service assumes this role on behalf of the Merged API to validate access to source APIs at runtime and to prompt the AUTO_MERGE to update the merged API endpoint with the source API changes automatically." SourceApiAssociationConfig$mergeType (MANUAL_MERGE default, AUTO_MERGE) describes the merged API being kept in step with the source APIs. Recorded behavior: once the association reaches sourceApiAssociationStatus MERGE_SUCCESS, a query of a merged root field POSTed to the MERGED API's uris.GRAPHQL (authorized with the merged API's own API key) is answered HTTP 200 with the value the SOURCE API's resolver returns under `data` and NO `errors` member — the merged endpoint runs the source API's resolver as the merged API's execution role. A field that is in no source API's schema is instead refused as a validation error naming the undefined field.
Emulator: the control plane side is right (the stack deploys, apiType is MERGED, mergedApiExecutionRoleArn and uris.GRAPHQL match, the association settles MERGE_SUCCESS with the requested mergeType), but every query of a merged source field on the MERGED endpoint is answered HTTP 200 with `{"data": {"<field>": null}}` and no `errors` member. The same resolver on the SOURCE API's own endpoint returns the correct value in the same run (the differential control inside deploy-appsync-3-cfn-merged-role-without-source-graphql-denied passes), so this is specific to execution through the merged endpoint, not to the resolver definition (the resolvers here are APPSYNC_JS Code over a NONE data source, deployed by AWS::AppSync::Resolver).
Why: a client that builds a merged (federated) API gets a schema that type-checks but resolves everything to null, with no error to diagnose it from — every field of every source API silently reads as absent data. AUTO_MERGE/MANUAL_MERGE propagation, disassociation withdrawal and the merged-endpoint auth checks are all unobservable because the baseline "the merged endpoint serves the source field" never holds.
Fix: the appsync GraphQL data-plane executor's merged-API dispatch — resolving a merged field to the owning source API and running that API's resolver (as the merged API's mergedApiExecutionRoleArn) instead of returning null; the control-plane merge and the resolver store already hold what is needed.
Related open issues (none carries these checks): 20261114T191235Z-appsync-4-net-new-causes-cloudformation-deployed-vtl.md (CloudFormation-deployed VTL resolvers never execute — here the resolvers are APPSYNC_JS and DO execute on the source endpoint), 20261112T024108Z-appsync-schema-merge-2-net-new-causes-a.md and 20261109T041647Z-appsync-control-plane-9-net-new-causes-merge.md (control-plane merge gaps only).
Traces: appsync.deploy-appsync-3-cfn-merged-api-auto-merge-serves-source-update, appsync.deploy-appsync-3-cfn-merged-api-manual-merge-withholds-update-until-merge, appsync.deploy-appsync-3-cfn-merged-api-disassociation-withdraws-source-fields, appsync.deploy-appsync-3-cfn-merged-endpoint-source-api-key-rejected, appsync.deploy-appsync-3-cfn-association-to-non-merged-api-rejected.

Cause 2 of 2: the merged endpoint does not authorize source-field execution against the merged API's execution role, so a role without appsync:SourceGraphQL produces no Unauthorized field error.
AWS: Smithy — GraphqlApi$mergedApiExecutionRoleArn is the role AppSync assumes "to validate access to source APIs at runtime"; recorded behavior: when that role is NOT granted appsync:SourceGraphQL on the source API, a query of a merged field on the merged endpoint is answered HTTP 200 with the field null AND exactly one `errors` entry whose errorType is "Unauthorized", whose path is the field path (e.g. ["greeting"]), and whose message is "Not authorized to perform: appsync:SourceGraphQL on resource <source API ARN>/types/<TypeName>/fields/<fieldName>" — the fixed text is "Not authorized to perform: appsync:SourceGraphQL on resource ", and the resource is built from the source API's arn plus the type and field being resolved (per-run ARN values). Granting exactly that one action makes the same endpoint answer with the value.
Emulator: the merged endpoint answers HTTP 200 with the field null and NO `errors` member at all (errors == []), whether or not the execution role carries appsync:SourceGraphQL — the denial is never produced, and the null is indistinguishable from Cause 1's unexecuted resolver.
Why: a client cannot tell a mis-scoped merged-API execution role from a working one: there is no Unauthorized error, no errorType and no message naming the missing action and resource, so IAM misconfiguration of a merged API is silent and the repair (adding appsync:SourceGraphQL) is unverifiable.
Fix: the appsync GraphQL data-plane merged-API authorization step — evaluate the merged API's mergedApiExecutionRoleArn for appsync:SourceGraphQL on the source API's type/field before dispatch and append the Unauthorized field error (errorType "Unauthorized", path of the field) when it is denied.
Traces: appsync.deploy-appsync-3-cfn-merged-role-without-source-graphql-denied.

## Evidence
Cause 1 — appsync.deploy-appsync-3-cfn-merged-api-auto-merge-serves-source-update:
  expected: HTTP 200, no `errors`, data.greeting == the literal the source API's APPSYNC_JS resolver returns
  actual:   AssertionError: merged endpoint returned {'data': {'greeting': None}}, expected the source resolver's '<literal>'
  (the literal is a per-run unique value; the divergence is null-with-no-errors vs the resolved value)
  appsync.deploy-appsync-3-cfn-merged-api-disassociation-withdraws-source-fields: AssertionError: merged endpoint returned {'data': {'greeting': None}}
  appsync.deploy-appsync-3-cfn-merged-api-manual-merge-withholds-update-until-merge: AssertionError: merged endpoint returned {'data': {'greeting': None}}
  appsync.deploy-appsync-3-cfn-merged-endpoint-source-api-key-rejected: AssertionError: merged endpoint returned {'data': {'greeting': None}}
  appsync.deploy-appsync-3-cfn-association-to-non-merged-api-rejected: AssertionError: merged endpoint after the rolled-back update returned {'data': {'greeting': None}}
  In all five the preceding control-plane assertions pass: stack CREATE_COMPLETE, GetGraphqlApi apiType == 'MERGED', mergedApiExecutionRoleArn == the stack's role, uris.GRAPHQL == the stack's GraphQLUrl output, and GetSourceApiAssociation sourceApiAssociationStatus == 'MERGE_SUCCESS' with the requested mergeType. The 180 s poll for "HTTP 200 and no errors" SUCCEEDS — the endpoint answers 200 with an empty error set — only the value is null.

Cause 2 — appsync.deploy-appsync-3-cfn-merged-role-without-source-graphql-denied:
  the SOURCE endpoint control passes in the same run (data.greeting == the literal), and the merged endpoint's body.data == {'greeting': None} assertion passes, then:
  expected: exactly one `errors` entry with errorType 'Unauthorized', path ['greeting'], message "Not authorized to perform: appsync:SourceGraphQL on resource <sourceApiArn>/types/Query/fields/greeting"
  actual:   AssertionError: expected one Unauthorized field error, got []
  (the source API ARN is a per-run value; the requirement is the error's presence, its errorType, its path and the fixed message text with the resolved type/field appended)

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (service appsync, group deploy-merged-api-source-association).
2. Create one CloudFormation stack containing: a source AWS::AppSync::GraphQLApi (API_KEY) + AWS::AppSync::GraphQLSchema + AWS::AppSync::DataSource (NONE) + AWS::AppSync::Resolver with Runtime APPSYNC_JS/1.0.0 and Code returning a literal; a MERGED AWS::AppSync::GraphQLApi with MergedApiExecutionRoleArn pointing at a role that trusts appsync and is granted appsync:SourceGraphQL on the source API; and an AWS::AppSync::SourceApiAssociation with SourceApiAssociationConfig.MergeType AUTO_MERGE (or MANUAL_MERGE plus StartSchemaMerge).
3. Wait for CREATE_COMPLETE and poll GetSourceApiAssociation to MERGE_SUCCESS.
4. Cause 1: POST {"query":"query { greeting }"} to the MERGED API's uris.GRAPHQL with the merged API's x-api-key. The response is HTTP 200 {'data': {'greeting': null}} with no errors; POSTing the same query to the SOURCE API's uris.GRAPHQL with the source key returns the literal.
5. Cause 2: redeploy the same stack with the merged execution role NOT granted appsync:SourceGraphQL and repeat step 4 against the merged endpoint: the response is again 200 with the field null and no `errors` member, instead of one Unauthorized field error naming appsync:SourceGraphQL on <sourceApiArn>/types/Query/fields/greeting. Granting the action back changes nothing observable.
