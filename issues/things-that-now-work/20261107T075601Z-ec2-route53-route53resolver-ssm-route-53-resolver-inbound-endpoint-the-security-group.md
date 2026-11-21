Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53
Issue-Key: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53|_poll#8|Timeout|time.sleep(interval_s)
Issue-Commit: 620c90c23a321396910fd66a692ef2fcecf33f7f

# Route 53 Resolver inbound endpoint: the security-group-revoked UDP/53 denial never converges

## Resolution

The cause this issue names is implemented: the security-group gate on an inbound Resolver
endpoint's addresses, and the private hosted zones those addresses answer out of, are now the
answer EVERY in-VPC client gets — the two resolver clients a managed node's guest has, and, as of
this fix, the datagram a program inside the VPC sends on its own socket, which is how the recorded
scenario observes the data plane. The reading lives once, in the service that owns the endpoints
(`route53resolver/src/state/vpc_dns.rs`), and the component that carries the query reads it out of
`services/common/src/vpc_private_dns.rs`. It is pinned end to end by
`core/router/tests/all/resolver_endpoint_data_plane_from_an_in_vpc_socket.rs`: with the rule in
place both endpoint addresses answer the zone's records, after `RevokeSecurityGroupIngress` of
UDP/53 neither answers anything while the VPC's own resolver keeps answering, and after
`AuthorizeSecurityGroupIngress` the query is answered again.

The recorded check still does not converge, and not on this cause: its probe is an
`AWS-RunShellScript` document whose `commands` are a heredoc writing a Python program plus a
`python3 …` line, and Systems Manager's guest execution runs each element as its own shell command
and provides no `python3`, so the probe never runs and never sends a query. That seam is tracked
separately by
`issues/things-that-do-not-work/20261114T161545Z-ec2-route53-route53resolver-ssm-an-aws-runshellscript-script.md`,
whose own fix statement names it; this issue's `Issue-Key` converges once that lands.

> _Note: this issue was identified against the code base at commit `620c90c23a321396910fd66a692ef2fcecf33f7f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolverEndpoint

## Description
One net-new root cause.

Cause 1 of 1: an inbound resolver endpoint's query path never reflects its security group, so the revoke-and-deny branch never settles.
AWS: queries to an inbound resolver endpoint's IP addresses are admitted only while a security group attached to the endpoint allows UDP/53 from the source; revoking that rule makes the query fail (time out) and re-authorizing it restores the answer (recorded behavior).
Emulator: the scenario never reaches either outcome — the check ends "Check timed out after 60.0s".
Why: the security-group gate on resolver traffic — the control an operator uses to scope DNS access — cannot be observed, so a wrongly open endpoint is indistinguishable from a correctly closed one.
Fix: the resolver-endpoint data path's security-group enforcement (and the endpoint IP state it depends on).
Traces: ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53.

## Evidence
From the behavioral replay:
- ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53 [mismatch]: Error: Check timed out after 60.0s.
(The sibling check inbound-resolver-endpoint-addresses-answer-the-private-hosted-zone failed with TimeoutError "... the run budget ended first; not graded" and is not filed.)

## Steps to Reproduce
Replay the behavioral suite and re-run ec2__route53__route53resolver__ssm.inbound-resolver-endpoint-query-denied-when-the-security-group-revokes-udp-53: create a private hosted zone and an inbound resolver endpoint with a security group allowing UDP/53, query it, revoke the rule and query again.
