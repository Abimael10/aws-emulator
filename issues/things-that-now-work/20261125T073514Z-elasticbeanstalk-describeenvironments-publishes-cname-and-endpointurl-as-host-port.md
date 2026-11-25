Issue-Source: trace-sweep-divergence
Issue-Trace: elasticbeanstalk-python-environment-runs-uploaded-version
Issue-Step: task#24
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticbeanstalk-python-environment-runs-uploaded-version
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk|DescribeEnvironments|2xx->2xx|$.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.CNAME
Issue-Key: elasticbeanstalk|DescribeEnvironments|2xx->2xx|$.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.EndpointURL
Issue-Commit: fc90a6588ac011398c7d49cbe030c73842e4efe1

# DescribeEnvironments publishes CNAME and EndpointURL as host:port/path strings instead of DNS names

> _Note: this issue was identified against the code base at commit `fc90a6588ac011398c7d49cbe030c73842e4efe1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEnvironments

## Description
ONE root cause in the elasticbeanstalk emulator, grounded in the wire-replay cluster of fixture 'elasticbeanstalk-python-environment-runs-uploaded-version' (steps task#24 and task#25, both DescribeEnvironments, both in this one cluster). ONE cause must be closed before submitting.

An adjacent open issue describes neighbouring ground but does NOT carry this replay key (the replay marked this failure neither ALREADY FILED nor OWNED): 20261124T063442Z-elasticbeanstalk-elastic-beanstalk-environment-cname-is-published-as-host.md, whose Cause 1 names the same CNAME form but is grounded only in the behavioral corpus (simparity-behavioral-aws-v1) check ids, so a fixer re-replaying it never exercises this WIRE fixture step; 20261109T144630Z-elasticbeanstalk-elastic-beanstalk-publishes-environment-cname-endpointurl-and-log.md and 20261109T143515Z-elasticbeanstalk-logs-an-elastic-beanstalk-environment-s-cname-is-published.md both describe the OLDER '<name>.<region>.elasticbeanstalk.com' value, which is not what is published here.

Cause 1 of 1: a launched environment's published CNAME (and its EndpointURL) is not a DNS name — it is a '<host>:<port>/_aws/elasticbeanstalk/<region>/<name>' string, so it carries a port separator and path separators that a DNS name cannot hold and its first dot-label is not the environment's CNAME prefix.
AWS: Smithy (com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService): DescribeEnvironments -> EnvironmentDescriptionsMessage/EnvironmentDescription; member CNAME targets DNSCname (string, @length 1..255) documented 'The URL to the CNAME for this environment'; member EndpointURL targets EndpointURL (string) documented 'For load-balanced, autoscaling environments, the URL to the LoadBalancer. For single-instance environments, the IP address of the instance.'; CheckDNSAvailability input CNAMEPrefix targets DNSCnamePrefix @length 4..63 and its output FullyQualifiedCNAME is 'The fully qualified CNAME to reserve when CreateEnvironment is called with the provided prefix'. Recorded behavior: the CNAME of an environment created with a CNAMEPrefix is the fully qualified name that prefix minted — a dot-separated DNS name whose FIRST label is exactly the request's CNAMEPrefix, whose next labels are the environment's generated sub-domain and the region, ending in the service's own elasticbeanstalk domain; it contains neither ':' nor '/', so the prefix is always recoverable as CNAME.split('.')[0] and can be fed straight back to CheckDNSAvailability. EndpointURL is likewise a host/address, not a host:port with a path.
Emulator: DescribeEnvironments serializes CNAME as '<host>:<port>/_aws/elasticbeanstalk/<region>/<cname-prefix>-<env-id-suffix>' and EndpointURL as '<host>:<port>/_aws/elasticbeanstalk/<region>/_environment/<environment-id>' — the region and the environment id are path segments and the whole value carries a port separator and path separators. Status is 200 in both the recording and the emulator; the divergence is purely in these two member values. The host, the port, the generated prefix suffix and the environment id are per-run values and are NOT the requirement; the requirement is the dot-separated DNS form for CNAME (first label = the request's CNAMEPrefix, region as a label, no ':' and no '/'), and a bare host/address for EndpointURL, with any port and path resolved at the listener rather than published into these members.
Why: the CNAME is the environment's public identity and the EndpointURL is its reachable address. A client that points a DNS alias at the environment, recovers the prefix to re-check availability after SwapEnvironmentCNAMEs, sets a Host header, or configures a health check has only these strings to work from; in this shape they are not names, the documented 'first dot-label is the prefix' recovery yields the first octet of a host literal, and the derived CheckDNSAvailability request is not even well-formed against DNSCnamePrefix @length min 4 — so every CNAME/DNS workflow breaks at its first step.
Fix: the environment-address helper that the CreateEnvironment / UpdateEnvironment / SwapEnvironmentCNAMEs handlers record into environment state, and the EnvironmentDescription serializer that emits CNAME and EndpointURL, must mint a dot-separated name on a domain the emulator answers on and keep the port and path out of the published values. Belongs in the elasticbeanstalk operation handlers, their EnvironmentDescription serializer and the service's environment state module.
Traces: elasticbeanstalk-python-environment-runs-uploaded-version#task#24 and #task#25 (same cluster).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200): cluster verdict 'mismatch', expected_status 200, actual_status 200, operation DescribeEnvironments, fixture 'elasticbeanstalk-python-environment-runs-uploaded-version', representative step 'task#24', also affecting 'task#25'.

Diverging members reported by the replay:
- $.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.CNAME: value_mismatch — expected a fully qualified DNS name of the form '<cname-prefix>.<generated-subdomain>.<region>.elasticbeanstalk.com' (the recording shows 'sim2real-probe-eb-64a903d3.eba-m2wv44gs.us-west-2.elasticbeanstalk.com'); got '127.0.0.1:32939/_aws/elasticbeanstalk/us-west-2/sim2real-probe-eb-64a903d3-c526313924'.
- $.DescribeEnvironmentsResponse.DescribeEnvironmentsResult.Environments.member.EndpointURL: value_mismatch — expected a shaped address value ('<shaped-not-asserted>' in the recording, i.e. a host/URL, not a host:port with a path); got '127.0.0.1:32939/_aws/elasticbeanstalk/us-west-2/_environment/e-c526313924'.

Per-run values in the above (host 127.0.0.1, port 32939, prefix suffix 64a903d3, environment id e-c526313924, generated sub-domain eba-m2wv44gs) are NOT requirements. The pinned requirements are: CNAME contains no ':' and no '/', its first dot-label is exactly the CNAMEPrefix the environment was created with and the region appears as a dot label; EndpointURL is an address, not a path-bearing host:port.

## Steps to Reproduce
1. Boot the emulator and point an AWS client at it (region us-west-2).
2. Replay fixture 'elasticbeanstalk-python-environment-runs-uploaded-version' from trace set simparity-wire-aws-v1 and inspect steps task#24 and task#25.
3. Equivalently, by hand: eb.create_application(ApplicationName=app); eb.create_storage_location(); s3.put_object of a python source bundle; eb.create_application_version(ApplicationName=app, VersionLabel=v, Process=True) and wait Status='Processed'; eb.create_environment(ApplicationName=app, EnvironmentName=env, CNAMEPrefix=<legal prefix of >=4 chars>, VersionLabel=v, SolutionStackName=<python stack>, SingleInstance + IAM option settings); wait Status='Ready'.
4. eb.describe_environments(EnvironmentNames=[env]) and read Environments[0]['CNAME'] and ['EndpointURL'] -> both are '<host>:<port>/_aws/elasticbeanstalk/<region>/...' strings rather than DNS names/addresses; CNAME does not start with '<prefix>.'.
5. Take prefix = CNAME.split('.')[0] (the documented route back to the prefix) and call eb.check_dns_availability(CNAMEPrefix=prefix) -> the label is the host literal's first octet and the SDK refuses the call against DNSCnamePrefix @length min 4.
6. Re-replay elasticbeanstalk-python-environment-runs-uploaded-version#task#24 and #task#25 to confirm a fix.
