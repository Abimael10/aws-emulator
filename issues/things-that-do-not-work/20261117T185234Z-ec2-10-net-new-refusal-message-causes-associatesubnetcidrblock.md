Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.associate-subnet-cidr-block-non-4-step-prefix-rejected ec2.create-key-pair-name-over-255-ascii-chars-rejected ec2.create-network-acl-entry-cidr-and-ipv6-cidr-mutually-exclusive-rejected ec2.create-subnet-cidr-reservation-outside-subnet-range-rejected ec2.create-tags-key-and-value-length-boundaries-rejected ec2.create-tags-key-and-value-length-boundary ec2.create-tags-value-256-unicode-boundary-and-aws-prefixed-key-rejected ec2.create-tags-overwrites-a-key-and-describe-tags-pages-by-max-results ec2.create-vpc-amazon-ipv6-with-explicit-ipv6-cidr-rejected ec2.describe-instance-types-max-results-below-the-floor-rejected ec2.describe-instance-types-maxresults-above-the-maximum-rejected ec2.keypair-create-tags-past-fifty-tags-rejected ec2.network-acl-entry-malformed-ipv6-cidr-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.associate-subnet-cidr-block-non-4-step-prefix-rejected|run#13|AssertionError|assert message == (
Issue-Key: ec2.create-key-pair-name-over-255-ascii-chars-rejected|run#12|AssertionError|assert (code, message) == _OVER_LIMIT, f"expected {_OVER_LIMIT}, got ({code!r}, {message!r})"
Issue-Key: ec2.create-network-acl-entry-cidr-and-ipv6-cidr-mutually-exclusive-rejected|run#9|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: ec2.create-subnet-cidr-reservation-outside-subnet-range-rejected|run#9|AssertionError|assert message == "Reservation CIDR should within subnet CIDR block 10.211.1.0/24", message
Issue-Key: ec2|CreateTags|InvalidParameterValue
Issue-Key: ec2.create-tags-key-and-value-length-boundary|run#9|AssertionError|assert (code, message) == (
Issue-Key: ec2.create-tags-value-256-unicode-boundary-and-aws-prefixed-key-rejected|run#15|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: ec2.create-tags-overwrites-a-key-and-describe-tags-pages-by-max-results|run#27|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: ec2.create-vpc-amazon-ipv6-with-explicit-ipv6-cidr-rejected|run#5|AssertionError|assert message == (
Issue-Key: ec2|DescribeInstanceTypes|InvalidMaxResults
Issue-Key: ec2.describe-instance-types-maxresults-above-the-maximum-rejected|expect_error#5|AssertionError|assert actual_message == message, f"unexpected message for {code}: {actual_message!r}"
Issue-Key: ec2.keypair-create-tags-past-fifty-tags-rejected|run#13|AssertionError|assert message == expected_message, f"unexpected message {message!r}"
Issue-Key: ec2.network-acl-entry-malformed-ipv6-cidr-rejected|run#9|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# ec2: 10 net-new refusal-message causes — AssociateSubnetCidrBlock, CreateKeyPair, CreateNetworkAclEntry, CreateSubnetCidrReservation, CreateTags (wrong 127-char key limit), DescribeTags/DescribeInstanceTypes MaxResults, CreateVpc and the key-pair tag limit all serve emulator-authored sentences

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTags

## Description
Ten independent causes where the emulator refuses with the RIGHT error code but an emulator-authored message, and in one case with the wrong limit altogether. An error message is part of the contract: operators read it, and tooling matches on its fixed text. For every cause below the fixed text is AWS's and the variable parts (ids, values, limits, member names) come from the request or the emulator's own state — never from one recording.

Cause 1 of 10: AssociateSubnetCidrBlock with an IPv6 prefix length that is not one of the /4-step lengths.
AWS: code InvalidSubnet.Range, message "The CIDR '<ipv6-cidr>' must be one of /44, /48, /52, /56, /60, /64" — fixed text after the quoted CIDR, <ipv6-cidr> echoed from the request.
Emulator: "The CIDR '<ipv6-cidr>' is invalid." — the enumerated legal lengths are dropped.
Why: the caller cannot tell WHICH prefix lengths are acceptable, so the error is unactionable.
Fix: AssociateSubnetCidrBlock validation message.
Traces: ec2.associate-subnet-cidr-block-non-4-step-prefix-rejected.

Cause 2 of 10: CreateKeyPair with a KeyName over 255 characters.
AWS: InvalidParameterValue, message "Value for parameter KeyName is invalid. Length exceeds maximum of 255." (all fixed text; the member name KeyName is AWS's capitalisation).
Emulator: "Value (<keyname>) for parameter keyName is invalid. A key pair name accepts at most 255 characters" — different sentence, lower-cased member name, and it echoes the whole oversized name.
Fix: CreateKeyPair validation message.
Traces: ec2.create-key-pair-name-over-255-ascii-chars-rejected.

Cause 3 of 10: CreateNetworkAclEntry with both CidrBlock and Ipv6CidrBlock.
AWS: "The parameter cidrBlock cannot be used with the parameter ipv6CidrBlock" (fixed text, member names as AWS words them).
Emulator: "Only one of cidrBlock and ipv6CidrBlock may be specified: a network ACL entry matches a range of one address family".
Fix: CreateNetworkAclEntry validation message.
Traces: ec2.create-network-acl-entry-cidr-and-ipv6-cidr-mutually-exclusive-rejected.

Cause 4 of 10: CreateSubnetCidrReservation with a CIDR outside the subnet.
AWS: "Reservation CIDR should within subnet CIDR block <subnet-cidr>" — fixed text with the SUBNET's CIDR appended (AWS's wording, including its grammar).
Emulator: "Value (<cidr>) for parameter cidr is invalid. The CIDR must be within the CIDR range of subnet '<subnet-id>'." — names the subnet id instead of its CIDR.
Fix: CreateSubnetCidrReservation validation message.
Traces: ec2.create-subnet-cidr-reservation-outside-subnet-range-rejected.

Cause 5 of 10: CreateTags key/value length — WRONG LIMIT plus wrong messages.
AWS: a tag key of 128 characters is ACCEPTED and a value of 256 characters is ACCEPTED; 129 is refused with InvalidParameterValue "Value ( <key> ) for parameter key is invalid. Tag key exceeds the maximum length of 128 characters" and 257 with "Value ( <value> ) for parameter value is invalid. Tag value exceeds the maximum length of 256 characters" (fixed text; <key>/<value> echoed from the request, surrounded by single spaces inside the parentheses).
Emulator: refuses the 128-character key with InvalidParameterValue "A tag key accepts at most 127 characters" — an off-by-one limit that rejects a key AWS stores — and words the over-length value refusal "The value of tag key '<key>' accepts at most 256 characters".
Why: tagging that works on AWS fails here; and the messages carry neither AWS's sentence nor the offending value.
Fix: the CreateTags validation (key limit 128 / value limit 256) and its message construction.
Traces: ec2.create-tags-key-and-value-length-boundaries-rejected, ec2.create-tags-key-and-value-length-boundary, ec2.create-tags-value-256-unicode-boundary-and-aws-prefixed-key-rejected.

Cause 6 of 10: DescribeTags with MaxResults below the floor.
AWS: "Value ( <n> ) for parameter maxResults is invalid. Expecting a value greater than 5." — fixed text, <n> from the request.
Emulator: "Maximum results allowed are between 5 and 1000".
Fix: DescribeTags MaxResults validation message.
Traces: ec2.create-tags-overwrites-a-key-and-describe-tags-pages-by-max-results.

Cause 7 of 10: CreateVpc with both Ipv6CidrBlock and AmazonProvidedIpv6CidrBlock.
AWS: refuses with its own sentence for the mutually exclusive pair.
Emulator: "You cannot specify Ipv6CidrBlock with AmazonProvidedIpv6CidrBlock" — emulator-authored prose.
Fix: CreateVpc validation message.
Traces: ec2.create-vpc-amazon-ipv6-with-explicit-ipv6-cidr-rejected.

Cause 8 of 10: DescribeInstanceTypes MaxResults out of range (both ends).
AWS: code InvalidMaxResults, message "Value ( <n> ) for parameter maxResults is invalid. Expecting a value from ( 5 ) to  ( 100 )." — one sentence for BOTH the below-floor and above-ceiling cases, with the model's own bounds.
Emulator: two different emulator-authored sentences, "Expecting a value greater than or equal to 5." and "Expecting a value smaller than or equal to 100.".
Fix: DescribeInstanceTypes MaxResults validation message. Smithy: DescribeInstanceTypes.MaxResults targets DITMaxResults with @range(min: 5, max: 100), which is exactly the pair AWS prints.
Traces: ec2.describe-instance-types-max-results-below-the-floor-rejected, ec2.describe-instance-types-maxresults-above-the-maximum-rejected.

Cause 9 of 10: CreateTags past 50 tags on a key pair.
AWS: code TagLimitExceeded (the emulator gets this right), message "The resultant tag set must not have more than 50 user tags." — entirely fixed text, no ids.
Emulator: "The resource '<resource-id>' would carry <n> tags, above the maximum of 50".
Fix: the tag-limit refusal message in the shared tag-write validation.
Traces: ec2.keypair-create-tags-past-fifty-tags-rejected.

Cause 10 of 10: CreateNetworkAclEntry with a malformed IPv6 CIDR.
AWS: "CIDR block <ipv6-cidr> is malformed" — fixed text, the CIDR unquoted and taken from the request.
Emulator: "Value (<ipv6-cidr>) for parameter ipv6CidrBlock is invalid. This is not a valid CIDR block.".
Fix: CreateNetworkAclEntry CIDR parsing/validation message.
Traces: ec2.network-acl-entry-malformed-ipv6-cidr-rejected.

## Evidence
Replay output per cause (expected = real AWS, got = emulator):
- associate-subnet-cidr-block-non-4-step-prefix-rejected: assert message == "The CIDR '<cidr>' must be one of /44, /48, /52, /56, /60, /64"; got "The CIDR '2600:1f00:0:9700::/57' is invalid."
- create-key-pair-name-over-255-ascii-chars-rejected: "expected ('InvalidParameterValue', 'Value for parameter KeyName is invalid. Length exceeds maximum of 255.'), got ('InvalidParameterValue', 'Value (sim2real-...aaaa) for parameter keyName is invalid. A key pair name accepts at most 255 characters')"
- create-network-acl-entry-cidr-and-ipv6-cidr-mutually-exclusive-rejected: "expected message 'The parameter cidrBlock cannot be used with the parameter ipv6CidrBlock', got 'Only one of cidrBlock and ipv6CidrBlock may be specified: a network ACL entry matches a range of one address family'"
- create-subnet-cidr-reservation-outside-subnet-range-rejected: assert message == "Reservation CIDR should within subnet CIDR block 10.211.1.0/24"; got "Value (10.211.2.0/28) for parameter cidr is invalid. The CIDR must be within the CIDR range of subnet 'subnet-00000000000000096'."
- create-tags-key-and-value-length-boundaries-rejected: the 128-char key itself was REFUSED — "ClientError (InvalidParameterValue) when calling the CreateTags operation: A tag key accepts at most 127 characters"; create-tags-key-and-value-length-boundary: "unexpected rejection for a 129-char key: 'InvalidParameterValue' ('A tag key accepts at most 127 characters')"; create-tags-value-256-unicode-boundary-and-aws-prefixed-key-rejected: "expected message 'Value ( <value> ) for parameter value is invalid. Tag value exceeds the maximum length of 256 characters', got \"The value of tag key 'sim2real-behav-tv257-...' accepts at most 256 characters\""
- create-tags-overwrites-a-key-and-describe-tags-pages-by-max-results: "expected message 'Value ( 4 ) for parameter maxResults is invalid. Expecting a value greater than 5.', got 'Maximum results allowed are between 5 and 1000'"
- create-vpc-amazon-ipv6-with-explicit-ipv6-cidr-rejected: "unexpected rejection message: 'You cannot specify Ipv6CidrBlock with AmazonProvidedIpv6CidrBlock'"
- describe-instance-types-max-results-below-the-floor-rejected: "expected ('InvalidMaxResults', 'Value ( 1 ) for parameter maxResults is invalid. Expecting a value from ( 5 ) to  ( 100 ).'), got ('InvalidMaxResults', 'Value ( 1 ) for parameter maxResults is invalid. Expecting a value greater than or equal to 5.')"; ...maxresults-above-the-maximum-rejected: "unexpected message for InvalidMaxResults: 'Value ( 101 ) for parameter maxResults is invalid. Expecting a value smaller than or equal to 100.'"
- keypair-create-tags-past-fifty-tags-rejected: expected_message = "The resultant tag set must not have more than 50 user tags."; got "The resource 'key-0000000000000008d' would carry 51 tags, above the maximum of 50"
- network-acl-entry-malformed-ipv6-cidr-rejected: "expected message 'CIDR block 2001:db8::/129 is malformed', got 'Value (2001:db8::/129) for parameter ipv6CidrBlock is invalid. This is not a valid CIDR block.'"

Smithy (services/service-com-amazonaws-ec2-amazonec2): CreateTags declares errors: [] and no length constraints on TagList members (the 128/256 limits are service behavior); DescribeInstanceTypes.MaxResults is DITMaxResults @range(min: 5, max: 100).

## Steps to Reproduce
Replay the checks in trace_ids. Minimal reproductions:
1. AssociateSubnetCidrBlock with an IPv6 /57 -> compare the message to AWS's "must be one of /44, /48, /52, /56, /60, /64".
2. CreateKeyPair with a 256-character KeyName.
3. CreateNetworkAclEntry with CidrBlock and Ipv6CidrBlock together, and separately with Ipv6CidrBlock=2001:db8::/129.
4. CreateSubnetCidrReservation with a CIDR outside the subnet.
5. CreateTags with a 128-character key (must SUCCEED), then 129 (refused with AWS's sentence); same for a 256- and 257-character value.
6. DescribeTags with MaxResults=4.
7. CreateVpc with Ipv6CidrBlock and AmazonProvidedIpv6CidrBlock.
8. DescribeInstanceTypes with MaxResults=1 and MaxResults=101.
9. CreateTags adding a 51st tag to a key pair.
