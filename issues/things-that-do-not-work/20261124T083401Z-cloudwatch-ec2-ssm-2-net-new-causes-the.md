Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ssm.dcwagent-custom-statsd-value-and-guest-only-memory-metric-reach-cloudwatch cloudwatch__ec2__ssm.dcwagent-disk-used-percent-tracks-bytes-written-in-guest-while-an-unconfigured-metric-never-appears
Issue-Key: cloudwatch__ec2__ssm.dcwagent-custom-statsd-value-and-guest-only-memory-metric-reach-cloudwatch|run#31|ValueError|guest_mem = float(_marker(out, "CWAGENT-GUEST-MEM"))
Issue-Key: cloudwatch__ec2__ssm.dcwagent-disk-used-percent-tracks-bytes-written-in-guest-while-an-unconfigured-metric-never-appears|run#27|ValueError|df_before = float(_marker(out, "CWAGENT-DF-BEFORE"))
Issue-Commit: 831ce6778f4fe61ae9858b780889b40df5a51fbd

# cloudwatch__ec2__ssm: 2 net-new causes — the managed node's guest answers /proc/meminfo and `df` with empty output, so the CloudWatch-agent memory and disk legs reach no verdict

> _Note: this issue was identified against the code base at commit `831ce6778f4fe61ae9858b780889b40df5a51fbd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
2 net-new root causes (Cause 1 of 2, Cause 2 of 2). Both are observed in the guest reached through SSM Run Command (AWS-RunShellScript) on an EC2 managed node, at a stage LATER than the open issues 20261114T143448Z-cloudwatch-ec2-ssm-cloudwatch-agent-scenarios-never-finish-in-guest-statsd.md and 20261107T064927Z-cloudwatch-ec2-ssm-the-ssm-installed-cloudwatch-agent.md describe: in this replay the invocation DOES reach a terminal status, the amazon-cloudwatch-agent package IS installed, systemd reports it 'active', its own control interface reports {"status":"running"}, the kernel socket tables show its UDP listener, and the guest reads its own instance id back from IMDSv2. Those earlier issues therefore do not cover what fails now; they describe an invocation that never terminated and an agent that was never installed. The remaining divergence is that specific kernel/coreutils introspection surfaces inside the guest produce NO output at all.

Cause 1 of 2: the guest's /proc/meminfo yields nothing, so no memory reading can be taken inside a managed node.
AWS: recorded behavior only — neither the SSM model nor the CloudWatch model describes guest file contents. Smithy: GetCommandInvocation -> Status (CommandInvocationStatus), ResponseCode (default 0), StandardOutputContent (@length max 24000) — i.e. the stdout of the script AWS ran in the guest is returned verbatim. On a real AL2023 managed node, a shell line of the form `echo "<MARKER> $(awk '/^MemTotal:/{t=$2}/^MemFree:/{f=$2}/^Buffers:/{b=$2}/^Cached:/{c=$2} END{printf "%.2f", (t-f-b-c)/t*100}' /proc/meminfo)"` emits the marker followed by a used-memory percentage strictly between 0 and 100, and the CloudWatch agent's `mem` plugin publishes mem_used_percent within a few points of it.
Emulator: the invocation ends Status 'Success' and the other markers emitted by the SAME script in the SAME invocation are populated, but the /proc/meminfo-derived marker's value is the empty string — the awk over /proc/meminfo produced no bytes (no MemTotal/MemFree/Buffers/Cached lines, or no such file). The check's `float('')` on that marker raises `ValueError: could not convert string to float: ''` before any CloudWatch assertion is reached.
Why: a client that runs Run Command to read guest-only memory state, or installs the CloudWatch agent to publish mem_used_percent, gets a successful invocation whose output is silently missing the number. There is no error code and no non-zero ResponseCode to distinguish 'this host has no memory accounting' from 'the agent is misconfigured', and the entire reason the agent exists (a metric the hypervisor cannot see) cannot be exercised.
Fix: the emulated guest's procfs/filesystem surface behind SSM Run Command execution (the managed-node guest execution component that already serves /proc/net/udp, rpm, systemctl and IMDS for these scripts) must expose /proc/meminfo with MemTotal/MemFree/Buffers/Cached lines, and the CloudWatch-agent mem plugin path must publish mem_used_percent into the configured namespace on the InstanceId dimension.
Traces: cloudwatch__ec2__ssm.dcwagent-custom-statsd-value-and-guest-only-memory-metric-reach-cloudwatch.

Cause 2 of 2: `df` in the guest yields nothing, so root-filesystem usage cannot be read inside a managed node.
AWS: recorded behavior only — not modelled. Smithy: GetCommandInvocation returns StandardOutputContent verbatim, Status and ResponseCode. On a real AL2023 managed node, `df --output=pcent / | tail -1 | tr -dc 0-9.` prints a percentage strictly between 0 and 100, `df --output=avail -m /` prints the available MiB, and after `fallocate -l 2G <path>; sync` the percentage rises; the CloudWatch agent's `disk` plugin publishes disk_used_percent within a few points of it and rises with it.
Emulator: the install invocation ends Status 'Success' with the agent reported installed, active, running and reachable over IMDS, but the `df --output=pcent /` marker's value is the empty string — `df` produced no output for the root filesystem. The check's `float('')` on that marker raises `ValueError: could not convert string to float: ''`, so the published-series legs (baseline disk_used_percent, the post-fill rise, and the companion assertion that an UNCONFIGURED metric must never appear in that namespace) are never evaluated.
Why: a client that reads filesystem fullness in a guest, or relies on the agent's disk_used_percent, sees a successful command with no number and no error, and cannot tell an empty filesystem report from an agent that is not collecting. Capacity-driven automation built on this reads a missing value as zero or crashes.
Fix: the emulated guest's filesystem/stat surface behind SSM Run Command execution must answer `df` for the root mount (percent used and available blocks) and must reflect real byte writes such as fallocate, and the CloudWatch-agent disk plugin path must publish disk_used_percent for the configured resource into the configured namespace while publishing nothing for metrics absent from the agent configuration.
Traces: cloudwatch__ec2__ssm.dcwagent-disk-used-percent-tracks-bytes-written-in-guest-while-an-unconfigured-metric-never-appears.

## Evidence
Cause 1 — check cloudwatch__ec2__ssm.dcwagent-custom-statsd-value-and-guest-only-memory-metric-reach-cloudwatch, verdict [error]:
  File "check_cloudwatch__ec2__ssm_dcwagent_custom_statsd_value_and_guest_only_memory_metric_reach_cloudwatch.py", line 487, in run
    guest_mem = float(_marker(out, "CWAGENT-GUEST-MEM"))
  ValueError: could not convert string to float: ''
Expected: the marker line carries a used-memory percentage, 0.0 < value < 100.0, computed as (MemTotal-MemFree-Buffers-Cached)/MemTotal*100 from the guest's /proc/meminfo; AWS/<per-run namespace> then serves mem_used_percent within 25 points of it.
Actual: the marker's value is '' while, in the same invocation and the same stdout, the preceding assertions on CWAGENT-RPM (amazon-cloudwatch-agent-*), CWAGENT-SYSTEMD ('active'), CWAGENT-CTL ('"status":"running"'), CWAGENT-LISTEN (>=1 UDP socket on the statsd port, counted out of /proc/net/udp and /proc/net/udp6), CWAGENT-IID (equal to the id RunInstances returned) and CWAGENT-SENT all passed. The percentage, the instance id, the namespace and the per-run gauge value are per-run values, not requirements; the requirement is that a non-empty percentage is emitted at all.

Cause 2 — check cloudwatch__ec2__ssm.dcwagent-disk-used-percent-tracks-bytes-written-in-guest-while-an-unconfigured-metric-never-appears, verdict [error]:
  File "check_cloudwatch__ec2__ssm_dcwagent_disk_used_percent_tracks_bytes_written_in_guest_while_an_unconfigured_metric_never_appears.py", line 471, in run
    df_before = float(_marker(out, "CWAGENT-DF-BEFORE"))
  ValueError: could not convert string to float: ''
Expected: `df --output=pcent /` in the guest prints a percentage, 0.0 < value < 100.0, and `df --output=avail -m /` prints enough available MiB to write 2 GiB; after the fill the percentage is higher and the agent's published disk_used_percent is within 5 points of it.
Actual: the df-derived marker's value is '' on an invocation whose Status is 'Success' and whose agent markers (RPM, SYSTEMD 'active', CTL '"status":"running"', IID) all matched. No HTTP error and no non-zero ResponseCode accompanies the missing output.

## Steps to Reproduce
1. Create a VPC with one public subnet and an egress security group, and an instance profile whose role carries arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore and arn:aws:iam::aws:policy/CloudWatchAgentServerPolicy.
2. ec2 RunInstances a t3.micro from the SSM public parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 with that profile and detailed monitoring enabled; wait for ssm DescribeInstanceInformation to report it Online with PlatformType 'Linux'.
3. ssm SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[<id>]) with a script that (a) installs amazon-cloudwatch-agent and starts it on a config collecting disk_used_percent for "/" and a statsd metric, then (b) echoes markers: `echo "M1 $(awk '/^MemTotal:/{t=$2}/^MemFree:/{f=$2}/^Buffers:/{b=$2}/^Cached:/{c=$2} END{printf "%.2f", (t-f-b-c)/t*100}' /proc/meminfo)"` and `echo "M2 $(df --output=pcent / | tail -1 | tr -dc 0-9.)"` and `echo "M3 $(df --output=avail -m / | tail -1 | tr -dc 0-9)"`.
4. Poll ssm GetCommandInvocation(CommandId, InstanceId) to a terminal Status and read StandardOutputContent: Status is 'Success' and the agent markers are populated, but the M1, M2 and M3 marker values are empty.
5. `fallocate -l 2G /var/tmp/fill; sync; stat -c %s /var/tmp/fill` in a second invocation and re-read `df --output=pcent /`: the df output is still empty, so no rise can be observed.
6. cloudwatch GetMetricStatistics(Namespace=<per-run namespace>, MetricName='mem_used_percent' / 'disk_used_percent', Dimensions=[{InstanceId}], Period=60): no guest-derived datapoint can be correlated with any guest reading.
