Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-catalog-describe-instance-type-offerings
Issue-Step: task#30
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-catalog-describe-instance-type-offerings
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="c8ib.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="c8ib.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="i3.metal"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8gb.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8gb.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8gn.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8gn.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8ib.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8ib.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8idb.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8idb.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8in.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m8in.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m9g.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="m9gd.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8gb.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8gb.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8gn.metal-24xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8gn.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8ib.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8ib.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8idb.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8idb.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8idn.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8idn.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8in.metal-48xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="r8in.metal-96xl"]
Issue-Key: ec2|DescribeInstanceTypeOfferings|2xx->2xx|$.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="u-6tb1.metal"]
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 DescribeInstanceTypeOfferings lists instance types not offered in the region

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceTypeOfferings

## Description
DescribeInstanceTypeOfferings answers 200 but advertises instance types the region does not offer. 16 divergence clusters of fixture ec2-catalog-describe-instance-type-offerings are net-new (no stub, no owning issue) and all have the same root cause: the offerings table is generated from the emulator's full instance-type list instead of the region's actual offering set.

GENERAL FAULT
AWS: DescribeInstanceTypeOfferings with LocationType 'region' returns one offering per instance type that is actually offered in that region; a type that exists in the service model but is not offered there is absent from the response.
Emulator: it emits an offering for every type in its catalog, so queries that AWS answers with nothing (or with fewer rows) come back with extra instanceTypeOfferingSet items - chiefly the bare-metal sizes of the newest families and some retired types.
Why: this call is exactly how clients decide what they may launch in a region. Extra rows make a client select a type that RunInstances in that region will reject, and capacity/pricing tooling that enumerates offerings reports hardware that is not there.
Fix: the region offering data the DescribeInstanceTypeOfferings handler reads (the per-region offering table), not the serializer - the offering rows themselves are shaped correctly.

Cause 1 of 1 (16 clusters, one per queried type family): the offering set includes types the recording does not offer in us-west-2 - the metal sizes of the c8ib, m8gb, m8gn, m8ib, m8idb, m8in, m9g, m9gd, r8gb, r8gn, r8ib, r8idb, r8idn and r8in families, plus i3.metal and u-6tb1.metal. Each of the 16 steps queries one family and gets one or two extra items. Traces: ec2-catalog-describe-instance-type-offerings steps task#30, task#53, task#91, task#93, task#96, task#98, task#100, task#102, task#103, task#144, task#146, task#149, task#151, task#152, task#153 and task#165 (all 16 listed in the pointer so a fix can be confirmed against every family).

## Evidence
All 16 clusters: expected status 200, actual status 200, verdict mismatch - extra items in the body.
task#30: $.DescribeInstanceTypeOfferingsResponse.instanceTypeOfferingSet.item[instanceType="c8ib.metal-48xl"] and ["c8ib.metal-96xl"] expected None, got {'instanceType': ..., 'locationType': 'region', 'location': 'us-west-2'}.
task#53: i3.metal extra. task#91: m8gb.metal-24xl / -48xl extra. task#93: m8gn.metal-24xl / -48xl extra. task#96: m8ib.metal-48xl / -96xl extra. task#98: m8idb.metal-48xl / -96xl extra. task#100: m8in.metal-48xl / -96xl extra. task#102: m9g.metal-24xl extra. task#103: m9gd.metal-24xl extra. task#144: r8gb.metal-24xl / -48xl extra. task#146: r8gn.metal-24xl / -48xl extra. task#149: r8ib.metal-48xl / -96xl extra. task#151: r8idb.metal-48xl / -96xl extra. task#152: r8idn.metal-48xl / -96xl extra. task#153: r8in.metal-48xl / -96xl extra. task#165: u-6tb1.metal extra.
(us-west-2 is the region of these recordings; the requirement is that the response contain exactly the types the region offers.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-catalog-describe-instance-type-offerings step task#30 - DescribeInstanceTypeOfferings with LocationType 'region' filtered to the c8ib family - and compare instanceTypeOfferingSet with the recording: the two metal sizes are extra.
3. Repeat for steps task#53, task#91, task#93, task#96, task#98, task#100, task#102, task#103, task#144, task#146, task#149, task#151, task#152, task#153 and task#165; each returns one or two offerings the recording does not contain.
