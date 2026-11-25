Issue-Source: trace-sweep-divergence
Issue-Trace: lakeformation-gtr-lf-role-grant-fix-on-governed-table
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lakeformation-gtr-lf-role-grant-fix-on-governed-table
Issue-Shape-Id: com.amazonaws.lakeformation#AWSLakeFormation
Issue-Key: lakeformation|GrantPermissions|4xx->2xx|$
Issue-Key: lakeformation|GrantPermissions|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: lakeformation|GrantPermissions|4xx->2xx|status
Issue-Commit: aa86e9e75ce0346509f1e7a26ee54199b6f59f45

# Lake Formation GrantPermissions accepts an invalid permissions modification and answers 200 instead of 400 InvalidInputException

> _Note: this issue was identified against the code base at commit `aa86e9e75ce0346509f1e7a26ee54199b6f59f45`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GrantPermissions

## Description
Cause 1 of 1: a GrantPermissions request whose requested permission set is not a valid modification for the resource it names is recorded by the emulator and answered HTTP 200 with an empty success body, where AWS refuses it with HTTP 400 InvalidInputException and the message "Permissions modification is invalid.".

AWS: Smithy: GrantPermissions (restJson1, POST /GrantPermissions, success code 200 with an empty body; input Principal: DataLakePrincipal required, Resource: Resource required, Permissions: PermissionList required, plus Condition and PermissionsWithGrantOption) models InvalidInputException (errorKind client, httpError 400, sole member Message - "A message describing the problem"), beside EntityNotFoundException and ConcurrentModificationException. Smithy: Resource is a structure whose members each name a DIFFERENT kind of Lake Formation resource (Catalog, Database, Table, TableWithColumns, DataLocation, DataCellsFilter, LFTag, LFTagPolicy, LFTagExpression, RLSPolicy) and Smithy: Permission is an enum of ALL, SELECT, ALTER, DROP, DELETE, INSERT, DESCRIBE, CREATE_DATABASE, CREATE_TABLE, DATA_LOCATION_ACCESS, CREATE_LF_TAG, ASSOCIATE, GRANT_WITH_LF_TAG_EXPRESSION, CREATE_LF_TAG_EXPRESSION, CREATE_CATALOG, SUPER_USER - a permission vocabulary shared across all resource kinds, only a subset of which is meaningful on any one kind (TableWithColumnsResource's own documentation, "is only used when granting a SELECT permission", is the one such restriction the model states outright). The refusal wording for a permission modification AWS will not make is recorded behavior only (the model states no message template): the recorded response is HTTP 400, carries an x-amzn-errortype header naming the error shape, and its body is exactly one key, {"Message": "Permissions modification is invalid."}. That whole sentence is FIXED text: it has no variable part - no principal, resource name, permission or limit is interpolated into it, so nothing in it is taken from the request or from state. Hypothesis: Lake Formation validates each requested permission (and each PermissionsWithGrantOption value) against the resource kind the Resource member names, and refuses the whole request with this sentence when the combination is not one it can record - e.g. DATA_LOCATION_ACCESS asked for on a Table/Database rather than a DataLocation, a catalog- or database-scoped permission (CREATE_DATABASE, CREATE_TABLE) asked for on a table, a table-scoped permission asked for on a database, or PermissionsWithGrantOption naming a permission that is not also in Permissions. The replay record carries the recorded response only, not the request body, so which of those combinations this fixture's step sends cannot be read off it; the requirement it establishes is the refusal itself, for the grant this step makes.

Emulator: for the same step the emulator answers HTTP 200 with an empty body - no 400, no $.Message, and no x-amzn-errortype header at all - and records the grant. Its grant path validates only: that every named value is a member of the Permission enum, that the grantee identifier is a well-formed Lake Formation principal, that at least one permission is named, that Resource names exactly one member, that a request addressing TableWithColumns asks only for column-scoped permissions, and that the Data Catalog holds the named database/table. There is no check that a requested permission is applicable to the KIND of resource named, and no check that PermissionsWithGrantOption is a subset of Permissions, so this class of request falls through to the store and is reported as a successful grant.

Why: a client is told a grant it issued succeeded when on AWS no permission was granted at all. The typed InvalidInputException it would handle (correcting the permission set, failing the deployment, retrying with a valid resource kind) never arrives, so the client proceeds believing the principal holds access it does not have; worse, the emulator's own state now holds a grant AWS would never hold, so every later ListPermissions/RevokePermissions read-back and any infrastructure convergence check built on it disagrees with AWS. A caller that parses the error body or keys off x-amzn-errortype gets nothing to parse.

Fix: GrantPermissions' validation in the lakeformation service crate - the pre-write planning path the handler calls (services/service-com-amazonaws-lakeformation-awslakeformation/src/ops/grant_permissions.rs handle -> crate::state::plan_grant / crate::state::read_grant_request in src/state/grant_requests.rs, beside the existing column-scope check in src/state.rs). Add the permission-to-resource-kind applicability rule (and the PermissionsWithGrantOption-subset-of-Permissions rule) to that validation so an invalid modification is refused BEFORE anything is recorded, raising the modeled InvalidInputException with HTTP 400, the x-amzn-errortype header, and a body of exactly {"Message": "Permissions modification is invalid."} - the fixed sentence, with nothing interpolated. Note this is not covered by an open issue: the resolved issue 20261112T061322Z-lakeformation-lake-formation-grantpermissions-principal-path-unvalidated-and-invalidinputexception.md once listed this fixture as a secondary member of its "Invalid path for user, arn: <principalArn>" cluster, but that cause (principal-ARN path validation) is fixed and is a different refusal with a different message; this step still diverges and the replay reports it with an empty owned_by and an empty stub_path.

Traces: lakeformation-gtr-lf-role-grant-fix-on-governed-table#task#7 (GrantPermissions, expected 400, got 200).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200, score 76.8%, 2 divergence clusters). This cluster is verdict=mismatch with an EMPTY stub_path and an EMPTY owned_by (the other cluster, ListPermissions, is owned by an open issue and is not filed here).

Fixture lakeformation-gtr-lf-role-grant-fix-on-governed-table, step task#7, operation GrantPermissions, expected_status 400, actual_status 200.
Divergence detail, verbatim:
  status: expected 400, got 200
  $: empty_body (expected {'Message': 'Permissions modification is invalid.'}, got {})
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
Divergence key: service=lakeformation|GrantPermissions|status=400->200|keys=$,$headers.x-amzn-errortype,status

Expected (AWS, recorded): HTTP 400, x-amzn-errortype present, body exactly {"Message": "Permissions modification is invalid."} - the entire message is fixed text with no request- or state-derived part.
Actual (emulator): HTTP 200, empty JSON body ({}), no x-amzn-errortype header, and the grant persisted.

Model citations used: GrantPermissions operation signature + errors (InvalidInputException, errorKind client, httpError 400, member Message); InvalidInputException shape; Resource structure members (Catalog/Database/Table/TableWithColumns/DataLocation/DataCellsFilter/LFTag/LFTagPolicy/LFTagExpression/RLSPolicy); Permission enum values; TableWithColumnsResource doc ("is only used when granting a SELECT permission").
Code read for the fix location: src/ops/grant_permissions.rs (handle: read_grant_request -> unresolved_grant_principal -> plan_grant -> record_grants), src/state/grant_requests.rs:125-221, src/state.rs (COLUMN_SCOPED_TABLE_PERMISSIONS / column_scope_violation) - no permission-to-resource-kind check and no grant-option-subset check exists in any of them.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and inspect fixture lakeformation-gtr-lf-role-grant-fix-on-governed-table at step task#7.
3. Equivalently, by hand: create the Glue database and table the fixture uses, then call GrantPermissions with a valid IAM role principal ARN and a Permissions/Resource combination Lake Formation will not record - e.g. Permissions ['DATA_LOCATION_ACCESS'] with Resource {"Table": {CatalogId, DatabaseName, Name}}, or Permissions ['CREATE_TABLE'] with Resource {"Table": ...}, or PermissionsWithGrantOption naming a permission absent from Permissions.
4. Expected (AWS): HTTP 400 with an x-amzn-errortype header and body exactly {"Message": "Permissions modification is invalid."}, and NO grant recorded.
5. Observed (emulator): HTTP 200 with an empty body and no x-amzn-errortype header; a following ListPermissions shows the grant was recorded.
6. Diff the status, the x-amzn-errortype header and the full JSON body (key set included) against the recorded AWS response.
