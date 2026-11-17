Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.asgssh-nacl-entries-round-trip-deny-22-before-allow-all ec2.create-fpga-image-invalid-bucket-rejected ec2.op-contract-create-secondary-network ec2.op-contract-detach-image-watermark ec2.ipam-deprovision-cidr-under-live-vpc-accepted-then-rejected-as-failed-deprovision ec2.op-contract-get-transit-gateway-attachment-propagations
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.asgssh-nacl-entries-round-trip-deny-22-before-allow-all|run#18|AssertionError|assert ranged == [(50, False)], (
Issue-Key: ec2|CreateFpgaImage|InaccessibleStorageLocation
Issue-Key: ec2.op-contract-create-secondary-network|run#12|AssertionError|assert message == (
Issue-Key: ec2.op-contract-detach-image-watermark|run#16|AssertionError|assert resp["Return"] is False, resp
Issue-Key: ec2.ipam-deprovision-cidr-under-live-vpc-accepted-then-rejected-as-failed-deprovision|_poll#10|Timeout|time.sleep(interval_s)
Issue-Key: ec2.op-contract-get-transit-gateway-attachment-propagations|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# ec2: 6 net-new causes — implicit all-traffic NACL entries carry a PortRange, CreateFpgaImage refuses a valid storage location, CreateSecondaryNetwork's Range message is reworded, DetachImageWatermark returns Return=true for a watermark it never held, and the IPAM deprovision / TGW attachment-propagation happy paths never converge

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFpgaImage

## Description
Six independent EC2 defects, each grounded in one failing behavioral check. They share the service token `ec2` and are bundled as one work item; all six must be closed before submitting.

Cause 1 of 6: a network ACL's implicit all-traffic deny entries carry a PortRange.
AWS: Smithy — CreateNetworkAclEntry.PortRange targets the optional `PortRange` shape (xmlName `portRange`), and NetworkAclEntry carries PortRange only for a protocol that has ports (tcp=6, udp=17). A rule whose Protocol is "-1" (all traffic) has no port concept, so AWS omits the PortRange member entirely from that entry in CreateNetworkAcl/DescribeNetworkAcls. The two implicit rules a fresh ACL is born with (RuleNumber 32767, Protocol "-1", RuleAction deny, CidrBlock 0.0.0.0/0, one ingress and one egress) are exactly such all-traffic rules.
Emulator: after three entries are added, the entries that expose a PortRange are rule 50 ingress (correct, tcp 22-22) plus BOTH implicit 32767 entries — `got ranges on [(50, False), (32767, False), (32767, True)]`. The emulator attaches a PortRange to the protocol "-1" implicit entries.
Why: a client that keys off the presence of PortRange to decide whether a rule is port-scoped (every NACL rule renderer, every drift/diff tool, the CLI's own output) reads the implicit catch-all deny as a port-restricted rule, so rule evaluation and diffs against a real-AWS-recorded ACL both come out wrong.
Fix: the network-ACL state/serializer for NetworkAclEntry — the PortRange member must be left unset for an entry whose Protocol is not a ported protocol, including the two implicit deny-all entries synthesized by CreateNetworkAcl.
Traces: ec2.asgssh-nacl-entries-round-trip-deny-22-before-allow-all.

Cause 2 of 6: CreateFpgaImage refuses an InputStorageLocation that names an existing, same-region bucket and an existing key.
AWS: Smithy — CreateFpgaImage takes a required InputStorageLocation (StorageLocation: Bucket + Key) and answers CreateFpgaImageResult{FpgaImageId, FpgaImageGlobalId}. The refusal the emulator emits is the real AWS one for an INACCESSIBLE location: error code `InaccessibleStorageLocation`, message "Error accessing resource in S3. Check permissions, naming, and ensure the bucket is within the same region as the API endpoint. Bucket: <bucket>, Key: <key>." — fixed text with the Bucket and Key values taken from the request. AWS raises it only when the location cannot be read; a bucket created in the endpoint's own region whose key was just PUT is accessible, and the call succeeds.
Emulator: the check's two negative boundaries pass (a malformed bucket name is InaccessibleStorageLocation with the exact message, a location missing Key is InvalidParameterValue with "InputStorageLocation is mandatory. Its bucket and key cannot be null or of length zero."). The positive control then fails: after CreateBucket in the endpoint region and PutObject of key `dcp/example.tar`, CreateFpgaImage answers the same `InaccessibleStorageLocation` refusal naming that real bucket and key. The accessibility check never consults S3 state, so EVERY well-formed location is refused.
Why: CreateFpgaImage can never succeed, so no FPGA image can be created or read back through DescribeFpgaImages; a client cannot distinguish "my bucket is wrong" from "this API always refuses".
Fix: CreateFpgaImage's validation — the storage-location accessibility test must resolve the named bucket against S3 state (existence, region match, key presence) and only refuse when that lookup fails; a resolvable Bucket+Key must proceed to the create path.
Traces: ec2.create-fpga-image-invalid-bucket-rejected.

Cause 3 of 6: CreateSecondaryNetwork's InvalidSecondaryNetwork.Range refusal message is emulator-authored prose.
AWS: Smithy — CreateSecondaryNetwork requires Ipv4CidrBlock and NetworkType; the CIDR size is bounded to /12-/28. AWS refuses an out-of-range prefix with code `InvalidSecondaryNetwork.Range` and the message "The CIDR block '<cidr>' is invalid. Secondary network CIDR blocks must be between /12 and /28." The fixed text is everything but `<cidr>`, which is the Ipv4CidrBlock value copied from the request.
Emulator: the code is right but the message is "The CIDR block size must be between /12 and /28" — it drops the quoted offending CIDR, drops the "is invalid"/"Secondary network CIDR blocks" wording, and drops the trailing period.
Why: the message is the only part of the refusal that names WHICH CIDR was rejected; a client (or a human reading a CloudFormation ResourceStatusReason that relays it) cannot identify the offending value, and message-exact assertions against real AWS fail.
Fix: CreateSecondaryNetwork's validation/refusal construction — emit the AWS wording with the request's Ipv4CidrBlock interpolated.
Traces: ec2.op-contract-create-secondary-network.

Cause 4 of 6: DetachImageWatermark reports Return=true for an AMI that carries no watermark.
AWS: Smithy — DetachImageWatermark takes a required ImageId and WatermarkKey and answers DetachImageWatermarkResult{Return: Boolean} (xmlName `return`). `Return` reports whether a watermark was actually detached; for an owned, available AMI that holds no watermark at all, nothing is detached and AWS answers HTTP 200 with Return=false, and DescribeImages continues to report no ImageWatermarks.
Emulator: HTTP 200 with `{'Return': True}` for an AMI registered in the same check that never had a watermark attached.
Why: Return is the operation's only result; a client that treats true as "a watermark was removed" records a state change that never happened, and idempotency/reconciliation logic built on it diverges permanently.
Fix: DetachImageWatermark's handler/state — look the WatermarkKey up on the image's watermark set and return false when no matching watermark is held, true only when one was removed.
Traces: ec2.op-contract-detach-image-watermark.

Cause 5 of 6: a DeprovisionIpamPoolCidr never settles, so the pool CIDR stays pending-deprovision forever.
AWS: Smithy — DeprovisionIpamPoolCidr answers DeprovisionIpamPoolCidrResult{IpamPoolCidr}; the record is returned in State `pending-deprovision` and then settles asynchronously. Recorded behavior: for a CIDR a live VPC still draws from, it settles at `failed-deprovision` carrying a FailureReason whose Message is populated, while the VPC stays `available` and GetIpamPoolAllocations still lists the VPC allocation; for a child pool with no allocations the same call settles at `deprovisioned`.
Emulator: the check is cut off at the harness's 60 s budget with no verdict while polling the CIDR record for a State that does not start with "pending-" (its own ceiling is 240 s). Hypothesis: the emulator answers the deprovision with pending-deprovision but never advances that record, so neither the failed-deprovision nor the deprovisioned terminal state is ever observable.
Why: every client of IPAM deprovisioning polls for a terminal State; a record stuck in pending-deprovision makes the operation indistinguishable from a hang, and the FailureReason that tells an operator WHY a busy CIDR cannot be released is never produced.
Fix: the IPAM pool-CIDR state machine behind DeprovisionIpamPoolCidr — advance a pending-deprovision record to `deprovisioned` when the pool has no allocations against it, and to `failed-deprovision` with a populated FailureReason when an allocation (a live VPC) still holds space, leaving the allocation and the VPC untouched.
Traces: ec2.ipam-deprovision-cidr-under-live-vpc-accepted-then-rejected-as-failed-deprovision.

Cause 6 of 6: the GetTransitGatewayAttachmentPropagations happy path never converges.
AWS: Smithy — GetTransitGatewayAttachmentPropagations takes a required TransitGatewayAttachmentId and answers a paginated TransitGatewayAttachmentPropagations list. Recorded behavior: a transit gateway created with default options reports a PropagationDefaultRouteTableId, and a VPC attachment to it (DefaultRouteTablePropagation enabled by default) auto-propagates, so the list contains an entry whose TransitGatewayRouteTableId is that default propagation route table with State `enabled`.
Emulator: the check is cut off at the harness's 60 s budget with no verdict (its own poll ceiling is 180 s) while waiting on the sequence create_transit_gateway → TGW state `available` → create_transit_gateway_vpc_attachment → attachment state `available` → an `enabled` propagation entry for the default propagation route table. Hypothesis: the attachment is never auto-propagated into the TGW's default propagation route table, so the polled list never contains that entry; the TGW/attachment state waits are the other candidates on the same path.
Why: the attachment-propagation view is how a client confirms that a VPC attachment is actually reachable through the transit gateway; with it permanently empty, no TGW routing topology can be verified, and the operation's modeled NotFound boundary (code `InvalidTransitGatewayAttachmentID.NotFound`, message "Transit Gateway Attachment <id> was deleted or does not exist.") is never reached either.
Fix: the transit-gateway attachment/propagation state — on a VPC attachment becoming available under a TGW whose DefaultRouteTablePropagation is enable, record an `enabled` propagation against the TGW's PropagationDefaultRouteTableId and serve it from GetTransitGatewayAttachmentPropagations; ensure the TGW and attachment states converge to `available`.
Traces: ec2.op-contract-get-transit-gateway-attachment-propagations.

None of these six is covered by an open issue: the replay report marks neither an owning issue nor an unimplemented-stub block for any of the five checks above (the related open items 20261031T144948Z-ec2-transit-gateway-4-net-new-behavioral-root.md and 20261104T142958Z-ec2-state-never-converges-for-six-waited-on.md carry different replay keys and do not name these operations).

## Evidence
Cause 1 — ec2.asgssh-nacl-entries-round-trip-deny-22-before-allow-all [mismatch]:
  AssertionError: only the tcp rule carries a PortRange; an all-traffic rule must OMIT it, got ranges on [(50, False), (32767, False), (32767, True)]
  Expected (AWS): PortRange present only on the tcp/22 deny (rule 50, ingress). Actual: also present on both implicit Protocol "-1" deny entries (32767 ingress, 32767 egress).

Cause 2 — ec2.create-fpga-image-invalid-bucket-rejected [error]:
  botocore.exceptions.ClientError: An error occurred (InaccessibleStorageLocation) when calling the CreateFpgaImage operation: Error accessing resource in S3. Check permissions, naming, and ensure the bucket is within the same region as the API endpoint. Bucket: <bucket created by the check in the endpoint region>, Key: dcp/example.tar.
  Raised on the POSITIVE control, after s3.create_bucket(<same region>) and s3.put_object(Key="dcp/example.tar"). Expected (AWS): HTTP 200 with CreateFpgaImageResult{FpgaImageId, FpgaImageGlobalId}, the image then visible through DescribeFpgaImages. The two negative boundaries in the same check matched AWS exactly (malformed bucket → InaccessibleStorageLocation with the quoted message; location missing Key → InvalidParameterValue "InputStorageLocation is mandatory. Its bucket and key cannot be null or of length zero.").

Cause 3 — ec2.op-contract-create-secondary-network [mismatch]:
  AssertionError: unexpected Range message: 'The CIDR block size must be between /12 and /28'
  Expected (AWS), code InvalidSecondaryNetwork.Range: "The CIDR block '<Ipv4CidrBlock from the request>' is invalid. Secondary network CIDR blocks must be between /12 and /28." Fixed text = everything outside the quotes; the quoted CIDR is the request value (the recording used a /30). Code matched; message did not.

Cause 4 — ec2.op-contract-detach-image-watermark [mismatch]:
  AssertionError: {'Return': True, ... 'HTTPStatusCode': 200 ...}
  Expected (AWS): HTTP 200 with Return=false for an owned, available AMI holding no watermark, and DescribeImages reporting no ImageWatermarks.

Cause 5 — ec2.ipam-deprovision-cidr-under-live-vpc-accepted-then-rejected-as-failed-deprovision [mismatch]:
  Error: Check timed out after 60.0s
  The check's own ceilings are 240 s; it polls _cidr_record() until State does not start with "pending-". Expected (AWS): busy CIDR → State 'failed-deprovision' with a non-empty FailureReason.Message, VPC still 'available', the VPC allocation still listed; idle child CIDR → State 'deprovisioned'. Actual: no terminal state observed at all.

Cause 6 — ec2.op-contract-get-transit-gateway-attachment-propagations [mismatch]:
  Error: Check timed out after 60.0s
  The check's own poll ceiling is 180 s; it polls GetTransitGatewayAttachmentPropagations until some entry has TransitGatewayRouteTableId == the TGW's Options.PropagationDefaultRouteTableId and State == 'enabled'. Expected (AWS): that entry appears once the VPC attachment is available. Actual: predicate never satisfied inside the budget; the NotFound boundary (InvalidTransitGatewayAttachmentID.NotFound, "Transit Gateway Attachment <id> was deleted or does not exist.") was never reached.

## Steps to Reproduce
Replay the behavioral suite against the emulator and re-run exactly these checks:
1. ec2.asgssh-nacl-entries-round-trip-deny-22-before-allow-all — CreateVpc, CreateNetworkAcl, add a tcp/22 deny at rule 50 plus two all-traffic allows at rule 100 (ingress+egress), then DescribeNetworkAcls and inspect which entries expose PortRange.
2. ec2.create-fpga-image-invalid-bucket-rejected — CreateBucket in the endpoint's own region, PutObject key 'dcp/example.tar', then CreateFpgaImage with InputStorageLocation={Bucket, Key} pointing at them; expect 200 + FpgaImageId.
3. ec2.op-contract-create-secondary-network — CreateSecondaryNetwork(Ipv4CidrBlock='<a /30 inside 100.64.0.0/10>', NetworkType='rdma') and compare the InvalidSecondaryNetwork.Range message text.
4. ec2.op-contract-detach-image-watermark — CreateVolume → CreateSnapshot → RegisterImage, then DetachImageWatermark(ImageId, WatermarkKey='<account>:never-attached') and read Return.
5. ec2.ipam-deprovision-cidr-under-live-vpc-accepted-then-rejected-as-failed-deprovision — build an IPAM + private scope + top pool + child pool, provision a /16, create a VPC from the pool, then DeprovisionIpamPoolCidr on the busy CIDR and poll GetIpamPoolCidrs for a non-pending State; repeat on a child pool with no allocations.
6. ec2.op-contract-get-transit-gateway-attachment-propagations — CreateTransitGateway, wait 'available', CreateTransitGatewayVpcAttachment on a subnet, wait 'available', then GetTransitGatewayAttachmentPropagations and look for the TGW's PropagationDefaultRouteTableId with State 'enabled'.
