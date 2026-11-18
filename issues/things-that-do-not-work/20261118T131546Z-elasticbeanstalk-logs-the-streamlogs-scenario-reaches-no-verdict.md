Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group
Issue-Key: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group|_http_until#20|Timeout|time.sleep(interval_s)
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# elasticbeanstalk__logs: the StreamLogs scenario reaches no verdict — the run is killed at its budget before the environment serves anything or any log group is read

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEnvironment

## Description
One root cause (Cause 1 of 1), grounded in the one failing elasticbeanstalk__logs check the replay report binds to no open issue.

Cause 1 of 1: the Elastic Beanstalk log-streaming scenario never becomes observable — the run exhausts its whole per-check budget with no assertion reached, so nothing about StreamLogs delivery to the derived CloudWatch Logs group is measured.

AWS: recorded behavior only for the timing (the Smithy model declares no latency). Against real AWS the scenario completes: CreateApplicationVersion(Process=true) leaves Status 'PROCESSED', CreateEnvironment on an Amazon Linux 2023 Python solution stack settles DescribeEnvironments Status 'Ready' with a CNAME that answers HTTP 200 for the bundle's nonce, DescribeEnvironmentResources lists at least one instance, and with StreamLogs enabled the application's stdout arrives in the group derived by prefixing the on-instance path with /aws/elasticbeanstalk/<env>, with the configured RetentionInDays and no group for a file the platform does not stream.
Emulator: the check is terminated by the harness with `Error: Check timed out after 60.0s` — the only detail the replay carries. No step-level assertion was reached. Hypothesis (neither the model nor the recording states where it stalls): the run is consumed by one of its first observable waits — the application version never leaving 'PROCESSING', or DescribeEnvironments never reporting Status 'Ready' (the check's own ceilings are 300 s and 1500 s) — both of which the elasticbeanstalk issue history reports for this family (20261112T072923Z-elasticbeanstalk-elastic-beanstalk-publishes-cnames-that-are-not-dns.md: CloudFormation-launched environments never leave Launching; 20261107T153156Z-elasticbeanstalk-logs-the-streamlogs-scenario-never-reaches-an.md, marked fixed, reported exactly this scenario reaching no observable state). An emulator must settle these transitions inside a client's budget rather than in AWS's provisioning time.
Why: a client cannot verify that Elastic Beanstalk streams an application's stdout to the derived log group at all; the whole StreamLogs feature is unobservable, and an environment that never becomes Ready is indistinguishable from log delivery that is silently broken.
Fix: the elasticbeanstalk state machine — the application-version processing transition and the environment lifecycle that must settle to 'Ready' with a reachable CNAME — and the log-streaming path that creates /aws/elasticbeanstalk/<env>/<on-instance path> and writes the instance's stdout into it.
Related open issues, neither of which carries this failure signature: 20261109T143515Z-elasticbeanstalk-logs-an-elastic-beanstalk-environment-s-cname-is-published.md (the CNAME names a host the emulator does not serve) and 20261107T192913Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-data-plane-endpointurl.md.
Traces: elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: elasticbeanstalk__logs 0/1 (1 failed).

elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group [mismatch]
Error: Check timed out after 60.0s

Expected (AWS): DescribeApplicationVersions reports Status 'PROCESSED'; DescribeEnvironments reports Status 'Ready'; an HTTP GET of http://<CNAME>/ answers 200 carrying the bundle's nonce; DescribeLogGroups holds /aws/elasticbeanstalk/<env>/var/log/web.stdout.log; FilterLogEvents on it returns the line the application printed; no group exists for a log file the platform does not stream.
Actual (emulator): none of those observations is made — the check is killed with no assertion reached. The 60 s figure is this run's harness budget; the environment, application and nonce names are per-run values. The requirement is that the version processes, the environment reaches Ready and the derived log group receives the application's stdout within a client's budget.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1; observe elasticbeanstalk__logs.eb-stream-logs-delivers-the-application-stdout-to-the-derived-log-group end as 'Check timed out'.
2. Minimal reproduction: CreateApplication; CreateStorageLocation and upload a stdlib-only source bundle (Procfile 'web: python3 -u server.py'); CreateApplicationVersion(Process=true) and poll DescribeApplicationVersions — expect Status 'PROCESSED' within seconds.
3. Create the EB instance profile and service role; CreateEnvironment on a 'running Python' Amazon Linux 2023 solution stack with EnvironmentType SingleInstance and StreamLogs enabled; poll DescribeEnvironments — expect Status 'Ready' within seconds, not minutes.
4. GET http://<CNAME>/ — expect 200 with the bundle's nonce; then DescribeLogGroups and FilterLogEvents on /aws/elasticbeanstalk/<env>/var/log/web.stdout.log — expect the stdout line.
5. Re-replay the check; it must reach those assertions instead of being killed at its budget.
