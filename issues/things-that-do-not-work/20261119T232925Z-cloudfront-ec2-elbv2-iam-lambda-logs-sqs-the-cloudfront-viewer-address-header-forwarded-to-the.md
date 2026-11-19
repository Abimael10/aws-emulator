Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation
Issue-Key: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation|run#51|AssertionError|assert first_headers["cloudfront-viewer-address"] and _VIEWER_ADDRESS_RE.match(
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# The CloudFront-Viewer-Address header forwarded to the origin carries only an address, not AWS's 'ip:port' form

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause, reached by 1 check.

Cause 1 of 1: when a cache behavior's origin request policy explicitly names the CloudFront-generated header `CloudFront-Viewer-Address`, the value CloudFront adds to the origin request is the viewer's address AND port joined by a colon — "<ip>:<port>". The emulator's edge forwards a bare address with no ":<port>" part.

AWS: recorded behavior only — no Smithy shape models the value of a CloudFront-generated request header; the CloudFront developer guide defines CloudFront-Viewer-Address as the viewer's IP address and source port in the form `IP-address:port` (IPv4, e.g. `198.51.100.10:50011`, or an IPv6 address with its port). The header is added only when the origin request policy names it explicitly, which is what the recorded distribution does. The requirement is the FORM — an address, a colon, and the viewer's ephemeral source port — not any particular address or port, both of which differ on every request.

Emulator: the origin (an ALB-fronted Lambda target that echoes the request headers it received) sees `cloudfront-viewer-address: 127.0.0.1` — an address with no colon and no port — so it fails the `ip:port` form.

Why: an origin that parses CloudFront-Viewer-Address to recover the client address and source port (rate limiting per client, geo/abuse logging, access decisions, any `host, port = value.rsplit(":", 1)` split) either crashes or silently mis-reads the whole value as the address. The header is the only way an origin behind CloudFront learns the viewer's source port, so the information is simply unavailable.

Fix: the CloudFront edge's origin-request header generation — the component that populates the CloudFront-Viewer-* headers when the attached origin request policy names them. It must emit the viewer connection's address and its source port as "<address>:<port>" (bracketing an IPv6 literal as AWS does), taking both from the viewer connection rather than from a fixed string.

Traces: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation — the assertion on the first origin hit's headers, before the cache-hit half of the check is reached.

## Evidence
Expected: the origin request carries header CloudFront-Viewer-Address whose value matches the `ip:port` form (e.g. `<viewer-ip>:<viewer-source-port>`), because the cache behavior's origin request policy names that header explicitly.
Actual: AssertionError: the listed CloudFront-Viewer-Address header must reach the origin as 'ip:port', got '127.0.0.1' — the value reached the origin (so the header is forwarded) but with no colon and no port component. The particular address is a per-environment value; the missing ":<port>" is the divergence.

## Steps to Reproduce
1. Stand up an internal ALB in a VPC with a Lambda target that returns the request headers it received.
2. Create a CloudFront distribution over that ALB (VPC origin) with a custom origin request policy that explicitly names the CloudFront-generated headers CloudFront-Viewer-Address and CloudFront-Viewer-Country in addition to all viewer headers.
3. Make a viewer GET through the distribution's edge for an uncached path.
4. Read the headers the Lambda target echoes back. Expected: cloudfront-viewer-address matches `^<ip>:<port>$`. Observed: a bare address such as `127.0.0.1` with no port.
