Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.ddb-ltr-resource-policy-naming-a-nonexistent-principal-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.ddb-ltr-resource-policy-naming-a-nonexistent-principal-rejected|run#12|AssertionError|assert (code, message) == ("ValidationException", expected_invalid), (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# dynamodb: PutResourcePolicy's invalid-principal refusal is emulator-authored — it interpolates the offending Principal entry instead of AWS's fixed 'Invalid principal in policy document.' sentence

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutResourcePolicy

## Description
Cause 1 of 1: the refusal of a table resource policy whose Principal names an IAM user that does not exist carries the wrong message text.

AWS: Smithy (dynamodb, DynamoDB_20120810): PutResourcePolicy{ResourceArn (required, length 1..1283), Policy (required), ExpectedRevisionId, ConfirmRemoveSelfResourceAccess} -> {RevisionId}; modeled errors InternalServerError (500), InvalidEndpointException (421), LimitExceededException (400), PolicyNotFoundException (400), ResourceInUseException (400), ResourceNotFoundException (400), plus the service-common ValidationException. A policy whose Principal.AWS names an IAM user that does not yet exist is refused with HTTP 400, error code ValidationException, and the message "One or more parameter values were invalid: Invalid principal in policy document." — that string is AWS's FIXED text in its entirety: it has no variable part, and in particular AWS does NOT quote or echo back the offending Principal key or ARN. Nothing is attached, so a following GetResourcePolicy on the same ResourceArn answers PolicyNotFoundException with "Resource-based policy not found for the provided ResourceArn: <the ResourceArn from the request>" (fixed text plus the request's ARN). The identical PutResourcePolicy then succeeds once the named user exists.

Emulator: the error CODE and status are right (400 ValidationException) but the message is a different sentence that also interpolates request data AWS does not: 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: "AWS":"<the principal ARN from the request>"'. The fixed part the emulator emits ("Invalid policy document: Invalid principal in policy: ") is not AWS's wording, and the trailing '"AWS":"<arn>"' is a value taken from the submitted policy document, which has no place in this message at all.

Why: the message is a requirement of this error class. Clients that branch on AWS's wording never match — including the standard retry loop that re-issues PutResourcePolicy while IAM's own eventual consistency still reports the just-created principal as unresolvable, which is keyed on this text. The emulator also leaks the submitted Principal entry into an error string AWS keeps value-free. Two distinct refusal classes (an unresolvable principal vs. a structurally invalid policy document) are additionally collapsed under the emulator's shared "Invalid policy document:" prefix, so a caller cannot tell them apart.

Fix: PutResourcePolicy's policy-document validation in the dynamodb service — the message emitted by its principal-existence check (it must be the fixed sentence with no interpolation).

Related open issue: 20261108T051530Z-dynamodb-6-net-new-resource-policy-causes-putresourcepolicy.md lists this check as its Cause 1 of 6, but for the opposite defect — there, PutResourcePolicy ACCEPTED a policy naming a nonexistent principal and returned a RevisionId. The call is now refused, so that cause's symptom and its replay key no longer match this check (the suite no longer reports it as owned by that issue), and what remains is purely the wording divergence described here. That issue does not cover it; the other five causes it carries are untouched by this one.

Traces: dynamodb.ddb-ltr-resource-policy-naming-a-nonexistent-principal-rejected (mismatch).

## Evidence
Replay output for dynamodb.ddb-ltr-resource-policy-naming-a-nonexistent-principal-rejected:

  AssertionError: PutResourcePolicy naming the not-yet-created user 'arn:aws:iam::<account>:user/<ghost-user>' answered ('ValidationException', 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: "AWS":"arn:aws:iam::<account>:user/<ghost-user>"'), expected ('ValidationException', 'One or more parameter values were invalid: Invalid principal in policy document.')

Expected (real AWS): HTTP 400, code ValidationException, message exactly "One or more parameter values were invalid: Invalid principal in policy document." — all fixed text, nothing interpolated.
Actual (emulator): HTTP 400, code ValidationException (correct), message 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: "AWS":"<principal ARN>"' — a different fixed prefix plus the Principal key and ARN copied out of the submitted policy document.

The check submits, against an ACTIVE PAY_PER_REQUEST table's TableArn, the document {"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:user/<user-not-yet-created>"},"Action":["dynamodb:GetItem","dynamodb:PutItem"],"Resource":"<TableArn>"}]}. It then asserts GetResourcePolicy answers PolicyNotFoundException with "Resource-based policy not found for the provided ResourceArn: <TableArn>" (nothing attached), creates the user, and asserts the identical put now succeeds and reads back with the RevisionId the put returned — the run stops at the message comparison before reaching those legs.

## Steps to Reproduce
1. CreateTable with BillingMode=PAY_PER_REQUEST, one HASH key attribute 'id' (S); wait for ACTIVE and take Table.TableArn.
2. Pick an IAM user name that does NOT exist and build principal = arn:aws:iam::<account>:user/<that-name>.
3. Call PutResourcePolicy(ResourceArn=<TableArn>, Policy=json of {"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":principal},"Action":["dynamodb:GetItem","dynamodb:PutItem"],"Resource":"<TableArn>"}]}).
4. Compare the refusal byte-for-byte: expect 400 ValidationException with message "One or more parameter values were invalid: Invalid principal in policy document."; observe 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: "AWS":"<principal>"'.
5. Then GetResourcePolicy(ResourceArn=<TableArn>) -> expect PolicyNotFoundException "Resource-based policy not found for the provided ResourceArn: <TableArn>" (nothing attached).
6. CreateUser with that name and repeat step 3 -> it must succeed, and GetResourcePolicy must read back the RevisionId the put returned.
Or simply re-replay the behavioral check dynamodb.ddb-ltr-resource-policy-naming-a-nonexistent-principal-rejected.
