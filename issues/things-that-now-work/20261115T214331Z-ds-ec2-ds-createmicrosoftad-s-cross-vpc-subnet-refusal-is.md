Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds__ec2.create-microsoft-ad-with-a-subnet-of-another-vpc-rejected-client-exception
Issue-Key: ds__ec2.create-microsoft-ad-with-a-subnet-of-another-vpc-rejected-client-exception|run#13|AssertionError|assert msg == want, f"the refusal should explain the boundary exactly: expected {want!r}, got {msg!r}"
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# ds CreateMicrosoftAD's cross-VPC subnet refusal is worded "The subnets must belong to the VPC specified." where AWS words it "Both subnets must belong to the specified VPC."

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
One net-new root cause in the Directory Service placement rules, on the ds/ec2 boundary.

Cause 1 of 1: the ClientException a CreateMicrosoftAD whose VpcSettings names a subnet of ANOTHER VPC is refused with carries an emulator-authored second sentence instead of AWS's.

AWS: Smithy (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json): CreateMicrosoftAD takes the @required VpcSettings (DirectoryVpcSettings: VpcId plus SubnetIds, documented as "the identifiers of the subnets for the directory servers. The two subnets must be in different Availability Zones.") and models ClientException (client, HTTP 400) for a placement whose network cannot carry the directory's servers. When one named subnet sits in a different VPC than VpcId, AWS refuses with ClientException and HTTP 400, message template — ENTIRELY FIXED text, no variable parts: "Invalid subnet ID(s). Both subnets must belong to the specified VPC." followed by the service envelope's " : RequestId: " <request id>. Neither subnet id nor either VPC id appears in it. The opener "Invalid subnet ID(s). " is shared with the sibling placement refusals; the second sentence is specific to the cross-VPC rule and is "Both subnets must belong to the specified VPC."

Emulator: the error class, HTTP 400 and the opener are right, but the second sentence is re-worded: "Invalid subnet ID(s). The subnets must belong to the VPC specified. : RequestId: <rid>" — "The subnets" for "Both subnets", and "the VPC specified" for "the specified VPC". The text is the constant SUBNETS_OUTSIDE_THE_VPC in services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/subnet_zones.rs lines 132-133, returned by foreign_vpc_defect (same file, lines 69-81) via placement_defect, which every create's placement check runs over what EC2 reported for the named subnets.

Why: this sentence is the only signal that distinguishes the cross-VPC placement fault from its two siblings — the one-Availability-Zone pick ("Invalid subnet ID(s). The two subnets must be in different Availability Zones.") and a settings block naming no two distinct subnets. All three share the opener and all three are ClientException/400, so a client that branches on the message (to re-pick subnets inside the right VPC rather than to re-pick zones) or that asserts on AWS's sentence by equality takes the wrong branch or never matches. The positive control in the trace shows the fault detection itself is correct: EC2 places each subnet in its own VPC, the create is refused, and no directory is left listed — only the wording diverges.

Fix: the ds placement diagnostics — the SUBNETS_OUTSIDE_THE_VPC constant in src/state/subnet_zones.rs that foreign_vpc_defect returns, shared by CreateMicrosoftAD, CreateDirectory, ConnectDirectory and AddRegion. The sibling one-zone constant (SUBNETS_IN_ONE_ZONE) is unaffected.

Related open issues that do NOT cover this (neither carries this check id): 20261115T185627Z-ds-createmicrosoftad-accepts-a-vpcsettings-subnet-pick-ec2.md (a pick EC2 resolves nothing for is ACCEPTED rather than refused — a different branch of the same placement check, about admission not wording) and 20261115T135855Z-ds-createmicrosoftad-two-net-new-causes-edition-enum.md.

Traces: ds__ec2.create-microsoft-ad-with-a-subnet-of-another-vpc-rejected-client-exception.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (ds__ec2: 18/21 passed).

Check ds__ec2.create-microsoft-ad-with-a-subnet-of-another-vpc-rejected-client-exception. Setup: two EC2 VPCs A and B, each with one /24 subnet per AZ; CreateMicrosoftAD(Name=<unique>.test, Edition=Standard, VpcSettings={VpcId: A, SubnetIds: [<a subnet of A>, <a subnet of B in a different AZ>]}). Positive control passes: EC2 DescribeSubnets reports each subnet in its own VPC, so the only fault is cross-VPC ownership.

  code:     ClientException (matches AWS), HTTP 400 (matches)
  expected: 'Invalid subnet ID(s). Both subnets must belong to the specified VPC. : RequestId: <rid>'
  actual:   'Invalid subnet ID(s). The subnets must belong to the VPC specified. : RequestId: <rid>'
AssertionError (check_ds__ec2_create_microsoft_ad_with_a_subnet_of_another_vpc_rejected_client_exception.py line 36): the refusal should explain the boundary exactly.

The whole message is fixed text (no request or state values at all) apart from the per-run RequestId the envelope appends, which is not part of the requirement. Diverging fixed text: "Both subnets" vs "The subnets", and "the specified VPC" vs "the VPC specified". The refused create correctly leaves no directory listed by DescribeDirectories.

Code evidence: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/subnet_zones.rs lines 132-133 define SUBNETS_OUTSIDE_THE_VPC = "Invalid subnet ID(s). The subnets must belong to the VPC specified."; foreign_vpc_defect (lines 69-81) returns it whenever EC2 reported any named subnet with a vpc_id other than the requested VpcId, and placement_defect (lines 47-53) is what the creates call.

## Steps to Reproduce
1. Clone this checkout and boot the emulator; replay suite simparity-behavioral-aws-v1.
2. Run check ds__ec2.create-microsoft-ad-with-a-subnet-of-another-vpc-rejected-client-exception, or by hand:
   a. ec2 CreateVpc two VPCs (A: 10.184.0.0/16, B: 10.185.0.0/16) and one subnet per available AZ in each.
   b. ec2 DescribeSubnets on the two chosen subnets to confirm one belongs to A and the other to B, and that their AZs differ (so the one-zone rule is not what fires).
   c. ds CreateMicrosoftAD(Name=<unique>.test, Password=<valid>, Edition=Standard, VpcSettings={VpcId: <A>, SubnetIds: [<subnet of A>, <subnet of B>]}).
3. Observe HTTP 400 ClientException with Error.Message 'Invalid subnet ID(s). The subnets must belong to the VPC specified. : RequestId: <rid>'.
4. AWS answers 'Invalid subnet ID(s). Both subnets must belong to the specified VPC. : RequestId: <rid>'. Compare Error.Message character for character; also confirm the sibling one-zone refusal still reads 'Invalid subnet ID(s). The two subnets must be in different Availability Zones.'
