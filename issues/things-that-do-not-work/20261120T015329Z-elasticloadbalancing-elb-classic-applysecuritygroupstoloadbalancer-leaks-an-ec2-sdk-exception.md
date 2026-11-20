Issue-Source: trace-sweep-divergence
Issue-Trace: elbclassic-subnet-attach-detach-and-security-group-reassignment
Issue-Step: task#15
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elbclassic-subnet-attach-detach-and-security-group-reassignment
Issue-Shape-Id: com.amazonaws.elasticloadbalancing#ElasticLoadBalancing_v7
Issue-Key: elasticloadbalancing|ApplySecurityGroupsToLoadBalancer|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 15f808f6512aed05afde1caaa5d6c2b7e80884c0

# ELB Classic ApplySecurityGroupsToLoadBalancer leaks an EC2 SDK exception string as its InvalidSecurityGroup message

> _Note: this issue was identified against the code base at commit `15f808f6512aed05afde1caaa5d6c2b7e80884c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ApplySecurityGroupsToLoadBalancer

## Description
Cause 1 of 1: ApplySecurityGroupsToLoadBalancer rejects a non-existent security group with the wrong error message — an embedded AWS-SDK-for-Java exception rendering of a *foreign* service's (EC2) error, instead of Elastic Load Balancing's own message for the modeled error.

AWS: Smithy (com.amazonaws.elasticloadbalancing) — ApplySecurityGroupsToLoadBalancer errors: AccessPointNotFoundException (awsQueryError LoadBalancerNotFound, 400), InvalidConfigurationRequestException (InvalidConfigurationRequest, 409), InvalidSecurityGroupException (awsQueryError code InvalidSecurityGroup, httpError 400), documented as "One or more of the specified security groups do not exist." For a security group id that does not exist, ELB Classic answers HTTP 400 with an awsQuery ErrorResponse whose Error/Code is InvalidSecurityGroup and whose Error/Message is that sentence. The message is fixed text with no variable parts: ELB does not interpolate the offending group id, and it never embeds a nested service/status/error-code/request-id/proxy envelope — the request id belongs in ResponseMetadata.RequestId, not in the message. The recording asserts the status and error shape and marks the message body as shaped-not-asserted, so the requirement here is the modeled wording above (recorded behavior confirms the 400 + ErrorResponse shape).

Emulator: replay of elbclassic-subnet-attach-detach-and-security-group-reassignment step task#15 returns HTTP 400 (status matches) but $.ErrorResponse.Error.Message is "The security group 'sg-0123456789abcdef0' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidGroup.NotFound; Request ID: 65528aae-a05a-4dd6-a0e4-7f0f10cf5c57; Proxy: null)". Breaking that apart: the emulator is surfacing an *EC2* error ("The security group '<id>' does not exist", EC2 code InvalidGroup.NotFound) wrapped in an SDK ClientException toString suffix "(Service: AmazonEC2; Status Code: 400; Error Code: ...; Request ID: ...; Proxy: null)". Only the group id and the request id in that text are per-run values; the fixed text itself — the EC2 sentence plus the whole parenthesised SDK envelope naming AmazonEC2 and InvalidGroup.NotFound — is what diverges and is not AWS wording for this operation. Expected fixed text: "One or more of the specified security groups do not exist."

Why: a client that handles this failure by matching ELB's documented message, or that logs/parses the error message, sees a different service's error code and message for an elasticloadbalancing call. Tooling that pattern-matches "One or more of the specified security groups do not exist" (or that treats an embedded "Error Code: InvalidGroup.NotFound" as an EC2 failure needing an EC2-level retry/remediation) mis-routes the failure, and the leaked "Request ID:" inside the message makes the message non-deterministic, so retries and log de-duplication on message equality break.

Fix: the ApplySecurityGroupsToLoadBalancer handler's error mapping in the elasticloadbalancing (ELB Classic) service crate — where the internal EC2 security-group lookup failure is converted into the operation's response. It must raise the operation's own modeled InvalidSecurityGroupException (awsQuery code InvalidSecurityGroup, HTTP 400) with the modeled fixed message, rather than propagating the EC2 error's text; the propagated SDK envelope (service name, status code, error code, request id, proxy) must not appear in Error/Message at all, and the request id must be carried only in ResponseMetadata/RequestId.

Traces: elbclassic-subnet-attach-detach-and-security-group-reassignment#task#15 (verdict mismatch, expected_status 400, actual_status 400, divergent key $.ErrorResponse.Error.Message).

Note: no open issue covers this operation's error wording — the nearest neighbours (20261104T190207Z-elasticloadbalancing-elbv2-subnet-refusal-wording-and-describerules-paging-diverge.md) are about ELBv2 subnet refusal wording, a different service surface and operation.

## Evidence
Replay of fixture elbclassic-subnet-attach-detach-and-security-group-reassignment, step task#15, operation ApplySecurityGroupsToLoadBalancer, service elasticloadbalancing, verdict mismatch.

Expected (AWS, per Smithy InvalidSecurityGroupException / awsQueryError InvalidSecurityGroup, httpError 400):
  HTTP 400
  <ErrorResponse><Error><Type>Sender</Type><Code>InvalidSecurityGroup</Code><Message>One or more of the specified security groups do not exist.</Message></Error><RequestId>...</RequestId></ErrorResponse>
  (recording asserts status + shape; message marked '<shaped-not-asserted>')

Actual (emulator):
  HTTP 400
  $.ErrorResponse.Error.Message = "The security group 'sg-0123456789abcdef0' does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidGroup.NotFound; Request ID: 65528aae-a05a-4dd6-a0e4-7f0f10cf5c57; Proxy: null)"
  -> value_mismatch on $.ErrorResponse.Error.Message

Fixed vs variable in the actual message: fixed and wrong = the EC2 sentence "The security group '<id>' does not exist" and the SDK suffix "(Service: AmazonEC2; Status Code: 400; Error Code: InvalidGroup.NotFound; Request ID: <uuid>; Proxy: null)"; variable (not themselves the defect) = the security group id taken from the request and the per-run request id.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1); or manually: CreateLoadBalancer a VPC-attached ELB Classic load balancer, then call ApplySecurityGroupsToLoadBalancer with LoadBalancerName=<that LB> and SecurityGroups.member.1=sg-0123456789abcdef0 (a security group id that does not exist).
3. Observe the awsQuery ErrorResponse: status is 400 as AWS returns, but Error/Message is an EC2 SDK exception string ("... does not exist (Service: AmazonEC2; Status Code: 400; Error Code: InvalidGroup.NotFound; Request ID: ...; Proxy: null)") instead of ELB's modeled InvalidSecurityGroup message "One or more of the specified security groups do not exist."
4. Corresponds to fixture elbclassic-subnet-attach-detach-and-security-group-reassignment, step task#15.
