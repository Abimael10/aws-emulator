Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.elasticache-revoked-security-group-ingress-blocked-from-cache-data-plane
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.elasticache-revoked-security-group-ingress-blocked-from-cache-data-plane|run#41|TimeoutError|raise TimeoutError(f"[{src}] the revoked rule taking effect on {host}:{port}: the run "
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ElastiCache cache-node data plane ignores its security group: the endpoint keeps accepting RESP connections after the only admitting ingress rule is revoked (and answers a client outside the VPC)

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
1 net-new cause.

Cause 1 of 1: the ElastiCache cache-node data plane is not gated by the cluster's security groups. Related open issues cover neighbouring services and none covers this one: 20261115T034057Z-ec2-elbv2-reopens-the-alb-still-announces-an.md (revoked ELBv2 target ingress never takes effect), 20261107T075601Z-ec2-route53-route53resolver-ssm-route-53-resolver-inbound-endpoint-the-security-group.md and 20261118T003001Z-cloudformation-ec2-ssm-removing-outbound-443-from-an.md. The open ElastiCache data-plane issues (20261108T152921Z-elasticache-in-vpc-data-plane-unverifiable..., 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait...) are the opposite finding — an endpoint that could not be reached at all — and do not cover an endpoint that is reachable when it must not be.

AWS: recorded behavior only — the cache data plane is not described by the ElastiCache Smithy model (smithy_query on the elasticache model exposes only control-plane operations; CreateCacheCluster.SecurityGroupIds / CacheCluster.SecurityGroups are the only modeled surface, and they carry no reachability semantics). AWS's documented rule (https://docs.aws.amazon.com/vpc/latest/userguide/vpc-security-groups.html, plus ElastiCache being VPC-only) is: a cache node's endpoint is reachable only from inside the cluster's VPC, and only from sources admitted by an ingress rule on one of the cluster's security groups. EC2 RevokeSecurityGroupIngress removing the one rule that admits tcp/6379 from the client's security group makes new connections fail at the TCP connect stage (the SYN is dropped, so connect() times out) — not at DNS resolution and not mid-session; a command attempted while the rule is withdrawn never reaches the engine and therefore leaves no key; re-authorizing the identical rule readmits the same client and the keyspace written before the revoke is untouched.

Emulator: the cache endpoint is served to a client that is NOT in the cluster's VPC at all — the check's vantage-point picker returned the `runner-outside-vpc` prober because its very first PING to the advertised endpoint succeeded, and the authorized leg (PING / SET / GET) then passed from there. After EC2 RevokeSecurityGroupIngress on the cluster's only security group returned Return=true with no UnknownIpPermissions, and DescribeSecurityGroups confirmed the group no longer admits tcp/6379 from the executor group, the endpoint kept answering PING on every attempt of the bounded poll — the probe result stayed ok=true, never reaching the ok=false/stage="connect" outcome AWS produces — until the check's run budget ran out and it raised `TimeoutError: [runner-outside-vpc] the revoked rule taking effect on <host>:<port>: the run budget ended first; not graded`. The control plane therefore records the revoke while the data plane ignores it.

Why: a client relying on AWS's behavior treats the security group as the gate on its cache. Against this emulator the gate is cosmetic: every cache node is reachable by anything that can route to the advertised address, including a caller with no VPC attachment and no admitting rule, so a negative-control or isolation test that must fail (connection refused after the revoke) instead succeeds, and a write issued through a withdrawn rule lands in the keyspace. The overly-permissive direction is the dangerous one — the emulator cannot be used to validate that a cache is closed.

Fix: the ElastiCache data-plane listener in the elasticache service crate (service-com-amazonaws-elasticache-amazonelasticachev9) must consult the cluster's SecurityGroups against the EC2 security-group state when accepting a connection to a node endpoint, and refuse at TCP accept (drop, so the client fails at connect) when no ingress rule admits the source; the same gate must restrict reachability of the advertised endpoint to clients inside the cluster's VPC, and must be re-evaluated per connection so an Authorize/Revoke on the group changes reachability for new connections.

## Evidence
Check: elasticache.elasticache-revoked-security-group-ingress-blocked-from-cache-data-plane (verdict: error).

Sequence and observations:
1. A dedicated security group was created and authorized for tcp/6379 from the executor group; DescribeSecurityGroups confirmed the rule (assertion passed).
2. A single-node redis cache cluster on that group reached available and advertised an endpoint; the probe picker's FIRST probe from `runner-outside-vpc` succeeded — on AWS no client outside the cluster's VPC can connect at all, so this vantage point could not exist.
3. PING/SET/GET through the authorized rule succeeded (+PONG, +OK, exact bulk value) — the positive barrier passed, so the data plane is genuinely serving.
4. RevokeSecurityGroupIngress on the same permission returned Return=true with no UnknownIpPermissions, and DescribeSecurityGroups confirmed the group no longer admits tcp/6379 from the executor group (both assertions passed).
5. Expected: every subsequent probe fails with ok=false and stage="connect" (AWS drops the packet, so TCP connect is what fails), the SET attempted meanwhile leaves no key (GET -> `$-1`, EXISTS -> `:0`), and re-authorizing readmits with DBSIZE 1.
   Actual: the probe kept returning ok=true for every attempt of the bounded poll after the revoke; the check never observed a refusal and ended with
   `TimeoutError: [runner-outside-vpc] the revoked rule taking effect on 127.0.0.1:43063: the run budget ended first; not graded`
   (host:port is a per-run value and is not part of the requirement; the requirement is that connections to the node endpoint stop succeeding once the only admitting rule is withdrawn).

Note on grading: the check's own wording says "not graded" because the harness run budget expired before its block-poll ceiling; the observation it did make is still the emulator's — every post-revoke connection attempt succeeded, and the pre-revoke success from a vantage point outside the VPC is itself unreachable on AWS.

## Steps to Reproduce
1. ec2: create a VPC with subnets and a self-referencing executor security group; create a second security group CACHE_SG and AuthorizeSecurityGroupIngress on it for tcp/6379 from the executor group (a group-pair rule, not a CIDR). Confirm with DescribeSecurityGroups.
2. elasticache: CreateCacheSubnetGroup over those subnets, then CreateCacheCluster (Engine=redis, CacheNodeType=cache.t3.micro, NumCacheNodes=1, Port=6379, SecurityGroupIds=[CACHE_SG], TransitEncryptionEnabled=false); wait for available and read the node endpoint via DescribeCacheClusters(ShowCacheNodeInfo=true).
3. From a client that is NOT in that VPC (the test runner itself), open a TCP connection to the advertised endpoint and send PING. Expected on AWS: no connection at all. Emulator: +PONG.
4. ec2: RevokeSecurityGroupIngress(GroupId=CACHE_SG, IpPermissions=[the same permission]); assert Return=true and that DescribeSecurityGroups no longer shows the rule.
5. Re-probe the endpoint with PING repeatedly for up to ~240 s. Expected: the connection fails at the TCP connect stage. Emulator: PING keeps returning +PONG indefinitely.
6. Issue SET <key> <value> through the withdrawn rule, re-authorize the identical rule, then GET <key>. Expected: `$-1` (the command never reached the engine). Emulator: the write lands.
