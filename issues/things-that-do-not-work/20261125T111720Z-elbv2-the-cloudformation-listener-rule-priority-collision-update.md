Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2.deploy-elbv2-2-rule-priority-collision-update-rejected
Issue-Shape-Id: com.amazonaws.elasticloadbalancingv2#ElasticLoadBalancing_v10
Issue-Key: elbv2.deploy-elbv2-2-rule-priority-collision-update-rejected|_poll_stable#42|Timeout|time.sleep(interval_s)
Issue-Commit: fcaef5045051e2f19f2ef72b682687f38c3f2a70

# elbv2: the CloudFormation listener-rule priority-collision update reaches no verdict — the check is cut off at the 60 s harness cap with nothing observed

> _Note: this issue was identified against the code base at commit `fcaef5045051e2f19f2ef72b682687f38c3f2a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetRulePriorities

## Description
1 net-new root cause, grounded in the one surviving failing check of the `elbv2` suite that no open issue carries the replay key of (the OWNED failures of this service are skipped).

Cause 1 of 1: a CloudFormation stack update that moves an `AWS::ElasticLoadBalancingV2::ListenerRule` onto a priority another rule already holds — and the rollback and the follow-up move to a free priority — produce no observable step at all; the check makes no progress and is killed by the harness budget. Failing check: elbv2.deploy-elbv2-2-rule-priority-collision-update-rejected.

AWS: a rule priority is unique per listener. Smithy (com.amazonaws.elasticloadbalancingv2): `CreateRule.Priority` is a required `RulePriority` with @range min 1 / max 50000, and both `CreateRule` and `SetRulePriorities` model `PriorityInUseException` — awsQueryError code `PriorityInUse`, HTTP 400, documented "The specified priority is in use." So moving a rule onto a taken priority is REFUSED, not accepted. Driven through CloudFormation, that refusal makes the rule's UPDATE fail, the stack enters UPDATE_ROLLBACK_COMPLETE, and the failing resource's event carries an `UPDATE_FAILED` ResourceStatusReason classified with CloudFormation's closed HandlerErrorCode vocabulary — `HandlerErrorCode: AlreadyExists` (the `HandlerErrorCode: <Code>` clause is the fixed part; the elbv2 prose embedded in the same reason is not asserted). After the rollback nothing of the failed update may leak: the rule keeps its ARN, its original priority and its original action (the fixed-response body), the sibling rule's priority is undisturbed, and the live listener keeps serving the original body. Moving the SAME rule to a FREE priority then succeeds in place (UPDATE_COMPLETE, same rule ARN, new priority, new served body). Recorded behavior only for the data-plane assertions: the check requires 5 consecutive GETs of the listener to answer the expected status/body before it accepts convergence.

Emulator: `Error: Check timed out after 60.0s`. No status code, no error code, no stack status and no HTTP answer was captured — unlike the other failures in this run there is nothing recorded to compare, because the flow produced none of the values the check waits on. Hypothesis (neither the model nor any captured response in this run states it): the flow stalls before its first assertion in one of the three waits it cannot get past — the CloudFormation stack poll (`describe_stacks` never leaving `*_IN_PROGRESS` for the create, the colliding update's rollback, or the free-priority update) or the listener data-plane poll (the published ALB address never answering the rule's fixed response 5 times in a row). The sibling check of the same group, elbv2.deploy-elbv2-1-rule-update-tg-replace-deletion-protection, stalls the same way and is already carried by open issue 20261123T141857Z-elbv2-4-net-new-causes-an-alb-answers.md (cause 4); that issue does NOT carry this check's key, so this cause is net-new and must be closed on its own.

Why: a client cannot rely on the emulator for the central safety property of a rule-priority change — an invalid change must be refused with PriorityInUse / HandlerErrorCode AlreadyExists and must leave the listener exactly as it was, while a valid change must apply in place on the same rule ARN. Against the emulator the call neither succeeds nor is refused: the client's stack poll and its data-plane poll both hang, which is strictly worse than a wrong answer because no code path (success, rejection, retry) is ever taken.

Fix: whatever blocks convergence of this path — the CloudFormation provisioning/rollback path for `AWS::ElasticLoadBalancingV2::ListenerRule` priority updates (the stack must leave `*_IN_PROGRESS` and reach UPDATE_ROLLBACK_COMPLETE / UPDATE_COMPLETE, and the failed resource event must carry `HandlerErrorCode: AlreadyExists`), the priority-uniqueness validation in the rule handler behind CreateRule/SetRulePriorities (refuse with the modeled `PriorityInUseException`, code `PriorityInUse`, HTTP 400, message "The specified priority is in use."), and the ALB listener readiness/data path that must serve the rule's fixed response at the published load-balancer address. Start by making the check reach its first assertion, since it records nothing today.

Traces: elbv2.deploy-elbv2-2-rule-priority-collision-update-rejected.

## Evidence
Suite run: 523/647 passed (80.8%); service `elbv2` scored 276/323 (85%), 47 failed, of which 46 are OWNED by already-filed issues and this one is not.

[mismatch] elbv2.deploy-elbv2-2-rule-priority-collision-update-rejected
  Error: Check timed out after 60.0s

Expected (AWS): CreateStack reaches CREATE_COMPLETE; GET http://{AlbDns}/a answers 200 with RuleA's body (priority 10 beats the sibling rule's 20). An UpdateStack that moves RuleA from priority 10 to 20 (the sibling's priority) ends UPDATE_ROLLBACK_COMPLETE, with an UPDATE_FAILED event for that rule whose ResourceStatusReason contains `HandlerErrorCode: AlreadyExists`; the elbv2 refusal underneath is HTTP 400 `PriorityInUse` / "The specified priority is in use." After rollback: same RuleA ARN, Priority '10', the original fixed-response MessageBody, sibling Priority '20' untouched, and the listener still serving the original body on 5 consecutive GETs. A second UpdateStack moving RuleA to the free priority 15 ends UPDATE_COMPLETE with the same rule ARN, Priority '15', and the listener serving the new body.

Actual (emulator): none of the above was reached. No stack status, no stack event, no error code and no HTTP response was captured — the harness cut the check off at 60 s with no completed step. The requirement is the status/code/reason vocabulary and the transitions themselves (UPDATE_ROLLBACK_COMPLETE, HandlerErrorCode AlreadyExists, PriorityInUse/400, priorities '10'/'15'/'20'); the stack name, ALB name, rule ARNs, DNS name and response bodies minted by any one run are values, not part of the requirement.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly the check in trace_ids.
1. DescribeAvailabilityZones and pick two available, opt-in-not-required zones.
2. CreateStack with a template that owns its VPC, IGW, route table, two subnets, an SG open on tcp/80, an internet-facing application load balancer on both subnets, an HTTP:80 listener whose default action is a fixed-response 404, plus RuleA (priority 10, path-pattern /a, fixed-response 200 body 'a1') and RuleB (priority 20, path-pattern /a, fixed-response 200 body 'b1'). Poll DescribeStacks until the status leaves *_IN_PROGRESS: expect CREATE_COMPLETE.
   -> Emulator: the flow never gets past this phase within the harness budget; nothing is recorded.
3. GET http://{AlbDns}/a until 5 consecutive answers are 200 'a1'.
4. UpdateStack with RuleA re-rendered at priority 20 (RuleB's priority) and body 'a2'. Poll DescribeStacks: expect UPDATE_ROLLBACK_COMPLETE, and in DescribeStackEvents an UPDATE_FAILED event for RuleA whose ResourceStatusReason contains `HandlerErrorCode: AlreadyExists`.
5. Assert the rollback leaked nothing: same RuleA ARN in the stack outputs, DescribeRules shows exactly RuleA+RuleB besides the default, RuleA Priority '10' with MessageBody 'a1', RuleB Priority '20'; and 5 consecutive GETs of /a still answer 200 'a1'.
6. UpdateStack with RuleA at the free priority 15 and body 'a2': expect UPDATE_COMPLETE, the same RuleA ARN, Priority '15', and 5 consecutive GETs answering 200 'a2'.
