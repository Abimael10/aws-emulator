Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3control.account-public-access-block-restrict-denies-anonymous-object-read-rejected
Issue-Shape-Id: com.amazonaws.s3control#AWSS3ControlServiceV20180820
Issue-Key: s3control.account-public-access-block-restrict-denies-anonymous-object-read-rejected|_read_account_pab#18|EgressRefused|return s3c.get_public_access_block(AccountId=account)["PublicAccessBlockConfiguration"]
Issue-Commit: fb5821f2bf5d6c104bc4cb031e578f4e9b29aed7

# s3control: the modeled {AccountId}. host prefix is unserved, so the account-level PublicAccessBlock / RestrictPublicBuckets check never reaches the emulator (1 cause)

> _Note: this issue was identified against the code base at commit `fb5821f2bf5d6c104bc4cb031e578f4e9b29aed7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPublicAccessBlock

## Description
This bundle has 1 cause; it must be closed. It is the only s3control behavioral failure in this replay that no open issue carries the replay key for — the sibling open issue 20261116T222947Z-s3control-the-account-id-host-prefix-every-operation.md describes the same front-door defect for the other 33 s3control checks and explicitly states that this check is 'already owned', but the replay reports it unowned, so it is filed here with its own check id so a fixer re-replays it.

Cause 1 of 1: the emulator announces/serves no address for the account-id-prefixed s3control authority that the API model mandates, so the SDK's compliant account-level PublicAccessBlock calls are never delivered and the RestrictPublicBuckets data-plane behaviour is never exercised.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's3-control', sigv4Name 's3'): GetPublicAccessBlock (GET /v20180820/configuration/publicAccessBlock, 200; error NoSuchPublicAccessBlockConfiguration, httpError 404) and PutPublicAccessBlock (PUT /v20180820/configuration/publicAccessBlock, 200) both carry @endpoint(hostPrefix: "{AccountId}."), and AccountId is both @httpHeader("x-amz-account-id") and @hostLabel with pattern ^\d{12}$. Every AWS SDK therefore prepends '<12-digit account id>.' to the resolved endpoint authority for these calls — against real AWS <account-id>.s3-control.<region>.amazonaws.com, against a custom endpoint <account-id>.<that authority> — and AWS answers that host with the operation's modeled body (or NoSuchPublicAccessBlockConfiguration / 404 when no account block exists). The account id is fixed only in its position; its value comes from the request's AccountId member.

Emulator: the emulator offers only the bare authority it is reached on. When the SDK applies the modeled host prefix the address is refused before any HTTP request is made: 'CapabilityNotOffered: the target serves no address for <account-id>.<endpoint-host>' / simparity.egress.EgressRefused: "egress refused: '<account-id>.<endpoint-host>' is not the endpoint under test (<endpoint-host>); an emulator run may reach only the emulator". No HTTP status, no error code and no body is produced. (The run showed host 000000000000.127.0.0.1 — the account id and the loopback authority are per-environment values, not part of the requirement; the requirement is that <AccountId>.<served authority> is served.)

Why: a client that relies on AWS behaviour gets a name-resolution/connection failure instead of a response, so the account-singleton PublicAccessBlock record cannot be read, written or deleted at all through boto3/CLI. Consequently the behaviour this check grades is unreachable: with account RestrictPublicBuckets=true an anonymous read of a public-policy bucket must be refused 403 with Code 'AccessDenied' and Message exactly 'Access Denied' (fixed text, no interpolation), the owner's signed GetObject must still succeed, GetBucketPolicy must return the byte-identical document and GetBucketPolicyStatus must still report IsPublic=true, and lifting the flag must restore the identical anonymous read (a live evaluation, not a one-time policy scrub). None of that can be verified while the control-plane front door is unreachable.

Fix: the emulator's HTTP front door — the set of authorities it announces/serves and its host-based request dispatch — must accept and route the modeled '{AccountId}.' host prefix for the s3control endpoint, validating the label against the x-amz-account-id header and the AccountId @hostLabel pattern ^\d{12}$, so a host-prefixed request lands on the same s3control PublicAccessBlock handlers as a bare-authority request. The account-level RestrictPublicBuckets enforcement in the S3 data-plane authorization path must then be exercised (see open issue 20261103T080052Z-s3control-s3-policy-enforcement-and-batch-operations-diverge.md, which covers the enforcement semantics but not this reachability defect).

Traces: s3control.account-public-access-block-restrict-denies-anonymous-object-read-rejected (the first s3control call, GetPublicAccessBlock in the check's _read_account_pab helper, is refused at address resolution).

## Evidence
Behavioral suite: 0/46 passed; s3control 0/35. Failing check s3control.account-public-access-block-restrict-denies-anonymous-object-read-rejected [mismatch]:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    File "egress.py", line 371, in resolve -> refuse(...)
    File "egress.py", line 291, in refuse -> raise EgressRefused(
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: botocore resolves the host for GetPublicAccessBlock/PutPublicAccessBlock as '<AccountId>.' + the endpoint authority (both ops carry endpoint.hostPrefix "{AccountId}.", AccountId is @hostLabel ^\d{12}$), and the endpoint answers with the modeled 200 PublicAccessBlockConfiguration payload, or 404 NoSuchPublicAccessBlockConfiguration when the account has no block; the check then proceeds to grade the anonymous read as 403 AccessDenied with Message 'Access Denied'.

Actual: nothing is served on the host-prefixed authority, so no HTTP request is made — no status, no error code, no body. The account id (000000000000) and authority (127.0.0.1) in the message are per-run values; the fixed requirement is that '<12-digit account id>.<served authority>' is served and routed to the s3control handlers.

## Steps to Reproduce
1. Clone the repo and boot the emulator; confirm the s3control endpoint answers on the bare authority (e.g. GET /v20180820/configuration/publicAccessBlock with header x-amz-account-id: <12-digit account id>).
2. With boto3, build an s3control client against the emulator endpoint and call get_public_access_block(AccountId=<account id>) — the first s3control call this check makes.
3. Observe botocore applying the modeled @endpoint(hostPrefix "{AccountId}.") and addressing '<account id>.<emulator authority>'; the request never reaches the emulator (under the harness: 'CapabilityNotOffered: the target serves no address for <account id>.<authority>').
4. After the fix the same call must route to the s3control PublicAccessBlock handlers and answer 200 with the stored configuration (or 404 NoSuchPublicAccessBlockConfiguration), and put_public_access_block(AccountId=..., PublicAccessBlockConfiguration={...RestrictPublicBuckets: true}) must take effect so an unsigned GetObject on a bucket whose policy grants '*' is refused 403 AccessDenied / 'Access Denied' while the owner's signed read, the stored policy document and GetBucketPolicyStatus IsPublic=true are unchanged, and clearing the flag restores the anonymous read.
5. Re-replay s3control.account-public-access-block-restrict-denies-anonymous-object-read-rejected to confirm.
