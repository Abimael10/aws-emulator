Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iot.e2e-iot-thing-restrict-vpce-lifecycle
Issue-Key: ec2__iot.e2e-iot-thing-restrict-vpce-lifecycle|phase#7|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__iot: a mutual-TLS IoT publish from outside the permitted VPC endpoint is answered 404 UnknownService instead of 403, so an aws:SourceVpce restriction is written down but never enforced

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
Cause 1 of 1: the IoT data-plane HTTPS publish endpoint does not route a mutual-TLS request at all — it falls through to the emulator's generic service-dispatch error — so an IoT policy that restricts publishing to one interface VPC endpoint is accepted and stored but never applied to traffic.
  AWS: recorded behavior only for the exact wire shape (the mTLS data-plane path is reached by client certificate, not SigV4, so it carries no X-Amz-Target and no SigV4 credential scope). AWS's IoT data plane serves `POST /topics/{topic}` on the account's ats data endpoint authenticated by the X.509 client certificate attached to the thing's principal, and authorizes it against the attached IoT policy. An IoT policy statement whose Condition denies the request — here `aws:SourceVpce` naming the permitted interface endpoint — makes the publish an authorization FAILURE: the request is answered HTTP 403, not 404, and the topic is not published to. The endpoint id in the condition and the topic in the path are values from the request and from state; the 403 classification is the requirement.
  Emulator: the request is answered HTTP 404 with body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}. The status (404 vs 403), the error code (`UnknownService` vs an IoT authorization refusal) and the message (a dispatcher sentence about missing SigV4/X-Amz-Target/Host, which is fixed emulator text with no variable part) all diverge. Because the request never reaches IoT at all, the restriction the scenario solves for is unenforced: a caller outside the permitted VPC endpoint is not refused on authorization grounds, and the permitted caller is equally unroutable.
  Why: a client that locks an IoT thing's publishing down to a single interface VPC endpoint gets no enforcement — the policy is inert. Worse, the 404/UnknownService answer is indistinguishable from "wrong URL", so a client cannot tell a denied publish from a misaddressed one, and a security control reads as configured while admitting nothing and refusing nothing on the grounds it was written for.
  Fix: the IoT data-plane request router — it must resolve the mTLS `POST /topics/{topic}` path to the IoT data-plane Publish handler using the client certificate and request path rather than requiring a SigV4 credential scope / X-Amz-Target — and that handler's authorization against the attached IoT policy, including its aws:SourceVpce condition, so a denied publish answers 403.
  Traces: ec2__iot.e2e-iot-thing-restrict-vpce-lifecycle (phase 2/4 'solve').

Note: the open issues 20261110T160612Z-ec2-iot-the-iot-data-plane-publish-path.md (publish path never answers) and 20261109T122953Z-ec2-iot-iot-describeendpoint-publishes-a-real-aws.md (DescribeEndpoint publishes an unserved hostname) describe neighbouring defects on this path; the replay report does not mark this check as owned by either, and the observed answer here is a routed 404 UnknownService rather than no answer.

## Evidence
ec2__iot.e2e-iot-thing-restrict-vpce-lifecycle [mismatch]
  Expected: a publish over mutual TLS originating OUTSIDE the permitted interface VPC endpoint is refused HTTP 403 — the solved aws:SourceVpce restriction must be enforced on traffic, not merely stored.
  Actual: AssertionError: "[phase 2/4 solve] a publish over mutual TLS from outside vpce-<id> returned HTTP 404 (b'{\"__type\":\"UnknownService\",\"message\":\"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)\"}'); expected 403 — the solved restriction must be enforced, not merely written down".
  Divergence: status 404 instead of 403; error code 'UnknownService' instead of an IoT authorization refusal; message is the emulator's fixed dispatcher sentence 'could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)' with no variable part, instead of IoT's authorization-failure response for the denied publish.

## Steps to Reproduce
Deploy the stack that creates an AWS::IoT::Thing, an interface VPC endpoint for the IoT data plane, a certificate attached as the thing's principal, and an IoT policy whose publish statement is conditioned on aws:SourceVpce naming that endpoint. Resolve the data endpoint (iot DescribeEndpoint, endpointType iot:Data-ATS) and issue POST /topics/<topic> over mutual TLS with the thing's client certificate from a source that is NOT the permitted interface VPC endpoint. The emulator answers 404 with {"__type":"UnknownService", ...}; AWS answers 403 for the policy-denied publish.
