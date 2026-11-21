Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-ipam-trajwire-provision-cidr-mistakes
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-ipam-trajwire-provision-cidr-mistakes
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|GetIpamPoolCidrs|2xx->2xx|$.GetIpamPoolCidrsResponse.ipamPoolCidrSet.item.failureReason.message
Issue-Commit: f0b15e4697d4cc42deb410e2f16ec2bb91ba8f5b

# ec2 GetIpamPoolCidrs reports an emulator-authored failureReason message for a CIDR that is not available in the parent pool

> _Note: this issue was identified against the code base at commit `f0b15e4697d4cc42deb410e2f16ec2bb91ba8f5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIpamPoolCidrs

## Description
Cause 1 of 1: the failed-provisioning reason text on an IPAM pool CIDR is written by the emulator instead of using AWS's wording.

AWS: Smithy com.amazonaws.ec2#IpamPoolCidrFailureReason carries Code (enum com.amazonaws.ec2#IpamPoolCidrFailureCode, values 'cidr-not-available' and 'limit-exceeded') and Message ('A message related to why an IPAM pool CIDR failed to be provisioned'), surfaced by GetIpamPoolCidrs as ipamPoolCidrSet.item.failureReason. When provisioning fails because the parent pool has no free space of the requested netmask length, the recorded response's message is the fixed sentence 'A CIDR with this netmask length is not available in the parent pool.' -- it has no variable parts: no pool id and no netmask length are interpolated into it (recorded behavior only; the model documents the member but not the text). The HTTP status is 200 and the failure is reported in failureReason, not as an error code.

Emulator: the step returns HTTP 200 with the right shape, but failureReason.message reads "The source IPAM pool '<pool-id>' has no free CIDR of netmask length /<n>" -- emulator-authored prose that interpolates the source pool id and the requested netmask length (in this replay, pool ipam-pool-000000000000000a6 and /7; those values are this run's request/state, not part of any template).

Why: a client that matches on the failure message to distinguish 'no space in the parent pool' from other provisioning failures -- or that shows AWS's sentence to an operator -- never matches, and anything asserting on the documented text fails while the emulator looks healthy because the status and failureReason.code are right.

Fix: the GetIpamPoolCidrs / provision-CIDR failure path -- the state that records a pool CIDR's failureReason must store AWS's fixed sentence for the cidr-not-available case instead of formatting its own message from the pool id and netmask length.

Traces: ec2-ipam-trajwire-provision-cidr-mistakes#task#11. Note: the open issue 20261103T071133Z-ec2-getipampoolcidrs-stub.md covers GetIpamPoolCidrs being an unimplemented stub, and 20261107T184734Z-ec2-ipam-pool-cidr-refusals-provisioning-state-and.md covers IPAM pool CIDR refusals and provisioning state; the replay reports this cluster as owned by neither (owned_by empty, stub_path empty) -- the operation now answers 200 and this message divergence is net-new.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture ec2-ipam-trajwire-provision-cidr-mistakes, step task#11, GetIpamPoolCidrs, status 200 expected and 200 returned:

  $.GetIpamPoolCidrsResponse.ipamPoolCidrSet.item.failureReason.message: value_mismatch
    expected: 'A CIDR with this netmask length is not available in the parent pool.'
    actual:   "The source IPAM pool 'ipam-pool-000000000000000a6' has no free CIDR of netmask length /7"

Fixed text (the requirement): the whole expected sentence 'A CIDR with this netmask length is not available in the parent pool.' -- it contains no request- or state-derived values.
Values from this run only (not part of the template, and not a requirement): the pool id ipam-pool-000000000000000a6 and the netmask length /7 that the emulator interpolates.

Smithy citation: com.amazonaws.ec2#IpamPoolCidrFailureReason { Code: IpamPoolCidrFailureCode enum ['cidr-not-available','limit-exceeded'], Message: String }.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture ec2-ipam-trajwire-provision-cidr-mistakes through step task#11: provision a CIDR into an IPAM pool whose parent pool has no free space at the requested netmask length, then call GetIpamPoolCidrs for that pool.
3. The call returns HTTP 200; read ipamPoolCidrSet.item.failureReason.message and compare it with the recorded response -- the emulator's sentence names the source pool and netmask length instead of AWS's fixed 'A CIDR with this netmask length is not available in the parent pool.'
