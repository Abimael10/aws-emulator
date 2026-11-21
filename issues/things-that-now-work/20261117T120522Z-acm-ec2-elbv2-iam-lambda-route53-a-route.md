Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header
Issue-Key: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header|_poll_until#29|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# acm__ec2__elbv2__iam__lambda__route53: a Route 53 alias record for an ALB never resolves to an address the load balancer's own DNS name resolves to

## Resolution

The invariant this report names — "the resolver/state that answers a query for an alias record must
return the address set the aliased load balancer announces, and the ALIAS record must become
resolvable once the change is applied" — HOLDS in the emulator, and is now pinned end to end by
`core/router/tests/all/route53_alias_fronted_alb_https_listener.rs`, which drives exactly this
report's trajectory over three alias names (a label under the zone's apex, two labels, the apex
itself):

* an `A` alias whose `AliasTarget.DNSName` is what `CreateLoadBalancer` published as the load
  balancer's `DNSName`, written with the load balancer's own `CanonicalHostedZoneId`, is answered by
  `TestDNSAnswer` with the ADDRESS that published name resolves to and with no port
  (`services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/test_dns_answer.rs`,
  `src/state.rs::dns_name_address`) — so the alias name and the load balancer's own name share an
  address;
* the name is resolvable BECAUSE of the change: before the batch the same query answers `NXDOMAIN`
  (`NOERROR` with no record data at the zone's apex, which carries the zone's `NS`/`SOA` sets), and
  after it reaches `INSYNC` it answers that address;
* and the address really is the load balancer's: a client that resolves the alias name to it and
  addresses the load balancer by that name over TLS is served by the HTTPS listener on the issued
  ACM certificate — the rule's `200` for a request carrying the gating header, the listener's
  default `403` for one without.

What REMAINS for this check, and no service crate can serve it: the check resolves BOTH names
through the HOST resolver of the vantage point it runs from. Nothing the emulator publishes is
resolvable there — a load balancer reports the `host:port` of the socket the core opened for it
(`services/.../src/state/load_balancer_endpoint.rs`), and a hostname (the alias name, or a name in
Elastic Load Balancing's own zone) would answer nowhere. Serving it needs the core listener API to
accept a REQUESTED port and a UDP socket: `Listeners` offers `tcp`, `tcp_plaintext` and `http` only,
each bound by `bind_std`/`bind_tokio` on an OS- or operator-chosen port
(`vendor/server/src/listeners.rs`), so this emulator can neither stand up the authoritative name
server a resolver would consult on 53 nor answer a bare host on 443. That gap is the subject of the
open issues `20261117T145616Z-route53-7-net-new-causes-the-authoritative-dns.md` and
`20261117T134237Z-acm-cloudfront-iam-lambda-route53-s3-a-route-53-public-hosted-zone-s-delegated`
(the delegation set resolves nowhere and answers nothing), and it is the same core gap recorded for
the API Gateway custom domain in
`issues/things-that-now-work/20261114T121226Z-acm-apigateway-a-regional-custom-domain-s-data.md`.

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ChangeResourceRecordSets

## Description
Cause 1 of 1: a Route 53 ALIAS A record whose AliasTarget is an Application Load Balancer never becomes resolvable to the same address the load balancer's own DNS name resolves to, so an HTTPS data-plane scenario fronted by a custom alias name can never be exercised.

AWS: recorded behavior only for the resolution step — the Route 53 model (ChangeResourceRecordSets / ResourceRecordSet.AliasTarget with DNSName + HostedZoneId) states only that an alias target is accepted; AWS's documented behavior is that an A-type alias to an ELBv2 load balancer resolves to the very same address set as that load balancer's published DNSName, and it becomes resolvable once the change set reaches INSYNC. The recording shows the check polling for exactly that invariant ("the alias answers sharing an address with the load balancer's own name").

Emulator: the poll never succeeds. The check is cut off with `TimeoutError: the alias answers sharing an address with the load balancer's own name: the run budget ended first; not graded`. Either the alias name resolves to nothing, or it resolves to an address disjoint from the one the load balancer's own name resolves to; in both cases the two names never agree, so no request can be addressed to the ALB through its alias.

Why: a client that provisions an ALB, publishes a friendly name for it with a Route 53 alias, and then drives traffic (here: an HTTPS listener on an issued ACM certificate gating a fixed 200 on a secret header) at that friendly name never reaches the load balancer. Every alias-fronted integration — certificate SNI selection, listener-rule gating, redirect targets — silently stops working, and because the name simply never answers the failure looks like a hang rather than an error.

Fix: the Route 53 alias resolution path and its binding to the ELBv2 load balancer's published address — the resolver/state that answers a query for an alias record must return the address set the aliased load balancer announces, and the ALIAS record must become resolvable once the change is applied.

Traces: acm__ec2__elbv2__iam__lambda__route53.alb-https-listener-on-an-issued-certificate-gates-a-fixed-200-on-a-secret-header.

Related but not covering: 20261115T034057Z-ec2-elbv2-reopens-the-alb-still-announces-an.md records that the ALB announces an address that is not the served endpoint; this finding is about the Route 53 alias never agreeing with the load balancer's own name at all.

## Evidence
Expected: the alias name resolves to at least one address that the load balancer's own DNS name also resolves to, within the check's poll budget, after which the HTTPS listener serves a fixed 200 for a request carrying X-Sim2real-Origin-Token and a fixed 403 without it.

Actual (replay):
  TimeoutError: the alias answers sharing an address with the load balancer's own name: the run budget ended first; not graded
  (check_acm__ec2__elbv2__iam__lambda__route53_alb_https_listener_...py line 534 -> _poll_until line 71)

The check never reaches its grading step, so the listener behavior behind the alias is unverified.

## Steps to Reproduce
1. Create a VPC with two subnets, a security group, and an internet-facing ALB; wait for State.Code=active.
2. Request/issue an ACM certificate and create an HTTPS listener on the ALB whose default action is a fixed 403 and whose rule returns a fixed 200 when the secret header is present.
3. Create a Route 53 hosted zone and a ChangeResourceRecordSets UPSERT of an A record with AliasTarget = {DNSName: <alb DNSName>, HostedZoneId: <alb CanonicalHostedZoneId>, EvaluateTargetHealth: false}.
4. Resolve the alias name and the load balancer's own DNSName and compare their address sets.
Observed: the two never share an address; the poll exhausts its budget. Expected: they agree, and requests to the alias name reach the listener.
