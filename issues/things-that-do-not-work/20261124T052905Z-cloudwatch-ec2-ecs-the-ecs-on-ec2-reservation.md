Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100
Issue-Key: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100|_poll_until#51|Timeout|time.sleep(interval_s)
Issue-Commit: ee29b9ed80499ffda1e629bc10e5c9c45fea7a64

# cloudwatch__ec2__ecs: the ECS-on-EC2 reservation-metric scenario reaches no verdict — AWS/ECS CPUReservation / MemoryReservation and AWS/ECS/ManagedScaling CapacityProviderReservation never arrive inside the budget

> _Note: this issue was identified against the code base at commit `ee29b9ed80499ffda1e629bc10e5c9c45fea7a64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: on an EC2 capacity-provider cluster with one placed task, the per-minute ECS reservation metric series never appear in CloudWatch, so the check polls to the harness cap and no step of the scenario is verdicted. Failing check: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100 (reported this run as 'Check timed out after 600.0s').

AWS: recorded behavior only — the Smithy model describes GetMetricStatistics but not which metrics ECS publishes. AWS/ECS publishes CPUReservation and MemoryReservation for a cluster every 60 s with Unit 'Percent', equal to the placed tasks' reserved share of the container instance's registeredResources (for a task reserving cpu 256 / memory 256 on an instance whose registeredResources report CPU and MEMORY, the datapoint is 256*100/registered, to float precision, in both Minimum and Maximum); AWS/ECS/ManagedScaling publishes CapacityProviderReservation with dimensions {CapacityProviderName, ClusterName}, reading exactly 100.0 while one instance hosts the only task and M/N*100 (200.0 for one unplaceable task beside one full instance) once a task cannot be placed; and a PROVISIONING task is never counted into CPUReservation/MemoryReservation.

Emulator: the check's polls for the first AWS/ECS MemoryReservation datapoint at or after the steady-state minute (20 s interval, 600 s budget each, then the CapacityProviderReservation polls) never satisfy, and the check is cut off at the 600 s harness cap with no assertion reached. Hypothesis: neither the AWS/ECS cluster reservation series nor the AWS/ECS/ManagedScaling CapacityProviderReservation series is ever published, so every poll reads an empty datapoint list; the emulator returns no error, it returns nothing.

Why: a client cannot observe ECS capacity pressure at all — reservation dashboards stay empty, and the CapacityProviderReservation series that AWS's own managed-scaling target-tracking policy consumes has no data, so managed scaling can never act on pressure. A caller waiting for these metrics (as every autoscaling consumer does) hangs instead of being told the metric does not exist.

Fix: the ECS metric-emission path (cluster CPUReservation/MemoryReservation from registeredResources vs placed task reservations, and the managed-scaling CapacityProviderReservation series) and its publication into the CloudWatch metric store that GetMetricStatistics reads.

Open issue 20261120T071621Z-cloudwatch-ec2-ecs-2-net-new-causes-an.md names this same check, but under a different failure key — it recorded the check failing at its task-A 'lastStatus == RUNNING' assertion (stopCode EssentialContainerExited). That assertion is now passed; the check gets as far as the metric polls and times out there, which is the cause filed here.

Traces: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100.

## Evidence
Check: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100.
Suite output: 'Error: Check timed out after 600.0s' (verdict: mismatch) — no AssertionError, i.e. the check never reached a comparison.
The blocking reads are:
  cw.get_metric_statistics(Namespace='AWS/ECS', MetricName='MemoryReservation', Dimensions=[{ClusterName}], Period=60, Statistics=['Minimum','Maximum','Average'])  -> polled every 20 s for up to 600 s, requires >= 1 datapoint at/after the steady minute
  same for MetricName='CPUReservation'
  cw.get_metric_statistics(Namespace='AWS/ECS/ManagedScaling', MetricName='CapacityProviderReservation', Dimensions=[{CapacityProviderName},{ClusterName}], Period=60)
Expected (AWS): MemoryReservation/CPUReservation Minimum and Maximum == task reservation * 100 / registeredResources value (within 0.05 pp), Unit 'Percent'; CapacityProviderReservation Minimum == Maximum == 100.0 with one instance and one placed task, then a datapoint with Maximum == 200.0 after an unplaceable task is submitted; no AWS/ECS reservation minute exceeding the placed task's share.
Actual (emulator): no datapoint for any of these series within the check's budget; the check is terminated by the 600 s cap.

## Steps to Reproduce
1. Create a cluster, a launch template from the ECS-optimized AMI and an ASG; create an ASG capacity provider with managedScaling ENABLED and associate it with the cluster.
2. RegisterTaskDefinition (EC2/bridge, cpu 256, memory 256, one essential long-running container) and RunTask through the capacity provider; wait for the container instance to register ACTIVE and the task to reach RUNNING.
3. Call GetMetricStatistics for Namespace 'AWS/ECS', MetricName 'MemoryReservation' and 'CPUReservation' with Dimensions [{Name:'ClusterName',Value:<cluster>}], Period 60, Statistics ['Minimum','Maximum','Average'] over the minutes since the task started. AWS returns per-minute datapoints with Unit 'Percent' equal to 256*100/registeredResources; the emulator returns none.
4. Call GetMetricStatistics for Namespace 'AWS/ECS/ManagedScaling', MetricName 'CapacityProviderReservation', Dimensions [{CapacityProviderName},{ClusterName}] — AWS returns 100.0, and 200.0 after a second task that cannot be placed is submitted; the emulator returns none.
