Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__rds__secretsmanager.lambda-rds-missing-permission-denied lambda__rds__secretsmanager.cli-lambda-invoke-denied-without-rds-policy-negative lambda__rds__secretsmanager.deploy-cfn-lambda-rds-missing-permission-negative lambda__rds__secretsmanager.lambda-rds-missing-describedbinstances-permission-negative lambda__rds__secretsmanager.lambda-rds-wrong-db-instance-arn-denied
Issue-Key: lambda__rds__secretsmanager.lambda-rds-missing-permission-denied|run#9|AssertionError|assert denied_result["ok"] is False, f"expected the ungranted role to be denied, got {denied_result}"
Issue-Key: lambda__rds__secretsmanager.cli-lambda-invoke-denied-without-rds-policy-negative|run#27|AssertionError|assert body["rds_success"] is False, "RDS call should have been denied"
Issue-Key: lambda__rds__secretsmanager.deploy-cfn-lambda-rds-missing-permission-negative|run#6|AssertionError|assert denied["ok"] is False, f"expected the RDS call to be denied, got {denied!r}"
Issue-Key: lambda__rds__secretsmanager.lambda-rds-missing-describedbinstances-permission-negative|run#6|AssertionError|assert "AccessDenied" in denied["code"], f"expected AccessDenied error code, got {denied['code']!r}"
Issue-Key: lambda__rds__secretsmanager.lambda-rds-wrong-db-instance-arn-denied|run#17|AssertionError|assert denied_result["code"] == "AccessDenied", (
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# lambda__rds__secretsmanager: 2 net-new causes — an in-function RDS DescribeDBInstances call is not authorized against the execution role at all (succeeds ungranted), and a resource-scoped grant is bypassed so the refusal is DBInstanceNotFound instead of AccessDenied

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstances

## Description
Two net-new root causes, both grounded in failing checks of the lambda__rds__secretsmanager suite group; neither is marked ALREADY FILED or OWNED in the replay report. Both are over-permissiveness: the RDS control plane does not evaluate the calling identity's policy before serving the request.

Cause 1 of 2: an RDS read issued from a Lambda whose execution role grants no rds actions SUCCEEDS instead of being denied.
AWS: every RDS control-plane call is authorized against the caller's identity-based policy before the operation runs. A function running as an assumed role with no rds:DescribeDBInstances grant has DescribeDBInstances refused — the RDS Query protocol answers HTTP 403 with error code `AccessDenied` and a message of the form `User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: rds:DescribeDBInstances` (the fixed text is `User: ... is not authorized to perform: rds:DescribeDBInstances`; the principal ARN and session name come from the invoking identity). No instance data is returned. Smithy: DescribeDBInstances is an RDS awsQuery operation whose output is DBInstanceMessage; authorization is a service-wide precondition, so the denial is the service's common access-denied error rather than a modeled per-operation error. The checks assert the code and that no metadata is returned.
Emulator: the ungranted role's call is served successfully — the handler returns `{'ok': True, 'count': 0}`, i.e. a 200 DBInstanceMessage with an empty DBInstances list, and the same sibling check sees the call it expected to be denied simply succeed. The identity's policy is never consulted; a role with zero statements is treated exactly like a role granting rds:DescribeDBInstances (the granted positive control in the same check also succeeds, so the two are indistinguishable).
Why: least-privilege is unenforceable and untestable. A client cannot verify that a function is confined to the RDS actions it was granted, a missing grant produces no failure in test and a real failure only in production, and a negative test that asserts a denial passes silently with a 200.
Fix: the identity-based authorization gate in front of the RDS control-plane handlers (DescribeDBInstances and its siblings) — evaluate the invoking principal's inline/attached policy for the action before the operation reads state, and refuse with the service's AccessDenied error when no statement allows it.
Traces: lambda__rds__secretsmanager.lambda-rds-missing-permission-denied, lambda__rds__secretsmanager.cli-lambda-invoke-denied-without-rds-policy-negative, lambda__rds__secretsmanager.deploy-cfn-lambda-rds-missing-permission-negative.

Cause 2 of 2: when the request names a DB instance outside the grant's Resource (or the role has no grant at all and the id does not resolve), the refusal is the resource-lookup error DBInstanceNotFound instead of AccessDenied — authorization is evaluated after, or instead of, existence.
AWS: authorization precedes resource resolution. A role whose rds:DescribeDBInstances statement has `Resource: <arn of instance A>` can describe A but is refused `AccessDenied` when the request names instance B, regardless of whether B exists; and a role with no rds grant at all is refused `AccessDenied` even when the DBInstanceIdentifier is a placeholder that does not exist. `DBInstanceNotFound` (message `DBInstance <identifier> not found.`, with the identifier taken from the request) is what AWS returns only ONCE the caller is authorized and the named instance genuinely does not exist — it is the positive control these checks use to prove the earlier refusal was the missing permission.
Emulator: both the out-of-scope ARN case and the no-grant-plus-placeholder-id case are answered `DBInstanceNotFound`. The emulator goes straight to the instance lookup, so the authorized and the unauthorized outcome collapse onto the same error code and the resource-scoped grant has no effect.
Why: a caller cannot distinguish "you may not ask about this instance" from "this instance does not exist" — the two have opposite remediations — and resource-scoped RDS policies cannot be validated at all, because the deny they are written to produce never appears. It also leaks existence information in the authorized direction and hides it in the unauthorized one.
Fix: the same authorization gate as cause 1, ordered BEFORE the DBInstanceIdentifier lookup in the DescribeDBInstances handler, and evaluating the statement's Resource against the ARN of the instance named in the request.
Traces: lambda__rds__secretsmanager.lambda-rds-missing-describedbinstances-permission-negative, lambda__rds__secretsmanager.lambda-rds-wrong-db-instance-arn-denied.

## Evidence
Cause 1 — expected the ungranted role's RDS call to be refused with code `AccessDenied` and no data; got a success:
`AssertionError: expected the ungranted role to be denied, got {'ok': True, 'count': 0}` (lambda__rds__secretsmanager.lambda-rds-missing-permission-denied; handler body is `boto3.client('rds').describe_db_instances(MaxRecords=20)` under a role created with `statements=[]`)
`AssertionError: expected the RDS call to be denied, got {'ok': True, 'count': 0}` (lambda__rds__secretsmanager.deploy-cfn-lambda-rds-missing-permission-negative)
`AssertionError: RDS call should have been denied` (lambda__rds__secretsmanager.cli-lambda-invoke-denied-without-rds-policy-negative)

Cause 2 — expected `AccessDenied`; got `DBInstanceNotFound`:
`AssertionError: expected AccessDenied error code, got 'DBInstanceNotFound'` (lambda__rds__secretsmanager.lambda-rds-missing-describedbinstances-permission-negative; the role has no rds grant and the handler calls describe_db_instances(DBInstanceIdentifier=<placeholder>) — AWS denies before the lookup, and only AFTER rds:DescribeDBInstances is granted answers DBInstanceNotFound with `DBInstance <identifier> not found.`)
`AssertionError: expected error code AccessDenied, got 'DBInstanceNotFound'` (lambda__rds__secretsmanager.lambda-rds-wrong-db-instance-arn-denied; the grant's Resource is the ARN of instance A and the request names instance B, which the check created)

The generated role, function and DB-instance identifiers in these messages are per-run values; the error codes, their ordering relative to the resource lookup, and the `DBInstance <identifier> not found.` template are the requirement.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay the ids in trace_ids.
Cause 1: create two Lambda execution roles, one granting `rds:DescribeDBInstances` on `*` and one with no statements; deploy the same handler (`boto3.client('rds').describe_db_instances(MaxRecords=20)`) under each and Invoke both. Both succeed — the ungranted one must be refused with code `AccessDenied`.
Cause 2a: with a role that has NO rds grant, invoke a handler calling describe_db_instances(DBInstanceIdentifier=<a name that does not exist>). Observe `DBInstanceNotFound` instead of `AccessDenied`; then attach `rds:DescribeDBInstances` on `*` and observe that DBInstanceNotFound is the authorized answer, so the two states are indistinguishable.
Cause 2b: create DB instances A and B; give the role `rds:DescribeDBInstances` with `Resource` = A's ARN; invoke the handler for A (succeeds) and then for B. Observe `DBInstanceNotFound` instead of `AccessDenied`.
