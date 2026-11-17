Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-appends-admin-and-restores-on-cleanup cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-non-all-create-database-default-rejected
Issue-Key: cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-appends-admin-and-restores-on-cleanup|run#13|AssertionError|assert new_admin_ids == original_admin_ids | {admin_arn}, (
Issue-Key: cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-non-all-create-database-default-rejected|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"rejected stack ended {status!r}"
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# cloudformation__lakeformation: AWS::LakeFormation::DataLakeSettings deploys as a no-op — 2 net-new causes: the APPEND never reaches the account's data lake settings, and a non-ALL CreateDatabaseDefaultPermissions is never refused

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
The CloudFormation resource type AWS::LakeFormation::DataLakeSettings is registered in the emulator (CreateStack accepts the template and the stack reaches CREATE_COMPLETE), but its create handler never performs the Lake Formation write it models. Two distinct behaviors a client depends on are therefore absent. 2 causes, numbered below; both must be closed.

Cause 1 of 2: a DataLakeSettings resource with MutationType APPEND does not change the account's data lake settings.
AWS: the AWS::LakeFormation::DataLakeSettings resource's create handler calls lakeformation PutDataLakeSettings (Smithy: PutDataLakeSettings input DataLakeSettings @required; GetDataLakeSettings output DataLakeSettings), so with MutationType APPEND and Admins = [{DataLakePrincipalIdentifier: <the deploying account's root ARN>}] the stack reaches CREATE_COMPLETE and GetDataLakeSettings afterwards returns DataLakeAdmins containing the previously present admins PLUS that principal, with every other member of DataLakeSettings (ReadOnlyAdmins, CreateDatabaseDefaultPermissions, CreateTableDefaultPermissions, Parameters, TrustedResourceOwners, AllowExternalDataFiltering, AllowFullTableExternalDataAccess, ExternalDataFilteringAllowList, AuthorizedSessionTagValueList) unchanged. Deleting the stack writes the prior settings back (the write adds an empty AuthorizedSessionTagValueList when the prior settings carried none).
Emulator: the stack reaches CREATE_COMPLETE, yet GetDataLakeSettings returns DataLakeAdmins as an empty list — the admin principal the template appends is nowhere in the settings (replay: "DataLakeAdmins read back set(), expected {'arn:aws:iam::<account>:root'}"; the account id in that text is this run's account, not part of the requirement). No PutDataLakeSettings effect is observable at all, so the delete-time restore cannot be observed either.
Why: a client that deploys its data lake administrators through CloudFormation — the documented way to bootstrap Lake Formation — sees a successful stack and then every subsequent Lake Formation admin-gated call behaving as if no admin was ever configured. Grants, settings reads and permission checks built on that admin silently diverge, and the stack reports success so nothing surfaces the gap.
Fix: the CloudFormation resource-type handler for AWS::LakeFormation::DataLakeSettings — its create path must apply the modeled properties to Lake Formation state through PutDataLakeSettings semantics (APPEND merging Admins/ReadOnlyAdmins into the existing lists, leaving unspecified members untouched), and its delete path must write the prior settings back.

Cause 2 of 2: a CreateDatabaseDefaultPermissions default granting anything other than ALL is accepted instead of refused.
AWS: the only CreateDatabaseDefaultPermissions grant Lake Formation accepts on a data lake settings write is the full-access one (Permissions ["ALL"], e.g. to IAM_ALLOWED_PRINCIPALS); any other permission set is refused with HTTP 400 InvalidInputException (Smithy: PutDataLakeSettings errors InvalidInputException, errorKind client, httpError 400, "The input provided was not valid"). Deployed through CloudFormation that refusal surfaces as a single Settings CREATE_FAILED event whose ResourceStatusReason is the registry-handler wrapping of that service error — fixed text "Create Database Default not supported for given permissions (Service: AWSLakeFormation; Status Code: 400; Error Code: InvalidInputException; Request ID: <request id>; Proxy: null)", where only the Request ID is a per-call value and no HandlerErrorCode is attached — and the stack ends ROLLBACK_COMPLETE with the account's data lake settings unchanged. The same template with Permissions ["ALL"] deploys to CREATE_COMPLETE and reads back as that one grant.
Emulator: the stack carrying Permissions ["SELECT"] ends CREATE_COMPLETE (replay: "rejected stack ended 'CREATE_COMPLETE'"). No InvalidInputException is raised, no Settings CREATE_FAILED event with that reason is emitted, and there is no rollback.
Why: a client validating its Lake Formation configuration in a template gets a green stack for a configuration real AWS rejects, so the defect only appears against real AWS; conversely any test asserting the refusal path (CREATE_FAILED reason, ROLLBACK_COMPLETE, settings left untouched) cannot pass against the emulator.
Fix: validation on the write path — the AWS::LakeFormation::DataLakeSettings handler's create path and the Lake Formation PutDataLakeSettings validation it must call, which has to reject a CreateDatabaseDefaultPermissions entry whose Permissions is not ["ALL"] with InvalidInputException/400 and that message, and the CloudFormation handler-reason serializer that wraps a service error into the Settings CREATE_FAILED ResourceStatusReason.

Traces: the two failing behavioral checks listed under trace_ids; no existing open issue covers this resource type (the filed AWS::LakeFormation::PrincipalPermissions, AWS::LakeFormation::DataCellsFilter and AWS::LakeFormation::Resource issues are different resource types).

## Evidence
Check cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-appends-admin-and-restores-on-cleanup [mismatch]:
  expected: after CREATE_COMPLETE, GetDataLakeSettings().DataLakeSettings.DataLakeAdmins == prior admins + the account root ARN
  actual:   AssertionError: DataLakeAdmins read back set(), expected {'arn:aws:iam::000000000000:root'} (the account id is this run's; the requirement is "the appended principal is present")

Check cloudformation__lakeformation.deploy-cfn-lakeformation-datalakesettings-non-all-create-database-default-rejected [mismatch]:
  template: AWS::LakeFormation::DataLakeSettings, MutationType APPEND, CreateDatabaseDefaultPermissions [{Principal: {DataLakePrincipalIdentifier: IAM_ALLOWED_PRINCIPALS}, Permissions: [SELECT]}]
  expected: stack status ROLLBACK_COMPLETE; exactly one Settings CREATE_FAILED event with ResourceStatusReason "Create Database Default not supported for given permissions (Service: AWSLakeFormation; Status Code: 400; Error Code: InvalidInputException; Request ID: <id>; Proxy: null)" and no HandlerErrorCode; data lake settings unchanged
  actual:   AssertionError: rejected stack ended 'CREATE_COMPLETE' — no refusal, no CREATE_FAILED event, no rollback

## Steps to Reproduce
Cause 1: lakeformation GetDataLakeSettings to record the current settings. cloudformation CreateStack with a template whose single resource is Type AWS::LakeFormation::DataLakeSettings, Properties {Admins: [{DataLakePrincipalIdentifier: arn:<partition>:iam::<account>:root}], MutationType: APPEND}. Poll DescribeStacks until CREATE_COMPLETE, then GetDataLakeSettings: DataLakeAdmins is empty instead of containing the appended principal. DeleteStack and read the settings again to see that no restore write happens either.

Cause 2: PutDataLakeSettings with CreateDatabaseDefaultPermissions: [] to clear the defaults. CreateStack with the same resource type and Properties {CreateDatabaseDefaultPermissions: [{Principal: {DataLakePrincipalIdentifier: IAM_ALLOWED_PRINCIPALS}, Permissions: ["SELECT"]}], MutationType: APPEND}. Poll DescribeStacks: the status becomes CREATE_COMPLETE instead of ROLLBACK_COMPLETE, and DescribeStackEvents shows no Settings CREATE_FAILED row carrying the InvalidInputException handler reason. Repeating with Permissions ["ALL"] is the positive control: it must deploy and read back as that one grant.
