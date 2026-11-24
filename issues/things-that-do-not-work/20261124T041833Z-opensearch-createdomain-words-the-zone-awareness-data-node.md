Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.overly-permissive-cluster-config-rules
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.overly-permissive-cluster-config-rules|run#13|AssertionError|assert message == 'You must choose an even number of data nodes for a two Availability Zone deployment', f'unexpected refusal message for zone awareness with on
Issue-Commit: c1008a2f261923e0945727c1f4566178454471ab

# opensearch CreateDomain words the zone-awareness/data-node-count refusal as a "minimum of two data nodes" message instead of AWS's "even number of data nodes"

> _Note: this issue was identified against the code base at commit `c1008a2f261923e0945727c1f4566178454471ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
1 net-new root cause in the opensearch CreateDomain ClusterConfig validator.

Cause 1 of 1: the ValidationException message for a zone-aware domain whose data-node count is not compatible with its AvailabilityZoneCount is emulator-authored wording, not AWS's wording. Failing check: opensearch.overly-permissive-cluster-config-rules.

AWS: CreateDomain with ClusterConfig.ZoneAwarenessEnabled=true and ZoneAwarenessConfig.AvailabilityZoneCount=2 requires the data-node count to be distributable evenly across the two zones. A request with an odd InstanceCount (the recording uses InstanceCount=1 with InstanceType t3.small.search, EngineVersion OpenSearch_2.11, minimal gp3/10 GiB EBSOptions) is refused with HTTP 400 and Error.Code 'ValidationException' (Smithy: com.amazonaws.opensearch#ValidationException, errorKind client, httpError 400, single `message` member; CreateDomain.ClusterConfig.ZoneAwarenessConfig.AvailabilityZoneCount — "Valid values are 2 and 3"; ClusterConfig.InstanceCount — "This number must be greater than 1, otherwise you receive a validation exception"). The recorded message for this error class is, in full and as fixed text with no variable parts:

  You must choose an even number of data nodes for a two Availability Zone deployment

The Smithy model states the constraint but not the message text, so the message itself is recorded behavior only: every token above is fixed text (the phrase names the zone count in words, "two", matching the requested AvailabilityZoneCount=2); no name, id, number or limit from the request appears in it.

Emulator: the status (400) and the error code (ValidationException) are right, but the message is

  You must choose a minimum of two data nodes for a two Availability Zone deployment

i.e. the emulator states a *minimum-count* rule ("a minimum of two data nodes") where AWS states an *evenness* rule ("an even number of data nodes"). Those are different requirements: under AWS's rule InstanceCount=3 across 2 AZs is also refused; under the emulator's wording it reads as allowed.

Why: a client that reads refusal messages — a CLI user, a CloudFormation/Terraform error surface, or code that matches the message to decide how to repair the request — is told to raise the node count to two *or more*, so it will retry an odd count such as 3 and be refused again (or, worse, treat an odd count as valid). Tests and docs that assert AWS's exact wording for this error class also fail against the emulator.

Fix: the CreateDomain ClusterConfig validation for zone awareness in the opensearch service handler — the branch that rejects an incompatible data-node count for ZoneAwarenessEnabled=true must emit AWS's evenness wording (and enforce evenness, not a floor of two) while keeping HTTP 400 / ValidationException.

Traces: opensearch.overly-permissive-cluster-config-rules. The check asserts the refusal of the zone-aware single-node CreateDomain before it reaches its InstanceCount=0 assertion ('Instance Count should be greater than 0.'), so that later rule is unverified by this run and is not part of this issue.

## Evidence
Check opensearch.overly-permissive-cluster-config-rules [mismatch]

Request: CreateDomain(DomainName=<unique>, EngineVersion='OpenSearch_2.11', ClusterConfig={'InstanceType': 't3.small.search', 'InstanceCount': 1, 'ZoneAwarenessEnabled': True, 'ZoneAwarenessConfig': {'AvailabilityZoneCount': 2}}, EBSOptions={'EBSEnabled': True, 'VolumeType': 'gp3', 'VolumeSize': 10}, TagList=[...])

Expected (AWS, recorded):
  HTTP 400, Error.Code = 'ValidationException'
  Error.Message = 'You must choose an even number of data nodes for a two Availability Zone deployment'

Actual (emulator):
  HTTP 400, Error.Code = 'ValidationException'  (code and status match)
  Error.Message = 'You must choose a minimum of two data nodes for a two Availability Zone deployment'

Replay assertion:
  File "check_opensearch_overly_permissive_cluster_config_rules.py", line 59, in run
    assert message == 'You must choose an even number of data nodes for a two Availability Zone deployment'
  AssertionError: unexpected refusal message for zone awareness with one data node: 'You must choose a minimum of two data nodes for a two Availability Zone deployment'

Only the message diverges; the whole string is fixed text for this error class (no request- or state-derived values), so the divergence is the emulator's wording.

## Steps to Reproduce
1. Boot the emulator and point an opensearch client at it.
2. Call CreateDomain with a valid single-node ClusterConfig to confirm the positive control is accepted:
   create_domain(DomainName='os-ctrl-1', EngineVersion='OpenSearch_2.11', ClusterConfig={'InstanceType':'t3.small.search','InstanceCount':1}, EBSOptions={'EBSEnabled':True,'VolumeType':'gp3','VolumeSize':10})
3. Call CreateDomain with zone awareness over two AZs and an odd data-node count:
   create_domain(DomainName='os-za-1', EngineVersion='OpenSearch_2.11', ClusterConfig={'InstanceType':'t3.small.search','InstanceCount':1,'ZoneAwarenessEnabled':True,'ZoneAwarenessConfig':{'AvailabilityZoneCount':2}}, EBSOptions={'EBSEnabled':True,'VolumeType':'gp3','VolumeSize':10})
4. Observe the ClientError: Code is 'ValidationException' (correct) but Message is 'You must choose a minimum of two data nodes for a two Availability Zone deployment' instead of AWS's 'You must choose an even number of data nodes for a two Availability Zone deployment'.
5. Repeat step 3 with InstanceCount=3 to see the consequence of the wrong rule: AWS refuses an odd count across 2 AZs; the emulator's minimum-count rule accepts it.
