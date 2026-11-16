Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__s3.log-edge-producers-alb-https-access-log-records-negotiated-tls-and-sni
Issue-Key: elbv2__s3.log-edge-producers-alb-https-access-log-records-negotiated-tls-and-sni|_poll_until#29|Timeout|time.sleep(interval_s)
Issue-Commit: 04fa8901c0470ed069cb581dfd2246db1b5bced9

# elbv2__s3: an HTTPS ALB writes no access-log record, so the negotiated TLS protocol/cipher and SNI are never recorded

> _Note: this issue was identified against the code base at commit `04fa8901c0470ed069cb581dfd2246db1b5bced9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyLoadBalancerAttributes

## Description
Cause 1 of 1: with access logging enabled on an HTTPS listener, the ALB produces no access-log object at all.
AWS: recorded behavior only — an HTTPS request through an ALB with access_logs.s3.enabled=true appends a gzipped record under <prefix>/AWSLogs/<account>/elasticloadbalancing/<region>/<yyyy/mm/dd>/ whose fields include the request type 'https', the negotiated ssl_protocol and ssl_cipher, and the domain_name (SNI) the client sent.
Emulator: nothing is served or written; the check polls the log bucket and fails with 'Check timed out after 60.0s'.
Why: TLS-layer observability of an ALB is absent — a client cannot audit which protocol/cipher was negotiated or which SNI was presented, and waits out its whole polling budget against an empty prefix.
Fix: the elbv2 data plane in the elbv2 service crate — the HTTPS listener path and the access-log writer that must emit the record (including its TLS and SNI fields).
Traces: elbv2__s3.log-edge-producers-alb-https-access-log-records-negotiated-tls-and-sni

## Evidence
Replay detail: 'Error: Check timed out after 60.0s'. Expected: at least one gzipped access-log object under the configured prefix whose record carries type 'https', the negotiated ssl_protocol/ssl_cipher and the domain_name from SNI. Actual: no object appears within the polling budget.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite. 2. Replay elbv2__s3.log-edge-producers-alb-https-access-log-records-negotiated-tls-and-sni. 3. Directly: create an ALB with an HTTPS listener, enable access_logs.s3.enabled with a granted bucket and prefix, issue an HTTPS request with SNI, then list the prefix.

## Resolution

Partly implemented, partly blocked on the core listener API.

* FIXED — the access-log record of a request an HTTPS listener served now carries the facts of
  the CONNECTION that request arrived over: the protocol version and the cipher suite the
  handshake negotiated (`ssl_protocol`, `ssl_cipher`, spelled as `SslPolicy$SslProtocols` and
  `Cipher$Name` spell them in the catalogue `DescribeSSLPolicies` publishes), the name the
  `ClientHello` asked for (`domain_name`) and the ARN of the certificate presented for that
  name (`chosen_cert_arn`). Each of the four was written as the absent marker before, for
  every request, because the handshake is terminated on the load balancer's own socket and
  the plane that answers the request never saw it; the socket now states what it observed on
  the leg it hands the plane, and the plane takes those values off the request so neither a
  listener's rules nor a target sees a header of the emulator's own. A request that arrived
  in the clear still carries the absent marker in all four, which is what Elastic Load
  Balancing writes for a listener that terminates no TLS. Pinned end to end over a real TLS
  connection to the published address in
  `tests/all/alb_https_access_log_tls.rs`.
* BLOCKED (vendor) — the recorded check dials the load balancer's `DNSName` on the PORT its
  listener declares (443) and is refused, so it never reaches the record above. A load
  balancer can only answer there once a resource listener can be opened on a requested port:
  the core owns the socket, the interface and the port (`vendor/server/src/listeners.rs`,
  `bind_std`/`bind_tokio` and `tcp_plaintext`, which takes no requested port — an OS-assigned
  port or the first free port of `EMULATOR_LISTENER_PORT_RANGE`), and
  `vendor/engine/src/lib.rs` states that a service never binds a socket or chooses a port
  itself. Needed there: a listener API that accepts a requested port, or a per-resource
  loopback address the service can then open the declared ports on — with which
  `crate::state::open_load_balancer_endpoint` would open one listener per `Listener$Port` and
  publish `DNSName` as the bare host the model documents. This is the same core gap recorded
  under Cause 1 of
  `issues/things-that-now-work/20261110T220525Z-elbv2-5-net-new-data-plane-causes-dnsname.md`.

