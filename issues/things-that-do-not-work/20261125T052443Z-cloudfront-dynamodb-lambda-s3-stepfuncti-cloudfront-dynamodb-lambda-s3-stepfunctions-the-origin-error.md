Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__dynamodb__lambda__s3__stepfunctions.e2e-cloudfront-distribution-origin-error-diagnosis-lifecycle
Issue-Key: cloudfront__dynamodb__lambda__s3__stepfunctions.e2e-cloudfront-distribution-origin-error-diagnosis-lifecycle|phase#11|_PhaseError|raise _PhaseError(
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# cloudfront__dynamodb__lambda__s3__stepfunctions: the origin-error diagnosis lifecycle never reaches a verdict — the edge never serves the tenant object

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution's edge never answers a viewer request for the tenant's object, so the diagnose-and-repair lifecycle cannot advance past its solve phase.

AWS: Smithy (com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net." A viewer GET to https://<DomainName>/<prefix>/index.html is answered by the edge once the distribution is Deployed: the origin's object with 200 when the origin access grant is correct, and the edge's own 403 AccessDenied (origin refused) while it is not — which is exactly the signal a diagnosis workflow reads, then re-reads as a 200 after the grant is repaired.

Emulator: the published DomainName is "127.0.0.1/_aws/cloudfront/<DistributionId>", not a DNS host, and the 127.0.0.1:443 it implies has no listener while the control plane answers elsewhere. The check's solve phase polls the edge and never gets either answer: "_PhaseError: [phase 3/5 solve] TimeoutError: the edge serving <prefix>/index.html: the run budget ended first; not graded". The lifecycle's later phases are never run.

Why: a client cannot diagnose or confirm the repair of an origin-access fault, because the edge produces no observable before-state (403) and no after-state (200) — only an absent endpoint.

Fix: the CloudFront CreateDistribution/UpdateDistribution handler and distribution state must publish a DomainName the emulator serves, and the edge data plane must perform the origin fetch and answer with the origin's result, including the 403 AccessDenied refusal when the origin denies the signed request.

Note: open issues 20261105T111951Z-cloudfront-dynamodb-lambda-a-deployed-distribution-never-serves.md and 20261029T175823Z-cloudfront-10-more-divergences-the-edge-data-plane.md cover the same area but not this check's replay key.

Traces: the failing check in trace_ids.

## Evidence
Expected (AWS): the edge answers https://<DomainName>/<prefix>/index.html with 403 AccessDenied while the origin grant is missing and with 200 plus the object's bytes after it is repaired, letting the diagnosis workflow observe both states.

Actual (emulator): "_PhaseError: [phase 3/5 solve] TimeoutError: the edge serving <prefix>/index.html: the run budget ended first; not graded" — the published DomainName "127.0.0.1/_aws/cloudfront/<Id>" has no listener, so neither state is ever observed. The prefix, id, host and port are per-run values.

## Steps to Reproduce
1. Deploy the estate: an S3 origin with a tenant prefix, a distribution with OAC over it, and the Lambda/Step Functions diagnosis workflow.
2. Read Distribution.DomainName and poll GET https://<DomainName>/<prefix>/index.html.
3. Observe the edge never answers (connect refused at the announced address) and the solve phase ends with TimeoutError, leaving phases 4 and 5 unrun.
4. Re-replay the check id in trace_ids to confirm.
