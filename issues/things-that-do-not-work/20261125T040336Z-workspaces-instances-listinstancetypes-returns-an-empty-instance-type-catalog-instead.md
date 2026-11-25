Issue-Source: trace-sweep-divergence
Issue-Trace: workspaces-instances-catalog-list-instance-types
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: workspaces-instances-catalog-list-instance-types
Issue-Shape-Id: com.amazonaws.workspacesinstances#EUCMIFrontendAPIService
Issue-Key: workspaces-instances|ListInstanceTypes|2xx->2xx|$.InstanceTypes[*]
Issue-Commit: 42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35

# ListInstanceTypes returns an empty instance-type catalog instead of the supported WorkSpace Instance types

> _Note: this issue was identified against the code base at commit `42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstanceTypes

## Description
Cause 1 of 1: ListInstanceTypes answers HTTP 200 with an InstanceTypes list that contains none of the catalog entries AWS returns, so the WorkSpace Instance type catalog is effectively empty.

AWS: Smithy (com.amazonaws.workspacesinstances#ListInstanceTypes) models output ListInstanceTypesResponse with member InstanceTypes (target InstanceTypes, @required) plus an optional NextToken (@length 1..2048, @sensitive); the operation is @readonly and @paginated (inputToken/outputToken NextToken, pageSize MaxResults, items InstanceTypes) with MaxResults @range 1..600. Each list member is InstanceTypeInfo with InstanceType (@length 1..64, @pattern ^([a-z0-9-]+)\.([a-z0-9]+)$) and SupportedInstanceConfigurations, a list of SupportedInstanceConfiguration { BillingMode, PlatformType, Tenancy } whose docs state it "Lists all valid combinations of tenancy, platform type, and billing mode supported for the specific WorkSpace Instance type". The model does not enumerate the catalog contents, so for the catalog itself this is recorded behavior only: the recording shows an unfiltered ListInstanceTypes call returning a populated catalog of 597 InstanceTypeInfo entries (indices 0..596), each carrying a well-formed EC2-style instance type name (e.g. the entry at index 0 is a c5-family 12xlarge) and the full cross-product of its supported BillingMode (HOURLY, MONTHLY) x PlatformType (Windows, Windows BYOL, Linux/UNIX, Ubuntu Pro Linux, Red Hat Enterprise Linux, Red Hat BYOL Linux, SUSE Linux) x Tenancy (SHARED, DEDICATED) combinations as SupportedInstanceConfigurations. The specific type names and the exact count are the recording's values; the requirement is that the operation serves the service's instance-type catalog, with each entry's supported configuration combinations, rather than nothing.

Emulator: the replayed step returns the expected status (200) but every single expected element of the list is absent — the divergence reports $.InstanceTypes[0] through $.InstanceTypes[596] each as "missing", with no entry matched at any index. The emulator therefore serves a structurally valid but empty InstanceTypes collection; no instance type and no SupportedInstanceConfiguration is produced. Hypothesis: the operation is wired up as a shape-correct stub with no backing instance-type catalog seeded in state.

Why: ListInstanceTypes is the discovery call a client makes before it can create or manage a WorkSpace Instance. A client that enumerates the catalog to choose a type, or that filters it via InstanceConfigurationFilter (BillingMode/PlatformType/Tenancy, all @required in that filter) to find types supporting a wanted billing/platform/tenancy combination, gets zero candidates and concludes the region supports no WorkSpace Instance types at all. SDK paginators terminate immediately with an empty page, console-style and Terraform-style validation of a requested instance type against the catalog fails for every value, and any workflow that picks a type programmatically cannot proceed — an availability error surfaced as an empty success, which is harder to diagnose than a modeled failure.

Fix: the ListInstanceTypes handler in the workspaces-instances service and the instance-type catalog it reads from state. The catalog must be seeded with the service's supported InstanceTypeInfo entries, each with its SupportedInstanceConfigurations cross-product, and the handler must project that catalog into the response (honouring InstanceConfigurationFilter and the MaxResults/NextToken pagination contract) instead of emitting an empty required list.

Traces: fixture workspaces-instances-catalog-list-instance-types, step task#1 (expected 200, actual 200, all 597 InstanceTypes elements missing).

## Evidence
Fixture workspaces-instances-catalog-list-instance-types, step task#1 — verdict: mismatch; expected_status 200, actual_status 200.

Expected (recorded AWS response): InstanceTypes populated with 597 InstanceTypeInfo entries, indices 0..596. Example expected element at $.InstanceTypes[0]: {"InstanceType": "c5.12xlarge", "SupportedInstanceConfigurations": [{"BillingMode": "MONTHLY", "PlatformType": "Red Hat Enterprise Linux", "Tenancy": "DEDICATED"}, {"BillingMode": "HOURLY", "PlatformType": "Windows BYOL", "Tenancy": "SHARED"}, {"BillingMode": "HOURLY", "PlatformType": "Red Hat BYOL Linux", "Tenancy": "SHARED"}, {"BillingMode": "MONTHLY", "PlatformType": "Linux/UNIX", "Tenancy": "DEDICATED"}, ... all HOURLY/MONTHLY x platform x SHARED/DEDICATED combinations ...]}.

Actual (emulator): $.InstanceTypes[0]: missing. The same "missing" verdict is reported for every index from $.InstanceTypes[0] to $.InstanceTypes[596] — i.e. the emulator produced no catalog entries whatsoever, only an empty InstanceTypes list under a 200 response.

Model evidence: Smithy ListInstanceTypes output member InstanceTypes is @required; InstanceTypeInfo.InstanceType is @length 1..64 @pattern ^([a-z0-9-]+)\.([a-z0-9]+)$; SupportedInstanceConfiguration = {BillingMode, PlatformType, Tenancy}; operation is @readonly and @paginated(items: InstanceTypes, pageSize: MaxResults @range 1..600).

Note: this is distinct from the one other open workspaces-instances issue (20261122T202647Z-workspaces-instances-getworkspaceinstance-reports-an-ec2managedinstance-instanceid-that-is-not.md), which concerns GetWorkspaceInstance's EC2ManagedInstance.InstanceId and does not cover the instance-type catalog.

## Steps to Reproduce
1. git_clone() and boot the emulator built from the checkout.
2. Replay the wire corpus: replay_wire_corpus().
3. Inspect the divergence for fixture workspaces-instances-catalog-list-instance-types, step task#1 — a workspaces-instances ListInstanceTypes call with no InstanceConfigurationFilter and no NextToken.
4. Equivalently, call ListInstanceTypes against the emulator's workspaces-instances endpoint with an empty request body.
5. Observe HTTP 200 whose InstanceTypes list is empty, where AWS returns the service's full instance-type catalog (597 InstanceTypeInfo entries in the recording), each with its SupportedInstanceConfigurations billing-mode/platform/tenancy combinations.
6. Repeat with an InstanceConfigurationFilter (e.g. BillingMode HOURLY, PlatformType Linux/UNIX, Tenancy SHARED) and observe that no candidate type is returned either.
