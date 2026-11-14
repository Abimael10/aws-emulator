Issue-Source: trace-sweep-divergence
Issue-Trace: vpclattice-service-and-listener-lifecycle
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: vpclattice-service-and-listener-lifecycle vpclattice-service-network-service-association
Issue-Shape-Id: com.amazonaws.vpclattice#MercuryControlPlane
Issue-Key: vpc-lattice|CreateService|2xx->2xx|$.dnsEntry.domainName
Issue-Key: vpc-lattice|GetService|2xx->2xx|$.dnsEntry.domainName
Issue-Key: vpc-lattice|CreateServiceNetworkServiceAssociation|2xx->2xx|$.dnsEntry.domainName
Issue-Key: vpc-lattice|GetServiceNetworkServiceAssociation|2xx->2xx|$.dnsEntry.domainName
Issue-Key: vpc-lattice|ListServiceNetworkServiceAssociations|2xx->2xx|$.items[*].dnsEntry.domainName
Issue-Commit: 3c7c88759e40d16b7eb6d71c08044cc0e7353249

# vpc-lattice announces DnsEntry.domainName in the loopback zone instead of a host the emulator serves

> _Note: this issue was identified against the code base at commit `3c7c88759e40d16b7eb6d71c08044cc0e7353249`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Every vpc-lattice response that carries a `DnsEntry` announces a `domainName` whose zone is the literal label `localhost`, which is not a host this emulator is reached on or serves. The replay checker flags each one `host_not_own`.

This is ONE shared root cause (`announced_dns_zone` in `src/state/announced_dns.rs` collapses any authority whose host is an IP ADDRESS to the constant `LOOPBACK_ZONE = "localhost"`, so a caller that reached the emulator at its address is handed a name in a zone that is the caller's own loopback) surfacing in FIVE distinct operation clusters. A fixer must close ALL FIVE: the entry is rebuilt on every read, so correcting only the create path leaves the read and list paths diverging.

Cause 1 of 5: CreateService. AWS: Smithy `CreateService` output member `dnsEntry` (DnsEntry) is documented "The public DNS name of the service"; `DnsEntry.domainName` (String) is "The domain name of the service", paired with `hostedZoneId`, "The ID of the hosted zone". AWS returns a resolvable public DNS name for the service in a zone AWS actually serves, so a client can feed it to a resolver, put it in a `Host` header and use it as a TLS server name and reach the service. The literal name is account/region specific, so the fixture asserts the SHAPE and OWNERSHIP of the value, not its text (recorded expectation `<shaped-not-asserted>`): the announced host must be one the responder serves. Emulator: on the 201 CreateService response the emulator returns `$.dnsEntry.domainName` ending in the zone `localhost` (recorded run: `<service-name>-<id-suffix>.localhost`), while the request reached the emulator on its own data-plane address, not on `localhost`. Why: a client that resolves the announced name connects to its OWN loopback interface rather than to the emulated lattice service, so every service-to-service call built from a CreateService response fails to reach the emulator; the value is also unusable as the target of a DNS record paired with the returned `hostedZoneId`. Fix: the DNS-zone reading shared by this service's `DnsEntry` construction — `announced_dns_zone`/`zone_of_authority` must announce the host the request actually reached the emulator on (including when that authority's host is an address) rather than substituting the loopback name, and `announced_dns_entry` must build every entry through it. Traces: vpclattice-service-and-listener-lifecycle#task#1.

Cause 2 of 5: GetService. AWS: Smithy `GetService` output carries the same `dnsEntry` (DnsEntry) member; a read reports the same public DNS name the create announced, in a zone the responder serves. Emulator: the 200 GetService response repeats `$.dnsEntry.domainName` in the `localhost` zone — the entry is recomputed on read through the same zone reading, so the read diverges independently of the create. Why: a client that re-reads a service to discover its DNS name (the normal discovery path, since the name is not stored by the client) is handed an unreachable loopback name. Fix: the GetService response's DnsEntry construction, via the same shared zone reading. Traces: vpclattice-service-and-listener-lifecycle#task#2.

Cause 3 of 5: CreateServiceNetworkServiceAssociation. AWS: Smithy `CreateServiceNetworkServiceAssociation` output member `dnsEntry` (DnsEntry) reports the ASSOCIATED SERVICE's DNS information — the name under which the service is reachable through the service network. Emulator: the 200 response returns `$.dnsEntry.domainName` in the `localhost` zone. Why: the association response is exactly what a caller uses to wire traffic from the service network to the service, so an unreachable loopback name breaks the association's only usable output. Fix: this operation's DnsEntry construction, via the shared zone reading. Traces: vpclattice-service-network-service-association#task#4.

Cause 4 of 5: GetServiceNetworkServiceAssociation. AWS: Smithy `GetServiceNetworkServiceAssociation` output carries the same `dnsEntry` (DnsEntry), reporting the associated service's DNS name. Emulator: the 200 read response returns `$.dnsEntry.domainName` in the `localhost` zone. Why: same as cause 3 for the read path a client polls while the association settles. Fix: this operation's DnsEntry construction, via the shared zone reading. Traces: vpclattice-service-network-service-association#task#5.

Cause 5 of 5: ListServiceNetworkServiceAssociations. AWS: each `ServiceNetworkServiceAssociationSummary` in the output `items` list carries `dnsEntry` (DnsEntry) with the associated service's DNS name. Emulator: the 200 listing returns `$.items[0].dnsEntry.domainName` in the `localhost` zone. Why: a client enumerating associations to build its routing/DNS configuration receives only unreachable loopback names. Fix: the summary serializer for this operation's `items`, via the shared zone reading. Traces: vpclattice-service-network-service-association#task#7.

No open issue covers this: the existing vpc-lattice issues concern id minting (20261030T175927Z, 20261103T105308Z), the BatchUpdateRule/UpdateRule path (20261101T145430Z), target registration and target state (20261114T134415Z), unimplemented stubs (PutAuthPolicy, TagResource, CreateServiceNetworkVpcAssociation) and CloudFormation resource-type coverage. None of them mentions `DnsEntry` or the announced DNS zone.

## Evidence
Replay of trace set simparity-wire-aws-v1 (5 net-new mismatch clusters, all verdict `mismatch`, all `host_not_own` on the announced domain name; status matched in every case):

1. vpclattice-service-and-listener-lifecycle#task#1 CreateService — expected 201, got 201; `$.dnsEntry.domainName: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-lat-svc-b52d9f07-09e9c1e7812e84565.localhost')`
2. vpclattice-service-and-listener-lifecycle#task#2 GetService — expected 200, got 200; `$.dnsEntry.domainName: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-lat-svc-b52d9f07-09e9c1e7812e84565.localhost')`
3. vpclattice-service-network-service-association#task#4 CreateServiceNetworkServiceAssociation — expected 200, got 200; `$.dnsEntry.domainName: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-lat-assvc-f73b1d40-07bdab70d55ce3c83.localhost')`
4. vpclattice-service-network-service-association#task#5 GetServiceNetworkServiceAssociation — expected 200, got 200; same `host_not_own` on `$.dnsEntry.domainName`
5. vpclattice-service-network-service-association#task#7 ListServiceNetworkServiceAssociations — expected 200, got 200; `$.items[0].dnsEntry.domainName: host_not_own`

The recorded expectation is `<shaped-not-asserted>`: the fixture does not pin the literal domain name (it is account/region/environment specific) — it requires the announced host to be one the responder serves. The first label (`<service-name>-<service-id-suffix>`) is correct and environment-independent; the diverging part is the ZONE, the fixed label `localhost`.

Source of the behavior in the checkout: `services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state/announced_dns.rs` — `const LOOPBACK_ZONE: &str = "localhost"` (line 45), `zone_of_authority` returns `LOOPBACK_ZONE` whenever `host.parse::<std::net::IpAddr>().is_ok()` (lines 178-180), and `announced_dns_entry` (lines 117-125) is the one construction every operation above calls (e.g. `src/ops/create_service.rs` line 174). The emulator's own health probe reports it is reached on the `:40683` data listener, i.e. at an address, so every replayed request takes the `LOOPBACK_ZONE` branch.

## Steps to Reproduce
1. Boot the emulator and note the data-plane listener it is reached on (the replay reports `/__admin/health answered 200 and the :40683 data listener accepts`); address it by that host/address, not by the name `localhost`.
2. Replay fixture `vpclattice-service-and-listener-lifecycle`: step `task#1` calls CreateService with a service name, step `task#2` calls GetService on the returned id. Inspect `$.dnsEntry.domainName` in both responses — it ends in `.localhost` instead of a zone this emulator answers in for the authority the request used.
3. Replay fixture `vpclattice-service-network-service-association`: step `task#4` (CreateServiceNetworkServiceAssociation), step `task#5` (GetServiceNetworkServiceAssociation) and step `task#7` (ListServiceNetworkServiceAssociations). Inspect `$.dnsEntry.domainName` on steps 4 and 5 and `$.items[0].dnsEntry.domainName` on step 7 — all three carry the same `.localhost` zone.
4. Try to resolve/connect to any announced `domainName`: it resolves to the caller's own loopback interface rather than to the emulator, so the announced name reaches no emulated lattice service.
5. Re-run `replay_wire_corpus` after the fix and confirm all five steps pass (the first label stays as-is; only the zone must change).
