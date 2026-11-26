Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 0124150dcd3f028fd1f7b257c908b2802f0f11c2

# A SELF_DEPLOYED create is refused for the placement references of its inert `hosts` entries, although every sibling check on that same member is gated on installs_vcf

_Filed: 2026-11-19T06:29:01Z_
_Operation: CreateEnvironment | Class: validation_

> _Note: this issue was identified against the code base at commit `0124150dcd3f028fd1f7b257c908b2802f0f11c2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateEnvironment` resolves `hosts[*].placementGroupId` / `hosts[*].dedicatedHostId` against the account's Amazon EC2 store UNCONDITIONALLY, so a `vcfVersion: SELF_DEPLOYED` request whose (ignored) `hosts` entries name a placement group or Dedicated Host the account does not hold is answered HTTP 400 ValidationException and creates nothing.

In that mode the handler adds no host at all — `add_requested_hosts` returns immediately — so no host is ever placed on the named group or Dedicated Host, and there is nothing for the reference to be resolved for. Every OTHER check this handler runs over a member the mode "does not support" is gated on `installs_vcf` for exactly that reason: the `vcfHostnames` uniqueness rule (line 443), the `connectivityInfo` route-server-peer resolution (line 500) and the host DNS-hostname uniqueness rule over the SAME `hosts` member (line 562). Only the EC2 placement resolution reads a fact out of `hosts` in a mode where the crate's own rule calls the member inert.

In scope: the EC2 resolution block at create_environment.rs:462-491 should be gated on `installs_vcf`, like its three siblings. NOT in scope: the `dedicatedHostId`/`placementGroupId` conflict check (line 437 / `host_failures`), which is the SHAPE's own rule ("You cannot use dedicatedHostId and placementGroupId together in the same HostInfoForCreate object. This results in a ValidationException response.") and so holds whenever the object is present.

## Evidence

Smithy, `CreateEnvironmentRequest$hosts` (smithy_query doc): "The ESX hosts to add to the environment... Not supported when <code>vcfVersion</code> is <code>SELF_DEPLOYED</code>. In that case, you can add hosts using <code>CreateEnvironmentHost</code> after the environment is created." — the same sentence the gated sibling quotes.

UNGATED — src/ops/create_environment.rs:462-491:
```
// The EC2 resources each requested host is PLACED on, from that same store: a
// `placementGroupId` or `dedicatedHostId` names a resource of the caller's own
// account, which Amazon EVS does not create. ...
        {
            let references: Vec<crate::state::ec2_resources::HostPlacementReferences<'_>> = input
                .hosts
...
            failures.extend(crate::state::ec2_resources::host_placement_failures(
                &*self.engine,
                &references,
            ));
        }
```

GATED SIBLING on the same doc sentence — src/ops/create_environment.rs:496-500:
```
// one the account already holds, resolved exactly as the network is. Asked only
// where Amazon EVS INSTALLS VCF, because that is the mode the peering is part of
// and the member is "Not supported when vcfVersion is SELF_DEPLOYED", where it is
// accepted and does nothing.
        if installs_vcf {
```

GATED SIBLING on the SAME `hosts` member — src/ops/create_environment.rs:556-562:
```
// entry that named it is refused. Asked only where the hosts ARE added, because a mode
// that adds none takes no name.
...
        if installs_vcf {
```

Nothing is placed in that mode — src/ops/create_environment.rs:942-955 (`add_requested_hosts`): "`installs_vcf` false adds NOTHING: 'When you specify SELF_DEPLOYED for vcfVersion, Amazon EVS provisions only the VLAN subnets; no hosts are added...' — in that mode the member is accepted and does nothing, exactly as `clientToken` is." followed by `if !installs_vcf { return Vec::new(); }`.

The crate's own rule — src/state.rs:2081-2084 (`StoredEnvironment::installs_vcf`): "The members that mode \"does not support\" — `licenseInfo`, `hosts`, `vcfHostnames`, `siteId` and `connectivityInfo` — are accepted and reported back as supplied, and this is what says they are INERT: the environment installs no VCF, so nothing the service derives may read a fact out of one of them."

Test coverage: no test pins the current behaviour. tests/all/created_environment_hosts.rs:398-408 is the only SELF_DEPLOYED `hosts` row and it uses `Placement::None`; tests/all/create_environment_validation.rs:212-225 ("every parameter SELF_DEPLOYED does not use, and terms not accepted") sends `hosts_with(&json!({}), &[])`, i.e. no placement ids, and expects only `termsAccepted` to be refused; tests/all/host_placement_references.rs exercises placement references only through `CreateEnvironmentHost` and through installing VCF versions.

## Steps to Reproduce

1. Mount the EVS and EC2 services on one engine and register a VPC plus a subnet in the EC2 store (as tests/all/host_placement_references.rs does), and grant the Premium Support subscription.
2. Call CreateEnvironment with `vcfVersion: "SELF_DEPLOYED"`, `termsAccepted: true`, valid `initialVlans`, and `hosts` = four HostInfoForCreate entries where one carries `"placementGroupId": "pg-00000000"` (an id the account holds no placement group for).
3. Observe HTTP 400 ValidationException with a `fieldList` entry named `hosts[N].placementGroupId`, and no environment created.
4. Expected: HTTP 200 — the mode adds no host, so the reference places nothing; compare the same request with a repeated `vcfHostnames` hostname or two identical host names, both of which ARE accepted in this mode (create_environment.rs:443, :562), and with `connectivityInfo.privateRouteServerPeerings` naming peers the account does not hold, which is accepted in this mode (create_environment.rs:500).
