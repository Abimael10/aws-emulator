Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-instances-catalog-list-instance-types
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-instances-catalog-list-instance-types
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances|ListInstanceTypes|2xx->2xx|$.InstanceTypes[*]
Issue-Commit: 40b4c40578424e9a73c61204c7ab51c695d87313

# workspaces-instances ListInstanceTypes serves none of the WorkSpace Instance type catalog

> _Note: this issue was identified against the code base at commit `40b4c40578424e9a73c61204c7ab51c695d87313`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstanceTypes

## Description
One cause.

Cause 1 of 1: ListInstanceTypes answers HTTP 200 with an InstanceTypes list that contains none of the WorkSpace Instance types the service publishes, so the catalog read returns no usable record.

AWS: ListInstanceTypes is a read-only, paginated catalog read (Smithy: ListInstanceTypes, input ListInstanceTypesRequest {MaxResults @range 1..600, NextToken @length 1..2048 @sensitive, InstanceConfigurationFilter}, output ListInstanceTypesResponse {InstanceTypes @required, NextToken}; paginated inputToken=NextToken outputToken=NextToken pageSize=MaxResults items=InstanceTypes; errors AccessDeniedException 403, ValidationException 400, ThrottlingException 429, InternalServerException 500). Each element is InstanceTypeInfo {InstanceType @length 1..64 @pattern ^([a-z0-9-]+)\.([a-z0-9]+)$, SupportedInstanceConfigurations}, and each SupportedInstanceConfiguration names a valid BillingMode / PlatformType / Tenancy combination for that type. The content of the catalog itself is recorded behavior only: the recording shows a large catalog (hundreds of entries, indices 0..596 in the recorded page), e.g. InstanceType "c5.12xlarge" carrying the full cross-product of BillingMode HOURLY|MONTHLY, Tenancy SHARED|DEDICATED and PlatformType values such as "Linux/UNIX", "Windows", "Windows BYOL", "Red Hat Enterprise Linux", "Red Hat BYOL Linux", "Ubuntu Pro Linux" and "SUSE Linux". The requirement is that the operation enumerates the service's supported instance types with their supported configuration combinations; the specific type names and the exact page length are the catalog's content, not a per-run value.

Emulator: status matches (200), but every recorded element is reported missing and the diff reports no unexpected element, i.e. the served InstanceTypes list carries none of the recorded types ($.InstanceTypes[0] missing (expected InstanceType c5.12xlarge with its SupportedInstanceConfigurations) through $.InstanceTypes[596] missing). The @required output member is therefore present but empty of real catalog content.

Why: a client that enumerates supported WorkSpace Instance types before allocating one — to pick a type, or to validate a tenancy/platform/billing combination — sees an empty catalog. It concludes no instance type is available and cannot proceed, or it skips validation entirely and sends a type the service will later refuse. Pagination built on this read (MaxResults/NextToken) also exercises nothing.

Fix: the ListInstanceTypes handler and the static instance-type catalog it projects from: the catalog needs the service's instance types, each with its supported (BillingMode, PlatformType, Tenancy) combinations, and the handler must page over them honoring MaxResults/NextToken.

No open issue carries this replay key (the divergence reports owned_by empty). The nearest open issue is "ec2 static catalogs serve synthesized records: instance types, type offerings and VPN device types (3 causes)" (20261114T052927Z-ec2-static-catalogs-serve-synthesized-records-instance-types.md), but that one is filed against ec2's DescribeInstanceTypes catalog and does not cover workspaces-instances ListInstanceTypes.

Traces: workspaces-instances-catalog-list-instance-types#task#1.

## Evidence
Fixture workspaces-instances-catalog-list-instance-types, step task#1, operation ListInstanceTypes (service workspaces-instances), verdict mismatch.
Expected status 200, actual status 200.
Diff (from replay): "$.InstanceTypes[0]: missing (expected {'InstanceType': 'c5.12xlarge', 'SupportedInstanceConfigurations': [{'BillingMode': 'MONTHLY', 'PlatformType': 'Red Hat Enterprise Linux', 'Tenancy': 'DEDICATED'}, {'BillingMode': 'HOURLY', 'PlatformType': 'Windows BYOL', 'Tenancy': 'SHARED'}, ...]})" and the same "missing" verdict for $.InstanceTypes[1] .. $.InstanceTypes[596] — every recorded element, with no unexpected element reported in the other direction.
Smithy: ListInstanceTypesResponse.InstanceTypes is @required; member InstanceTypeInfo {InstanceType @pattern ^([a-z0-9-]+)\.([a-z0-9]+)$, SupportedInstanceConfigurations}; operation is @readonly and @paginated (items=InstanceTypes).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture workspaces-instances-catalog-list-instance-types, step task#1: a workspaces-instances ListInstanceTypes call with no InstanceConfigurationFilter.
3. Observe HTTP 200 whose InstanceTypes list contains none of the recorded WorkSpace Instance types (e.g. no element with InstanceType "c5.12xlarge" and its SupportedInstanceConfigurations); the recording's page carries 597 entries.

## Resolution

The premise "the served list carries none of the recorded types" does not hold against this
checkout: `ListInstanceTypes` answers 200 with 588 instance types in name order, the first of
them `c5.12xlarge` — the recording's first entry — carrying exactly the 28 `(BillingMode,
PlatformType, Tenancy)` combinations the recording lists for it. Every recorded element is
reported "missing" because the replay compares an element as a whole, and each element differs
in the ORDER its `SupportedInstanceConfigurations` are serialised in.

FIXED: catalog MEMBERSHIP is now the rule rather than transcribed data. A family's size ladder
was written out per family in `state.rs` (20 `SIZES_*` constants), a hand-kept copy of the
published instance-type names whose drift nothing could catch; `published_sizes` now reads the
ladder out of the instance-type enumeration this workspace ships (the EC2 service model's
`InstanceType`), bare metal and any size outside the modelled `InstanceType` pattern excluded.
A catalogued family is therefore reported at every size that enumeration publishes it at.
Covered by `tests/all/instance_type_catalog_published_sizes.rs`, which pins the whole ladder of
33 families across generation, vendor and ladder shape, checks no bare-metal or non-x86 name is
reported, and launches a type picked out of the answer so a reported record is a usable one.

RESIDUAL, declined — the replay key `workspaces-instances|ListInstanceTypes|2xx->2xx|$.InstanceTypes[*]`
still diverges, so do NOT read this file as that key passing:

1. The SEQUENCE each entry's combinations are serialised in. `SupportedInstanceConfigurations`
   is documented as "all valid combinations … the complete set of configuration options
   available for this instance type" — a set — and the model attaches no ordering to it, so the
   recorded permutation is one run's serialisation order and follows no rule AWS publishes. Four
   of the 28 recorded positions are even readable from the divergence, so it could only be met
   by pasting a recording, not by implementing anything. The served sequence is deterministic
   and stable across calls.
2. The entry COUNT: the recording holds 597 names, this catalog 588. Which further names the
   service's catalog holds is service data that no model, enumeration or published list in this
   checkout names, and inventing nine family rows to close the count would be synthesis.
