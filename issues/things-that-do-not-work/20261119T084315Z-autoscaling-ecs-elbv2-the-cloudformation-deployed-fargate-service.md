Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.deploy-ecs-task-role-trust-policy-cfn
Issue-Key: autoscaling__ecs__elbv2.deploy-ecs-task-role-trust-policy-cfn|_poll_until#14|Timeout|last = probe()
Issue-Commit: 8465be9c60d4839857b29976b5cf2d08632066fc

# autoscaling__ecs__elbv2: the CloudFormation-deployed Fargate service with an ecs-tasks.amazonaws.com execution role never reaches a RUNNING task with awslogs output — the check exhausts the suite budget with no verdict

> _Note: this issue was identified against the code base at commit `8465be9c60d4839857b29976b5cf2d08632066fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
One net-new root cause. The failing check is not marked OWNED or ALREADY FILED by the replay report. The nearest open issues are 20261111T093414Z-autoscaling-ecs-elbv2-cloudformation-stacks-combining-an-auto.md and 20261115T091925Z-autoscaling-ecs-elbv2-the-cloudformation-deployed-ecs-capacity.md; neither carries this check's replay key, so this is filed separately and names them for context.

Cause 1 of 1 — a CloudFormation stack declaring an IAM role trusting ecs-tasks.amazonaws.com, a log group, an ECS cluster, a FARGATE/awsvpc task definition using that role as ExecutionRoleArn, and an ECS service with DesiredCount=1 never produces an observable running task or container log output.
AWS: recorded behavior only (no model trait describes the convergence). CreateStack reaches CREATE_COMPLETE and publishes the Cluster/Service/LogGroup outputs; ECS then assumes the execution role, so ListTasks(cluster, serviceName) returns at least one task ARN, DescribeTasks reports that task's lastStatus='RUNNING' (not 'STOPPED' — a STOPPED task is exactly how a broken execution-role trust shows up), and the awslogs driver ships container output so FilterLogEvents(logGroupName, logStreamNamePrefix='app') returns at least one event.
Emulator: the check reaches no graded step; the replay report shows only 'Error: Check timed out after 60.0s'. The report does not localize which wait consumed the budget — the check waits, in order, on stack completion (its own cap 400 s), ListTasks >= 1 task (180 s), DescribeTasks lastStatus in {RUNNING, STOPPED} (180 s) and FilterLogEvents >= 1 event (120 s) — but none of them is ever satisfied inside a budget in which the suite's other control-plane calls complete, so the stack's ECS service never advances to a task that an API caller can see, let alone to container logs.
Why: a client cannot verify the single most common ECS deployment: it can never observe that its service placed a task, that the task actually ran (rather than stopping on an execution-role failure), or that the execution role shipped container output to CloudWatch Logs. Every deploy pipeline that waits for a service to become stable hangs, and the negative control — a broken trust policy producing a STOPPED task — is indistinguishable from the emulator's silence.
Fix: ECS service reconciliation (the service-to-task placement loop behind ListTasks/DescribeTasks, including the task lastStatus progression to RUNNING and the execution-role assumption that gates it) and the awslogs log-driver path that writes container output into the declared CloudWatch Logs group, together with the CloudFormation AWS::ECS::Service / AWS::ECS::TaskDefinition handlers that must provision them.
Traces: autoscaling__ecs__elbv2.deploy-ecs-task-role-trust-policy-cfn.

## Evidence
Expected (AWS): CreateStack -> CREATE_COMPLETE with outputs ClusterOut/ServiceOut/LogGroupOut; ListTasks(cluster=<cluster>, serviceName=<service>).taskArns length >= 1; DescribeTasks(...).tasks[0].lastStatus == 'RUNNING'; FilterLogEvents(logGroupName=<log group>, logStreamNamePrefix='app').events length >= 1.

Actual (emulator): no step of that sequence is observed. The replay report carries only 'Error: Check timed out after 60.0s' for the check; no task ARN, no lastStatus and no log event is ever produced within the budget. (The check's own declared waits are 400 s for the stack and 180/180/120 s for the three ECS/Logs polls, i.e. it is designed to tolerate real-AWS latency.)

## Steps to Reproduce
1. Deploy a CloudFormation stack containing: AWS::EC2::SecurityGroup in the default VPC; AWS::IAM::Role whose AssumeRolePolicyDocument allows sts:AssumeRole for Service ecs-tasks.amazonaws.com with ManagedPolicyArns [arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy]; AWS::Logs::LogGroup (RetentionInDays 1); AWS::ECS::Cluster; AWS::ECS::TaskDefinition (RequiresCompatibilities [FARGATE], NetworkMode awsvpc, Cpu 256, Memory 512, ExecutionRoleArn !GetAtt ExecutionRole.Arn, one container 'app' with an awslogs LogConfiguration pointing at the log group and awslogs-stream-prefix 'app'); AWS::ECS::Service (DesiredCount 1, LaunchType FARGATE, awsvpc network configuration in one subnet with the security group). Outputs: cluster, service, log group.
2. Wait for the stack to reach CREATE_COMPLETE and read the outputs.
3. Poll ecs ListTasks(cluster, serviceName) until at least one task ARN appears -> observed: none within the budget.
4. Poll ecs DescribeTasks for lastStatus in {RUNNING, STOPPED} and assert RUNNING -> observed: never reached.
5. Poll logs FilterLogEvents(logGroupName, logStreamNamePrefix='app') for >= 1 event -> observed: never reached.
