Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-protocol-policy-mismatch-rejected cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-secret-header-direct-request-forbidden
Issue-Key: cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-protocol-policy-mismatch-rejected|run#15|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc}") from exc
Issue-Key: cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-secret-header-direct-request-forbidden|run#18|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc}") from exc
Issue-Commit: a11be7ebac31f84405a5fd36d8a18986542b4650

# cloudfront__elbv2: the ALB-origin distribution publishes an unserved <id>.cloudfront.net DomainName again, so both ALB-origin edge checks are refused at the first request

> _Note: this issue was identified against the code base at commit `a11be7ebac31f84405a5fd36d8a18986542b4650`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
REOPENS 20261115T163206Z-cloudfront-elbv2-a-cloudfront-distribution-over-an-alb.md: its fix left the call refused. That issue reported that a distribution over an ALB custom origin never serves at the edge; the edge is still unaddressable, now because the published DomainName is an <id>.cloudfront.net host the emulator does not answer on, so this is the same defect in a new form.

Cause 1 of 1: a distribution whose origin is an Application Load Balancer publishes Distribution.DomainName as a real-AWS <id>.cloudfront.net host the emulator does not serve, so no viewer request reaches the edge.

AWS: Smithy (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31) documents Distribution$DomainName as "The distribution's CloudFront domain name. For example: d111111abcdef8.cloudfront.net", the address the edge serves on. Recorded behavior only for the viewer side: a GET addressed to it on a Deployed distribution is answered by the edge after fetching from the ALB, an OriginProtocolPolicy that does not match the listener's protocol surfaces as the edge's 502-class refusal rather than a successful fetch, and a request sent directly to the ALB without the distribution's secret custom header is refused 403 by the listener rule while the same request through the distribution succeeds.

Emulator: the published host is not one of the emulator's own hosts, so the harness refuses to route it: 'endpoint_url was given https://<id>.cloudfront.net/<path> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener' — reported by the checks as 'AssertionError: DATAPLANE_UNIMPLEMENTED: <that message>'. Both checks end there.

Why: a client fronting an ALB with CloudFront cannot verify either the origin-protocol boundary or the secret-header origin lock, because the only address published for the edge is not one the emulator answers on.

Fix: the CloudFront distribution state/serializer that mints the published domain name plus the edge listener it must name, with the viewer pipeline fetching from the ALB custom origin on that listener.

Traces: cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-protocol-policy-mismatch-rejected, cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-secret-header-direct-request-forbidden.

## Evidence
Expected: a viewer GET to https://<published DomainName>/<path> reaches the emulator's edge, which fetches from the ALB origin (200 through the distribution; the protocol-mismatch variant failing at the origin fetch, and the direct-to-ALB request without the secret header refused 403).

Actual (both checks):
  AssertionError: DATAPLANE_UNIMPLEMENTED: endpoint_url was given https://d0lc535g268ho1.cloudfront.net/plain/a (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:44757/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
    (xsvc-cloudfront-2-alb-origin-protocol-policy-mismatch-rejected)
  AssertionError: DATAPLANE_UNIMPLEMENTED: endpoint_url was given https://d0lc535g268ho1.cloudfront.net/sim2real/resource ... needs http(s)://127.0.0.1:36391/<resource path> ...
    (xsvc-cloudfront-2-alb-origin-secret-header-direct-request-forbidden)

Host label, port and path are per-run values; the divergence is the published host belonging to real AWS instead of the emulator.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Replay cloudfront__elbv2.xsvc-cloudfront-2-alb-origin-secret-header-direct-request-forbidden: stand up an ALB, create a distribution with it as a custom origin and a secret custom header, wait for Deployed, read Distribution.DomainName.
3. Observe DomainName is <id>.cloudfront.net; the viewer GET through it is refused by the harness because that host is not one the emulator serves.
