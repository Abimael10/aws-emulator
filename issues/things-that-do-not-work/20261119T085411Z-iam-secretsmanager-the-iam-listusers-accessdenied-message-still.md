Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol
Issue-Key: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol|run#16|AssertionError|assert iam_msg == want, f"IAM denial message: expected {want!r}, got {iam_msg!r}"
Issue-Commit: e4a7a3b48c24597c7ccc34be8282569ef10bbe11

# iam__secretsmanager: the iam:ListUsers AccessDenied message still omits AWS's GetRequestAuthorizationDetails pointer sentence

> _Note: this issue was identified against the code base at commit `e4a7a3b48c24597c7ccc34be8282569ef10bbe11`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListUsers

## Description
REOPENS 20261117T135333Z-iam-secretsmanager-iam-accessdenied-message-omits-the-getrequestauthorizationdetails-pointer-iam.md: its fix left the call refused. That issue is marked fixed and carries this check's replay key, but iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol still fails on the same iam:ListUsers refusal with the same truncated message, so this is the same cause un-fixed rather than a new defect. The same truncation is reproduced across the whole `### iam` heading - see the companion issue filed in this sweep for service `iam` (26 checks) - so a fix belongs in the shared IAM denial-message builder, not in this cross-protocol check alone.

=== Cause 1 of 1: the AccessDenied message for a signed, unauthorized iam:ListUsers ends at '... action' and omits the authorization-details pointer sentence ===

AWS: IAM speaks awsQuery (service com.amazonaws.iam#AWSIdentityManagementV20100508, version 2010-05-08). Smithy: ListUsers models only ServiceFailureException (awsQueryError ServiceFailure, HTTP 500); the authorization refusal AccessDenied / HTTP 403 is a service-common error whose message text is not modeled, so the message requirement is recorded behavior only. The recorded refusal is two sentences: `User: <caller-arn> is not authorized to perform: iam:ListUsers on resource: <resource-description> because no identity-based policy allows the iam:ListUsers action` followed by `. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>`. Fixed text: both sentence skeletons including the literal us-east-1 console host and the `region=us-east-1` query parameter (IAM is global and signed in us-east-1). Variable parts, taken from the signed request and the emulator's state: `<caller-arn>` (the principal whose key signed the call), `<resource-description>` (how IAM names the evaluated resource for ListUsers - the recorded form is the path-prefixed user ARN, e.g. `arn:aws:iam::<account>:user/<path>/`), and `<authorization-id>` (the per-request authorization id, read back out of the message by the check and therefore never asserted).

Emulator: the code and status are right (403 / AccessDenied) and the first sentence is right; the message then ends, so the pointer sentence and the authorization id are both absent. Observed: `User: arn:aws:iam::<account>:user/<user> is not authorized to perform: iam:ListUsers on resource: arn:aws:iam::<account>:user/<path>/ because no identity-based policy allows the iam:ListUsers action`. The check's expected value renders the missing tail as `Go to <authorization-details pointer>`, i.e. the marker is not present in the received message at all.

Why: this check exists to compare how one denial is shaped across protocol families (awsQuery IAM vs Secrets Manager's JSON protocol). A client that reads the IAM denial text to follow the console authorization-details link, or to extract the authorization id for GetRequestAuthorizationDetails, finds neither, and a cross-protocol comparison of the two denial bodies is made against a message AWS never emits.

Fix: the shared authorization-refusal denial-message builder in the IAM crate (the component rendering AccessDenied bodies for iam operations, and the one the fix for 20261117T135333Z-iam-secretsmanager-... was meant to change). It must append the pointer sentence for refusals AWS records with it, with the console host and region query parameter pinned to the literal us-east-1 and the authorization id repeated in both positions. The Secrets Manager side of the check is not implicated.

Traces: iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol (assertion `assert iam_msg == want` at check line 114).

## Evidence
Check iam__secretsmanager.xsvc-iam-2-secretsmanager-access-denied-status-code-differs-by-protocol, verdict mismatch:

AssertionError: IAM denial message: expected 'User: arn:aws:iam::<account>:user/sim2real-xsvc-iamsm-denied-<id> is not authorized to perform: iam:ListUsers on resource: arn:aws:iam::<account>:user/ because no identity-based policy allows the iam:ListUsers action. Go to <authorization-details pointer>', got 'User: arn:aws:iam::<account>:user/sim2real-xsvc-iamsm-denied-<id> is not authorized to perform: iam:ListUsers on resource: arn:aws:iam::<account>:user/ because no identity-based policy allows the iam:ListUsers action'

EXPECTED (AWS): HTTP 403, Error.Code AccessDenied, Error.Message = '<deny-reason sentence>. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>'.
ACTUAL (emulator): HTTP 403, Error.Code AccessDenied, Error.Message = the deny-reason sentence only; the pointer sentence is absent, which is why the check prints its '<authorization-details pointer>' placeholder.

The account id, user name suffix and authorization id are per-run values filling the template; the two sentences' fixed text and the us-east-1 host/region literal are the requirement. The error code, the 403 status and the first sentence all match - the divergence is exactly the missing second sentence.

## Steps to Reproduce
1. iam CreateUser(UserName=<u>) and iam CreateAccessKey(UserName=<u>); give <u> an identity policy that does NOT allow iam:ListUsers (the check also grants it a Secrets Manager action so the two protocol families can be compared).
2. Build an iam client signing with that access key alone (no session token).
3. Call iam ListUsers(PathPrefix='/<something>/') with it.
4. Observe HTTP 403 and Error.Code 'AccessDenied' (both correct) and Error.Message ending at '... because no identity-based policy allows the iam:ListUsers action'.
5. AWS's recorded message continues with '. Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1#/authorization-details/<authorization-id> for complete details, or call the GetRequestAuthorizationDetails API with the following authorization id: <authorization-id>'; the emulator emits no such sentence and no authorization id.
