Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: vpc-lattice.service-network-service-association-propagates-dns-and-clears
Issue-Shape-Id: com.amazonaws.vpclattice#MercuryControlPlane
Issue-Key: vpc-lattice.service-network-service-association-propagates-dns-and-clears|run#9|AssertionError|assert re.fullmatch(r"[A-Za-z0-9.-]+", minted) and "." in minted, (
Issue-Commit: 8f1e14e752eefe962d0fea3a4a4f8466d2e6dda3

# vpc-lattice dnsEntry.domainName is an emulator host:port authority, not a DNS name

> _Note: this issue was identified against the code base at commit `8f1e14e752eefe962d0fea3a4a4f8466d2e6dda3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
One net-new root cause.

Cause 1 of 1: every vpc-lattice response that serializes a `DnsEntry` fills `domainName` with the emulator's reached ENDPOINT AUTHORITY — a `host[:port]` string — instead of a DNS domain name. Failing check: vpc-lattice.service-network-service-association-propagates-dns-and-clears.

  AWS: Smithy `com.amazonaws.vpclattice#DnsEntry` has members `domainName` (String, "The domain name of the service.") and `hostedZoneId` ("The ID of the hosted zone."); `CreateService`'s output member `dnsEntry` is documented "The public DNS name of the service." (smithy_query query="shape" shape="DnsEntry"; query="doc" shape="CreateServiceResponse" member="dnsEntry"; `CreateService` 201 POST /services). The model places no `@pattern` or `@length` on `domainName`, so the exact text is recorded behavior only — but the member is a DNS NAME, paired with a hosted-zone id: it is a sequence of dot-separated DNS labels drawn from `[A-Za-z0-9-]`, with at least one dot, and it carries NO port and NO `:` separator. Real AWS hands out a resolvable multi-label hostname for the service; the port a client connects on comes from the service's listener, never from this member.

  Emulator: `CreateService` answers 201 with `$.dnsEntry.domainName` = the authority the request reached the emulator on, e.g. `127.0.0.1:40263` — a host plus a colon and a port. The same value is re-served by `GetService`, `CreateServiceNetworkServiceAssociation`, `GetServiceNetworkServiceAssociation` and `ListServiceNetworkServiceAssociations`, because they all build the entry through the same helper. The behavioral check asserts only the member's SHAPE (`re.fullmatch(r"[A-Za-z0-9.-]+", minted) and "." in minted`) and fails: `AssertionError: CreateService minted domain '127.0.0.1:40263', which is not a well-formed DNS name`. The host and port themselves are per-run values and are NOT the requirement; the requirement is that the member's VALUE SPACE be a DNS name — the `:<port>` suffix (and, when the emulator is reached on a single-label host, the absence of any dot) is the defect.

  Why: a client takes `dnsEntry.domainName` as a hostname — it feeds it to a resolver, puts it in a `Host` header, uses it as a TLS SNI/certificate subject, or stores it as a CNAME target alongside the `hostedZoneId` it is handed in the same structure. None of those accept an authority with an embedded port: resolution fails, the `Host` header is malformed, and any code that splits the value on `:` to recover a hostname gets a different string than the service minted. So every in-VPC service-to-service flow modelled on a Lattice service endpoint breaks at the first use of the announced name, even though the status and every other member match.

  Note: this is NOT covered by the resolved issue `20261110T212607Z-vpc-lattice-dnsentry-domainname-announces-a-real-aws.md`, which made the announced host the one the emulator is reached on. That change is the right direction and must be kept — the fix here is not to go back to AWS's `.vpc-lattice-svcs.<region>.on.aws` authority. The remaining defect is the FORM of the value: a raw endpoint authority was substituted where a DNS name belongs. No open vpc-lattice issue covers it either (the nearest, `20261030T175927Z`, `20261103T105308Z` and `20261101T145430Z`, concern rule/target-group id character form and rule-update messages; `20261114T134415Z` concerns target registration).

  Fix: the vpc-lattice state module's announced-DNS helper — the one place every `DnsEntry` of this service is built — must render a DNS NAME derived from the resource (its name and id) under a resolvable suffix the emulator serves, rather than returning the reached endpoint authority verbatim: no port may appear in `domainName`, and the result must be multi-label. The operations' serializers, which already read the helper, then need no change. Component: `services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state/announced_dns.rs` (`announced_dns_host` / `announced_dns_entry`), consumed by each op's `output_of` (e.g. `src/ops/create_service.rs`).

  Traces: vpc-lattice.service-network-service-association-propagates-dns-and-clears.

## Evidence
Behavioral check `vpc-lattice.service-network-service-association-propagates-dns-and-clears` — verdict `mismatch`:

```
File "check_vpc_lattice_service_network_service_association_propagates_dns_and_clears.py", line 71, in run
    assert re.fullmatch(r"[A-Za-z0-9.-]+", minted) and "." in minted, (
AssertionError: CreateService minted domain '127.0.0.1:40263', which is not a well-formed DNS name
```

The check creates a service network and a service, waits for the service to reach ACTIVE, then reads `svc["dnsEntry"]["domainName"]` from the CreateService response. The emulator answered 201 with every other member well-formed; only `dnsEntry.domainName` diverges, carrying an endpoint authority (`<host>:<port>`) where a DNS name is required. The literal `127.0.0.1` and `40263` are this run's host and port and are not part of the requirement — the requirement broken is that `domainName` must be a DNS name (dot-separated `[A-Za-z0-9-]` labels, at least one dot, no `:` and no port).

Because the assertion fires at the CreateService read, the rest of the check never ran: the later assertions that the association is born `CREATE_IN_PROGRESS`, that `GetServiceNetworkServiceAssociation`'s `dnsEntry.domainName` equals the service's minted name, and that `serviceArn` points at the created service, are unverified behind this one cause.

Source of the value in the checkout:
- services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state/announced_dns.rs:36-47 — `announced_dns_host()` returns `::service_common::data_plane_addressing::reached_emulator_endpoint(...)` / `emulator_endpoint_authority(ctx)`, documented as "The `host[:port]` every `DnsEntry` of this service announces".
- services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state/announced_dns.rs:55-63 — `announced_dns_entry()` puts that authority straight into `DnsEntry.domain_name`.
- services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state.rs:13-19 — the module is the single reading every operation that serializes a `DnsEntry` goes through.
- services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/ops/create_service.rs:174-177 — `output_of` serializes `crate::state::announced_dns_entry(ctx, &service.dns_hosted_zone_id)` into the 201 response.

Smithy consulted: shape `com.amazonaws.vpclattice#DnsEntry` (members `domainName`, `hostedZoneId`, both plain `String`, no constraints); `CreateServiceResponse$dnsEntry` doc "The public DNS name of the service."; `CreateService` http 201 POST /services.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a vpc-lattice client at it.
2. `CreateServiceNetwork(name=<unique>, authType="NONE")`.
3. `CreateService(name=<unique>, authType="NONE")` — it answers 201.
4. Read `$.dnsEntry.domainName` from that response. It is the authority the request reached the emulator on, e.g. `127.0.0.1:40263`: it contains a `:` and a port, so it is not a DNS name. Expected: a DNS name — dot-separated `[A-Za-z0-9-]` labels with at least one dot and no port — naming a host the emulator serves.
5. `GetService(serviceIdentifier=<id>)` once the service is ACTIVE, then `CreateServiceNetworkServiceAssociation`, `GetServiceNetworkServiceAssociation` and `ListServiceNetworkServiceAssociations`: each re-serves the same `host:port` value at `$.dnsEntry.domainName` (`$.items[*].dnsEntry.domainName` for the listing), since all of them build the entry through `announced_dns_entry`.
6. Feed the returned value to a resolver or use it as a `Host` header / TLS SNI value: it is rejected as a hostname.
