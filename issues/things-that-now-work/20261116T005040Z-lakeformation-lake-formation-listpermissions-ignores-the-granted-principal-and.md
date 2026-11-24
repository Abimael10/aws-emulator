Issue-Source: trace-sweep-divergence
Issue-Trace: lakeformation-list-permissions-maxresults-membership
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lakeformation-list-permissions-maxresults-membership
Issue-Shape-Id: com.amazonaws.lakeformation#AWSLakeFormation
Issue-Key: lakeformation|ListPermissions|2xx->2xx|$.PrincipalResourcePermissions[*]
Issue-Commit: f83e35bfedd0a6ac8c5511dde67d521244288906

# Lake Formation ListPermissions ignores the granted principal and returns a synthetic user/emulator grant without LastUpdated/LastUpdatedBy

> _Note: this issue was identified against the code base at commit `f83e35bfedd0a6ac8c5511dde67d521244288906`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPermissions

## Description
Cause 1 of 1: ListPermissions does not read back the grants that GrantPermissions recorded. Instead of the explicitly granted PrincipalResourcePermissions rows for the principal under test, the emulator answers 200 with rows describing a fixed, emulator-invented principal, in a resource form that was never granted, and with the record-keeping members of the entry omitted.

AWS: ListPermissions (Smithy: ListPermissions, POST /ListPermissions, 200, input members Principal/ResourceType/Resource/MaxResults/NextToken, output PrincipalResourcePermissions: PrincipalResourcePermissionsList) "Returns a list of the principal permissions on the resource... This operation returns only those permissions that have been explicitly granted." Each element is a PrincipalResourcePermissions whose Principal.DataLakePrincipalIdentifier is the principal the grant was made FOR, whose Resource is the resource form the grant was made ON (a Table grant stays a Table entry; a TableWithColumns grant stays a TableWithColumns entry carrying the ColumnWildcard/ColumnNames it was granted with), whose Permissions / PermissionsWithGrantOption are exactly the permission sets of that grant, and which also carries LastUpdated (Smithy: PrincipalResourcePermissions.LastUpdated, LastModifiedTimestamp, "The date and time when the resource was last updated") and LastUpdatedBy (Smithy: PrincipalResourcePermissions.LastUpdatedBy, NameString @length 1..255, "The user who updated the record"). So after granting DESCRIBE on a table and SELECT on that table's column wildcard to one IAM role principal, listing permissions returns one entry per explicit grant, each naming that role as the principal, each in its granted resource form, and each with LastUpdated/LastUpdatedBy populated.

Emulator: for the same step the emulator returns 200 but the first entries of PrincipalResourcePermissions are not the granted rows. Element [0] expected a Table entry (Resource.Table with CatalogId/DatabaseName/Name, no column selection) with Permissions ['DESCRIBE'], PermissionsWithGrantOption [], Principal.DataLakePrincipalIdentifier = the granted IAM role ARN, plus LastUpdated and LastUpdatedBy; the emulator served Principal.DataLakePrincipalIdentifier 'arn:aws:iam::<account>:user/emulator', Resource.TableWithColumns with ColumnWildcard {}, Permissions ['SELECT'], PermissionsWithGrantOption ['SELECT'], and no LastUpdated or LastUpdatedBy member at all. Element [1] expected the TableWithColumns / ColumnWildcard entry with Permissions ['SELECT'] and PermissionsWithGrantOption [] for the same role principal; the emulator again served an entry keyed to its own internal principal. The identifier 'user/emulator' and the grant-option-on-everything shape show the emulator is projecting an internal implicit-admin/creator record rather than the grant store, and that neither the Principal filter in the request nor the resource form of each stored grant is honoured. (Note: the account id, database/table names, role name and the LastUpdated instant in the recording are per-run values, not requirements; the requirement is that the principal echoed is the one granted to, the resource form is the one granted on, the permission sets match the grant, and LastUpdated/LastUpdatedBy are present.)

Why: a client uses ListPermissions to verify, audit and revoke grants. Because the emulator never echoes the principal it was asked about, any read-back of "what does this role have" returns rows for a principal the caller never granted to, so a grant appears lost while a permission the caller never issued (SELECT with grant option, for user/emulator) appears to exist. Revoke flows built on the listing target the wrong principal and resource form, column-level grants are indistinguishable from table-level grants, and tooling that reads LastUpdated/LastUpdatedBy for change tracking gets a null or a missing-member deserialization gap on a member the model documents as part of every record.

Why this is not the open lakeformation issue: 20261112T061322Z-lakeformation-lake-formation-grantpermissions-principal-path-unvalidated-and-invalidinputexception.md owns the GrantPermissions principal-path validation and the InvalidInputException __type body (the other two clusters in this replay); it says nothing about ListPermissions read-back, and this cluster is reported with no owner.

Fix: ListPermissions' handler and the Lake Formation permission state it reads. The handler must enumerate the stored explicit grants, filter them by the request's Principal / Resource / ResourceType, and serialize one PrincipalResourcePermissions per stored grant preserving that grant's principal identifier, its resource form (Table vs TableWithColumns + its column selection), its Permissions and PermissionsWithGrantOption, and the LastUpdated / LastUpdatedBy recorded when the grant was written — rather than synthesizing an entry for a built-in principal.

## Evidence
Fixture lakeformation-list-permissions-maxresults-membership, step task#7, ListPermissions, expected 200 / actual 200, body mismatch on $.PrincipalResourcePermissions[0] and $.PrincipalResourcePermissions[1].

Expected [0]: {'Permissions': ['DESCRIBE'], 'PermissionsWithGrantOption': [], 'Principal': {'DataLakePrincipalIdentifier': 'arn:aws:iam::000000000000:role/sim2real-probe-lf-principal-089082bc'}, 'Resource': {'Table': {'CatalogId': '000000000000', 'DatabaseName': 'sim2real_probe_lf_db_cb72ac5d', 'Name': 'sim2real_probe_lf_tbl_d716022a'}}, 'LastUpdated': <timestamp>, 'LastUpdatedBy': '<shaped-not-asserted>'}
Actual   [0]: {'Principal': {'DataLakePrincipalIdentifier': 'arn:aws:iam::000000000000:user/emulator'}, 'Resource': {'TableWithColumns': {'CatalogId': '000000000000', 'DatabaseName': 'sim2real_probe_lf_db_cb72ac5d', 'Name': 'sim2real_probe_lf_tbl_d716022a', 'ColumnWildcard': {}}}, 'Permissions': ['SELECT'], 'PermissionsWithGrantOption': ['SELECT']}  -- no LastUpdated, no LastUpdatedBy

Expected [1]: {'Permissions': ['SELECT'], 'PermissionsWithGrantOption': [], 'Principal': {'DataLakePrincipalIdentifier': 'arn:aws:iam::000000000000:role/sim2real-probe-lf-principal-089082bc'}, 'Resource': {'TableWithColumns': {'CatalogId': '000000000000', 'DatabaseName': 'sim2real_probe_lf_db_cb72ac5d', 'Name': 'sim2real_probe_lf_tbl_d716022a', 'ColumnWildcard': {}}}, 'LastUpdated': <timestamp>, 'LastUpdatedBy': '<shaped-not-asserted>'}
Actual   [1]: another entry keyed to the emulator's own DataLakePrincipal, not the granted role.

Divergence key: service=lakeformation|ListPermissions|status=200->200|keys=$.PrincipalResourcePermissions[0],$.PrincipalResourcePermissions[1]. Model citations: ListPermissions docs ("returns only those permissions that have been explicitly granted"), PrincipalResourcePermissions members Principal/Resource/Permissions/PermissionsWithGrantOption/LastUpdated/LastUpdatedBy.

## Steps to Reproduce
1. Replay fixture lakeformation-list-permissions-maxresults-membership against the emulator (replay_wire_corpus) and inspect step task#7.
2. Equivalently, by hand: register a data lake location/database and create a table; GrantPermissions DESCRIBE on {Resource: {Table: {CatalogId, DatabaseName, Name}}} to an IAM role principal ARN; GrantPermissions SELECT on {Resource: {TableWithColumns: {CatalogId, DatabaseName, Name, ColumnWildcard: {}}}} to the same role principal.
3. Call ListPermissions with that role ARN as Principal (and a MaxResults page size).
4. Observe PrincipalResourcePermissions contains entries whose Principal.DataLakePrincipalIdentifier is 'arn:aws:iam::<account>:user/emulator' with Permissions ['SELECT'] and PermissionsWithGrantOption ['SELECT'], and with no LastUpdated/LastUpdatedBy members, instead of one entry per explicit grant for the requested role (the Table/DESCRIBE entry and the TableWithColumns/SELECT entry, each with LastUpdated and LastUpdatedBy present).
