Issue-Source: trace-sweep-divergence
Issue-Trace: elasticmapreduce-release-label-and-instance-type-catalog
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticmapreduce-release-label-and-instance-type-catalog
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: elasticmapreduce|DescribeReleaseLabel|2xx->2xx|$.AvailableOSReleases[*]
Issue-Commit: f2e6a4662e66854bb360a08423e8578f1603b4d0

# EMR DescribeReleaseLabel serves a stale hard-coded AvailableOSReleases catalog

> _Note: this issue was identified against the code base at commit `f2e6a4662e66854bb360a08423e8578f1603b4d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReleaseLabel

## Description
DescribeReleaseLabel answers 200 but the AvailableOSReleases list it serves is a fixed, stale catalog baked into the emulator rather than the set of Amazon Linux releases that the requested ReleaseLabel actually supports.

THIS ISSUE BUNDLES 1 CAUSE. It must be closed.

Cause 1 of 1: AvailableOSReleases is a hard-coded Amazon Linux catalog, not the one resolved for the requested ReleaseLabel.
  AWS: 200 with DescribeReleaseLabelOutput.AvailableOSReleases (Smithy: OSReleaseList of OSRelease, member Label: String; OSRelease.Label doc: "The Amazon Linux release specified for a cluster in the RunJobFlow request. The format is as shown in the Amazon Linux 2 Release Notes. For example, 2.0.20220218.1.") listing the Amazon Linux releases available for the ReleaseLabel named in the request, in the service's newest-first order. The set and its ordering are properties of the requested release label and of the service's current Amazon Linux inventory; they are not a constant. The concrete labels in the recording (its newest entries are 2.0.2026xxxx/2.0.2025xxxx releases) are recorded values that fill that template, not the requirement itself.
  Emulator: 200, but AvailableOSReleases is the same fixed list of older Amazon Linux releases on every call — the recorded reply's entries are all 2.0.2024xxxx releases, so every compared element differs by position (36 element positions reported mismatched, from $.AvailableOSReleases[0] through $.AvailableOSReleases[45]), and the list length does not line up with the catalog either. This is the whole list being the wrong catalog, not one wrong field.
  Why: a client that calls DescribeReleaseLabel to discover which Amazon Linux release it may pin for a cluster (passing it as OSReleaseLabel to RunJobFlow) is handed releases that the requested EMR release label does not offer. Picking the newest entry — the normal use of this call — yields a label the service would reject, and any client that pins or compares the OS release it read back gets a different answer from the emulator than from AWS.
  Fix: the DescribeReleaseLabel handler and the per-release-label Amazon Linux catalog backing it — AvailableOSReleases must be resolved from the ReleaseLabel in the request (and paginated with MaxResults/NextToken, MaxResultsNumber range 1..100) instead of returning a literal list. Note this is the DescribeReleaseLabel catalog, a separate component from the ListSupportedInstanceTypes catalog already reported in 20261102T083234Z-elasticmapreduce-emr-cluster-and-instance-group-reads-omit-modeled.md (cause 5), which owns no replay key for DescribeReleaseLabel and does not cover this.
  Traces: elasticmapreduce-release-label-and-instance-type-catalog#task#2.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture elasticmapreduce-release-label-and-instance-type-catalog, step task#2, operation DescribeReleaseLabel, verdict=mismatch, expected_status 200 / actual_status 200.

The status matches; the body does not. 36 positions of $.AvailableOSReleases diverge. First few, expected vs actual:
  $.AvailableOSReleases[0]: expected {'Label': '2.0.20260629.0'}, got {'Label': '2.0.20241217.0'}
  $.AvailableOSReleases[1]: expected {'Label': '2.0.20260615.0'}, got {'Label': '2.0.20241113.0'}
  $.AvailableOSReleases[2]: expected {'Label': '2.0.20260515.0'}, got {'Label': '2.0.20241031.0'}
  $.AvailableOSReleases[3]: expected {'Label': '2.0.20260508.0'}, got {'Label': '2.0.20241011.0'}
  ... continuing through $.AvailableOSReleases[45].

Every compared entry is well-formed (an OSRelease with a Label in the documented 2.0.<date>.<n> form), so the defect is not the shape but the catalog's contents and ordering: the emulator replies with the same literal list of older Amazon Linux releases irrespective of the ReleaseLabel requested. The individual date strings above are this recording's values and are not themselves the requirement — the requirement is that the list is the Amazon Linux inventory resolved for the requested ReleaseLabel, newest-first.

Smithy consulted: DescribeReleaseLabel input {ReleaseLabel: String, NextToken: String, MaxResults: MaxResultsNumber @range 1..100}, output {ReleaseLabel, Applications: SimplifiedApplicationList, NextToken, AvailableOSReleases: OSReleaseList}; errors InvalidRequestException (400), InternalServerException (500). OSRelease{Label: String}.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus against it.
3. Representative: fixture elasticmapreduce-release-label-and-instance-type-catalog, step task#2 — call DescribeReleaseLabel with the fixture's ReleaseLabel and compare the AvailableOSReleases list element by element against the recorded response.
4. Observe that the emulator's list is a fixed catalog of older Amazon Linux releases, identical regardless of the ReleaseLabel supplied, so element 0 (and 35 further positions up to index 45) differ from the catalog AWS serves for that release label.
