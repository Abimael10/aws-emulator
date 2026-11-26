Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied
Issue-Key: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied|_poll_until#32|Timeout|time.sleep(interval_s)
Issue-Commit: 590a46ff404522650c3e609eb9b73b6bb68c4301

# cloudfront__kms__s3: the OAC/SSE-KMS edge scenario reaches no verdict — the check exhausts its whole 60 s budget before it can read the edge's refusal

> _Note: this issue was identified against the code base at commit `590a46ff404522650c3e609eb9b73b6bb68c4301`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistribution

## Description
One net-new root cause.

Cause 1 of 1: the OAC-over-SSE-KMS edge scenario makes no observable progress inside the suite's per-check budget — the check is cut off at 60 s with no verdict, before it reaches the first assertion about the edge's answer. Failing check: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied.

AWS: a distribution created with CreateDistribution is returned with Status InProgress and then converges to Deployed; Smithy documents cloudfront#Distribution$Status as "The distribution's status. When the status is Deployed, the distribution's information is fully propagated to all CloudFront edge locations." A client's contract is therefore: poll GetDistribution until Status == "Deployed", then issue a viewer GET to the distribution's published DomainName and get an answer from the origin through the edge — HTTP 200 with the object's bytes for an object the OAC and bucket policy admit, and HTTP 403 with <Code>AccessDenied</Code> / <Message>Access Denied</Message> for an SSE-KMS object whose key policy does not admit cloudfront.amazonaws.com for this distribution's aws:SourceArn. In an emulator that convergence and that edge answer are expected to be immediate, not minutes; everything after the poll depends on both.

Emulator: replay output is only "Error: Check timed out after 60.0s" — the scenario is killed at the cap with no exception from any AWS call, i.e. one of its bounded polls never satisfies its predicate. The two polls that can consume the whole budget are (a) _wait_distribution_deployed, which polls GetDistribution every 10 s for Status == "Deployed" (bound 900 s), and (b) BEAT 1's edge poll, which GETs the SSE-S3 barrier object at the published DomainName every 5 s until it answers 200 (bound 240 s). Either a created distribution never reports Deployed, or the published edge address never answers 200 for an object the OAC plus bucket policy admit. Note that an earlier replay of this same check DID reach BEAT 2 and read the edge's 403 body (see the fixed issue below), so the distribution-to-edge path regressed from answering to not converging at all.

Why: every viewer-facing behavior of a distribution in this scenario is unobservable. A client that follows AWS's documented contract — poll GetDistribution for Deployed, then request the object at DomainName — hangs forever or spins on a never-satisfied condition instead of getting the object (200) or S3's AccessDenied (403) through the edge. The SSE-KMS-at-the-edge authorization contract (key policy without a cloudfront.amazonaws.com principal denies, admitting it for this distribution's ARN serves, re-scoping the grant to another distribution's ARN denies again) can neither be exercised nor relied on.

Fix: the CloudFront distribution lifecycle that advances Status to Deployed (the CreateDistribution/GetDistribution state transition) and the edge data-plane handler that serves a viewer request on the host published as DomainName and performs the OAC-signed origin fetch against S3. Whichever of the two does not converge, both must reach the states the documented poll waits on, immediately.

Related, not duplicates (neither carries this check's replay key): open 20261117T143723Z-cloudfront-s3-6-net-new-causes-a-distribution.md reports a distribution never leaving InProgress under the cloudfront__s3 token; fixed 20261110T080432Z-cloudfront-kms-s3-sse-kms-object-refused-at-the-cloudfront-edge.md was filed from this very check when it still got as far as reading the edge's 403 message, and fixed 20261109T164957Z-cloudfront-kms-s3-a-distribution-s-domainname-is.md covered the unserved published DomainName. This is the next, distinct failure on the same check: it now reaches no verdict at all.

Traces: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied.

## Evidence
From the behavioral replay of cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied [error]:

  Error: Check timed out after 60.0s

Expected: within the budget, GetDistribution reports Status == "Deployed" for the just-created distribution, and a viewer GET to the published DomainName for the SSE-S3 barrier object answers HTTP 200 with that object's bytes; the check then reads the SSE-KMS object's edge answer (HTTP 403, Code AccessDenied, Message "Access Denied" — fixed text, no variable parts) and, after the key policy is repaired for this distribution's ARN, HTTP 200 with the object's bytes.

Actual: no AWS call raises; the check is killed at the 60 s cap with no verdict, so neither the Deployed transition nor the first 200 at the edge is ever observed. The two candidate stalls are the GetDistribution poll for Status == "Deployed" (interval 10 s, bound 900 s) and BEAT 1's edge poll for a 200 on the SSE-S3 object (interval 5 s, bound 240 s) — both bounds exceed the per-check budget, so whichever never converges consumes the whole run.

Corroboration that this is a regression of the same path: an earlier replay of this identical check reached BEAT 2 and returned a 403 body from the edge (recorded in fixed issue 20261110T080432Z-cloudfront-kms-s3-sse-kms-object-refused-at-the-cloudfront-edge.md), so the distribution did reach Deployed and the edge did answer then.

## Steps to Reproduce
Replay cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied. In outline:
1. CreateKey with a key policy that grants only the account root (no cloudfront.amazonaws.com statement).
2. Create a bucket, block public access, and set default encryption to aws:kms with that key and BucketKeyEnabled=false. Put one SSE-KMS object (kms/asset.txt) and one SSE-S3 (AES256) object (sse-s3/asset.txt).
3. CreateOriginAccessControl (origin type s3, SigningBehavior always, sigv4) and CreateDistribution with the bucket's regional domain as an S3 origin referencing that OAC, CachePolicyId = managed CachingDisabled, CustomErrorResponses 403 with ErrorCachingMinTTL 0.
4. Put a bucket policy allowing the cloudfront.amazonaws.com service principal s3:GetObject on the bucket, conditioned on AWS:SourceArn = the distribution ARN.
5. Poll GetDistribution until Distribution.Status == "Deployed" — observe whether it ever reports Deployed.
6. GET https://<published DomainName>/sse-s3/asset.txt — observe whether the edge ever answers 200 with that object's bytes.
Both steps 5 and 6 must complete promptly; today the check is cut off at 60 s before step 6's assertion is ever evaluated.
