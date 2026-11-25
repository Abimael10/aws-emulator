Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.ecs-service-action-event-triggers-target-lambda
Issue-Key: autoscaling__ecs__elbv2.ecs-service-action-event-triggers-target-lambda|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# autoscaling__ecs__elbv2: an ECS service reaching steady state publishes no 'ECS Service Action' / SERVICE_STEADY_STATE event, so an EventBridge rule never invokes its target Lambda

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: a Fargate ECS service created with desiredCount=1 never produces the "ECS Service Action" event with detail.eventName SERVICE_STEADY_STATE, so an EventBridge rule matching {source:[aws.ecs], detail-type:["ECS Service Action"], detail:{clusterArn:[<cluster>], eventName:["SERVICE_STEADY_STATE"]}} never fires and the target Lambda is never invoked — its log group never gains an invocation record. The check makes no observable step and is cut off at the harness's 60 s cap.
Failing check: autoscaling__ecs__elbv2.ecs-service-action-event-triggers-target-lambda

AWS: recorded behavior only — the ECS and EventBridge Smithy models declare no event-emission rule (ECS: services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/..., EventBridge: services/service-com-amazonaws-eventbridge-awsevents/...). AWS behaviour: when a service's running task count reaches its desired count and the deployment settles, ECS publishes an event to the default event bus with source 'aws.ecs', detail-type 'ECS Service Action', resources naming the service ARN, and a detail object carrying clusterArn and eventName 'SERVICE_STEADY_STATE'. An ENABLED rule on the default bus whose pattern matches that event invokes each configured target; a Lambda target with a resource policy granting lambda:InvokeFunction to principal events.amazonaws.com for that rule ARN is invoked with the event as its payload, so the function's own /aws/lambda/<name> log group gains the output it prints. The pattern's fixed parts are the source, detail-type and eventName tokens quoted above; the cluster and service ARNs and the function name are values taken from the request.

Emulator: nothing is delivered. The check creates the cluster, task definition, Lambda, rule, target and permission, creates the service, then polls /aws/lambda/<fn> for 'SERVICE_STEADY_STATE' for up to 300 s; no observable step completes and the replay reports "Check timed out after 60.0s". The service's task is also never seen to hold RUNNING in this emulator (see the sibling ECS task-lifecycle findings), so the steady-state condition that would produce the event is never reached in the first place, and no 'ECS Service Action' event is published for it.

Why: a client that drives automation off ECS service lifecycle events — notifying on a completed rollout, gating a deploy pipeline on steady state, or paging on SERVICE_TASK_CONFIGURATION_FAILURE — gets silence. The rule and target configure successfully, which is the worst failure mode: the wiring reads as correct while no event ever arrives, so the automation simply never runs and nothing reports an error.

Fix: the ECS service-scheduler event publisher (the component that emits 'ECS Service Action' detail-type events to the default event bus when a deployment settles) together with the ECS service/task convergence that must reach runningCount == desiredCount for steady state to be declared; the EventBridge rule-matching and Lambda target-invocation path must then carry that event to the target.

Traces: autoscaling__ecs__elbv2.ecs-service-action-event-triggers-target-lambda. Related open issues that describe the same missing event class but do NOT carry this check's replay key: 20261125T024526Z-ecs-events-sqs-no-ecs-service-action-event.md and 20261122T172353Z-ecs-events-sqs-2-net-new-causes-ecs.md.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator (519/647 passed, bootstrap_signal false). Service heading `### autoscaling__ecs__elbv2`: 30/35 (86%), 5 failed; this check carries no OWNED / ALREADY FILED marker.

[mismatch] autoscaling__ecs__elbv2.ecs-service-action-event-triggers-target-lambda
  Error: Check timed out after 60.0s

The check (source quoted in the replay report) does: CreateCluster -> RegisterTaskDefinition (awsvpc/FARGATE, container 'app' image public.ecr.aws/amazonlinux/amazonlinux:2023 command ['sleep','3600'], essential) -> CreateFunction -> PutRule(EventPattern={"source":["aws.ecs"],"detail-type":["ECS Service Action"],"detail":{"clusterArn":[<cluster arn>],"eventName":["SERVICE_STEADY_STATE"]}}, State='ENABLED') -> AddPermission(Principal='events.amazonaws.com', SourceArn=<rule arn>) -> PutTargets -> CreateService(desiredCount=1, launchType='FARGATE') -> poll FilterLogEvents on /aws/lambda/<fn> for 'SERVICE_STEADY_STATE' with timeout_s=300.

Expected (AWS): once the service's running count reaches desiredCount, an 'ECS Service Action' event with detail.eventName 'SERVICE_STEADY_STATE' and detail.clusterArn = the cluster ARN is delivered on the default bus, the rule matches, the permitted Lambda target is invoked with that event, and the function's log group contains the line it prints for eventName SERVICE_STEADY_STATE.
Actual (emulator): no such event is delivered and the Lambda is never invoked; no log event ever appears and the check completes no graded step before the 60 s cap. The cluster/function/rule names in the replay are per-run values; the requirement is the event's source, detail-type and eventName and its delivery to a matching rule's target.

Smithy consulted: ECS and EventBridge models declare no event-publication rule for service lifecycle events, so this is recorded behavior only.

## Steps to Reproduce
1. git_clone the repo and replay the behavioral suite (simparity-behavioral-aws-v1); the check in trace_ids reproduces under the `### autoscaling__ecs__elbv2` heading.
2. CreateCluster and RegisterTaskDefinition (networkMode='awsvpc', requiresCompatibilities=['FARGATE'], cpu='256', memory='512', one essential container running a non-terminating command).
3. CreateFunction for a Lambda that prints event['detail']['eventName'], and AddPermission(Action='lambda:InvokeFunction', Principal='events.amazonaws.com', SourceArn=<rule arn>).
4. PutRule(EventPattern={"source":["aws.ecs"],"detail-type":["ECS Service Action"],"detail":{"clusterArn":[<cluster arn>],"eventName":["SERVICE_STEADY_STATE"]}}, State='ENABLED') and PutTargets with the function ARN.
5. CreateService(cluster=<cluster>, desiredCount=1, launchType='FARGATE', awsvpcConfiguration with a subnet, a security group and assignPublicIp='ENABLED').
6. Poll FilterLogEvents on /aws/lambda/<function> for 'SERVICE_STEADY_STATE'.
   Expected: the service reaches steady state, the event is published and matched, and the function's log group contains the SERVICE_STEADY_STATE line.
   Observed: no event, no invocation, no log events; the scenario makes no observable progress.
7. Re-replay the check id in trace_ids to confirm the fix.
