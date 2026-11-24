Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-describe-reserved-instances-offerings
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-describe-reserved-instances-offerings|run#9|AssertionError|assert withdrawn == [], (
Issue-Commit: ee29b9ed80499ffda1e629bc10e5c9c45fea7a64

# ec2 DescribeReservedInstancesOfferings serves direct-purchase RI offerings for instance families AWS has withdrawn from the catalog

> _Note: this issue was identified against the code base at commit `ee29b9ed80499ffda1e629bc10e5c9c45fea7a64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReservedInstancesOfferings

## Description
Cause 1 of 1: the emulator's Reserved Instances offering catalog is generated across instance types instead of mirroring the offerings AWS still sells directly, so a filtered query for an instance type whose direct-purchase offerings AWS has withdrawn returns a full set of offerings instead of an empty list. Failing check: ec2.op-contract-describe-reserved-instances-offerings.

AWS: recorded behavior only — Smithy (services/service-com-amazonaws-ec2-amazonec2) models DescribeReservedInstancesOfferings(Filters, OfferingClass, ProductDescription, InstanceType, ...) -> DescribeReservedInstancesOfferingsResult{ReservedInstancesOfferings (reservedInstancesOfferingsSet), NextToken} with no modeled errors, so which offerings exist is the catalog's content and not a modelled refusal. The check pins AWS's live catalog: a standard / Linux/UNIX query filtered to an instance type AWS still lists (a1.large in the recording) returns at least one offering, each echoing the filtered InstanceType, OfferingClass 'standard', ProductDescription 'Linux/UNIX', a non-empty ReservedInstancesOfferingId and Duration in (31536000, 94608000) seconds; the identical query for an instance type whose direct RI purchase AWS has withdrawn (m5.large in the recording) returns ReservedInstancesOfferings == [] — a 200 with an empty list, not a refusal.

Emulator: the withdrawn-type query answers HTTP 200 with a populated list (24 offerings for the filtered type in this run; the count is a per-run artifact of the generator, the divergence is that the list is non-empty at all), i.e. the catalog is synthesized per instance type rather than taken from AWS's published offerings.

Why: a client that enumerates purchasable Reserved Instances is shown offerings it cannot buy, and PurchaseReservedInstancesOffering against such an offering id can only be refused or silently succeed against an offering AWS does not sell. Capacity-planning and cost tooling that distinguishes 'still sold directly' from 'withdrawn' by the emptiness of this list reads the opposite answer.

Fix: the DescribeReservedInstancesOfferings read path and the RI offering catalog data behind it (it must serve AWS's published offering set, with withdrawn families absent, rather than deriving one offering set per instance type). Note the check carries the suite tag sweep:never, so the suite does not score it; it is filed because the emulator does answer, and answers with a catalog AWS does not have.

Traces: ec2.op-contract-describe-reserved-instances-offerings.

## Evidence
Check: ec2.op-contract-describe-reserved-instances-offerings.
Positive control (passes): describe_reserved_instances_offerings(Filters=[{'Name':'instance-type','Values':['a1.large']}], OfferingClass='standard', ProductDescription='Linux/UNIX') returns offerings, each with InstanceType 'a1.large', OfferingClass 'standard', ProductDescription 'Linux/UNIX', a ReservedInstancesOfferingId and Duration in (31536000, 94608000).
Divergence: the same query with Values=['m5.large'] —
  Expected (AWS): HTTP 200 with ReservedInstancesOfferings == [].
  Actual (emulator): HTTP 200 with 24 m5.large offerings.
  AssertionError: expected AWS's direct-purchase RI catalog for m5.large to be withdrawn (empty) — the same query that finds a1.large offerings above returned 24 m5.large offering(s), which means the withdrawal this check pins has reversed.

## Steps to Reproduce
1. Boot the emulator and replay the behavioral suite; run ec2.op-contract-describe-reserved-instances-offerings.
2. Equivalently: call DescribeReservedInstancesOfferings with Filters=[{Name:'instance-type',Values:['a1.large']}], OfferingClass='standard', ProductDescription='Linux/UNIX' (expect a non-empty, correctly filtered list), then the identical call with Values=['m5.large'] (expect an empty ReservedInstancesOfferings list; the emulator returns offerings).
3. DryRun=True on the same operation must still answer DryRunOperation with 'Request would have succeeded, but DryRun flag is set.'
