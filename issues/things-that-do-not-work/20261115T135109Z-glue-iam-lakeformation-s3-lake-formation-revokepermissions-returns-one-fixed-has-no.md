Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__lakeformation__s3.lf-revoke-of-absent-iam-allowed-principals-grant-rejected-and-grant-is-idempotent
Issue-Key: glue__iam__lakeformation__s3.lf-revoke-of-absent-iam-allowed-principals-grant-rejected-and-grant-is-idempotent|run#10|AssertionError|assert err == want, f"revoking a grant that does not exist returned {err!r}, expected {want!r}"
Issue-Commit: 9f0ff9d16f233843e16803334c784cdf6a4e13d9

# Lake Formation RevokePermissions returns one fixed "has no permissions and no grantable permissions" sentence for every nothing-to-revoke refusal, instead of the per-permission "Grantee does not does not have:[...]" sentence AWS returns for IAM_ALLOWED_PRINCIPALS on a governed table

> _Note: this issue was identified against the code base at commit `9f0ff9d16f233843e16803334c784cdf6a4e13d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RevokePermissions

## Description
One net-new cause on the Lake Formation revoke refusal path. 1 cause.

Cause 1 of 1 — RevokePermissions collapses AWS's two distinct nothing-to-revoke refusal sentences into a single constant, so the IAM_ALLOWED_PRINCIPALS-on-a-governed-table case gets the wrong message text.
AWS: Smithy (AWSLakeFormation, restJson1): RevokePermissions(Principal: DataLakePrincipal required, Resource: Resource required, Permissions: PermissionList required) -> POST /RevokePermissions, success code 200 with an empty body; modeled errors InvalidInputException (errorKind client, httpError 400), EntityNotFoundException and ConcurrentModificationException. The refusal TEXT is recorded behavior only — the model states no message template. The recording for this case shows HTTP 400 / error code InvalidInputException with the message "No permissions revoked. Grantee does not does not have:[ALL]". Its fixed text is "No permissions revoked. Grantee does not does not have:[" + "]" (the doubled "does not does not" is AWS's own wording, not a typo to be corrected); the ONLY variable part is the comma-free list of the Permissions values taken verbatim from the request — here [ALL] because the request asked to revoke ["ALL"]. This is a DIFFERENT refusal class from the one AWS words "No permissions revoked. Grantee has no permissions and no grantable permissions on resource." (all fixed text, no interpolation), which the already-fixed issue 20261103T022922Z-lakeformation-lake-formation-permission-list-revoke-paths-batch-grants.md recorded for revoking on behalf of an IAM role principal that holds nothing. Hypothesis: Lake Formation reports the per-permission difference ("does not ... have:[<requested>]") when the grantee is a principal it always models as present — IAM_ALLOWED_PRINCIPALS on a governed table — and the "has no permissions and no grantable permissions on resource" sentence when the grantee holds nothing at all; the two are not interchangeable.
Emulator: HTTP 400 with error code InvalidInputException (status and code correct) but the message "No permissions revoked. Grantee has no permissions and no grantable permissions on resource." for the IAM_ALLOWED_PRINCIPALS/[ALL] revoke on a governed table — the single constant installed by the fix for 20261103T022922Z, emitted unconditionally, with no branch for the per-permission form and no interpolation of the request's Permissions list. Note this is the exact inverse of the pre-fix defect: the emulator previously emitted the per-permission form everywhere and now emits the generic form everywhere; neither covers both classes.
Why: a revoke-if-present cleanup or teardown path that tolerates exactly the refusal AWS words for this case cannot recognise it, so it treats a benign no-op revoke as an unknown failure and aborts; a caller that parses the bracketed permission list out of the message to learn WHICH permissions were absent gets nothing to parse, and a pinned assertion on AWS's sentence fails even though the status and error code match.
Fix: the RevokePermissions handler's no-op refusal path in the lakeformation service crate (the branch that builds the InvalidInputException message) — pick the refusal sentence by class: emit "No permissions revoked. Grantee does not does not have:[<requested permissions from the request>]" where the grantee is modeled as present but lacks the requested permissions (IAM_ALLOWED_PRINCIPALS on a governed table), and keep the fixed "No permissions revoked. Grantee has no permissions and no grantable permissions on resource." for the grantee-holds-nothing class. Do not delete either sentence in favour of the other, and do not "correct" the doubled "does not does not".
Traces: glue__iam__lakeformation__s3.lf-revoke-of-absent-iam-allowed-principals-grant-rejected-and-grant-is-idempotent.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 23/30 passed. Failing check (verdict=mismatch, not marked ALREADY FILED and not OWNED by any open issue):

### glue__iam__lakeformation__s3
[mismatch] glue__iam__lakeformation__s3.lf-revoke-of-absent-iam-allowed-principals-grant-rejected-and-grant-is-idempotent
  AssertionError: revoking a grant that does not exist returned ('InvalidInputException', 'No permissions revoked. Grantee has no permissions and no grantable permissions on resource.'), expected ('InvalidInputException', 'No permissions revoked. Grantee does not does not have:[ALL]')

The check creates a governed Glue database (DatabaseInput.CreateTableDefaultPermissions = []) and a parquet table in it, asserts ListPermissions for Principal {DataLakePrincipalIdentifier: IAM_ALLOWED_PRINCIPALS} on that Table resource is empty, then calls RevokePermissions(Principal=IAM_ALLOWED_PRINCIPALS, Resource={Table:{DatabaseName,Name}}, Permissions=["ALL"]). Error code matches (InvalidInputException, HTTP 400); only the message text diverges. [ALL] in the expected message is the request's Permissions list, not a literal from one run.

Context: the fixed issue 20261103T022922Z-lakeformation-lake-formation-permission-list-revoke-paths-batch-grants.md (Cause 4) replaced the per-permission sentence with the generic one for the IAM-role-principal case; this check shows the generic sentence is now also served for the IAM_ALLOWED_PRINCIPALS case, where AWS returns the per-permission sentence. The open issue 20261112T061322Z-lakeformation-lake-formation-grantpermissions-principal-path-unvalidated-and-invalidinputexception.md covers GrantPermissions principal validation and the restJson1 error-body __type member, and does NOT cover this RevokePermissions message cause.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay the behavioral suite, or drive the check directly:
   a. glue.CreateDatabase with DatabaseInput {Name: <db>, CreateTableDefaultPermissions: []} (a governed database, so its tables carry no IAM_ALLOWED_PRINCIPALS grant).
   b. glue.CreateTable in <db> (any EXTERNAL_TABLE, e.g. the parquet table the check builds).
   c. lakeformation.ListPermissions(Principal={DataLakePrincipalIdentifier: "IAM_ALLOWED_PRINCIPALS"}, Resource={Table:{DatabaseName:<db>, Name:<table>}}) and confirm PrincipalResourcePermissions is empty.
   d. lakeformation.RevokePermissions(Principal={DataLakePrincipalIdentifier: "IAM_ALLOWED_PRINCIPALS"}, Resource={Table:{DatabaseName:<db>, Name:<table>}}, Permissions=["ALL"]).
3. Expect HTTP 400, error code InvalidInputException, message "No permissions revoked. Grantee does not does not have:[ALL]" (bracketed list = the request's Permissions). Observe the same status and code with the message "No permissions revoked. Grantee has no permissions and no grantable permissions on resource.".
4. Regression guard for the other class: grant IAM_ALLOWED_PRINCIPALS ["ALL"] on the same table twice (ListPermissions must then show exactly one ["ALL"] entry), revoke once (ListPermissions empty again), and revoke a second time — that second revoke must again produce the per-permission sentence; the grantee-holds-nothing sentence must still be produced for an IAM role principal that was never granted anything.
