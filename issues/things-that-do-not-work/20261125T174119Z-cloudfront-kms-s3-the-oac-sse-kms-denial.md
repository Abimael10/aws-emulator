Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied
Issue-Key: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied|run#28|PublishedUrlRejected|base, route_headers = ctx.endpoint_route(domain)
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront__kms__s3: the OAC+SSE-KMS denial cannot be observed — the distribution publishes an <id>.cloudfront.net DomainName the emulator does not serve

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the distribution over an OAC-protected, SSE-KMS-encrypted S3 origin publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host the emulator does not answer on, so the viewer request that should surface the KMS key-policy denial is never made.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", and that host is the address the edge serves on. Recorded behavior only for the rest: a GET addressed to it on a Deployed distribution causes the edge to fetch the SSE-KMS object from the bucket with the OAC's signature, and when the KMS key policy does not grant the CloudFront service principal the decrypt, the origin fetch fails and the viewer is served the resulting S3 access-denied response rather than the object.

Emulator: the published host is not one of the emulator's own hosts, so the harness refuses to route the viewer request: 'endpoint_url was given https://<id>.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' (simparity.published.PublishedUrlRejected). The check ends there, so the denial path is never exercised.

Why: a client that encrypts its origin objects with a customer-managed KMS key and fronts them with CloudFront cannot see whether the key policy is enforced on the OAC origin fetch, because the only address published for the edge is not one the emulator answers on.

Fix: the CloudFront distribution state/serializer that mints the published domain name plus the edge listener it must name, with the OAC-signed S3 origin fetch (and its KMS decrypt authorization) served on that listener.

Traces: cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied.

## Evidence
Expected: a viewer GET to https://<published DomainName>/<key> reaches the emulator's edge, which performs the OAC-signed origin fetch and, with no CloudFront principal in the KMS key policy, serves S3's access-denied response instead of the object.

Actual:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://dgzni4pnd7h2s3.cloudfront.net (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:40339/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Host label and port are per-run values; the divergence is the published host being in real AWS's cloudfront.net namespace instead of an address the emulator is reached on.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__kms__s3.xsvc-cloudfront-2-oac-kms-key-policy-without-cloudfront-principal-denied: create a KMS key whose policy omits the CloudFront service principal, an SSE-KMS object in a private bucket, and an OAC distribution over it; wait for Deployed and read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the viewer GET through it is refused by the harness because that host is not one the emulator serves.
