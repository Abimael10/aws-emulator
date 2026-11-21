Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.eb-deploying-a-new-application-version-replaces-the-served-content elasticbeanstalk.eb-describe-instances-health-reports-the-environments-real-instances elasticbeanstalk.eb-failed-version-deploy-leaves-the-previous-version-serving elasticbeanstalk.eb-rolling-deployment-keeps-serving-where-all-at-once-does-not elasticbeanstalk.eb-retrieved-environment-info-carries-the-applications-own-log-line elasticbeanstalk.eb-terminated-environment-stops-serving-and-frees-its-cname-prefix
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.eb-deploying-a-new-application-version-replaces-the-served-content|run#38|AssertionError|assert after_deploys == before_deploys + 1, (
Issue-Key: elasticbeanstalk.eb-describe-instances-health-reports-the-environments-real-instances|run#24|AssertionError|assert isinstance(inst.get("System"), dict), (
Issue-Key: elasticbeanstalk.eb-failed-version-deploy-leaves-the-previous-version-serving|run#20|AssertionError|assert st_bad.upper() == "PROCESSED", (
Issue-Key: elasticbeanstalk.eb-rolling-deployment-keeps-serving-where-all-at-once-does-not|_deploy_and_sample#25|AssertionError|assert len(samples) >= 20, (
Issue-Key: elasticbeanstalk.eb-retrieved-environment-info-carries-the-applications-own-log-line|_poll#28|Timeout|return last
Issue-Key: elasticbeanstalk.eb-terminated-environment-stops-serving-and-frees-its-cname-prefix|run#35|AssertionError|assert freed["FullyQualifiedCNAME"] == cname, (
Issue-Commit: 8cbf0dcd0020846f13fa76c0319ab04fca2e29a2

# Elastic Beanstalk control-plane records diverge: no deployment event, DescribeInstancesHealth omits System, Process=true fails an unvalidated bundle, an instant deployment, no retrievable log tail, and an inconsistent FullyQualifiedCNAME (6 causes)

> _Note: this issue was identified against the code base at commit `8cbf0dcd0020846f13fa76c0319ab04fca2e29a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstancesHealth

## Description
Six net-new root causes in the Elastic Beanstalk control plane's records of a deployment. Each is independent of the others and must be closed separately.

Cause 1 of 6: a version deployment emits no deployment event.
AWS: UpdateEnvironment(VersionLabel=...) against a running environment records, in DescribeEvents for that environment, the INFO event whose Message is exactly "New application version was deployed to running EC2 instances." — one such event per deployment, alongside the "updateEnvironment is starting."/"updateEnvironment completed successfully." pair. Recorded behavior only (the event text is not in the model; its fixed text is the whole sentence and has no variable part).
Emulator: that message appeared 0 times before and 0 times after the update. The stream held only [('INFO','updateEnvironment completed successfully.'), ('INFO','updateEnvironment is starting.'), ('INFO','Successfully launched environment: <env>'), ('INFO','createEnvironment is starting.')].
Why: tooling and CI gates that confirm a deploy landed by scanning DescribeEvents for that line never see it, so a successful deployment is indistinguishable from a configuration-only update.
Fix: the UpdateEnvironment/deployment path's event emission in the elasticbeanstalk service.
Traces: elasticbeanstalk.eb-deploying-a-new-application-version-replaces-the-served-content.

Cause 2 of 6: DescribeInstancesHealth with AttributeNames=['All'] omits the System attribute.
AWS: Smithy: DescribeInstancesHealth output InstanceHealthList of SingleInstanceHealth, whose member System targets SystemStatus {CPUUtilization, LoadAverage}, documented "Operating system metrics from the instance." AttributeNames selects which attributes are returned, and the 'All' value requests every one of them, so each entry carries System.
Emulator: with AttributeNames=['All'] the entry for instance 'i-...' carried InstanceId, HealthStatus, Color, Causes, LaunchedAt, ApplicationMetrics, Deployment, AvailabilityZone and InstanceType but no System member at all.
Why: a client reading enhanced-health OS metrics (CPU utilization, load average) gets a missing key and either crashes on the access or silently reports no system load for every instance.
Fix: the DescribeInstancesHealth handler/serializer — populate SystemStatus and honor 'All' in AttributeNames.
Traces: elasticbeanstalk.eb-describe-instances-health-reports-the-environments-real-instances.

Cause 3 of 6: CreateApplicationVersion(Process=true) fails a bundle AWS does not validate.
AWS: Smithy: CreateApplicationVersion input member Process (ApplicationVersionProccess boolean). Process=true runs preprocessing/validation of the version's configuration files, not of the archive itself: a source bundle that is not a valid zip still settles with DescribeApplicationVersions Status 'PROCESSED' (the failure surfaces later, at deploy time, as a failed deployment that leaves the previous version serving).
Emulator: the version settled Status 'FAILED' instead of 'PROCESSED'.
Why: the premature refusal short-circuits every failed-deployment scenario — a client can never reach the state "a bad version is deployable and its deployment fails, leaving the previous version serving", which is the exact rollback behavior it needs to test. The version is also refused for a reason AWS does not apply.
Fix: the CreateApplicationVersion handler's processing/validation step — Process=true must not validate the archive's format.
Traces: elasticbeanstalk.eb-failed-version-deploy-leaves-the-previous-version-serving.

Cause 4 of 6: a deployment completes with no observable in-progress window, so deployment policies are indistinguishable.
AWS: a deployment governed by aws:elasticbeanstalk:command DeploymentPolicy (Rolling with BatchSize/BatchSizeType, versus AllAtOnce) takes a non-trivial amount of wall time during which the environment keeps answering (Rolling) or stops answering (AllAtOnce); a client sampling the endpoint while the deployment runs collects many samples. Recorded behavior only.
Emulator: during a Rolling(Fixed,1) deployment only 1 sample could be taken before the deployment was already finished, so the availability difference between Rolling and AllAtOnce cannot be observed.
Why: deployment-policy behavior — the availability guarantee a client chooses Rolling for — is unmodelled; any test that measures downtime during a deploy is vacuous.
Fix: the deployment state machine — honor the environment's DeploymentPolicy option and model the batched, time-extended transition instead of an instantaneous one.
Traces: elasticbeanstalk.eb-rolling-deployment-keeps-serving-where-all-at-once-does-not.

Cause 5 of 6: RequestEnvironmentInfo/RetrieveEnvironmentInfo never produce a log tail.
AWS: Smithy: RequestEnvironmentInfo(InfoType) then RetrieveEnvironmentInfo(InfoType) returning EnvironmentInfo, a list of EnvironmentInfoDescription {InfoType, Ec2InstanceId, SampleTimestamp, Message}, where Message is documented as "a presigned Amazon S3 URL" whose bytes are the requested tail. For InfoType 'tail' there is one entry per instance of the environment, attributed to the instance ids DescribeEnvironmentResources reports, and the fetched bytes are the multi-file tail: they name the on-instance paths they concatenate (a '/var/log/' section header) and contain the lines the running application wrote to stdout.
Emulator: after RequestEnvironmentInfo(InfoType='tail') the poll on RetrieveEnvironmentInfo never returns a non-empty EnvironmentInfo list, so the check runs to the harness cap ("Check timed out after 60.0s") and no tail URL is ever published.
Why: log retrieval, the primary way a client diagnoses an environment through the API, returns nothing forever; code that waits for the tail hangs rather than failing.
Fix: the RequestEnvironmentInfo/RetrieveEnvironmentInfo handlers and the instance log store — a requested tail must become retrievable, attributed to the environment's instance ids, with a fetchable Message URL.
Traces: elasticbeanstalk.eb-retrieved-environment-info-carries-the-applications-own-log-line.

Cause 6 of 6: CheckDNSAvailability's FullyQualifiedCNAME contradicts the CNAME the environment is published with.
AWS: Smithy: CheckDNSAvailability(CNAMEPrefix) -> {Available, FullyQualifiedCNAME: DNSCname}. FullyQualifiedCNAME is the hostname a prefix resolves to, and it is the same hostname DescribeEnvironments reports as that environment's CNAME when the prefix is in use — so after an environment is terminated and its prefix freed, the FullyQualifiedCNAME for that prefix is exactly the hostname the environment held.
Emulator: the two reads disagree in FORM, not just in value: CheckDNSAvailability named '<prefix>.us-west-2.elasticbeanstalk.com' while DescribeEnvironments had published the environment's CNAME as '127.0.0.1:44961/_aws/elasticbeanstalk/us-west-2/<prefix>' — a host:port plus path, not a hostname. (The host and port are per-run values and are not the requirement; the requirement is that both operations publish the SAME address for one prefix, in DNSCname form.) Open issues 20261112T072923Z-elasticbeanstalk-elastic-beanstalk-publishes-cnames-that-are-not-dns.md and 20261112T115323Z-elasticbeanstalk-elastic-beanstalk-checkdnsavailability-returns-a-fullyqualifiedcname-that-is.md each describe one side of this; neither owns this check, and neither covers the inconsistency BETWEEN the two operations.
Why: a client cannot correlate a prefix with the environment that holds it: prefix-reuse logic built on CheckDNSAvailability compares against a CNAME that can never match, so a freed prefix never looks freed.
Fix: the CNAME publication shared by CheckDNSAvailability and the environment description/state — one address form, produced from one place.
Traces: elasticbeanstalk.eb-terminated-environment-stops-serving-and-frees-its-cname-prefix.

## Evidence
Cause 1: AssertionError: the control plane must record exactly the one deployment it performed: 'New application version was deployed to running EC2 instances.' appeared 0 times before the update and 0 times after; the stream held 4 events [('INFO', 'updateEnvironment completed successfully.'), ('INFO', 'updateEnvironment is starting.'), ('INFO', 'Successfully launched environment: sim2real-eb-ver-env-8a3aeb6cd80e'), ('INFO', 'createEnvironment is starting.')].
Cause 2: AssertionError: AttributeNames=['All'] must populate System for instance 'i-c7f7aeb3254e4e9b9'; the entry was {'InstanceId': ..., 'HealthStatus': 'Ok', 'Color': 'Green', 'Causes': [], 'LaunchedAt': ..., 'ApplicationMetrics': {...}, 'Deployment': {...}, 'AvailabilityZone': 'us-west-2a', 'InstanceType': 't3.micro'} — no 'System' key.
Cause 3: AssertionError: measured on real AWS: CreateApplicationVersion(Process=True) does not validate the archive, so even a non-zip bundle settles PROCESSED; got 'FAILED'.
Cause 4: AssertionError: the Rolling(Fixed,1) deployment must have been sampled enough times to say anything about availability; only 1 samples were taken.
Cause 5: Error: Check timed out after 60.0s — eb.request_environment_info(InfoType='tail') followed by a poll on eb.retrieve_environment_info(InfoType='tail') never returning a non-empty EnvironmentInfo.
Cause 6: AssertionError: the released prefix must resolve to the hostname the environment held; CheckDNSAvailability named 'sim2real-eb-term-env-b2b37874e46a.us-west-2.elasticbeanstalk.com', the environment had '127.0.0.1:44961/_aws/elasticbeanstalk/us-west-2/sim2real-eb-term-env-b2b37874e46a'.

## Steps to Reproduce
1. Create an application, two application versions, and an environment; wait for Ready.
2. UpdateEnvironment(VersionLabel=<second version>), wait for Ready, then DescribeEvents for the environment and grep for "New application version was deployed to running EC2 instances." — absent (Cause 1).
3. DescribeInstancesHealth(EnvironmentName=<env>, AttributeNames=['All']) and read InstanceHealthList[0]['System'] — missing (Cause 2).
4. CreateApplicationVersion(Process=True) with a SourceBundle that is not a zip archive, then poll DescribeApplicationVersions: Status settles 'FAILED' instead of 'PROCESSED' (Cause 3).
5. Set aws:elasticbeanstalk:command DeploymentPolicy=Rolling, BatchSizeType=Fixed, BatchSize=1, deploy a new version while sampling the environment endpoint in a loop: the deployment finishes after a single sample (Cause 4).
6. RequestEnvironmentInfo(EnvironmentName=<env>, InfoType='tail'), then poll RetrieveEnvironmentInfo(InfoType='tail'): EnvironmentInfo stays empty (Cause 5).
7. Read the environment's CNAME from DescribeEnvironments, TerminateEnvironment, then CheckDNSAvailability(CNAMEPrefix=<the environment's prefix>) and compare FullyQualifiedCNAME with that CNAME (Cause 6).
