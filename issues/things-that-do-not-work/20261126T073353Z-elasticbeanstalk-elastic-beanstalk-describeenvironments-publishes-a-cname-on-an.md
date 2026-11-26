Issue-Source: trace-sweep-divergence
Issue-Trace: elasticbeanstalk-python-environment-runs-uploaded-version
Issue-Step: task#24
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticbeanstalk-python-environment-runs-uploaded-version
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk|DescribeEnvironments|2xx->2xx|$.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.CNAME
Issue-Commit: 2ec6cf810098400bb16ee5a2a46828e5bdc9c424

# Elastic Beanstalk DescribeEnvironments publishes a CNAME on an unresolvable .elasticbeanstalk.localhost host the emulator does not serve

> _Note: this issue was identified against the code base at commit `2ec6cf810098400bb16ee5a2a46828e5bdc9c424`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEnvironments

## Description
Cause 1 of 1: the environment CNAME DescribeEnvironments reports is minted on a DNS suffix that is not AWS's and that the emulator does not answer on.

AWS: Smithy (elasticbeanstalk, DescribeEnvironments -> EnvironmentDescriptionsMessage.Environments: EnvironmentDescriptionsList of EnvironmentDescription, member CNAME: DNSCname @length min 1 max 255, documented as "The URL to the CNAME for this environment"). The model constrains CNAME to a DNS name but not its composition, so the composition is recorded behavior only: AWS publishes it as '<cname-prefix>.<environment-dns-label>.<region>.elasticbeanstalk.com'. The fixed text of that template is the trailing '.<region>.elasticbeanstalk.com' suffix and the three-label structure; the variable parts are the CNAME prefix (which AWS takes verbatim from the CNAMEPrefix the caller supplied to CreateEnvironment, defaulting to the environment name), the generated 'eba-<id>' DNS label and the region. The recorded response shows that shape ('<cname-prefix>.eba-<id>.us-west-2.elasticbeanstalk.com'); the specific prefix string and 'eba-' id in any one recording are per-run values that fill the template, not requirements.

Emulator: for the same DescribeEnvironments call on a Ready environment the CNAME is serialized on a '.elasticbeanstalk.localhost' suffix instead of '.<region>.elasticbeanstalk.com', and the first label is not the caller's CNAME prefix - the emulator appends an extra generated segment to it. Replay reports the member as host_not_own: expected '<cname-prefix>.eba-<id>.us-west-2.elasticbeanstalk.com', got '<cname-prefix>-<generated>.eba-<id>.us-west-2.elasticbeanstalk.localhost'. The host the emulator announces is therefore not the host it is actually reached on, which is what host_not_own flags. Status is 200 on both sides, so no error code or message is involved; the divergence is the published host in the response body.

Why: CNAME is the one value a client uses to reach the deployed application. A '.localhost' TLD is unresolvable outside the emulator host and is not in AWS's namespace, so every consumer of this field breaks: a smoke test that GETs http://<CNAME>/ after a deploy cannot resolve it; a Route 53 ALIAS/CNAME record pointed at the returned value is invalid; SwapEnvironmentCNAMEs round-trips become meaningless because neither side is a servable name; and because the first label is not the CNAME prefix the caller supplied, a client that asserts its requested CNAMEPrefix was honoured (or that reconstructs the URL from the prefix it chose) reads a name it never asked for.

Fix: the CNAME minting in the environment state (the value stored at CreateEnvironment / UpdateEnvironment time) and the DescribeEnvironments serializer that emits EnvironmentDescription.CNAME - the published host must be the name the emulator actually serves the environment on, composed as the caller's CNAME prefix plus the generated DNS label, region and the '.elasticbeanstalk.com' suffix, with no extra segment grafted onto the prefix.

Traces: elasticbeanstalk-python-environment-runs-uploaded-version#task#24 (cluster representative; the same cluster also fails at elasticbeanstalk-python-environment-runs-uploaded-version#task#25).

The replay marks this cluster unowned (owned_by empty, no stub), so no open issue carries its replay key. Several open issues are adjacent but do not cover this step: 20261109T144630Z-elasticbeanstalk-elastic-beanstalk-publishes-environment-cname-endpointurl-and-log.md and 20261124T063442Z-elasticbeanstalk-elastic-beanstalk-environment-cname-is-published-as-host.md are bound to other fixtures/steps (and the latter describes a host:port/path CNAME, a different serialization), 20261107T192913Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-data-plane-endpointurl.md is about EndpointURL and the absent data plane, 20261126T004345Z-elasticbeanstalk-logs-a-ready-environment-s-published-cname.md is a behavioral log-streaming check, and 20261123T012852Z-elasticbeanstalk-elastic-beanstalk-swapenvironmentcnames-moves-no-traffic-the-served.md is about SwapEnvironmentCNAMEs traffic. This wire step is filed here so it is re-replayed when the CNAME composition is fixed.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up, 157 steps, 10 mismatched, score 93.6%).

Fixture elasticbeanstalk-python-environment-runs-uploaded-version, step task#24 (cluster also covers task#25), operation DescribeEnvironments, expected status 200 / actual status 200, verdict mismatch:
  $.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.CNAME: host_not_own (expected 'sim2real-probe-eb-587398f0.eba-m2wv44gs.us-west-2.elasticbeanstalk.com', got 'sim2real-probe-eb-587398f0-3689c63835.eba-06268404.us-west-2.elasticbeanstalk.localhost')

divergence_key: service=elasticbeanstalk|DescribeEnvironments|status=200->200|keys=$.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.CNAME

Reading of that diff: the 'sim2real-probe-eb-587398f0' prefix, the 'eba-m2wv44gs'/'eba-06268404' DNS labels and the environment id are per-run values and are NOT the requirement. The requirement the emulator violates is structural and is visible in both halves: (a) the suffix must be '.<region>.elasticbeanstalk.com' and the emulator emits '.<region>.elasticbeanstalk.localhost'; (b) the first label must be exactly the CNAME prefix of the environment, and the emulator appends an extra generated segment ('-3689c63835') to it; (c) the announced host is not one the emulator answers on (host_not_own).

Model grounding: Smithy elasticbeanstalk DescribeEnvironments (input DescribeEnvironmentsMessage{ApplicationName @length 1-100, VersionLabel @length 1-100, EnvironmentIds, EnvironmentNames, IncludeDeleted, IncludedDeletedBackTo, MaxRecords @range 1-1000, NextToken}; output EnvironmentDescriptionsMessage{Environments: EnvironmentDescriptionsList, NextToken}; no modeled errors). EnvironmentDescription.CNAME targets DNSCname @length min 1 max 255, doc "The URL to the CNAME for this environment." The host composition itself is not modeled - recorded behavior only.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Replay fixture elasticbeanstalk-python-environment-runs-uploaded-version: it creates an application, uploads and creates an application version, creates an environment with a CNAME prefix, waits for it to reach Ready, and then calls DescribeEnvironments at step task#24 (and again at task#25).
4. Read Environments[0].CNAME from the task#24 response. It comes back on a '.elasticbeanstalk.localhost' suffix rather than '.<region>.elasticbeanstalk.com', and its first label is the requested CNAME prefix with an extra generated segment appended.
5. Try to resolve or GET the returned CNAME: the name is not in AWS's namespace, is not resolvable off the emulator host, and is not the host the emulator serves the environment on.
6. Both sides answer HTTP 200, so the check is a response-body diff on EnvironmentDescription.CNAME, not a status or error-code comparison.
7. Confirm a fix by re-replaying elasticbeanstalk-python-environment-runs-uploaded-version steps task#24 and task#25: the published CNAME must be a '<prefix>.<label>.<region>.elasticbeanstalk.com' DNS name whose first label is exactly the environment's CNAME prefix, and it must be a host the emulator actually answers on.
