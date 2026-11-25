Issue-Source: trace-sweep-divergence
Issue-Trace: lakeformation-gtr-lf-batch-grant-partial-failure
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lakeformation-gtr-lf-batch-grant-partial-failure lakeformation-gtr-lf-path-role-principal-forms lakeformation-gtr-lf-role-grant-fix-on-governed-table lakeformation-table-permission-grant-revoke
Issue-Shape-Id: com.amazonaws.lakeformation#AWSLakeFormation
Issue-Key: lakeformation|ListPermissions|2xx->2xx|$.PrincipalResourcePermissions[*].LastUpdated
Issue-Key: lakeformation|ListPermissions|2xx->2xx|$.PrincipalResourcePermissions[*].LastUpdatedBy
Issue-Key: lakeformation|ListPermissions|2xx->2xx|$.PrincipalResourcePermissions[*]
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# Lake Formation ListPermissions synthesizes LastUpdated and LastUpdatedBy on permission entries AWS returns unset

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPermissions

## Description
2 net-new causes, both on lakeformation ListPermissions, sharing one root cause: the emulator stamps audit fields onto every PrincipalResourcePermissions entry it projects out of its permissions state, where AWS leaves those members absent for permissions granted through GrantPermissions.

Cause 1 of 2 - extra LastUpdated / LastUpdatedBy members on ListPermissions entries (fixture lakeformation-gtr-lf-batch-grant-partial-failure, step task#5).
AWS: Smithy: ListPermissions -> ListPermissionsResponse.PrincipalResourcePermissions, element shape com.amazonaws.lakeformation#PrincipalResourcePermissions. Its members LastUpdated (target LastModifiedTimestamp, timestamp) and LastUpdatedBy (target NameString, @length min 1 max 255) are OPTIONAL and carry no @default - the model documents them as 'The date and time when the resource was last updated' / 'The user who updated the record', i.e. populated only when the record actually carries that provenance. Recorded behavior: for permissions created by GrantPermissions in this account, AWS returns the entry with LastUpdated and LastUpdatedBy ABSENT (both observed as unset in the recording), and the response status is 200. The requirement is the absence of the members, not any particular timestamp or principal value.
Emulator: answers 200 with the entry, but adds LastUpdated as a populated epoch-seconds timestamp and LastUpdatedBy as a populated IAM-ARN string - replay reports '$.PrincipalResourcePermissions[0].LastUpdated: type_mismatch (expected None, got <epoch seconds>)' and '$.PrincipalResourcePermissions[0].LastUpdatedBy: type_mismatch (expected None, got <an arn:aws:iam::<account>:user/... string>)'. The concrete epoch value and the particular ARN in the recording are per-run values and are not themselves the requirement; the defect is that the members are emitted at all.
Why: a client that treats LastUpdated/LastUpdatedBy as the signal that a permission record has update provenance (change-detection, drift reporting, 'who last modified this grant' audit views, or a diff against a previously fetched snapshot) sees provenance that AWS never reported. Because the emulator derives the timestamp at response time, two ListPermissions calls over an unchanged grant disagree, so any client caching or diffing permission entries sees phantom modifications. Clients that round-trip the entry back into GrantPermissions/RevokePermissions payloads also carry a LastUpdatedBy string that must satisfy NameString's @length/@pattern constraints and was never part of the resource.
Fix: the ListPermissions handler's response projection in the lakeformation service crate - the mapping from the stored permission record to PrincipalResourcePermissions. It must leave LastUpdated and LastUpdatedBy unset for records whose provenance was not supplied, rather than filling them from the current clock and a hardcoded caller identity; equivalently, stop defaulting those two state fields at grant time.
Traces: lakeformation-gtr-lf-batch-grant-partial-failure#task#5 (representative); also lakeformation-gtr-lf-path-role-principal-forms#task#9.

Cause 2 of 2 - the same synthesized audit fields make whole ListPermissions entries non-matching for table and table-with-columns grants (fixture lakeformation-gtr-lf-role-grant-fix-on-governed-table, step task#9).
AWS: Smithy: ListPermissions -> PrincipalResourcePermissions list; element members Principal (DataLakePrincipal), Resource (Resource, with the Table / TableWithColumns variants - com.amazonaws.lakeformation#TableWithColumnsResource carrying CatalogId, DatabaseName, Name and ColumnWildcard), Permissions and PermissionsWithGrantOption (PermissionList), plus the optional Condition, AdditionalDetails, LastUpdated, LastUpdatedBy. Recorded behavior: after granting DESCRIBE on a Table resource and SELECT on the corresponding TableWithColumns resource, AWS returns 200 with one entry per grant, each entry carrying Principal, Resource, Permissions and PermissionsWithGrantOption, and with Condition, AdditionalDetails, LastUpdated and LastUpdatedBy all ABSENT.
Emulator: answers 200 but each entry it returns carries populated LastUpdated (epoch seconds from the current clock) and LastUpdatedBy (an arn:aws:iam::<account>:user/... string), so the entries do not match the recorded ones - replay reports '$.PrincipalResourcePermissions[0]: missing' and '$.PrincipalResourcePermissions[1]: missing' for the DESCRIBE-on-Table and SELECT-on-TableWithColumns entries respectively. No status or error-code divergence is involved: both sides are 200.
Why: the entry-level mismatch means any client that matches a returned permission entry against the grant it issued - to confirm a grant landed, to locate the entry to revoke, or to assert idempotence of a re-grant - fails to match it, even though the permission itself is correct. This affects both the Table and the TableWithColumns resource variants, so it is not specific to one resource form.
Fix: same component as Cause 1 - the ListPermissions response projection / the permission record in lakeformation state; one change to stop defaulting LastUpdated and LastUpdatedBy closes both causes, but both fixtures must be re-replayed because they exercise different resource variants (Table, TableWithColumns) and different list shapes.
Traces: lakeformation-gtr-lf-role-grant-fix-on-governed-table#task#9; also lakeformation-table-permission-grant-revoke#task#7.

Not covered by any open issue: open issue 20261120T155738Z-lakeformation-lake-formation-grantpermissions-accepts-an-invalid-permissions-modification.md owns the GrantPermissions 400-vs-200 divergence on the same fixture (step task#7) and says nothing about ListPermissions response projection; the cloudformation__*__lakeformation issues concern CloudFormation resource types, not ListPermissions.

## Evidence
Cause 1 (fixture lakeformation-gtr-lf-batch-grant-partial-failure, step task#5, verdict mismatch, status 200 expected and 200 actual):
  expected: $.PrincipalResourcePermissions[0].LastUpdated absent (None); $.PrincipalResourcePermissions[0].LastUpdatedBy absent (None)
  actual:   $.PrincipalResourcePermissions[0].LastUpdated = 1795576499.88 (type_mismatch); $.PrincipalResourcePermissions[0].LastUpdatedBy = 'arn:aws:iam::000000000000:user/emulator' (type_mismatch)
  cluster key: service=lakeformation|ListPermissions|status=200->200|keys=$.PrincipalResourcePermissions[0].LastUpdated,$.PrincipalResourcePermissions[0].LastUpdatedBy
  also affected: lakeformation-gtr-lf-path-role-principal-forms#task#9
  (the epoch number and the user ARN above are per-run values from one recording; the requirement is that both members are ABSENT)

Cause 2 (fixture lakeformation-gtr-lf-role-grant-fix-on-governed-table, step task#9, verdict mismatch, status 200 expected and 200 actual):
  expected $.PrincipalResourcePermissions[0]: Permissions ['DESCRIBE'], PermissionsWithGrantOption [], Principal.DataLakePrincipalIdentifier the granted role ARN, Resource.Table {CatalogId, DatabaseName, Name}, and AdditionalDetails/Condition/LastUpdated/LastUpdatedBy all None
  actual   $.PrincipalResourcePermissions[0]: same Principal/Resource/Permissions/PermissionsWithGrantOption, but LastUpdated = 1795576541.383 and LastUpdatedBy = 'arn:aws:iam::000000000000:user/emulator' -> entry reported 'missing' (no match)
  expected $.PrincipalResourcePermissions[1]: Permissions ['SELECT'], PermissionsWithGrantOption [], Resource.TableWithColumns {CatalogId, DatabaseName, Name, ColumnWildcard {ExcludedColumnNames: None}}, and AdditionalDetails/Condition/LastUpdated/LastUpdatedBy all None -> likewise reported 'missing'
  cluster key: service=lakeformation|ListPermissions|status=200->200|keys=$.PrincipalResourcePermissions[0],$.PrincipalResourcePermissions[1]
  also affected: lakeformation-table-permission-grant-revoke#task#7

Smithy citations: ListPermissions output element com.amazonaws.lakeformation#PrincipalResourcePermissions - LastUpdated (LastModifiedTimestamp, optional, no default), LastUpdatedBy (NameString, optional, @length min 1 max 255, no default), Condition and AdditionalDetails also optional; Resource variants Table and TableWithColumns (TableWithColumnsResource.ColumnWildcard -> ColumnWildcard.ExcludedColumnNames).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture lakeformation-gtr-lf-batch-grant-partial-failure up to step task#5 - batch-grant Lake Formation permissions, then call ListPermissions for that principal/resource. Observe 200 with each PrincipalResourcePermissions entry carrying a populated LastUpdated timestamp and a populated LastUpdatedBy principal string; AWS returns those members absent. Same divergence at lakeformation-gtr-lf-path-role-principal-forms#task#9.
3. Cause 2: replay fixture lakeformation-gtr-lf-role-grant-fix-on-governed-table up to step task#9 - GrantPermissions DESCRIBE on a Table resource and SELECT on the matching TableWithColumns resource (ColumnWildcard), then ListPermissions. Observe 200 whose entries do not match the recorded entries because of the same two synthesized members, for both the Table and the TableWithColumns entry. Same divergence at lakeformation-table-permission-grant-revoke#task#7.
4. Expected after fix: ListPermissions omits LastUpdated and LastUpdatedBy from entries whose stored permission record has no update provenance, so every entry matches the recorded shape; re-replay all four fixture/step bindings listed above.
