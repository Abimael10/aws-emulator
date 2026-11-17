Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol
Issue-Key: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol|run#16|AssertionError|assert iam_msg == want, f"IAM denial message: expected {want!r}, got {iam_msg!r}"
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# IAM AccessDenied message omits the GetRequestAuthorizationDetails pointer (iam:ListUsers, cross-protocol denial check)

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListUsers

## Description
Cause 1 of 1: the IAM denial served alongside the Secrets Manager denial has no authorization-details tail.

AWS: recorded behavior only for the message text (no model carries it). An IAM AccessDenied message ends with `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>` — fixed text apart from the region and the per-request authorization id, which appears twice and must be identical in both places. The check grades every other character of the message, and the leading sentence (`User: <principal-arn> is not authorized to perform: iam:ListUsers on resource: <resource> because no identity-based policy allows the iam:ListUsers action`) is already correct here.

Emulator: the message stops after that leading sentence; no `. Go to ...` tail is emitted. The check compares the IAM and Secrets Manager denials protocol by protocol and fails on the IAM one.

Why: the authorization id is how a client or operator resolves why the request was denied; without it the denial cannot be traced, and clients that assert on the AWS message (or display the pointer) diverge.

Fix: the IAM authorization denial serializer — mint the per-request authorization id and append the pointer sentence to every AccessDenied message (the same defect reported for other checks in the open issue 20261109T163411Z-iam-3-net-new-causes-denials-omit-the.md, which does not carry this check).

Traces: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol.

## Evidence
Expected: `'User: arn:aws:iam::000000000000:user/<user> is not authorized to perform: iam:ListUsers on resource: arn:aws:iam::000000000000:user/ because no identity-based policy allows the iam:ListUsers action. Go to <authorization-details pointer>'`
Actual: `'User: arn:aws:iam::000000000000:user/<user> is not authorized to perform: iam:ListUsers on resource: arn:aws:iam::000000000000:user/ because no identity-based policy allows the iam:ListUsers action'` — the `. Go to https://...#/authorization-details/<id> ... authorization id: <id>` tail is missing.

## Steps to Reproduce
1. Create a user with an access key and a policy that allows neither iam:ListUsers nor secretsmanager:GetSecretValue.
2. Call iam:ListUsers with that key and compare the AccessDenied message against AWS's, which ends with the `. Go to https://<region>.console.aws.amazon.com/iam/home?region=<region>#/authorization-details/<id> ... authorization id: <id>` sentence.
Observed: the tail is absent.
