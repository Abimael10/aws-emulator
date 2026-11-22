Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header
Issue-Key: route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header|_nameserver_ips#5|AssertionError|assert entry["addresses"], (
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# route53: a hosted zone's delegated nameservers resolve nowhere, so no record the zone holds can be looked up

> _Note: this cause was filed against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`, as cause 2 of 2 of `20261117T143723Z-route53-s3-2-net-new-causes-an-apex.md`. Cause 1 of that report (an apex alias whose `AliasTarget.HostedZoneId` does not match the aliased S3 website endpoint was accepted) is fixed and that report is resolved; this cause is re-filed on its own because it is BLOCKED by the read-only core (see "Blocked by" below), not because it is unreal. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateHostedZone

## Description
Cause 1 of 1: the nameservers a created hosted zone is delegated to are AWS-shaped names that resolve to no address, so nothing can query the zone the emulator serves.

AWS: Smithy (route53) models CreateHostedZone -> DelegationSet.NameServers; those names resolve to addresses that answer DNS for the zone, which is the only way a record the zone holds can be observed. The host names themselves are environment-specific and are not a requirement — what is a requirement is that the names the service hands back are reachable where the caller is, and that DNS for the zone is answered at the address they resolve to.

Emulator: resolving the delegated nameserver fails — `AssertionError: the delegated nameserver ns-751.awsdns-27.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}`. The emulator announces public AWS nameserver names (`state.rs`, `mint_name_servers`) rather than an address it serves DNS on, so no record it holds can be looked up. (The particular `ns-…` label is a per-run value; the defect is announcing names the emulator serves no DNS on.)

Why: host-header-based selection of an S3 website bucket through a Route 53 record cannot be exercised; more generally, every DNS-driven integration (alias to a website endpoint, weighted/latency records, CNAME validation) is unobservable because the zone's own delegation points away from the emulator. Announcing a name AWS itself serves is also a name that, in an environment with outbound DNS, resolves to genuine AWS infrastructure.

Fix: the CreateHostedZone/GetHostedZone/CreateReusableDelegationSet `DelegationSet.NameServers` the emulator announces, and the DNS listener that must answer for the zone on the address it announces. The zone's records are already resolved in-process by `src/ops/test_dns_answer.rs`, so the answering logic exists; what is missing is a socket a resolver can reach.

Blocked by (as of the commit above): the core's listener API cannot serve DNS. A resolver asks port 53, over UDP first (`getaddrinfo`, `dig`, `dnspython`), and a service may not open a socket of its own — `vendor/engine/src/lib.rs:21-35` ("the service never binds a socket, chooses a port, or configures TLS itself — the core owns the socket, TLS and bind interface"). `vendor/server/src/listeners.rs` offers only `Listeners::tcp` (TLS, line 204), `Listeners::tcp_plaintext` (line 230) and `Listeners::http` (line 381); all three are TCP, and all three take their port from `bind_std`/`bind_tokio` (lines 67-107) — an OS-assigned ephemeral port or one from `EMULATOR_LISTENER_PORT_RANGE`. There is no UDP listener in `vendor/` and no way to request a protocol's own service port. Required core change: a `Listeners::udp(handler)` sibling of `tcp_plaintext`, bound on the operator-chosen interface, plus a way for a service to request the protocol's service port, so a zone's delegated nameserver can answer a resolver's datagram at the address the control plane announces.

Do NOT "fix" this by announcing a name that merely resolves (`localhost`, a loopback address) while nothing answers DNS on it, nor by serving DNS on an ephemeral TCP port no resolver will dial: both announce an endpoint the caller cannot use, which is a fake success.

Traces: route53__s3.route53-record-selects-the-s3-website-bucket-by-host-header.

## Evidence
Expected: each name in CreateHostedZone's DelegationSet.NameServers resolves to an address that answers DNS for the zone. Actual: 'the delegated nameserver ns-67.awsdns-50.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}' (replayed again after cause 1 was fixed; the label differs per run).

## Steps to Reproduce
CreateHostedZone and read DelegationSet.NameServers; resolve each name from the caller's vantage point and query it for a record in the zone.
