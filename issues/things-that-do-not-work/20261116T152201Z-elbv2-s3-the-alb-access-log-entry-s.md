Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields
Issue-Key: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields|run#60|AssertionError|assert ":" in client_field and client_field.rsplit(":", 1)[1].isdigit(), (
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# elbv2__s3: the ALB access-log entry's `client:port` field is emitted as '-' instead of the client's address and source port

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyLoadBalancerAttributes

## Description
Cause 1 of 1: an ALB access-log record delivered to S3 carries '-' in the `client:port` field (field 4, zero-based index 3) instead of the requesting client's IP address and source port. Failing check: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields.

AWS: access logging is a modeled load-balancer feature — Smithy: ModifyLoadBalancerAttributes / LoadBalancerAttribute.Key documents `access_logs.s3.enabled`, `access_logs.s3.bucket` and `access_logs.s3.prefix` as 'supported by both Application Load Balancers and Network Load Balancers'. The layout of the entry itself is not in the Smithy model, so for the field contents this is AWS's documented ALB access-log format plus recorded behavior only: every ALB access-log entry of `type` http/https carries, as its 4th space-separated field, `client:port` — the IP address and the source port of the requesting client, formatted `<address>:<port>` with a decimal port. Only fields that genuinely have no value for that request are written as the single-character placeholder '-' (e.g. `ssl_cipher`/`ssl_protocol` on a plain-HTTP listener, or `target:port` when the target is a Lambda function). `client:port` is never '-' for a request the load balancer accepted and logged: the load balancer accepted the TCP connection, so it always knows the peer address and port. The recording in this check shows the entry's other positional fields populated (`type`='http', `elb`=the load balancer id, `elb_status_code`/`target_status_code`='200', `target_group_arn`=this run's target group ARN), i.e. the line is written, delivered under the documented `<prefix>/AWSLogs/<account>/elasticloadbalancing/<region>/<yyyy>/<mm>/<dd>/` key layout, and only the client field is hollow.

Emulator: the delivered entry's 4th field is the literal '-'. The check asserts `':' in client_field and client_field.rsplit(':', 1)[1].isdigit()` and fails with 'the entry's `client:port` field is '-', expected <address>:<port>'. The preceding assertions on the same line pass, so the divergence is isolated to this one field: the access-log formatter is substituting the no-value placeholder where it should render the accepted connection's peer address and port. Note this is not one of the already-open 'no access-log record is ever produced' elbv2__s3/cloudwatch__elbv2__s3 issues (e.g. 20261110T210333Z-elbv2-s3-an-alb-s-https-listener-refuses.md, 20261111T030957Z-elbv2-s3-the-alb-answers-http-502-for.md, 20261111T112528Z-cloudwatch-elbv2-s3-enabling-alb-access-logs-writes-88b6cd.md) — here the record IS delivered and parses, and the defect is the value of a populated field.

Why: a client that relies on AWS's behavior reads the source of each request out of the access log — that is what the field exists for. With '-' there, every consumer that attributes traffic to a client address breaks: source-IP forensics and abuse investigation, per-client rate/volume accounting, geo and bot analytics, Athena/Glue tables and log-parsing pipelines whose schema types that field as `<address>:<port>` (a split on ':' yields no port, and a parser that casts the port to an integer raises). Worse, it is silent: the log is present, well-formed and complete-looking, so the client concludes no client address was observable rather than that the emulator dropped it.

Fix: the ALB data-plane access-log record builder in the elbv2 service crate — the component that formats the space-separated access-log entry for each served request and writes the gzipped object to the bucket named by `access_logs.s3.bucket` — must populate the `client:port` field from the peer address and peer port of the client connection the listener accepted, rendering it `<address>:<port>`, and reserve the '-' placeholder for fields that genuinely have no value for that request (such as `ssl_cipher`/`ssl_protocol` on an HTTP listener and `target:port` for a Lambda target). The port must be the client's ephemeral source port on the accepted socket, not the listener port.

Traces: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields (the only failing check in this cluster; the other elbv2__s3 failures in this run are owned by open issues).

## Evidence
Failing check: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields (verdict: mismatch).

Replay output:
  File "check_elbv2__s3_alb_access_log_lines_are_delivered_to_s3_with_the_documented_fields.py", line 651, in run
    assert ":" in client_field and client_field.rsplit(":", 1)[1].isdigit(), (
  AssertionError: the entry's `client:port` field is '-', expected <address>:<port>

Expected (AWS): field 4 of the access-log entry is `<address>:<port>` — the requesting client's IP address and its source port, e.g. the address the listener accepted the connection from followed by ':' and a decimal port. Fixed text of the field: none; both parts are values taken from the accepted connection (the client address and the client's source port). The '-' placeholder is correct only for fields with no value for that request.

Actual (emulator): field 4 is the single character '-'.

Same entry, assertions that PASSED (so the record is delivered and parses):
  - object key under `<prefix>/AWSLogs/<account>/elasticloadbalancing/<region>/<yyyy>/<mm>/<dd>/`
  - >= 29 space-separated fields
  - fields[0] `type` == 'http'
  - fields[2] `elb` == this load balancer's id

Model grounding: Smithy (elasticloadbalancingv2) ModifyLoadBalancerAttributes / LoadBalancerAttribute.Key documents `access_logs.s3.enabled`, `access_logs.s3.bucket`, `access_logs.s3.prefix`; the per-field layout of the log line is documented by AWS prose, not by the model (recorded behavior only for the field contents).

## Steps to Reproduce
1. Create a VPC with subnets and an internet-facing Application Load Balancer with an HTTP listener and a target group (the check uses a Lambda target group).
2. Create an S3 bucket with a bucket policy granting logdelivery.elasticloadbalancing.amazonaws.com write access, then call ModifyLoadBalancerAttributes on the load balancer with access_logs.s3.enabled=true, access_logs.s3.bucket=<bucket>, access_logs.s3.prefix=<prefix>.
3. Send a batch of plain HTTP GET requests to the load balancer's listener so each is answered 200 by the target.
4. Poll the bucket until access-log objects appear under <prefix>/AWSLogs/<account>/elasticloadbalancing/<region>/<yyyy>/<mm>/<dd>/, gunzip them and split each line on spaces.
5. Locate the entry for one of the requests just sent and read field index 3 (`client:port`).
   Expected: `<client-address>:<client-source-port>`.
   Observed: '-'.

Replay to confirm a fix: elbv2__s3.alb-access-log-lines-are-delivered-to-s3-with-the-documented-fields
