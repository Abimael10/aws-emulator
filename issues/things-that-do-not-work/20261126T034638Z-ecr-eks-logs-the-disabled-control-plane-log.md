Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__eks__logs.eks-disabled-log-type-produces-no-log-group
Issue-Key: ecr__eks__logs.eks-disabled-log-type-produces-no-log-group|_poll_log_group_present#8|Timeout|time.sleep(interval_s)
Issue-Commit: 857f59baf94ca876ddeb888a415f04d0868c32ab

# ecr__eks__logs: the disabled-control-plane-log-type differential reaches no verdict — neither the cluster's convergence nor the /aws/eks/<cluster>/cluster log group created by enabling 'api' is observable

> _Note: this issue was identified against the code base at commit `857f59baf94ca876ddeb888a415f04d0868c32ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateClusterConfig

## Description
Cause 1 of 1: the negative-delivery differential for EKS control-plane logging makes no observable progress — the check is cut at the 60 s replay cap before any of its steps (cluster reaching ACTIVE, the absence of the log group while every log type is disabled, and the creation of /aws/eks/<cluster>/cluster after enabling 'api') can be graded.

AWS: eks#CreateCluster with logging.clusterLogging = [{types: [api, audit, authenticator, controllerManager, scheduler], enabled: false}] creates a cluster that eventually reports status 'ACTIVE' from eks#DescribeCluster, and because no log type is enabled the control-plane log group '/aws/eks/<cluster-name>/cluster' is never created — logs#DescribeLogGroups(logGroupNamePrefix='/aws/eks/<cluster-name>/cluster') keeps returning no matching group. eks#UpdateClusterConfig(name, logging={clusterLogging:[{types:['api'],enabled:true}]}) returns an Update whose id reaches status 'Successful' via eks#DescribeUpdate, and that flip DOES create the '/aws/eks/<cluster-name>/cluster' log group (Smithy: eks#Logging.clusterLogging -> LogSetup{types: LogTypes, enabled}; LogType enum = api|audit|authenticator|controllerManager|scheduler; LogSetup.enabled doc: 'If a log type is enabled, that log type exports its control plane logs to CloudWatch Logs. If a log type isn't enabled, that log type doesn't export its control plane logs.'). The group name and the asynchronous settling are recorded behavior only; '<cluster-name>' is the only variable part of the group name.

Emulator: nothing settles. The replay output for the check is exactly `Error: Check timed out after 60.0s`, so no step reached a verdict. Hypothesis: the first blocked step cannot be identified from the recording — it is either DescribeCluster never reporting ACTIVE or, past that, the enable-'api' path never creating the log group; sibling checks in this same service that ARE filed report precisely 'no /aws/eks/<cluster>/cluster log group after enabling api logging', which makes the logging seam the likely stall.

Why: a client cannot tell enabled control-plane logging from disabled control-plane logging — the destination log group is never created when a type is enabled, so neither an audit pipeline nor a negative-control assertion ('no logs leak while logging is off') can ever be satisfied, and a caller that waits for the cluster or for the UpdateClusterConfig update to settle waits forever.

Fix: the EKS cluster state machine that must settle CreateCluster to ACTIVE and UpdateClusterConfig updates to 'Successful', and the control-plane-logging effect that must create and delete the /aws/eks/<cluster>/cluster log group in CloudWatch Logs as log types are enabled and disabled.

Similar open issues exist but do NOT cover this check: 20261120T212047Z-ecr-eks-logs-3-net-new-root-causes.md, 20261120T230157Z-ecr-eks-logs-3-net-new-root-causes-52a2b7.md and 20261120T231430Z-ecr-eks-logs-3-net-new-root-causes-8f8043.md all describe a missing /aws/eks/<cluster>/cluster log group after enabling 'api' logging, and 20261029T175608Z-eks-3-net-new-root-causes-startinsightsrefresh-response.md describes asynchronous EKS state that never settles; the replay did not mark this check as owned by any of them.

Traces: the failing check below is the example.

## Evidence
Check ecr__eks__logs.eks-disabled-log-type-produces-no-log-group, verdict mismatch.

Expected (AWS): DescribeCluster(name) eventually reports status 'ACTIVE'; while every LogType is disabled, DescribeLogGroups(logGroupNamePrefix='/aws/eks/<cluster>/cluster') never lists that group; UpdateClusterConfig(logging={clusterLogging:[{types:['api'],enabled:true}]}) returns an Update whose DescribeUpdate status becomes 'Successful', after which DescribeLogGroups does list '/aws/eks/<cluster>/cluster'.

Actual (emulator): no step is observable; the replay output for the check is exactly `Error: Check timed out after 60.0s`, so the cluster/update convergence and the log-group effect are both ungraded. (Caveat recorded for the fixer: the recording shows only the cap being hit, not which call stalled.)

## Steps to Reproduce
1. ec2:CreateVpc 10.42.0.0/16 with DNS support/hostnames and two subnets in different AZs.
2. iam:CreateRole trusted by eks.amazonaws.com with AmazonEKSClusterPolicy.
3. eks:CreateCluster(name=<name>, roleArn=<role>, resourcesVpcConfig={subnetIds:[...]}, logging={clusterLogging:[{types:['api','audit','authenticator','controllerManager','scheduler'],enabled:false}]}).
4. Poll eks:DescribeCluster until status is ACTIVE; then poll logs:DescribeLogGroups(logGroupNamePrefix='/aws/eks/<name>/cluster') and confirm the group never appears.
5. eks:UpdateClusterConfig(name=<name>, logging={clusterLogging:[{types:['api'],enabled:true}]}); poll eks:DescribeUpdate until status 'Successful'; then poll logs:DescribeLogGroups for '/aws/eks/<name>/cluster'.
6. Observe that the sequence never progresses to a verdict.
