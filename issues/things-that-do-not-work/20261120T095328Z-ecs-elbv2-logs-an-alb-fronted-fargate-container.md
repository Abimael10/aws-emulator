Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
Issue-Key: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target|_poll_until#26|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 2023ab28b75899ce9dcd52680a027d99b0db10e8

# ecs__elbv2__logs: an ALB-fronted Fargate container never answers a non-root path (its /cgi-bin CGI endpoint), so the container-health → task-replacement → target-swap chain is never exercised

> _Note: this issue was identified against the code base at commit `2023ab28b75899ce9dcd52680a027d99b0db10e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the ALB-fronted Fargate task serves `/` but never answers any other path, so the request that is supposed to break the container's own HEALTHCHECK probe never succeeds and the rest of the self-healing scenario is unreachable. Failing check id: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target.

AWS:      recorded behavior only (no Smithy member governs a data-plane HTTP body; ECS container health checks are documented at https://docs.aws.amazon.com/AmazonECS/latest/developerguide/healthcheck.html). The task definition's single essential container runs a shell `command` that writes /www/index.html, writes and chmod +x's an executable CGI script at /www/cgi-bin/break which removes the healthCheck sentinel /tmp/healthy and prints a confirmation line, touches /tmp/healthy, and execs `httpd -f -v -p 80 -h /www`. Through the ALB listener's forward action, a GET of `/` returns HTTP 200 with the container's own body, and a GET of `/cgi-bin/break` ALSO returns HTTP 200, with a body containing the marker-derived confirmation text `<marker>-BREAK-OK` (the fixed part is the literal suffix `-BREAK-OK`; `<marker>` is the per-run marker the request's task definition embedded, never a fixed value). Removing the sentinel then makes the container's `CMD-SHELL test -f /tmp/healthy` probe fail, DescribeTasks reports the container healthStatus UNHEALTHY, the service scheduler stops that task and starts a replacement, the old awsvpc IP leaves the target group while the replacement IP becomes healthy, and HTTP through the ALB answers 200 before and after.

Emulator: the earlier gates now pass — the service reaches one RUNNING task with an awsvpc private IP, DescribeTargetHealth reports that IP healthy, DescribeTasks reports the container healthStatus HEALTHY, and a GET of `/` through the load balancer returns 200 carrying the container's marker body. The very next step, a GET of `/cgi-bin/break` through the SAME load balancer URL, never returns 200 for the whole 180 s poll window: `_poll_until(... 'the /cgi-bin/break endpoint answering at http://<lb-endpoint>/')` exhausts its budget and raises `TimeoutError: the /cgi-bin/break endpoint answering at http://<lb-endpoint>/: the run budget ended first; not graded` (the host:port in that message is per-run and is not part of the requirement). So either the listener/forward path only serves the root path, or the emulated container runtime serves a canned root document instead of running the task definition's `command` (which is what creates and executes the CGI script). Because the probe sentinel is never removed, nothing after it is exercised: no UNHEALTHY healthStatus, no task replacement, no target-set swap, and no post-healing 200.

Why:      a client cannot drive or observe the one self-healing behaviour ECS promises — a container that fails its own HEALTHCHECK is replaced and the load balancer follows the swap. More generally, any application behind an emulated ALB that serves more than one URL path (an API route, a health path other than `/`, a CGI/script endpoint) appears broken: the client gets a non-200/no response for paths the container does serve, with no error explaining it, so request routing and container-driven state changes silently never happen.

Fix:      the ECS task container runtime that must execute the task definition's container `command` (so the files and the executable CGI endpoint that command creates are actually served) together with the ELBv2 listener forward data path that must proxy the full request path — not only `/` — to the registered ip target and return the target's status and body verbatim.

Nearest open issue, which does NOT carry this check's replay key (it was filed when the check died at its FIRST gate, 'service has 1 RUNNING task', a gate that now passes): 20261114T200121Z-ecs-elbv2-logs-a-fargate-service-with-a.md.

Traces:   ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target (suite report, check reported as [error]).

## Evidence
#### [error] ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
  File "check_ecs__elbv2__logs_ecs_container_health_check_failure_...py", line 152, in run
    break_status, break_body = _poll_until(
  File "...py", line 351, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: the /cgi-bin/break endpoint answering at http://<lb-endpoint>/: the run budget ended first; not graded

Expected: through the ALB, GET /cgi-bin/break returns HTTP 200 with a body containing `<marker>-BREAK-OK` (literal suffix `-BREAK-OK`; `<marker>` comes from the request's task definition), exactly as GET / already returns 200 with the container's marker body; the removed sentinel then makes the container healthStatus go UNHEALTHY, the service replaces the task, the old IP leaves the target group, the replacement IP becomes healthy, and GET / still answers 200.

Actual: GET / answers 200 with the container's body (so the target is registered, healthy and reachable), but GET /cgi-bin/break never answers 200 within the 180 s poll; the check is cut off there and every later assertion (UNHEALTHY healthStatus, replacement task arn, target-set swap, post-healing 200, replacement task's log line) is never graded. Only the per-run load-balancer host:port and the per-run marker differ between runs; they are not part of the requirement.

## Steps to Reproduce
1. In a public VPC with two subnets, create an ALB security group (tcp/80 from the client CIDRs) and a task security group (tcp/80 from the ALB security group); create a CloudWatch log group and an ECS task execution role allowing logs:CreateLogStream / logs:PutLogEvents.
2. ecs CreateCluster; RegisterTaskDefinition with one essential container (public busybox image, containerPort 80) whose `command` is `sh -c` of: write /www/index.html containing a marker, write an executable /www/cgi-bin/break CGI script that does `rm -f /tmp/healthy` and echoes `<marker>-BREAK-OK`, `touch /tmp/healthy`, echo a `<marker> httpd-start` line, then `exec httpd -f -v -p 80 -h /www`; healthCheck {command ['CMD-SHELL','test -f /tmp/healthy'], interval 5, timeout 2, retries 2, startPeriod 30}; awslogs logConfiguration to the log group.
3. elbv2 CreateTargetGroup (TargetType ip) + CreateLoadBalancer (the two subnets, ALB security group) + CreateListener on port 80 forwarding to the group.
4. ecs CreateService (FARGATE, desiredCount 1, awsvpc with the two subnets and the task security group, loadBalancers wired to the target group, healthCheckGracePeriodSeconds 10, deploymentConfiguration {minimumHealthyPercent 0, maximumPercent 200}).
5. Wait for one RUNNING task with an awsvpc private IP, for that IP to be `healthy` in DescribeTargetHealth, and for DescribeTasks to report the container healthStatus HEALTHY; GET `http://<DNSName>/` and observe 200 with the marker body.
6. GET `http://<DNSName>/cgi-bin/break` and observe it never returns 200 with `<marker>-BREAK-OK` — so the sentinel is never removed, the container never goes UNHEALTHY, and the task replacement plus ALB target swap never happen.
