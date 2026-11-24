Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain route53.route53-cname-answer-carries-the-chased-in-zone-target route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name route53.route53-alias-evaluate-target-health-withholds-the-alias-answer route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state route53.route53-second-primary-failover-record-in-the-same-group-rejected route53.calculated-health-check-threshold-decides-the-failover-answer route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver route53.route53-traffic-policy-instance-records-are-served-and-withdrawn
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve|_nameserver_ips#5|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-cname-answer-carries-the-chased-in-zone-target|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-alias-evaluate-target-health-withholds-the-alias-answer|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-second-primary-failover-record-in-the-same-group-rejected|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.calculated-health-check-threshold-decides-the-failover-answer|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-traffic-policy-instance-records-are-served-and-withdrawn|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# route53: the authoritative DNS plane is unreachable — the emulator serves no DNS wire at all, so the hosts `DelegationSet.NameServers` publishes resolve to nothing and no query following the delegation can ever be answered

> _Split out of `issues/things-that-now-work/20261117T145616Z-route53-7-net-new-causes-the-authoritative-dns.md`, whose causes 2-7 (the six control-plane refusal defects) are fixed. This report carries the 13 replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
CreateHostedZone

## Description
The name servers a hosted zone delegates to are AWS hostnames that resolve to nothing in
the environment, so no query can ever reach an authoritative server of this emulator —
and there is nothing for one to reach: no socket in this repository speaks the DNS wire.

AWS: `CreateHostedZone`/`GetHostedZone` report `DelegationSet.NameServers` (a list of
`DNSName`). The wire behaviour is not modelled, so this is recorded behaviour only: the
four hostnames Route 53 reports (form `ns-<n>.awsdns-<n>.<tld>`) are the zone's
authoritative servers, they RESOLVE to addresses, and those addresses answer queries for
names in the zone authoritatively (AA set) and `NXDOMAIN` for a name not in the zone.
Every check listed above first reads the delegation set, resolves each advertised host,
and only then queries it — exactly what a client following the delegation does.

Emulator: the advertised hostnames do not resolve at all from the vantage point the suite
queries from. Each check fails resolving the FIRST advertised name server, verbatim:
`AssertionError: the delegated nameserver ns-84.awsdns-03.com did not resolve from the
vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or
service not known'}` (the host label varies per zone and per run). The preceding
control-plane calls (`CreateHostedZone`, `ChangeResourceRecordSets`, `GetChange`) all
succeed, so the zone and its records exist; what is missing is that the emulator announces
a delegation set it is not reachable on. The generated label is a per-run value; the
requirement is that whatever `DelegationSet.NameServers` publishes must be resolvable AND
must be answered on, authoritatively, by a DNS responder of this emulator (UDP and TCP
port 53).

Why it is BLOCKED, and on what: a service crate may not open a socket of its own
("The service never binds a socket, chooses a port, or configures TLS itself",
`vendor/engine/src/lib.rs:21-35`), and the core listener API it must use offers no
datagram socket and no way to be served on a protocol's well-known port:
`vendor/server/src/listeners.rs` exposes only `Listeners::tcp` (:204),
`tcp_with_idle` (:213), `tcp_plaintext` (:230) and `http` (:381) — all stream sockets —
each bound on an EPHEMERAL port, or on one from the operator's port range
(`bind_std`/`bind_tokio`, :67-107). A delegated name server is reached by DATAGRAM on port
53, and an `NS` name carries no port at all, so neither half of the requirement is
reachable from a service crate today: there is no UDP listener to hand a DNS handler to,
and no way for a listener to take port 53.

The required core change, stated so a maintainer can make it: a datagram entry point on
`emulator_server::Listeners` (an `udp(handler)` beside `tcp`, handing each received
datagram to a handler and answering from the same socket), plus a way for such a listener
to take the protocol's well-known port and a name the control plane can publish that
resolves to it in the environment the suite queries from. With that in place the responder
itself is largely already modelled in-process and only needs wiring to the published
delegation set: `services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/test_dns_answer.rs`
already answers one simulated query from a zone's persisted record sets (routing policies,
alias chasing, `NOERROR`/`NXDOMAIN`), `services/common/src/public_dns_standard.rs` holds the
reserved-name readings every resolver here shares, and
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/vpc_network/`
shows the `simple_dns` packet parse/serialise already used in this tree.

What was ruled out by the parent fix, so it is not retried: publishing a name that
RESOLVES (an address literal, or a loopback name) while nothing answers on port 53 would
turn these 13 failures into timeouts and announce an endpoint nothing protocol-capable
serves — the forbidden "endpoint that resolves nowhere / speaks no protocol" answer — so
the delegation set was deliberately left exactly as it is until the responder exists.

Related: `20261117T134237Z-acm-cloudfront-iam-lambda-route53-s3-a-route-53-public-hosted-zone-s-delegated`
reports that the advertised servers never ANSWER over TCP; the 13 checks here never get
that far, because the hostname does not resolve. Both are the same missing capability.

Traces: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve,
route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver,
route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain,
route53.route53-cname-answer-carries-the-chased-in-zone-target,
route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name,
route53.route53-alias-evaluate-target-health-withholds-the-alias-answer,
route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state,
route53.route53-second-primary-failover-record-in-the-same-group-rejected,
route53.calculated-health-check-threshold-decides-the-failover-answer,
route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region,
route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records,
route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver,
route53.route53-traffic-policy-instance-records-are-served-and-withdrawn

## Evidence
Replay of the parent issue's 19 units after its fix: 6 passed (its causes 2-7, the
control-plane refusal defects), 13 failed, and the 13 keys above are still open.
`other_causes` was empty, so the parent fix caused none of these. Every one of the 13
fails identically, while resolving the FIRST host listed in the zone's
`DelegationSet.NameServers`:

    AssertionError: the delegated nameserver ns-<n>.awsdns-<n>.com did not resolve from
    the vantage point: {'addresses': [], 'errno': -2,
    'error': 'gaierror: [Errno -2] Name or service not known'}

Expected: every name published in `DelegationSet.NameServers` resolves and answers
authoritatively for names in the zone (and `NXDOMAIN` outside it). Actual: name resolution
of the advertised host fails, so no query is ever sent.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. `CreateHostedZone` for a public zone, then `ChangeResourceRecordSets` to put an `A`
   record in it and `GetChange` until the change is `INSYNC`.
3. Read `DelegationSet.NameServers` off the create (or `GetHostedZone`), and from a client
   in the environment resolve each advertised host name.
4. Observe `gaierror` (the name does not resolve) rather than addresses; a DNS query for
   the record is therefore never sent, and the authoritative answer the suite grades
   (routing policy, alias chasing, `NXDOMAIN` for an absent name) is never observed.
5. Replay any of the 13 check ids in Issue-Trace-Ids.
