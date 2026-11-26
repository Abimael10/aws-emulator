Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.termination-protection-blocks-terminate-job-flows
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.termination-protection-blocks-terminate-job-flows|_held#12|Timeout|time.sleep(interval_s)
Issue-Commit: 003b19ea8201179ec46ed1bcee5f22a6bf4a799a

# emr: the termination-protection TerminateJobFlows scenario reaches no verdict — the protected-terminate refusal, the absent-effect window and the cleared-protection control are all cut off at the suite budget

> _Note: this issue was identified against the code base at commit `003b19ea8201179ec46ed1bcee5f22a6bf4a799a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TerminateJobFlows

## Description
One net-new root cause (the only failing emr check in this replay that is neither marked ALREADY FILED nor OWNED by an open issue).

Cause 1 of 1: the cluster-lifecycle scenario that grades termination protection against TerminateJobFlows makes no observable progress and is killed at the suite's 60 s budget, so not one of its assertions is ever answered — the refusal's error code and message, the survival of the flag across a refused terminate, the absence of a USER_REQUEST state change, and the control termination after protection is cleared.

AWS: a cluster whose Instances.TerminationProtected is true cannot be shut down by TerminateJobFlows. The call is refused with one of EMR's cluster-validation error codes (ValidationException / InvalidRequestException) and the message "Could not shut down one or more job flows since they are termination protected." — that sentence is entirely fixed text: it is plural and id-free, nothing from the request or the state is interpolated into it. Every named cluster keeps its current state (a WAITING cluster stays WAITING), the flag stays true, and no StateChangeReason with Code USER_REQUEST is recorded for the refused attempt; DescribeCluster keeps reporting Cluster.TerminationProtected=true. The documented repair is SetTerminationProtection(TerminationProtected=false) and then the IDENTICAL TerminateJobFlows, which must succeed, drive the cluster to TERMINATED and report Status.StateChangeReason {Code: "USER_REQUEST", Message: "Terminated by user request"}. Smithy (services/service-com-amazonaws-emr-elasticmapreduce/smithy-model/com-amazonaws-emr-elasticmapreduce.json): TerminateJobFlows takes JobFlowIds (required) and models InternalServerError only; SetTerminationProtection takes JobFlowIds + TerminationProtected (both required) and models InternalServerError only; DescribeCluster returns Cluster and models InvalidRequestException/InternalServerException. So the ValidationException code, the two sentences above and the protection semantics are recorded behavior only, plus SetTerminationProtection's own doc ("To terminate a cluster that has been locked ... you must first unlock the job flow by a subsequent call to SetTerminationProtection in which you set the value to false").

Emulator: the check never returns a verdict — the replay reports only "Error: Check timed out after 60.0s" for emr.termination-protection-blocks-terminate-job-flows, with no assertion message. Its blocking waits, in order, are: the private-network/IAM prerequisites, RunJobFlow then a poll for the cluster to leave STARTING/BOOTSTRAPPING (interval 20 s, 1500 s ceiling), a poll for DescribeCluster to report TerminationProtected=true (interval 5 s, 120 s ceiling), the refused TerminateJobFlows, an effect-ABSENT window that must observe the cluster still WAITING for a continuous 180 s (interval 15 s), then the cleared-protection control terminate with a poll to a terminal state (interval 15 s, 1200 s ceiling). Hypothesis: the 180 s absent-effect window (the only way to prove the protected cluster did NOT go terminal) cannot complete inside the 60 s this replay allowed the check, and the polls in front of it consume part of that budget, so the scenario is cut off before it reaches its post-refusal reads and its control termination. Nothing in the replay output states which call the budget expired in, so everything after the setup poll is ungraded.

Why: termination protection is the guard a client puts on a long-running cluster precisely so a stray or scripted TerminateJobFlows cannot destroy it. Because this scenario produces no verdict, the emulator carries no evidence at all that a protected terminate is refused with AWS's code and sentence, that the refusal leaves TerminationProtected and the cluster's state and StateChangeReason untouched, or that the documented clear-then-terminate repair reaches TERMINATED with USER_REQUEST / "Terminated by user request". A client that catches the refusal, clears protection and retries is running against untested behavior, and any divergence behind the cut-off point stays hidden.

Fix: the emr service's cluster-lifecycle clock and the TerminateJobFlows / SetTerminationProtection / DescribeCluster handlers that this scenario drives — the protection decision on the terminate path, the protection flag's round-trip through the cluster read, and the state/StateChangeReason a refused versus an accepted shutdown leaves — must converge quickly enough, and hold the protected cluster steady, for the scenario to run to its assertions instead of expiring mid-wait.

Traces: emr.termination-protection-blocks-terminate-job-flows. Related but NOT covering: open issue 20261105T083215Z-ec2-emr-iam-emr-terminatejobflows-ignores-termination-protection-a-protected-multi.md states the same TerminateJobFlows protection cause, but it carries only the ec2__emr__iam.e2e-create-emr-cluster-multi-master-lifecycle replay key and the suite does not mark this check as owned by it; open issue 20261102T083234Z-elasticmapreduce-emr-cluster-and-instance-group-reads-omit-modeled.md is a wire-corpus issue about DescribeCluster's members and the same flag. Neither names this check, so this scenario's assertions are unaccounted for.

## Evidence
Failing check: [mismatch] emr.termination-protection-blocks-terminate-job-flows
Detail reported by the replay, in full: "Error: Check timed out after 60.0s" — no assertion was reached, no AWS/emulator value pair was produced.

Expected (AWS): the scenario completes end to end — RunJobFlow(KeepJobFlowAliveWhenNoSteps=true, one MASTER m5.xlarge group, release emr-6.15.0) reaches WAITING; SetTerminationProtection(TerminationProtected=true) round-trips so DescribeCluster reports Cluster.TerminationProtected=true; TerminateJobFlows(JobFlowIds=[<cluster id>]) is refused with Code in {ValidationException, InvalidRequestException} and Message exactly "Could not shut down one or more job flows since they are termination protected." (fixed text, no request- or state-derived parts); for the next 180 s DescribeCluster keeps reporting State=WAITING with TerminationProtected still true and no Status.StateChangeReason whose Code is USER_REQUEST; after SetTerminationProtection(TerminationProtected=false), the identical TerminateJobFlows succeeds, the cluster reaches State=TERMINATED and Status.StateChangeReason is {Code: "USER_REQUEST", Message: "Terminated by user request"}.

Actual (emulator): no verdict. The check is terminated by the suite after 60.0 s while still inside its waits, so none of the above was observed either way. The suite reports it as a mismatch for the emr service (### emr), and the check is marked neither ALREADY FILED nor OWNED by any open issue.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the simparity-behavioral-aws-v1 suite; check emr.termination-protection-blocks-terminate-job-flows ends "Check timed out after 60.0s".
2. By hand: create the EMR service role (trust elasticmapreduce.amazonaws.com + AmazonElasticMapReduceRole) and an EC2 instance profile/role (trust ec2.amazonaws.com + AmazonElasticMapReduceforEC2Role), and a VPC 10.0.0.0/16 with subnet 10.0.0.0/24 and an S3 gateway endpoint.
3. RunJobFlow: ReleaseLabel emr-6.15.0, one MASTER m5.xlarge ON_DEMAND group, Instances.KeepJobFlowAliveWhenNoSteps=true, Instances.TerminationProtected=false, Applications=[Hadoop], the roles from step 2, AutoTerminationPolicy.IdleTimeout=60. Poll DescribeCluster until State=WAITING.
4. SetTerminationProtection(JobFlowIds=[<cluster id>], TerminationProtected=true) and poll DescribeCluster until Cluster.TerminationProtected is true.
5. TerminateJobFlows(JobFlowIds=[<cluster id>]) — expect the refusal code and the fixed sentence above.
6. For 180 s, poll DescribeCluster every 15 s — expect State=WAITING throughout, TerminationProtected still true, and no StateChangeReason with Code USER_REQUEST.
7. SetTerminationProtection(..., TerminationProtected=false), then the identical TerminateJobFlows — expect TERMINATED with StateChangeReason {USER_REQUEST, "Terminated by user request"}.
Observed: the replayed scenario never gets far enough to answer steps 5-7 and is cut off at the 60 s suite budget.
