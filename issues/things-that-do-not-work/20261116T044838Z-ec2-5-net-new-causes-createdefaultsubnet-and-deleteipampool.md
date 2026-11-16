Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.create-default-subnet-contract ec2.ipam-pool-with-provisioned-cidr-deletion-rejected ec2.ipam-vpc-on-custom-reserved-cidr-rejected ec2.op-contract-describeSnapshots ec2.op-contract-describeSnapshotAttribute ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.create-default-subnet-contract|run#10|AssertionError|assert message == f"'{baseline_id}' is already the default subnet in {az}.", (
Issue-Key: ec2.ipam-pool-with-provisioned-cidr-deletion-rejected|run#17|AssertionError|assert message == (
Issue-Key: ec2.ipam-vpc-on-custom-reserved-cidr-rejected|run#9|AssertionError|assert err == want, f"a VPC on the reserved {held['Cidr']} returned {err!r}, expected {want!r}"
Issue-Key: ec2.op-contract-describeSnapshots|_error#14|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: ec2.op-contract-describeSnapshotAttribute|_error#10|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded and returned {result!r}")
Issue-Key: ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits|_run_guest#23|AssertionError|assert inv["Status"] == "Success", (
Issue-Commit: 2986c932db5644b5e53751894a3faacc2d59c288

# ec2: 5 net-new causes — CreateDefaultSubnet and DeleteIpamPool refusal messages are emulator-authored, CreateVpc from an IPAM pool with an overlapping CIDR answers 501 NotImplemented, snapshot-id lookups fail open onto an unrelated Amazon-owned snapshot, and a burstable guest's shell script exits 127

> _Note: this issue was identified against the code base at commit `2986c932db5644b5e53751894a3faacc2d59c288`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDefaultSubnet, DeleteIpamPool, CreateVpc, DescribeSnapshots, DescribeSnapshotAttribute

## Description
Five net-new root causes in the ec2 emulator, grounded in the six failing behavioural checks listed below. Each cause is independent and must be closed on its own; the bundle contains 5 causes.

=== Cause 1 of 5: CreateDefaultSubnet's DefaultSubnetAlreadyExistsInAvailabilityZone message does not name the subnet that already owns the zone ===
AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2): CreateDefaultSubnet input is {AvailabilityZone: AvailabilityZoneName, AvailabilityZoneId, Ipv6Native, DryRun}; the operation declares NO modeled errors — ec2Query carries an untyped error envelope, so the code and message are recorded behaviour only. Recorded: HTTP 400, Code `DefaultSubnetAlreadyExistsInAvailabilityZone`, Message `'<subnet-id>' is already the default subnet in <az>.` The fixed text is exactly `'` … `' is already the default subnet in ` … `.`; the variable parts are (a) the id of the subnet already marked default-for-az in that zone, taken from the emulator's own subnet state, and (b) the AvailabilityZone taken from the request.
Emulator: returns the right code but an emulator-authored sentence that names only the zone and never the subnet: `A default subnet already exists in availability zone <az>.`
Why: a client that reads the refusal to learn WHICH subnet already owns the zone (the documented way to recover the existing default subnet id from this refusal) gets no id at all, so it cannot distinguish "the zone already has the default subnet I expect" from "the zone has some other default subnet".
Fix: the CreateDefaultSubnet handler's already-exists refusal path — it must interpolate the existing default subnet's id from state into the message.
Traces: ec2.create-default-subnet-contract

=== Cause 2 of 5: DeleteIpamPool's DependencyViolation blames allocations when the blocking dependency is a provisioned pool CIDR ===
AWS: Smithy: DeleteIpamPool input is {IpamPoolId (required), Cascade, DryRun}; no modeled errors (untyped ec2Query envelope), so code/message are recorded behaviour only. Recorded for a pool that still holds a provisioned CIDR and an allocation carved out of it, deleted without Cascade: HTTP 400, Code `DependencyViolation`, Message `The specified action is not valid for <ipam-pool-id>. There are one or more pool cidrs provisioned in the pool.` Fixed text: `The specified action is not valid for ` … `. There are one or more pool cidrs provisioned in the pool.`; the only variable part is the IpamPoolId echoed from the request. The pool must survive the refusal in state create-complete with its provisioned CIDR intact.
Emulator: right code, wrong second sentence — `The specified action is not valid for <ipam-pool-id>. There are one or more allocations in the pool.` It reports the allocation as the blocker instead of the provisioned pool CIDR.
Why: the two dependency classes have different remedies (deprovision the pool CIDR vs. release the allocation). A client that branches on this sentence to decide which cleanup call to make takes the wrong branch and the delete keeps failing.
Fix: the DeleteIpamPool handler's dependency-check/message selection (which dependency class it reports for a non-Cascade delete).
Traces: ec2.ipam-pool-with-provisioned-cidr-deletion-rejected

=== Cause 3 of 5: CreateVpc with an IPAM-allocated CIDR answers 501 NotImplemented instead of refusing an overlapping CIDR with InvalidParameterValue ===
AWS: Smithy: CreateVpc input declares Ipv4IpamPoolId (IpamPoolId) and Ipv4NetmaskLength (NetmaskLength) alongside CidrBlock, so the IPAM-allocated path is part of the modeled surface; the operation declares no modeled errors (untyped ec2Query envelope). Recorded for a CreateVpc whose requested CIDR is already held by a custom AllocateIpamPoolCidr reservation in the named pool: HTTP 400, Code `InvalidParameterValue`, Message `The provided cidr either overlaps with an existing allocation or is not a subnet of any pool cidrs in the pool.` — this message is entirely fixed text with no variable parts.
Emulator: answers Code `NotImplemented` with the emulator's own scaffold prose `operation CreateVpc with an IPAM-allocated CIDR block is scaffolded but not implemented in this emulator`.
Why: a client cannot create a VPC from an IPAM pool at all, and worse, cannot tell a genuine overlap/outside-the-pool rejection (which it should retry with a different CIDR) from an unimplemented surface. Related open issue 20261107T075601Z-ec2-createvpc-from-an-ipam-pool-is-a.md describes CreateVpc-from-IPAM as a stub but does not carry this check's replay key, so this cause is reported here and grounded in the check below.
Fix: the CreateVpc handler's IPAM-allocation branch and its CIDR-overlap/subset-of-pool validation.
Traces: ec2.ipam-vpc-on-custom-reserved-cidr-rejected

=== Cause 4 of 5: snapshot-id resolution fails open — a never-issued snapshot id resolves to an unrelated Amazon-owned snapshot and the read answers 200 ===
AWS: Smithy: DescribeSnapshots (input SnapshotIds: SnapshotIdStringList, paginated on Snapshots/NextToken) and DescribeSnapshotAttribute (input SnapshotId + Attribute, both required) declare no modeled errors (untyped ec2Query envelope), so the refusal is recorded behaviour only. Recorded for a well-formed snapshot id of this account/region's own id length that was never issued: HTTP 400 with EITHER `InvalidSnapshot.NotFound`, Message `The snapshot '<snapshot-id>' does not exist.` OR `InvalidSnapshotID.Malformed`, whose message is per-operation — DescribeSnapshots: `Invalid id: "<snapshot-id>"`; DescribeSnapshotAttribute: `Value ( <snapshot-id> ) for parameter SNAPSHOT is invalid. ` (note the single trailing space). In every form the only variable part is the snapshot id echoed from the request; the rest is fixed text. A successful read must also echo back exactly the requested SnapshotId.
Emulator: both operations answer HTTP 200 for an id that was never issued, and the body describes a DIFFERENT, pre-seeded Amazon-owned AMI snapshot: DescribeSnapshots returns one Snapshot with SnapshotId set to that other snapshot, VolumeId `vol-ffffffff`, OwnerAlias `amazon`, a public Amazon Linux AMI Description and State `completed`; DescribeSnapshotAttribute returns that same foreign SnapshotId with CreateVolumePermissions `[{"Group": "all"}]`. The requested id is neither matched nor echoed.
Why: absence of a snapshot can never be observed. An existence probe always says "yes", a client restores or shares the wrong snapshot, and DescribeSnapshotAttribute reports a public (`Group: all`) create-volume permission for a snapshot the caller never created — a permission check that is wrong in the unsafe direction.
Fix: snapshot-id resolution in the DescribeSnapshots and DescribeSnapshotAttribute handlers and the shared snapshot state lookup they use: an id that was never issued must not match any row, the refusal must be raised with one of the codes/messages above, and a successful response must echo the requested SnapshotId.
Traces: ec2.op-contract-describeSnapshots, ec2.op-contract-describeSnapshotAttribute

=== Cause 5 of 5: a burstable guest cannot run the shell script SSM RunShellScript sends it — the invocation lands Failed with rc=127 and no output ===
AWS: recorded behaviour only (no model statement covers guest execution). Recorded: on a running t3.micro, an SSM RunShellScript invocation that drives CPU load through the credit balance into surplus completes with GetCommandInvocation Status `Success` and the script's stdout captured, so the burstable CPU-credit accounting (baseline, surplus, Unlimited sustaining throughput) is observable from inside the guest.
Emulator: the same invocation ends with Status `Failed`, ResponseCode 127, stdout `''` and stderr `''`. rc=127 is "command not found", i.e. the guest image does not carry the program the script invokes; and stderr is empty, so even that diagnostic is lost (a real shell writes a "command not found" line to stderr before exiting 127). Hypothesis: the emulator's guest executes commands against an image/toolchain that lacks the load-generating utility, and the invocation's stderr is not captured at all.
Why: no client can observe burstable CPU-credit behaviour — baseline vs. surplus throughput, the Unlimited credit mode, or any workload the guest is supposed to run — and the empty stderr means the failure cannot even be diagnosed from the invocation record.
Fix: the guest command-execution path (the in-guest image/toolchain the RunShellScript document runs against) and its stdout/stderr capture into the SSM invocation record. Neighbouring open issue 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md covers other guests observing nothing but does not carry this check's replay key.
Traces: ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits

## Evidence
Cause 1 — ec2.create-default-subnet-contract [mismatch]:
  expected message: f"'{baseline_id}' is already the default subnet in {az}."
  AssertionError: unexpected DefaultSubnetAlreadyExistsInAvailabilityZone message 'A default subnet already exists in availability zone us-west-2a.'
  (code DefaultSubnetAlreadyExistsInAvailabilityZone was correct; only the message diverges)

Cause 2 — ec2.ipam-pool-with-provisioned-cidr-deletion-rejected [mismatch]:
  expected: f"The specified action is not valid for {pool_id}. There are one or more pool cidrs provisioned in the pool."
  AssertionError: unexpected DependencyViolation message 'The specified action is not valid for ipam-pool-00000000000000092. There are one or more allocations in the pool.'
  (code DependencyViolation was correct; the second sentence diverges)

Cause 3 — ec2.ipam-vpc-on-custom-reserved-cidr-rejected [mismatch]:
  AssertionError: a VPC on the reserved 10.0.0.0/24 returned ('NotImplemented', 'operation CreateVpc with an IPAM-allocated CIDR block is scaffolded but not implemented in this emulator'), expected ('InvalidParameterValue', 'The provided cidr either overlaps with an existing allocation or is not a subnet of any pool cidrs in the pool.')

Cause 4 — ec2.op-contract-describeSnapshots [mismatch]:
  expected one of {InvalidSnapshotID.Malformed: 'Invalid id: "<fake-id>"', InvalidSnapshot.NotFound: "The snapshot '<fake-id>' does not exist."}
  AssertionError: expected a ClientError, but the call succeeded and returned {'Snapshots': [{'OwnerAlias': 'amazon', 'StorageTier': 'standard', 'SnapshotId': 'snap-00000000000000080', 'VolumeId': 'vol-ffffffff', 'State': 'completed', 'Progress': '100%', 'OwnerId': '137112412989', 'Description': 'Amazon Linux 2 ECS-Optimized Inferentia AMI ... ebs', 'VolumeSize': 8, 'Encrypted': False}], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
  -> the requested (never-issued) id was neither matched nor echoed; an unrelated Amazon-owned AMI snapshot was returned with HTTP 200.

Cause 4 — ec2.op-contract-describeSnapshotAttribute [mismatch]:
  expected one of {InvalidSnapshotID.Malformed: 'Value ( <fake-id> ) for parameter SNAPSHOT is invalid. ', InvalidSnapshot.NotFound: "The snapshot '<fake-id>' does not exist."}
  AssertionError: expected a ClientError, but the call succeeded and returned {'SnapshotId': 'snap-00000000000000080', 'CreateVolumePermissions': [{'Group': 'all'}], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
  -> HTTP 200, a foreign SnapshotId, and a public create-volume permission for a snapshot the caller never created.

Cause 5 — ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits [mismatch]:
  AssertionError: burning CPU past the credit balance and into surplus: the guest script was 'Failed' ('Failed'); rc=127 stdout='' stderr=''
  (expected GetCommandInvocation Status 'Success' with the burn script's stdout; rc=127 with empty stderr means the program was not found in the guest and no diagnostic was captured)

## Steps to Reproduce
Cause 1 (CreateDefaultSubnet message):
1. DescribeAvailabilityZones(Filters=[state=available]) and take the first zone; DescribeSubnets(Filters=[availability-zone=<az>, default-for-az=true]) and note the one subnet id.
2. Call CreateDefaultSubnet(AvailabilityZone=<az>).
3. Expect HTTP 400 DefaultSubnetAlreadyExistsInAvailabilityZone with message "'<that-subnet-id>' is already the default subnet in <az>."; observe a message that names only the zone.

Cause 2 (DeleteIpamPool dependency message):
1. CreateIpam(OperatingRegions=[<region>]); wait for create-complete and read PrivateDefaultScopeId.
2. CreateIpamPool(IpamScopeId=<private scope>, AddressFamily=ipv4, Locale=<region>); wait create-complete.
3. ProvisionIpamPoolCidr(IpamPoolId=<pool>, Cidr=10.99.0.0/16); wait until GetIpamPoolCidrs reports it provisioned. AllocateIpamPoolCidr(IpamPoolId=<pool>, NetmaskLength=28).
4. DeleteIpamPool(IpamPoolId=<pool>) with no Cascade.
5. Expect DependencyViolation "The specified action is not valid for <pool>. There are one or more pool cidrs provisioned in the pool."; observe "... There are one or more allocations in the pool."

Cause 3 (CreateVpc from an IPAM pool):
1. Stand up an IPAM with a regional child pool and ProvisionIpamPoolCidr(NetmaskLength=16) on it; wait provisioned.
2. AllocateIpamPoolCidr(IpamPoolId=<child>, NetmaskLength=24) to hold a /24 (e.g. 10.0.0.0/24).
3. CreateVpc(Ipv4IpamPoolId=<child>, CidrBlock=<that held /24>).
4. Expect HTTP 400 InvalidParameterValue "The provided cidr either overlaps with an existing allocation or is not a subnet of any pool cidrs in the pool."; observe NotImplemented with the emulator's scaffold prose.

Cause 4 (snapshot id fails open):
1. CreateVolume(AvailabilityZone=<az>, Size=1, VolumeType=gp3) and wait available; CreateSnapshot on it and wait until DescribeSnapshots reports State=completed. Note the real snapshot id.
2. Build a never-issued id of the same length by flipping the last hex digit of the real id.
3. DescribeSnapshots(SnapshotIds=[<flipped id>]) and DescribeSnapshotAttribute(SnapshotId=<flipped id>, Attribute=createVolumePermission).
4. Expect HTTP 400 InvalidSnapshot.NotFound ("The snapshot '<flipped id>' does not exist.") or InvalidSnapshotID.Malformed with that operation's exact message; observe HTTP 200 describing a different, Amazon-owned snapshot.

Cause 5 (burstable guest execution):
1. RunInstances a t3.micro with an instance profile carrying AmazonSSMManagedInstanceCore; wait until it is an SSM managed node.
2. SendCommand(DocumentName=AWS-RunShellScript) with a script that drives sustained CPU load through the credit balance into surplus, then GetCommandInvocation.
3. Expect Status=Success with the script's stdout; observe Status=Failed, ResponseCode=127, stdout='' and stderr=''.
