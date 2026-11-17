Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudfront__s3.origin-group-apigateway-mock-primary-fails-over-to-s3-secondary
Issue-Key: apigateway__cloudfront__s3.origin-group-apigateway-mock-primary-fails-over-to-s3-secondary|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# apigateway__cloudfront__s3: an origin-group distribution never serves, so the API-Gateway-primary → S3-secondary failover check never reaches a verdict

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: a distribution with an OriginGroup (API Gateway mock primary, S3 secondary) never answers a viewer request, so the check that drives the primary to fail and expects the secondary to answer exhausts its whole budget without a single graded assertion.

AWS: Smithy (cloudfront, CreateDistribution -> DistributionConfig.OriginGroups / Origins) models an origin group with a FailoverCriteria status-code list; AWS brings the distribution to Status 'Deployed' and the edge then serves a viewer GET from the primary origin and, on a failover status code, re-drives the same request to the secondary origin and serves its body. The modeled Distribution.Status member is the contract that the distribution becomes serviceable; the recording shows the check waiting on exactly that and then fetching through the published DomainName. Beyond Status/OriginGroups the error text here is recorded behavior only — the check never gets an error, it gets no response at all.

Emulator: the check reports 'Check timed out after 60.0s'. Nothing is graded: no viewer response, no failover, no status code. Sibling cloudfront checks in this run show the same shape and one of them states the mechanism outright — 'timed out waiting for distribution <id> to reach Deployed ... (last: InProgress)' — so the distribution lifecycle and/or the edge listener never become serviceable. Hypothesis: the distribution is minted with Status 'InProgress' and never transitions, so every check that waits for Deployed before fetching stalls.

Why: a client that creates a distribution and waits for it to deploy before cutting traffic over (the documented and SDK-waiter-supported sequence) waits forever. Origin-group failover — the whole point of a two-origin distribution — can never be exercised or observed against the emulator, so an availability design cannot be validated at all.

Fix: the CloudFront distribution lifecycle (the state transition that takes a created/updated distribution from InProgress to Deployed) and the edge request path that resolves an OriginGroup's primary/secondary members.

Traces: apigateway__cloudfront__s3.origin-group-apigateway-mock-primary-fails-over-to-s3-secondary.

## Evidence
Expected: the distribution reaches Status 'Deployed'; a viewer GET through its DomainName is answered by the API Gateway mock primary, and once the primary answers a failover status code the identical GET is answered with the S3 secondary's bytes (HTTP 200).
Actual: 'Error: Check timed out after 60.0s' — no viewer response at all. Corroborating sibling evidence in the same run: cloudfront__s3.continuous-deployment-staging-origin-path-promotion-serves-staging-bytes fails with 'AssertionError: timed out waiting for distribution EJZ50Z92T9DP30 to reach Deployed after 14.702488758000072s (last: 'InProgress')'. (Distribution ids, hosts and ports are per-run values, not requirements.)

## Steps to Reproduce
1. Create an S3 bucket and an API Gateway REST API with a MOCK integration.
2. CreateDistribution with two origins and an OriginGroup naming the API Gateway origin as primary member and the S3 origin as secondary, with FailoverCriteria status codes covering the primary's failure code.
3. Poll GetDistribution until Status == 'Deployed'.
4. GET the distribution's DomainName and observe which origin answered; then make the primary answer a failover status code and repeat the GET.
Observed: step 3 never completes — Status stays 'InProgress' — so step 4 is never reached and the check hits its 60 s budget.
