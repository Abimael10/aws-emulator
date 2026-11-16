Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__s3__sts.assume-role-denied-when-trust-policy-omits-lambda-role
Issue-Key: iam__lambda__s3__sts.assume-role-denied-when-trust-policy-omits-lambda-role|run#23|AssertionError|assert ghost_code == _UNRESOLVED_PRINCIPAL, (
Issue-Commit: 5bda3f4959ddaaecd7545612991d55d37567ef10

# iam__lambda__s3__sts: 2 net-new causes — UpdateAssumeRolePolicy accepts a trust document naming an unresolvable role principal, and AssumeRole credentials vended inside a function are EMUL-class not ASIA

> _Cause 1 (UpdateAssumeRolePolicy / CreateRole accepting a trust document whose `AWS` principal names an IAM role or user this account does not hold) is FIXED, and its key and trace are the ones this report now carries. Cause 2 (the `ASIA` access-key class) is NOT fixed: it cannot be served from a service crate, so it was split out with its own replay key into `issues/things-that-do-not-work/20261115T051545Z-sts-assumerole-inside-a-function-still-vends-an-emul-class-accesskeyid.md`, which carries the analysis and the core change it needs. Cause 2 below is kept for context; the split report, not this one, is its filing._

> _Note: this issue was identified against the code base at commit `5bda3f4959ddaaecd7545612991d55d37567ef10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAssumeRolePolicy

## Description
Two distinct root causes, both reached from the cross-service IAM/Lambda/S3/STS checks. Neither failing check is marked ALREADY FILED or OWNED by an open issue.

Cause 1 of 2 — IAM UpdateAssumeRolePolicy accepts a trust policy whose AWS principal cannot be resolved (failing check: iam__lambda__s3__sts.assume-role-denied-when-trust-policy-omits-lambda-role).
AWS: Smithy (service-com-amazonaws-iam-awsidentitymanagementv20100508): UpdateAssumeRolePolicy takes RoleName + PolicyDocument and models MalformedPolicyDocumentException (awsQueryError code MalformedPolicyDocument, httpError 400) alongside NoSuchEntity/LimitExceeded/UnmodifiableEntity. IAM enforces referential integrity of a trust document's principals at write time: a document whose Statement[].Principal.AWS names a role ARN that does not exist is refused with HTTP 400, Error.Code MalformedPolicyDocument — not NoSuchEntity and not InvalidParameterValue, because the DOCUMENT is judged malformed. The message's fixed text is "Invalid principal in policy: " followed by the principal type and the principal value exactly as supplied in the request, i.e. Invalid principal in policy: "AWS":"<the ARN from Statement[].Principal.AWS>"; the ARN is the only variable part.
Emulator: the call is accepted and returns success — the check's probe for an error code got None. The stored trust document therefore lists a principal that does not exist, and the role's trust list is fiction.
Why: a client that relies on IAM to validate trust documents never learns its policy is wrong. Worse, the accepted-but-unresolvable trust list is what the subsequent AssumeRole denial test depends on, so the emulator silently builds a permission graph that cannot exist in AWS — the exact overly-permissive shape this check class exists to catch — and an IaC pipeline whose trust document contains a typo deploys clean here and fails in AWS.
Fix: IAM's UpdateAssumeRolePolicy handler and the shared assume-role-policy validation it calls (also reached by CreateRole), which must resolve every Statement[].Principal.AWS entry against account state and raise MalformedPolicyDocumentException (400) with the message above when it does not resolve.
Traces: iam__lambda__s3__sts.assume-role-denied-when-trust-policy-omits-lambda-role.

Cause 2 of 2 — temporary credentials minted by STS AssumeRole are not in the ASIA access-key class (failing check: iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket).
AWS: STS AssumeRole returns Credentials.AccessKeyId; the Smithy model puts no pattern on it (recorded behavior only, plus AWS's documented access-key-id prefixes: long-term IAM user keys begin AKIA, STS temporary keys begin ASIA). Every AccessKeyId handed back by AssumeRole therefore begins with the four characters ASIA, and callers (and the emulator's own authorizer) use that class to tell a temporary credential from a long-term one.
Emulator: an in-handler boto3 sts:AssumeRole inside a Lambda returns an AccessKeyId whose first four characters are 'EMUL' — an emulator-authored class that exists in no AWS partition.
Why: any client that classifies a credential by its key-id prefix (SDK tooling, credential-scrubbers, policies conditioned on temporary vs long-term identities, and the emulator's own session lookups) misclassifies the session. This is the same defect family as the open issues 20261102T105139Z-sts-assumerole-mints-temporary-credentials-with-an-emulator.md and 20261027T130805Z-sts-5-net-new-behavioral-root-causes-temporary.md, neither of which the suite reports as owning this check; it is recorded here as reached through the Lambda in-handler AssumeRole path.
Fix: the STS AssumeRole handler's credential minting (the access-key-id generator in the STS session/credential state), so the issued key id is ASIA-classed and the authorizer recognises it.
Traces: iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket.

## Evidence
Cause 1: check iam__lambda__s3__sts.assume-role-denied-when-trust-policy-omits-lambda-role, line 480 — AssertionError: "UpdateAssumeRolePolicy must refuse a trust policy naming a role ARN that does not exist, with MalformedPolicyDocument -- a malformed DOCUMENT, not a NoSuchEntity -- got None (None means the document was accepted and the trust list is fiction)". Expected: HTTP 400, Error.Code MalformedPolicyDocument, Message 'Invalid principal in policy: "AWS":"<requested role ARN>"'. Actual: 2xx success, no error.

Cause 2: check iam__lambda__s3__sts.lambda-assumed-role-credential-reads-restricted-bucket, line 468 — AssertionError: "temporary credentials must use the ASIA access-key prefix, got 'EMUL'". The handler read Credentials.AccessKeyId[:4] from its own sts:AssumeRole response. Expected prefix: ASIA. Actual prefix: EMUL.

## Steps to Reproduce
Cause 1: create an IAM role; call iam:UpdateAssumeRolePolicy on it with a trust document whose Statement contains {"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:role/<a-role-that-does-not-exist>"},"Action":"sts:AssumeRole"}. The emulator returns success; AWS returns 400 MalformedPolicyDocument with 'Invalid principal in policy: "AWS":"<that ARN>"'.
Cause 2: create a Lambda execution role and a function whose handler calls boto3 sts:AssumeRole on a second role that trusts it, then inspect Credentials.AccessKeyId[:4] in the returned payload. The emulator yields 'EMUL'; AWS yields 'ASIA'.
