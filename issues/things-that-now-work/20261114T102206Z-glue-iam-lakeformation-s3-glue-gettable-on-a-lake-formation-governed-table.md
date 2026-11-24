Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__lakeformation__s3.lf-governed-table-get-table-denied-without-lf-grant-and-describe-grant-restores-it glue__iam__lakeformation__s3.lf-select-grant-alone-restores-get-table-no-describe-required
Issue-Key: glue__iam__lakeformation__s3.lf-governed-table-get-table-denied-without-lf-grant-and-describe-grant-restores-it|run#10|AssertionError|assert err == want, (
Issue-Key: glue__iam__lakeformation__s3.lf-select-grant-alone-restores-get-table-no-describe-required|run#10|AssertionError|assert (code, message) == want, (
Issue-Commit: 2b46cf3df3e6c8d83849f7668fee251a8b60f8ab

# Glue GetTable on a Lake Formation-governed table: the AccessDeniedException message is emulator-authored ("Insufficient Lake Formation permission(s) on <table>") instead of naming the permission Lake Formation required

> _Note: this issue was identified against the code base at commit `2b46cf3df3e6c8d83849f7668fee251a8b60f8ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTable

## Description
1 net-new root cause, grounded in two failing checks that fail identically.

Smithy consulted (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): GetTable, input DatabaseName and Name required (NameString, length 1..255), output Table; modeled errors are EntityNotFoundException, FederationSourceException, FederationSourceRetryableException, GlueEncryptionException, InternalServiceException 500, InvalidInputException, OperationTimeoutException, ResourceNotReadyException (all client errors httpError 400, defaulted). AccessDeniedException is not in GetTable's modeled error list — it is the Lake Formation authorization refusal injected into the Glue catalog read — so the message rule below is recorded behavior only.

Cause 1 of 1: the Lake Formation refusal message does not name the permission that was required.
AWS: a caller holding glue:GetDatabase/glue:GetTable/glue:GetTables in IAM, reading a table in a database created with an EMPTY CreateTableDefaultPermissions (so the table carries no IAM_ALLOWED_PRINCIPALS grant and Lake Formation enforces its own permissions), is refused with Error.Code 'AccessDeniedException' and Error.Message "Insufficient Lake Formation permission(s): Required Describe on <table name>". Fixed text: "Insufficient Lake Formation permission(s): Required Describe on "; the only variable part is the table name taken from the request (the check's table is named usage_events). The same caller reading a table in a database that DOES carry IAM_ALLOWED_PRINCIPALS succeeds, which is what isolates the refusal to Lake Formation rather than IAM.
Emulator: the status and the error code match, but the message is "Insufficient Lake Formation permission(s) on <table name>" — a different template that drops the ": Required <Permission> on" clause entirely and so never names the permission Lake Formation found missing.
Why: AWS's message is the only place the refusal says WHICH Lake Formation permission to grant. A client or operator reading the emulator's refusal is told a grant is missing but not that DESCRIBE (or a data-level permission that subsumes it) is the one to add, so the documented remediation path from this error cannot be followed, and any caller that matches on the AWS wording does not recognise the refusal. Status and code are unaffected, so this is a message-fidelity defect, not a status/code defect.
Fix: the refusal-message construction on the Lake Formation authorization path that gates the Glue GetTable handler — emit the "Insufficient Lake Formation permission(s): Required <Permission> on <name>" template, with <Permission> taken from the permission the check found missing and <name> from the request.
Related open issues that do NOT cover this: 20261107T185637Z-glue-iam-lakeformation-s3-revoking-iam-allowed-principals-on-a-lake-formation.md (GetDatabase does not refuse at all after a revoke — a missing refusal, not a reworded one) and 20261029T173933Z-lakeformation-lake-formation-client-error-bodies-reword-or-drop.md (lakeformation's own client-error bodies, not the Glue catalog read).
Traces: glue__iam__lakeformation__s3.lf-governed-table-get-table-denied-without-lf-grant-and-describe-grant-restores-it, glue__iam__lakeformation__s3.lf-select-grant-alone-restores-get-table-no-describe-required.

## Evidence
Both failing checks first prove the reader's IAM policy has propagated (GetTable on the open, IAM_ALLOWED_PRINCIPALS database returns OK), then read the governed table.

glue.get_table(DatabaseName=<governed db>, Name='usage_events') as an assumed role holding glue:GetDatabase/glue:GetTable/glue:GetTables and no Lake Formation grant:
  Expected (AWS): ('AccessDeniedException', 'Insufficient Lake Formation permission(s): Required Describe on usage_events')
  Actual (emulator): ('AccessDeniedException', 'Insufficient Lake Formation permission(s) on usage_events')

Identical expected/actual pair reported by both checks:
  lf-governed-table-get-table-denied-without-lf-grant-and-describe-grant-restores-it — AssertionError: "GetTable on a governed table with no Lake Formation grant returned ('AccessDeniedException', 'Insufficient Lake Formation permission(s) on usage_events'), expected ('AccessDeniedException', 'Insufficient Lake Formation permission(s): Required Describe on usage_events')"
  lf-select-grant-alone-restores-get-table-no-describe-required — AssertionError: "GetTable on a governed table with no grant returned ('AccessDeniedException', 'Insufficient Lake Formation permission(s) on usage_events'), expected ('AccessDeniedException', 'Insufficient Lake Formation permission(s): Required Describe on usage_events')"

'usage_events' is the table name from the request; the fixed text under test is 'Insufficient Lake Formation permission(s): Required Describe on '.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. glue.create_database(DatabaseInput={'Name': <governed_db>, 'CreateTableDefaultPermissions': []}) and glue.create_database(DatabaseInput={'Name': <open_db>}).
3. glue.create_table in BOTH databases with Name='usage_events' (EXTERNAL_TABLE, any StorageDescriptor.Location).
4. iam.create_role trusted by this account with an inline Allow of glue:GetDatabase, glue:GetTable, glue:GetTables (and lakeformation:GetDataAccess) on '*'; sts.assume_role it.
5. As that role, glue.get_table(DatabaseName=<open_db>, Name='usage_events') until it succeeds — this is the IAM-propagation barrier.
6. As that role, glue.get_table(DatabaseName=<governed_db>, Name='usage_events') and read Error.Code and Error.Message. Observe code AccessDeniedException with message 'Insufficient Lake Formation permission(s) on usage_events' instead of 'Insufficient Lake Formation permission(s): Required Describe on usage_events'.
7. Replay the two check ids in trace_ids to confirm a fix.
