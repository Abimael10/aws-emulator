Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.service-name-reuse-while-the-deleted-service-drains-rejected
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.service-name-reuse-while-the-deleted-service-drains-rejected|_poll#7|Timeout|time.sleep(interval_s)
Issue-Commit: e1a20d446b2578340370dda6b22d020378cd7235

# ecs: the draining-name reuse refusal is unobservable — a Fargate service's rollout/DRAINING→INACTIVE lifecycle never settles, so CreateService never refuses a name whose force-deleted service is still DRAINING

> _Note: this issue was identified against the code base at commit `e1a20d446b2578340370dda6b22d020378cd7235`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: ECS does not expose the service-name lifecycle that gates CreateService, so the name-reuse-while-DRAINING refusal cannot be reached at all.

AWS: DeleteService with force=true puts the service into DRAINING while its tasks are still stopping (a container whose PID 1 traps SIGTERM lives out its whole stopTimeout), and only then moves it to INACTIVE. While a service of that name is DRAINING, CreateService with the same serviceName in the same cluster is REFUSED — Smithy: CreateService errors include InvalidParameterException (errorKind client, httpError 400, defaulted); the refusal carries error code `InvalidParameterException` with the message `Unable to Start a service that is still Draining.` — that whole string is fixed text for this error class, it has no variable parts taken from the request. The refused create must create nothing: a following DescribeServices on that name must report no service with status ACTIVE. Once the deleted service reaches INACTIVE the name is free again and CreateService succeeds, returning serviceName = the requested name and status = ACTIVE. Before any of that, a FARGATE/awsvpc service with desiredCount 1 must settle: DescribeServices reports exactly one deployment, that deployment's taskDefinition equal to the registered revision with rolloutState COMPLETED, runningCount 1 and pendingCount 0, and ListTasks(serviceName, desiredStatus=RUNNING) returns exactly one task ARN.

Emulator: the check reaches NO verdict — it is killed with `Check timed out after 60.0s`, so neither the refusal nor the positive control is ever answered. Hypothesis: the first blocking wait is the pre-delete barrier (one deployment with rolloutState COMPLETED, runningCount 1, pendingCount 0 and one RUNNING task); the emulator's service scheduler never advances a Fargate service to that settled state, so the DeleteService→DRAINING window and the CreateService refusal that depends on it are never exercised. The replay output states only the timeout; the emulator produced no status, error code or message for the recreate call.

Why: a client that relies on AWS's name semantics gets the opposite of the real contract. Code that force-deletes a service and immediately recreates it under the same name must handle `InvalidParameterException: Unable to Start a service that is still Draining.` and retry until the old service is INACTIVE; against this emulator that refusal never arrives, so the retry/back-off path is never exercised and a tool that depends on DRAINING→INACTIVE sequencing (blue/green redeploys, teardown-and-recreate scripts) silently takes a different branch here than in AWS. Equally, a service that never reports a COMPLETED rollout with runningCount 1 makes every wait-for-steady-state loop time out.

Fix: the ECS service scheduler/state that drives a service's deployment to rolloutState COMPLETED with runningCount == desiredCount and then the DeleteService lifecycle DRAINING→INACTIVE (service state), plus CreateService's name-collision validation, which must refuse with InvalidParameterException and the fixed message above while a same-named service in the cluster is still DRAINING and admit the name once it is INACTIVE.

Traces: ecs.service-name-reuse-while-the-deleted-service-drains-rejected (mismatch; `Check timed out after 60.0s`). Related open issues describe neighbouring symptoms but the suite does not mark this check as owned by any of them: 20261117T121318Z-ecs-accepts-five-requests-aws-refuses-and-misreports.md (refusal wording/classification) and 20261124T015542Z-ecs-2-net-new-causes-a-draining-ec2.md (a draining EC2 container instance).

## Evidence
Expected (AWS): 1) DescribeServices settles to one deployment, rolloutState=COMPLETED, runningCount=1, pendingCount=0, and ListTasks(desiredStatus=RUNNING) returns 1 task ARN. 2) After DeleteService(force=true), CreateService with the same serviceName fails with HTTP 400, Error.Code=InvalidParameterException, Error.Message="Unable to Start a service that is still Draining." 3) DescribeServices then shows no ACTIVE service for that name. 4) After the service reaches INACTIVE, CreateService with the same name succeeds with serviceName=<the requested name>, status=ACTIVE.

Actual (emulator): no verdict at all — the check is killed with `Check timed out after 60.0s`. The emulator returned no refusal (no status, no error code, no message) for the recreate; the preceding steady-state barrier (one COMPLETED deployment with 1 RUNNING task) is never observed inside the budget.

## Steps to Reproduce
1. CreateCluster with a unique name.
2. RegisterTaskDefinition: family=<cluster token>, requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu=256, memory=512, one container `app` (public busybox image) with command ["sh","-c","trap '' TERM; sleep 3600 & wait"] and stopTimeout=60.
3. CreateService(cluster, serviceName=<tok>-svc, taskDefinition=<that revision>, desiredCount=1, launchType=FARGATE, awsvpcConfiguration with one subnet, one security group, assignPublicIp=ENABLED).
4. Poll DescribeServices until there is exactly one deployment of that revision with rolloutState=COMPLETED, runningCount=1, pendingCount=0, and ListTasks(serviceName, desiredStatus=RUNNING) returns exactly one ARN. (The emulator does not reach this.)
5. DeleteService(cluster, service, force=true).
6. Immediately CreateService again with the SAME serviceName → expect InvalidParameterException "Unable to Start a service that is still Draining."
7. DescribeServices on that name → expect no status ACTIVE.
8. Poll until the deleted service reports status INACTIVE, then CreateService with the same name → expect success with status ACTIVE.

Replay: run the behavioral suite and re-run check ecs.service-name-reuse-while-the-deleted-service-drains-rejected.
