Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object
Issue-Key: cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object|_get_until#21|Timeout|time.sleep(5)
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# cloudfront__s3: the invalidate-once/re-cache sequence over an OAI-protected S3 origin reaches no verdict — the distribution/edge never satisfies an awaited step and the check hangs to the 60 s cap

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
One root cause (Cause 1 of 1). Failing check: cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object.

Cause 1 of 1: a distribution created over a private S3 origin reached through a CloudFront origin access identity never reaches the state the check waits for, so the whole invalidation-semantics trajectory (first serve, /* invalidation refreshing to v2, v2 re-cached as a Hit, v3 staying unseen without a second invalidation, /index.html invalidation serving v3) is never graded. The run ends inside an awaited step at the suite's 60 s cap with no response or state transition observed.

AWS:      recorded behavior only for the edge legs (no Smithy model covers the CloudFront data plane; the control-plane shapes used are CreateDistribution/GetDistribution.Distribution.Status and CreateInvalidation/GetInvalidation.Invalidation.Status). On AWS the recording shows: GetDistribution advances the created distribution from 'InProgress' to 'Deployed'; a GET of '/' at the distribution's published DomainName then returns 200 with the DefaultRootObject (index.html) body; CreateInvalidation with Paths {Quantity:1, Items:['/*']} reaches Invalidation.Status 'Completed'; the next GET returns 200 with the UPDATED object bytes and the refetched object is cached again under the managed CachingOptimized policy (658327ea-f89d-4fab-a63d-7e88639e58f6), so a following GET answers 200 with an X-Cache value beginning 'Hit'; a third origin write with NO new invalidation is NOT seen (three successive GETs all return 200 with the previously cached bytes); and only a second invalidation, of '/index.html', makes the edge serve the newest bytes. The requirement is that sequence of statuses, bodies and cache outcomes; the generated bucket name, distribution id, invalidation ids and the page text of one run are not.
Emulator: the check produces no assertion result — the replay detail is 'Error: Check timed out after 60.0s', verdict mismatch. Every step it blocks on is an awaited effect on the emulator's side: _wait_deployed (GetDistribution until Status=='Deployed', declared deadline 900 s), _get_until (a bounded GET poll of the distribution's DomainName, declared deadline 300 s) and _wait_completed (GetInvalidation until Status=='Completed', declared deadline 300 s). None of them is satisfied, and the per-request GET timeout in the check is 20 s, so the budget is spent waiting on the emulator, not in the runner.
Why:      invalidation is the only mechanism a client has to publish a changed object through CloudFront. If a distribution never becomes Deployed, or its edge never answers, or an invalidation never completes, then a deploy pipeline that writes to S3 and invalidates can never observe its content going live, and the critical negative half of the contract — that one invalidation does NOT keep the object uncached, so every later change needs its own invalidation — is unobservable, so a client cannot tell a correct pipeline from a broken one.
Fix:      the CloudFront distribution lifecycle/state machine that must advance a created distribution from InProgress to Deployed, the edge data plane that must serve the DefaultRootObject from an OAI-granted private S3 origin at the distribution's published DomainName (and re-cache a refetched object under the managed CachingOptimized policy), and the CreateInvalidation/GetInvalidation state that must reach Status 'Completed' and evict exactly once.
Related, NOT covering: open issue 20261117T143723Z-cloudfront-s3-6-net-new-causes-a-distribution.md describes a distribution that never leaves 'InProgress' (its Cause 1, with siblings ending in the same bare 'Check timed out after 60.0s'), and open issues 20261102T102137Z-cloudfront-s3-four-causes-the-s3-origin-edge.md, 20261024T182555Z-cloudfront-s3-distributions-over-s3-origins-serve-nothing.md, 20261109T181319Z-cloudfront-s3-the-s3-origin-edge-is-unreachable.md and 20261110T153718Z-cloudfront-s3-the-oac-oai-granted-s3-origin.md describe the OAI/OAC-granted S3-origin edge serving nothing. None of them carries this check's replay key — the replay reported this check as neither OWNED nor ALREADY FILED — so this check's trajectory (invalidate-once semantics, including the no-second-invalidation negative) would stay unverified if they were fixed and closed.
Traces:   cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 531/702 passed (75.6%); bootstrap_signal false. Under '### cloudfront__s3': 19/33 (58%), 14 failed; 13 of those are OWNED/ALREADY FILED, this one is not.

  [mismatch] cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object
    Error: Check timed out after 60.0s

Actual: no assertion result. The check blocks in one of its awaited steps and is killed at the cap. Its declared deadlines are _DEPLOYED_DEADLINE_S=900 (GetDistribution -> 'Deployed'), _EDGE_DEADLINE_S=300 (GET '/' at the DomainName) and _INVALIDATION_DEADLINE_S=300 (GetInvalidation -> 'Completed'); each alone exceeds the 60 s the run got, and nothing in the sequence converged.

Expected (AWS), per the check's contract, in order:
  1. GetDistribution(Id).Distribution.Status becomes 'Deployed' for a distribution whose single origin is '<bucket>.s3.<region>.amazonaws.com' with S3OriginConfig.OriginAccessIdentity 'origin-access-identity/cloudfront/<oai id>', ViewerProtocolPolicy 'redirect-to-https', CachePolicyId 658327ea-f89d-4fab-a63d-7e88639e58f6 and DefaultRootObject 'index.html'.
  2. GET '/index.html' at the published DomainName -> 200 with the seeded v1 body.
  3. PutObject v2, then CreateInvalidation(Paths {Quantity:1, Items:['/*']}) -> Invalidation.Status 'Completed'; the next GET -> 200 with the v2 body (not v1).
  4. A following GET -> 200 with the v2 body and an X-Cache header starting with 'Hit' (the refetched object is cached again).
  5. PutObject v3 with NO new invalidation: three successive GETs all -> (200, v2 body).
  6. CreateInvalidation(Paths {Quantity:1, Items:['/index.html']}) -> 'Completed'; the next GET -> 200 with the v3 body.

The bucket is private (public access blocked, versioned, SSE-S3) and its policy grants s3:GetObject only to 'arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <oai id>' plus a Deny on aws:SecureTransport=false, so the edge is the only path to the bytes. Not a runner fault: each GET carries a 20 s timeout and the check follows no redirects.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout; replay the behavioral suite simparity-behavioral-aws-v1.
2. Create a private, versioned, SSE-S3 bucket with all public access blocked and seed index.html ('Version 1.0'), page1.html, page2.html.
3. CreateCloudFrontOriginAccessIdentity, then PutBucketPolicy granting s3:GetObject to 'arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <oai id>' (plus a Deny for aws:SecureTransport=false).
4. CreateDistribution with one origin '<bucket>.s3.<region>.amazonaws.com', S3OriginConfig.OriginAccessIdentity 'origin-access-identity/cloudfront/<oai id>', ViewerProtocolPolicy 'redirect-to-https', CachePolicyId 658327ea-f89d-4fab-a63d-7e88639e58f6, DefaultRootObject 'index.html'; poll GetDistribution until Status == 'Deployed'.
5. GET '/index.html' at the distribution's published DomainName and expect 200 with the v1 body.
6. PutObject index.html = v2, CreateInvalidation Paths ['/*'], poll GetInvalidation until Status == 'Completed', then GET again and expect 200 with v2; GET once more and expect 200 with v2 and X-Cache starting 'Hit'.
7. PutObject index.html = v3 with no invalidation and GET three times: expect (200, v2) each time.
8. CreateInvalidation Paths ['/index.html'], wait for 'Completed', then GET and expect 200 with v3.
9. Observed: the run never gets past one of the awaited steps and is killed at 'Check timed out after 60.0s' with no verdict.
10. Re-replay cloudfront__s3.invalidation-refreshes-once-and-the-edge-re-caches-the-updated-object to confirm the fix.
