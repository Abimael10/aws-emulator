Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__rds__secretsmanager.deep-rds-3-proxy-require-tls-plaintext-rejected lambda__rds__secretsmanager.rds-eventbridge-schedule-invokes-lambda-sdk-positive lambda__rds__secretsmanager.rds-eventbridge-schedule-missing-permission-negative
Issue-Key: lambda__rds__secretsmanager.deep-rds-3-proxy-require-tls-plaintext-rejected|run#13|PublishedUrlRejected|endpoint, port = ctx.endpoint_host(proxy["Endpoint"], _PORT)
Issue-Key: lambda__rds__secretsmanager.rds-eventbridge-schedule-invokes-lambda-sdk-positive|run#14|AssertionError|assert messages, "EventBridge schedule never invoked the Lambda (no message forwarded to SQS)"
Issue-Key: lambda__rds__secretsmanager.rds-eventbridge-schedule-missing-permission-negative|run#16|AssertionError|assert delivered, "no message was forwarded even after granting the invoke permission"
Issue-Commit: ae5156e659812a6269d5e057615ff3791f30822e

# lambda__rds__secretsmanager: DB proxy endpoints are published on rds.amazonaws.com hosts the emulator does not serve, and an EventBridge Scheduler schedule never invokes its Lambda target

> _Note: this issue was identified against the code base at commit `ae5156e659812a6269d5e057615ff3791f30822e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBProxy

## Description
2 net-new root causes.

Cause 1 of 2: a DB proxy's published Endpoint is an AWS-domain hostname that is not an address the emulator answers on.
AWS: DescribeDBProxies returns an Endpoint the client connects to; against real AWS that host resolves and accepts the MySQL/PostgreSQL handshake. An emulator must publish an address it itself serves - the endpoint host it is reached on, a documented own host, or a loopback address - because its customers connect to exactly the string it hands them. Recorded behavior only for the host's shape; what is required is that the published address be reachable.
Emulator: the proxy's endpoint is published as '<proxy-name>.proxy-<hex>.<region>.rds.amazonaws.com:3306', a real-AWS name the emulator does not serve, so every TLS/plaintext login test against the proxy is refused before a byte is sent (PublishedUrlRejected: 'its host is not one of the emulator's own hosts').
Why: no client can use a DB proxy at all - the one field whose entire purpose is to be dialled points somewhere else. RequireTLS enforcement, IAM proxy auth and connection pinning are all unreachable.
Fix: the DB proxy endpoint publisher in the RDS service (the Endpoint member of CreateDBProxy/DescribeDBProxies and of its proxy endpoints) must announce the emulator's own listener.
Traces: lambda__rds__secretsmanager.deep-rds-3-proxy-require-tls-plaintext-rejected.

Cause 2 of 2: an EventBridge Scheduler schedule with a Lambda target never invokes the function.
AWS: a schedule whose target is a Lambda function ARN invokes that function on its schedule once the execution role allows lambda:InvokeFunction; the function's own side effect (here forwarding a message to SQS) then appears. After an initially missing permission is granted, the next firing delivers.
Emulator: no invocation ever happens - the SQS queue the function writes to stays empty both in the positive case and after the invoke permission is granted in the negative case. Nothing is refused; the schedule simply never fires at its target.
Why: a client cannot schedule any Lambda work, and cannot distinguish 'my permissions are wrong' from 'the schedule never ran': both look like an empty queue.
Fix: the scheduler service's schedule firing/target-invocation path for Lambda targets (and the authorization decision it is supposed to make against the schedule's role).
Traces: lambda__rds__secretsmanager.rds-eventbridge-schedule-invokes-lambda-sdk-positive, lambda__rds__secretsmanager.rds-eventbridge-schedule-missing-permission-negative.

## Evidence
Cause 1: PublishedUrlRejected: endpoint_host was given sim2real-pxtls-<hex>.proxy-<hex>.us-west-2.rds.amazonaws.com:3306 (published by the emulator): its host is not one of the emulator's own hosts.
Cause 2: AssertionError: EventBridge schedule never invoked the Lambda (no message forwarded to SQS); AssertionError: no message was forwarded even after granting the invoke permission.

## Steps to Reproduce
Cause 1: CreateDBProxy with RequireTLS, wait for it to become available, read DescribeDBProxies' Endpoint and connect to it.
Cause 2: create a Lambda that writes to an SQS queue, CreateSchedule with that function as the target and a role allowing lambda:InvokeFunction, then poll the queue after the schedule's fire time.

## Resolution

* Cause 1 (FIXED) — a DB proxy of an `EngineFamily` whose database protocol this emulator
  speaks (`MYSQL`, `POSTGRESQL`) now opens a listener of its own through the core when it is
  created, and `DBProxy$Endpoint` publishes the address that listener answers on instead of a
  name under `rds.amazonaws.com`. The listener speaks the family's own wire and a session on
  it is served against the database the proxy's target group holds — the same storage, the
  same master user, the same engine version as that database's own endpoint — refused in the
  clear while `RequireTLS` is on. A proxy of an engine family this emulator speaks no protocol
  for (`SQLSERVER`) gets no listener and keeps the DNS-form endpoint, because a port that
  answers nothing would be worse than a name. The address is withdrawn, and its sessions
  severed, when the proxy is deleted.
* Cause 1, residual (BLOCKED, vendor) — the published address carries the PORT the core
  assigned it (`host:port`), because `DBProxy` models no port member and the core owns the
  socket, the interface and the port: `vendor/server/src/listeners.rs`'s `tcp_plaintext` takes
  no requested port and `bind_std` / `bind_tokio` take it from the operator's
  `EMULATOR_LISTENER_PORT_RANGE`. A client that reads the endpoint as a bare host and dials
  the engine's standard port (3306, 5432) therefore still cannot reach it. Needed in the core:
  a listener API that accepts a requested port, or a per-resource loopback address. The same
  constraint is recorded for `LoadBalancer$DNSName` in
  `issues/things-that-now-work/20261111T030957Z-ec2-elbv2-2-net-new-causes-the-alb.md`.
* Cause 2 (FIXED) — already served on the mainline this fix was branched from: the scheduler's
  clock invokes a Lambda target under the schedule's execution role, and both recorded units
  (the positive one and the one whose invoke permission is granted part-way) replay green.
