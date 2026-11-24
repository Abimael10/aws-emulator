Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sts.concurrent-get-federation-token-calls-produce-independent-policies
Issue-Shape-Id: com.amazonaws.sts#AWSSecurityTokenServiceV20110615
Issue-Key: sts.concurrent-get-federation-token-calls-produce-independent-policies|run#5|AssertionError|assert sm_result == _SUCCEEDED, (
Issue-Commit: a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c

# sts: a GetFederationToken inline session policy's Allow is not honored at the Secrets Manager enforcement site — the granted secretsmanager:ListSecrets is refused AccessDeniedException "no identity-based policy allows"

> _Note: this issue was identified against the code base at commit `a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFederationToken

## Description
REOPENS 20261029T120000Z-sts-3-of-the-8-session-scoping-causes-remain-open.md: its fix left the call refused. (That report's cause 1 was closed by making the Secrets Manager request-authorization path CAP a request by the credential's session scope — see the preamble of open issue 20261103T090000Z-sts-a-temporary-credentials-class-is-not-read-anywhere.md, "a session policy now caps secretsmanager:* there". Capping now happens, but it also denies the action the session policy explicitly ALLOWS.)

Cause 1 of 1: credentials minted by GetFederationToken with an inline session policy whose only statement is Allow secretsmanager:ListSecrets on Resource "*" are refused that very action by Secrets Manager. The session scope is evaluated as an empty / deny-all document set at that enforcement site, and the deciding boundary is additionally mis-reported as the principal's identity policies. Failing check: sts.concurrent-get-federation-token-calls-produce-independent-policies.

AWS: Smithy com.amazonaws.sts#GetFederationTokenRequest$Policy documents the parameter as "An IAM policy in JSON format that you want to use as an inline session policy", and states the rule: "When you pass session policies, the session permissions are the intersection of the IAM user policies and the session policies that you pass" and "if you do not pass any session policies, then the resulting federated user session has no permissions." So when the calling principal's own permissions allow secretsmanager:ListSecrets and the inline session policy is {"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"secretsmanager:ListSecrets","Resource":"*"}]}, secretsmanager:ListSecrets is INSIDE the intersection and secretsmanager.ListSecrets with those credentials must succeed (HTTP 200, a SecretList key). Only actions OUTSIDE the intersection are refused, and then — for the Secrets Manager JSON protocol — with HTTP 400 / Code=AccessDeniedException and the SESSION-policy clause: `User: <FederatedUser.Arn> is not authorized to perform: secretsmanager:ListSecrets because no session policy allows the secretsmanager:ListSecrets action` (recorded behavior only for the exact refusal text; the intersection rule itself is the modelled/documented requirement quoted above). Nothing about this is per-site: a session scope that grants an action must grant it at every service that accepts the credential, exactly as it now does at S3.

Emulator: the mint is faithful and the S3 half of the same scoping rule now works (an inline Allow of s3:ListAllMyBuckets lets ListBuckets succeed — that leg of this check passes), but the Secrets Manager leg of the identical shape is refused: secretsmanager.ListSecrets under a session policy that allows it answers Code=AccessDeniedException with Message `User: <FederatedUser.Arn> is not authorized to perform: secretsmanager:ListSecrets because no identity-based policy allows the secretsmanager:ListSecrets action`. Fixed text of that message: `User: `, ` is not authorized to perform: `, ` because no identity-based policy allows the `, ` action`; its variable parts (the federated-user ARN and the action) come from the request and the emulator's state — the account id and the generated federation name in any one replay line are per-run values, not requirements. Two things diverge in it: the request is refused at all where AWS serves 200, and the attribution clause names identity-based policies where the deciding boundary AWS names is the session policy. Because the granted call is refused, the check can never observe the requirement it exists for — that six concurrently issued federation sessions keep their own policies and a sibling's grant does not leak in.

Why: a client that uses GetFederationToken to hand out deliberately down-scoped credentials gets credentials that can read nothing from Secrets Manager, so a least-privilege secret-broker flow — the canonical use of federation tokens — is unusable against the emulator even though its S3 twin works. The wrong clause compounds it: an operator reads "no identity-based policy allows ..." and widens the IAM user's identity policy, which cannot help, because the grant being ignored is the session policy itself.

Fix: Secrets Manager's request-authorization path (the per-request gate in the Secrets Manager crate that reads the caller's session scope) must evaluate the session scope as an INTERSECTION with the caller's identity policies rather than as a deny-all cap — an Allow present in both authorizes the request; only an absent/empty session scope denies everything. The same path's implicit-deny message construction must select the session-policy clause when the session scope is the deciding boundary. The resolution of the scope is shared, so this belongs beside the already-working S3 reading of it rather than as a Secrets-Manager-only rule.

Related open issues that do NOT cover this and do not carry this check's current replay key: 20261114T014646Z-sts-a-getfederationtoken-inline-session-policy-s-allow.md claimed only the S3 half and states explicitly that the per-session isolation / secretsmanager:ListSecrets requirement "is NOT claimed here because the replays never reached them" — the replay now reaches it; 20261113T073604Z-sts-a-getfederationtoken-session-policy-refusal-is-attributed.md covers the identity-based-vs-session clause at the S3 site only; 20261103T090000Z-sts-a-temporary-credentials-class-is-not-read-anywhere.md covers the credential-CLASS rules (policy-less session, IAM ban, aws:PrincipalType), not an honored Allow.

Traces: sts.concurrent-get-federation-token-calls-produce-independent-policies (three of its six concurrent sessions carry Allow secretsmanager:ListSecrets; each is refused the granted action).

## Evidence
Check sts.concurrent-get-federation-token-calls-produce-independent-policies [mismatch].

Expected (AWS): resp = sts.get_federation_token(Name=<name>, Policy=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"secretsmanager:ListSecrets","Resource":"*"}]})); a Secrets Manager client built from resp["Credentials"] calling list_secrets() returns HTTP 200 (the action is in the intersection of the caller's policies and the session policy). The sibling action (s3:ListAllMyBuckets) is then refused HTTP 403 / Code=AccessDenied with `... because no session policy allows the s3:ListAllMyBuckets action`, and symmetrically an s3-granted session is refused ListSecrets with Code=AccessDeniedException and `... because no session policy allows the secretsmanager:ListSecrets action`.

Actual (emulator): the granted call is refused. Replay output:
  File "check_sts_concurrent_get_federation_token_calls_produce_independent_policies.py", line 62, in run
    assert sm_result == _SUCCEEDED, (
  AssertionError: session policy granted 'secretsmanager:ListSecrets': expected ListSecrets to succeed, got ('AccessDeniedException', 'User: arn:aws:sts::000000000000:federated-user/fed-45da58f5b2 is not authorized to perform: secretsmanager:ListSecrets because no identity-based policy allows the secretsmanager:ListSecrets action') -- a sibling call's grant must not leak in

(The account id and the federation name fed-<hex> above are per-run values filling the message template. What diverges as a requirement is (a) that an action ALLOWED by the session policy is refused at all, and (b) that the clause reads ` because no identity-based policy allows the ` where AWS reads ` because no session policy allows the `.)

Passing legs that precede the divergence, so neither the mint nor S3's reading of the scope is the defect: all six concurrent GetFederationToken calls return Credentials and a FederatedUser.Arn of the form arn:aws:sts::<account>:federated-user/<Name>, and the three sessions granted s3:ListAllMyBuckets do succeed at S3 ListBuckets (this same check failed at line 53 on that S3 leg before; it now reaches line 62).

## Steps to Reproduce
1. Boot the emulator and replay sts.concurrent-get-federation-token-calls-produce-independent-policies.
2. Or by hand, with credentials whose own permissions allow sts:GetFederationToken, secretsmanager:ListSecrets and s3:ListAllMyBuckets: confirm the ambient caller can call secretsmanager.list_secrets() (positive control).
3. resp = sts.get_federation_token(Name=<<=32 chars>, Policy=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"secretsmanager:ListSecrets","Resource":"*"}]})).
4. Build a Secrets Manager client from resp["Credentials"] (AccessKeyId / SecretAccessKey / SessionToken) and call list_secrets(). AWS returns 200 with SecretList; the emulator returns Code=AccessDeniedException, Message `User: <FederatedUser.Arn> is not authorized to perform: secretsmanager:ListSecrets because no identity-based policy allows the secretsmanager:ListSecrets action`.
5. Repeat step 3 with Allow s3:ListAllMyBuckets instead and call s3.list_buckets() with those credentials — that one succeeds, showing the scope is read as a grant at S3 but not at Secrets Manager.
6. With the s3-granted session, call secretsmanager.list_secrets(): it is correctly refused, but read the clause — it says `no identity-based policy allows` where AWS says `no session policy allows`.
7. Issue six tokens concurrently (three per action) as the check does to confirm the refusal is not a concurrency artifact: every secretsmanager-granted session is refused its granted action.
