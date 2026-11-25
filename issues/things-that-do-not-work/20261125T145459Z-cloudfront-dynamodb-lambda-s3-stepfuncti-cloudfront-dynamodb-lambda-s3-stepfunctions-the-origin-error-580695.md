Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__dynamodb__lambda__s3__stepfunctions.e2e-cloudfront-distribution-origin-error-diagnosis-lifecycle
Issue-Key: cloudfront__dynamodb__lambda__s3__stepfunctions.e2e-cloudfront-distribution-origin-error-diagnosis-lifecycle|phase#11|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# cloudfront__dynamodb__lambda__s3__stepfunctions: the origin-error diagnosis lifecycle cannot start — the distribution's published DomainName is not an address the emulator serves

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution under diagnosis publishes a <id>.cloudfront.net DomainName the emulator does not serve, so the lifecycle's solve phase cannot fetch the tenant object through the edge and the scenario reaches no verdict.

AWS: Smithy (cloudfront 2020-05-31) documents Distribution.DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net". With the distribution Deployed, GET https://<DomainName>/<prefix>/index.html is answered at the edge: either the S3 origin's object, or the origin error the diagnosis scenario is meant to observe and repair (e.g. 403/404 relayed from a misconfigured origin). The viewer response (status, body, X-Cache) at that address is the signal the whole lifecycle turns on.

Emulator: the announced DomainName (e.g. dgzni4pnd7h2s3.cloudfront.net) is not one of the emulator's own hosts and no per-distribution listener is published, so phase 3/5 fails before any viewer response exists: "AssertionError: [phase 3/5 solve] endpoint_url was given https://<id>.cloudfront.net/<prefix>/index.html (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener".

Why: a client cannot diagnose or even reach an origin error through the emulator's CloudFront: the address the control plane publishes for the distribution is not the address the emulator answers on, so every edge-visible symptom is invisible.

Fix: the CloudFront distribution's published DomainName and its viewer listener (distribution create/read serializer plus the edge data plane that relays to the S3 origin and surfaces origin errors).

Traces: the failing check below. The open issue 20261125T052443Z-cloudfront-dynamodb-lambda-s3-stepfuncti-cloudfront-dynamodb-lambda-s3-stepfunctions-the-origin-error.md describes the same lifecycle reaching no verdict but does not carry this check's replay key.

## Evidence
Expected (AWS): in the solve phase, GET https://<DomainName>/<tenant-prefix>/index.html returns the edge's response for that object (the origin's 200 body once the origin misconfiguration is repaired, the origin error before it).

Actual (emulator): "AssertionError: [phase 3/5 solve] endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net/basalt-tenant/index.html (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34459/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The host label, object prefix and loopback port vary per run; the defect is the unserveable announced DomainName.

## Steps to Reproduce
1. Create the S3 origin bucket and tenant object, then CreateDistribution over it; wait for Status=Deployed.
2. GET https://<Distribution.DomainName>/<tenant-prefix>/index.html.
3. Observe the published host is not one the emulator serves, so the viewer request cannot be issued and no origin-error symptom can be read.
