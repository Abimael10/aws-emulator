Issue-Source: trace-sweep-divergence
Issue-Trace: elbv2-lambda-target-type-rejects-network-fields
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elbv2-lambda-target-type-rejects-network-fields elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs elbv2-target-group-tag-change-and-remove-last
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elasticloadbalancing|CreateTargetGroup|4xx->2xx|$.ErrorResponse
Issue-Key: elasticloadbalancing|CreateTargetGroup|4xx->2xx|status
Issue-Key: elasticloadbalancing|DescribeTargetGroups|4xx->2xx|$.ErrorResponse
Issue-Key: elasticloadbalancing|DescribeTargetGroups|4xx->2xx|status
Issue-Key: elasticloadbalancing|CreateTargetGroup|2xx->2xx|$.CreateTargetGroupResponse.CreateTargetGroupResult.TargetGroups.member.HealthCheckProtocol
Issue-Key: elasticloadbalancing|CreateTargetGroup|2xx->2xx|$.CreateTargetGroupResponse.CreateTargetGroupResult.TargetGroups.member.HealthCheckTimeoutSeconds
Issue-Key: elasticloadbalancing|CreateTargetGroup|2xx->2xx|$.CreateTargetGroupResponse.CreateTargetGroupResult.TargetGroups.member.ProtocolVersion
Issue-Key: elasticloadbalancing|DescribeTargetGroups|2xx->2xx|$.DescribeTargetGroupsResponse.DescribeTargetGroupsResult.TargetGroups.member.HealthCheckProtocol
Issue-Key: elasticloadbalancing|DescribeTargetGroups|2xx->2xx|$.DescribeTargetGroupsResponse.DescribeTargetGroupsResult.TargetGroups.member.HealthCheckTimeoutSeconds
Issue-Key: elasticloadbalancing|DescribeTargetGroups|2xx->2xx|$.DescribeTargetGroupsResponse.DescribeTargetGroupsResult.TargetGroups.member.ProtocolVersion
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ELBv2 CreateTargetGroup ignores lambda target-type restrictions and requested health-check/protocol-version values

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTargetGroup

## Description
The ELBv2 target-group create/describe path does not validate the TargetType=lambda restrictions and does not carry the caller's health-check and protocol-version inputs into the stored target group, so it both accepts configurations AWS refuses and reports values the caller never asked for. 6 net-new causes in this bundle, all on service elasticloadbalancing.

Cause 1 of 6 — CreateTargetGroup accepts network-only fields together with TargetType=lambda (fixture elbv2-lambda-target-type-rejects-network-fields, step task#2; same cluster also task#4 and task#6).
AWS: a lambda target group has no network identity. Smithy: CreateTargetGroupInput.TargetType -> TargetTypeEnum values instance|ip|lambda|alb; CreateTargetGroupInput.Port doc 'If the target is a Lambda function, this parameter does not apply'; TargetGroup.Port doc 'This parameter is not used if the target is a Lambda function'. The modeled refusal class for a self-inconsistent create is the awsQuery protocol validation error (recorded behavior): HTTP 400, body ErrorResponse with Error.Type 'Sender', Error.Code 'ValidationError', Error.Message of the form '<Member> cannot be specified for target groups with target type '<targetType>'' — the sentence is fixed text, <Member> is the offending input member name taken from the request and <targetType> is the request's TargetType value. The recording quotes the Port form verbatim: "Port cannot be specified for target groups with target type 'lambda'". The two sibling steps in this cluster send the other network-only members (the request's remaining network/health-check-network fields) and expect the same 400 ValidationError with the member name substituted.
Emulator: answers HTTP 200 and creates the target group; no ErrorResponse element at all.
Why: a client that relies on AWS to reject an impossible lambda target group gets a successful create, so it proceeds with a target group whose Port/Protocol/VpcId can never be honored, and its negative tests (expecting ValidationError) silently pass against the emulator and fail against AWS.
Fix: CreateTargetGroup's input validation in the elasticloadbalancingv2 handler — add the TargetType=lambda cross-member check before the target group is inserted into state.
Traces: elbv2-lambda-target-type-rejects-network-fields#task#2 (plus task#4, task#6 in the same cluster).

Cause 2 of 6 — DescribeTargetGroups returns 200 where AWS answers TargetGroupNotFound (fixture elbv2-lambda-target-type-rejects-network-fields, step task#3; same cluster also task#5 and task#7).
AWS: Smithy: DescribeTargetGroups errors include TargetGroupNotFoundException, awsQueryError code 'TargetGroupNotFound', httpError 400. For a Names/TargetGroupArns lookup that resolves to nothing, AWS answers HTTP 400 with ErrorResponse Error.Type 'Sender', Error.Code 'TargetGroupNotFound', Error.Message 'One or more target groups not found' — that message is entirely fixed text with no variable part.
Emulator: answers HTTP 200 with a TargetGroups list (the target group Cause 1 wrongly created), and emits no ErrorResponse.
Why: the not-found contract is how a client distinguishes 'absent' from 'present'; a 200 makes a probe for a non-existent target group look like a hit, so create-if-missing and cleanup-verification loops take the wrong branch.
Fix: DescribeTargetGroups' handler/lookup in the elasticloadbalancingv2 crate — raise TargetGroupNotFoundException when a requested name/ARN resolves to no stored target group. Note this cause stays reproducible independently of Cause 1: the lookup must 400 whether or not the earlier create was refused.
Traces: elbv2-lambda-target-type-rejects-network-fields#task#3 (plus task#5, task#7 in the same cluster).

Cause 3 of 6 — CreateTargetGroup discards the request's HealthCheckProtocol and HealthCheckTimeoutSeconds and emits an empty ProtocolVersion (fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs, step task#2; same cluster also elbv2-target-group-tag-change-and-remove-last#task#2).
AWS: CreateTargetGroup echoes the configuration it stored. Smithy: CreateTargetGroupInput.HealthCheckProtocol -> ProtocolEnum, CreateTargetGroupInput.HealthCheckTimeoutSeconds @range min 2 max 120, and the response member TargetGroup.HealthCheckProtocol / TargetGroup.HealthCheckTimeoutSeconds carry those values back. CreateTargetGroupInput.ProtocolVersion doc: 'The default is HTTP1, which sends requests to targets using HTTP/1.1' — so an absent or empty ProtocolVersion in the request is reported back as 'HTTP1', never as an empty value.
Emulator: returns the service default instead of the requested value — HealthCheckTimeoutSeconds '10' where the request asked for a shorter timeout (recording: 5), HealthCheckProtocol 'HTTP' where the request asked for HTTPS — and serializes ProtocolVersion as an empty element instead of 'HTTP1'.
Why: the caller's health-check configuration is silently replaced, so a client that sets a tighter timeout or an HTTPS health check believes it took effect; and an empty ProtocolVersion deserializes to an empty string rather than HTTP1, breaking any comparison against the documented default.
Fix: CreateTargetGroup's handler/state construction — bind the supplied health-check members into the stored target group record and apply the modeled ProtocolVersion default (HTTP1 for HTTP/HTTPS protocols) when the input member is absent or empty.
Traces: elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs#task#2 (plus elbv2-target-group-tag-change-and-remove-last#task#2).

Cause 4 of 6 — DescribeTargetGroups reads back the same three wrong values (fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs, step task#3).
AWS: TargetGroup.HealthCheckProtocol / HealthCheckTimeoutSeconds / ProtocolVersion in a describe response equal what was stored at create time (the requested HTTPS health-check protocol, the requested timeout, and ProtocolVersion 'HTTP1').
Emulator: HealthCheckTimeoutSeconds '10', HealthCheckProtocol 'HTTP', ProtocolVersion '' — identical to Cause 3, which shows the divergence is in the persisted record and not only in the create response serializer.
Why: the wrong configuration is durable, so a client that re-reads the target group to confirm its settings is told the defaults are in force forever.
Fix: the target-group state record in the elasticloadbalancingv2 crate — it must retain the create-time values that DescribeTargetGroups projects.
Traces: elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs#task#3.

Cause 5 of 6 — CreateTargetGroup omits the ProtocolVersion default when the request does not set it (fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs, step task#8).
AWS: Smithy CreateTargetGroupInput.ProtocolVersion doc: for an HTTP/HTTPS target group the default is 'HTTP1'. The created TargetGroup therefore reports ProtocolVersion 'HTTP1'.
Emulator: serializes ProtocolVersion as an empty element ('').
Why: a client reading ProtocolVersion to decide between HTTP1/HTTP2/GRPC behavior gets an empty string, which matches none of the modeled values.
Fix: the ProtocolVersion default in CreateTargetGroup's handler/serializer — emit HTTP1 rather than an empty element.
Traces: elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs#task#8.

Cause 6 of 6 — DescribeTargetGroups reports the same empty ProtocolVersion (fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs, step task#9).
AWS: the describe projection reports ProtocolVersion 'HTTP1' for that target group.
Emulator: ProtocolVersion '' (empty element).
Why: same as Cause 5, and persistent across reads.
Fix: the target-group projection used by DescribeTargetGroups.
Traces: elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs#task#9.

Related open issues that do NOT cover these causes: 20261109T205846Z-elasticloadbalancing-elbv2-responses-omit-modeled-target-health-fields-advertise.md (target-health fields and DNSName) and 20261104T190207Z-elasticloadbalancing-elbv2-subnet-refusal-wording-and-describerules-paging-diverge.md (subnet wording, DescribeRules paging). All six causes above were reported with no owning issue.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 93.9%, 36 diverged steps):

Cause 1 — CreateTargetGroup, fixture elbv2-lambda-target-type-rejects-network-fields step task#2 (cluster also covers task#4, task#6):
  expected status 400, actual 200
  expected body $.ErrorResponse = {'Error': {'Type': 'Sender', 'Code': 'ValidationError', 'Message': "Port cannot be specified for target groups with target type 'lambda'"}, 'RequestId': <any>}
  actual: $.ErrorResponse missing (None)

Cause 2 — DescribeTargetGroups, same fixture step task#3 (cluster also task#5, task#7):
  expected status 400, actual 200
  expected $.ErrorResponse = {'Error': {'Type': 'Sender', 'Code': 'TargetGroupNotFound', 'Message': 'One or more target groups not found'}, 'RequestId': <any>}
  actual: $.ErrorResponse missing (None)

Cause 3 — CreateTargetGroup, fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs step task#2 (cluster also elbv2-target-group-tag-change-and-remove-last#task#2), status 200 both sides:
  $...TargetGroups.member.HealthCheckTimeoutSeconds: expected '5', got '10'
  $...TargetGroups.member.HealthCheckProtocol: expected 'HTTPS', got 'HTTP'
  $...TargetGroups.member.ProtocolVersion: expected 'HTTP1', got ''

Cause 4 — DescribeTargetGroups, same fixture step task#3, status 200 both sides:
  $...TargetGroups.member.HealthCheckTimeoutSeconds: expected '5', got '10'
  $...TargetGroups.member.HealthCheckProtocol: expected 'HTTPS', got 'HTTP'
  $...TargetGroups.member.ProtocolVersion: expected 'HTTP1', got ''

Cause 5 — CreateTargetGroup, same fixture step task#8, status 200 both sides:
  $...TargetGroups.member.ProtocolVersion: expected 'HTTP1', got ''

Cause 6 — DescribeTargetGroups, same fixture step task#9, status 200 both sides:
  $...TargetGroups.member.ProtocolVersion: expected 'HTTP1', got ''

Model evidence (services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model):
  CreateTargetGroup errors: DuplicateTargetGroupName, InvalidConfigurationRequest, TooManyTags, TooManyTargetGroups (all httpError 400)
  DescribeTargetGroups errors: LoadBalancerNotFound, TargetGroupNotFound (awsQueryError code 'TargetGroupNotFound', httpResponseCode 400)
  CreateTargetGroupInput.Port doc: 'If the target is a Lambda function, this parameter does not apply.'
  CreateTargetGroupInput.ProtocolVersion doc: 'The default is HTTP1 ...'
  CreateTargetGroupInput.HealthCheckTimeoutSeconds @range min 2 max 120; HealthCheckProtocol -> ProtocolEnum
  TargetTypeEnum values: instance, ip, lambda, alb

## Steps to Reproduce
1. git_clone() and boot the emulator built from the checkout.
2. Replay fixture elbv2-lambda-target-type-rejects-network-fields:
   - step task#2: CreateTargetGroup with TargetType=lambda plus a network-only member (Port). Expect HTTP 400 ErrorResponse Sender/ValidationError "Port cannot be specified for target groups with target type 'lambda'"; the emulator answers 200 and creates the group. Steps task#4 and task#6 repeat this for the other network-only members.
   - step task#3: DescribeTargetGroups for the name that must not exist. Expect HTTP 400 Sender/TargetGroupNotFound 'One or more target groups not found'; the emulator answers 200 with a TargetGroups list. Steps task#5 and task#7 are the same check.
3. Replay fixture elbv2-target-group-empty-protocol-version-and-explicit-health-check-inputs:
   - step task#2: CreateTargetGroup with HealthCheckProtocol=HTTPS, an explicit HealthCheckTimeoutSeconds and an empty ProtocolVersion. Compare the returned HealthCheckProtocol/HealthCheckTimeoutSeconds/ProtocolVersion against the request and against the modeled HTTP1 default.
   - step task#3: DescribeTargetGroups for that group and compare the same three members (shows the values were stored wrong).
   - step task#8: CreateTargetGroup with no ProtocolVersion; expect ProtocolVersion 'HTTP1'.
   - step task#9: DescribeTargetGroups for that group; expect ProtocolVersion 'HTTP1'.
4. Re-run replay_wire_corpus() and confirm all six clusters pass.
