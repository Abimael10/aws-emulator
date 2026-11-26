Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ses.smtp-accepts-both-advertised-auth-mechanisms ses.smtp-auth-before-starttls-is-rejected ses.smtp-derived-credential-authenticates-and-a-wrong-region-derivation-is-rejected ses.smtp-listener-advertises-starttls-then-withdraws-it-after-upgrade ses.smtp-rejects-a-tampered-sigv4-derived-password ses.smtp-send-from-unverified-sender-rejected-at-the-data-stage ses.account-sending-pause-outranks-the-identity-gate-on-both-front-doors
Issue-Shape-Id: com.amazonaws.ses#SimpleEmailService
Issue-Key: ses.smtp-accepts-both-advertised-auth-mechanisms|_advertised_mechanisms#2|ConnectionRefusedError|session = smtplib.SMTP(*ctx.service_host(_smtp_host(ctx.region), _SMTP_PORT), timeout=30)
Issue-Key: ses.smtp-auth-before-starttls-is-rejected|_authenticated_session#3|ConnectionRefusedError|session = smtplib.SMTP(host, port, timeout=30)
Issue-Key: ses.smtp-derived-credential-authenticates-and-a-wrong-region-derivation-is-rejected|_greeting#16|ConnectionRefusedError|socket.create_connection((host, port), timeout=timeout), server_hostname=host
Issue-Key: ses.smtp-listener-advertises-starttls-then-withdraws-it-after-upgrade|run#2|ConnectionRefusedError|session = smtplib.SMTP(host, port, timeout=30)
Issue-Key: ses.smtp-rejects-a-tampered-sigv4-derived-password|_upgraded_session#2|ConnectionRefusedError|session = smtplib.SMTP(*ctx.service_host(_smtp_host(ctx.region), _SMTP_PORT), timeout=30)
Issue-Key: ses.smtp-send-from-unverified-sender-rejected-at-the-data-stage|_authenticated_session#8|ConnectionRefusedError|session = smtplib.SMTP(host, port, timeout=30)
Issue-Key: ses.account-sending-pause-outranks-the-identity-gate-on-both-front-doors|_authed#22|ConnectionRefusedError|smtp = smtplib.SMTP_SSL(
Issue-Commit: 2b493b29800d7a854b42f905b4db8bab42375d7a

# ses: no SMTP front door is served at all (TCP refused on 465 and 587), and the core cannot bind a protocol's own service port

> _Split out of `issues/things-that-now-work/20261118T045941Z-ses-3-net-new-causes-no-smtp-front.md`, whose causes 2 and 3 (the v1 configuration-set-name charset refusal and the v1 reputation-metrics unknown-set refusal) are fixed. The cause below is NOT fixed; it keeps the original report's trace ids and Issue-Keys so it stays filed and replayable. It was identified against the code base at commit `2b493b29800d7a854b42f905b4db8bab42375d7a`, so trace it back to the current code base._

## Operation
The SES SMTP interface (EHLO capability set, STARTTLS upgrade, SigV4-derived credential AUTH, DATA-stage sender gate)

## Description
The SES SMTP front door is never served — every TCP connect to the SMTP endpoint is refused, so the
whole SMTP interface is unreachable.

AWS: recorded behavior only — the SES Smithy model (com.amazonaws.ses#SimpleEmailService) models only
the HTTPS awsQuery API and says nothing about SMTP. SES additionally serves an SMTP endpoint for the
Region (email-smtp.<region>.amazonaws.com) on the documented SMTP ports: 587 as a plaintext session
that advertises STARTTLS, AUTH PLAIN LOGIN and 8BITMIME in its EHLO reply and, after a TLS 1.2+
upgrade, advertises the same capability set with STARTTLS withdrawn; and 465 as an implicit-TLS
(RFC 8314 "smtps") session carrying the identical credential algebra. On that listener AUTH succeeds
for a password derived by the documented SMTP-credential formula
(https://docs.aws.amazon.com/ses/latest/dg/smtp-credentials.html — version byte 0x04 prepended to an
HMAC-SHA256 SigV4 chain keyed on "AWS4"+secret over the fixed date 11111111, then region, "ses",
"aws4_request", "SendRawEmail", base64-encoded) and is refused for a password derived for the wrong
region or tampered with; AUTH attempted before STARTTLS on 587 is refused; and a MAIL FROM/DATA
submission from an unverified sender is refused at the DATA stage with the same not-verified refusal
the API answers (fixed text "Message rejected: Email address is not verified." plus the per-request
identity detail), while the account-level sending pause outranks that identity gate on both the API
and the SMTP front door.

Emulator: nothing is listening. Every one of these checks dies in socket.create_connection with
`ConnectionRefusedError: [Errno 111] Connection refused` before a single SMTP verb is exchanged — on
port 465 and on port 587 alike.

Blocked by (as of the commit above), on two core facts:

* The core assigns the port of every listener a service opens. `vendor/server/src/listeners.rs`
  offers `Listeners::tcp` (TLS, line 204), `Listeners::tcp_plaintext` (the in-band-upgrade opener a
  STARTTLS front door needs, line 230) and `Listeners::http` (line 381); none takes a requested
  port, and the binds behind them (`bind_std` / `bind_tokio`, lines 67-107) use port 0 or walk the
  operator's `EMULATOR_LISTENER_PORT_RANGE`. A service crate may not bind a socket of its own
  (`vendor/engine/src/lib.rs:27`: "The service never binds a socket, chooses a port, or configures
  TLS itself — the core owns the socket, TLS and bind interface"), and `vendor/` is read-only from a
  service fix. The SMTP ports are the PROTOCOL's own (465, 587), not values any response could
  publish, and there is no endpoint member on this service's model that could announce another one:
  so a listener opened for this front door would be on an address the recorded client never dials.
* The front door of ONE Region is what the credential algebra is about: the password AUTH accepts is
  derived over the Region the endpoint belongs to, and a derivation for another Region must be
  refused. A request on the HTTPS front door carries its Region in the SigV4 credential scope
  (`crate::state::region_from_request`), but an SMTP connection carries no scope, no Region label and
  no server name the handler can read (the core owns TLS, so the SNI of an implicit-TLS connection
  never reaches a handler either). With one process serving every Region on one interface, nothing
  in-band tells the handler which Region's front door was dialled, so the wrong-region refusal cannot
  be decided faithfully.

Required core change: a `tcp_plaintext`-style opener (and a `tcp` sibling for the implicit-TLS port)
that takes a REQUESTED port, reporting whether it was granted so the service can answer its own
"port in use" case; plus a way for the handler to learn the server name or address the connection was
dialled at, so a Region-scoped endpoint can tell its own Region's derivation from another's. The same
requested-port gap is already filed for Amazon RDS in
`issues/things-that-do-not-work/20261105T090000Z-rds-a-db-instance-publishes-the-listeners-port-not-the-requested-port.md`,
for Aurora DSQL in
`issues/things-that-do-not-work/20261122T190000Z-dsql-the-served-cluster-data-plane-is-not-reachable-on-postgresqls-port.md`
and for a DNS listener in
`issues/things-that-do-not-work/20261122T001500Z-route53-a-hosted-zone-s-delegated-nameservers-resolve-nowhere.md`.

Why it matters: a client that sends mail through SES over SMTP (every MTA, every application using an
SMTP library instead of the SDK, every appliance configured with SES SMTP credentials) cannot connect
at all, and a connection refusal is indistinguishable to the caller from a networking fault rather
than a missing feature.

Traces: ses.smtp-accepts-both-advertised-auth-mechanisms, ses.smtp-auth-before-starttls-is-rejected,
ses.smtp-derived-credential-authenticates-and-a-wrong-region-derivation-is-rejected,
ses.smtp-listener-advertises-starttls-then-withdraws-it-after-upgrade,
ses.smtp-rejects-a-tampered-sigv4-derived-password,
ses.smtp-send-from-unverified-sender-rejected-at-the-data-stage,
ses.account-sending-pause-outranks-the-identity-gate-on-both-front-doors.

## Evidence
Expected a TLS/STARTTLS SMTP session on the SES SMTP endpoint for the Region (EHLO capability list,
AUTH with the SigV4-derived password, and a DATA-stage refusal for an unverified sender); actual —
every check aborts in socket.create_connection: `File "socket.py", line 850, in create_connection /
sock.connect(sa) / ConnectionRefusedError: [Errno 111] Connection refused`. This occurs for the
implicit-TLS port 465 checks (ses.smtp-derived-credential-authenticates-and-a-wrong-region-derivation-is-rejected,
ses.account-sending-pause-outranks-the-identity-gate-on-both-front-doors) and for the STARTTLS port
587 checks (ses.smtp-accepts-both-advertised-auth-mechanisms, ses.smtp-auth-before-starttls-is-rejected,
ses.smtp-listener-advertises-starttls-then-withdraws-it-after-upgrade,
ses.smtp-rejects-a-tampered-sigv4-derived-password,
ses.smtp-send-from-unverified-sender-rejected-at-the-data-stage). No SMTP reply line is ever read, so
no further SMTP-level assertion was reached.

## Steps to Reproduce
1. Boot the emulator and point boto3 at it with clients 'ses', 'sesv2' and 'iam'.
2. Resolve the SES SMTP endpoint the emulator serves for the Region and open smtplib.SMTP(host, 587)
   and smtplib.SMTP_SSL(host, 465) — observe ConnectionRefusedError on both instead of a 220
   greeting.
3. Once the core can bind a requested port: on 587, EHLO must advertise STARTTLS, AUTH PLAIN LOGIN
   and 8BITMIME, AUTH before STARTTLS must be refused, and after the TLS upgrade the same capability
   set must appear without STARTTLS; on both ports, AUTH with the password derived by the documented
   formula (0x04 || HMAC chain over 11111111, <region>, ses, aws4_request, SendRawEmail) must succeed
   while a wrong-region or tampered derivation is refused; a DATA submission from an unverified
   sender must be refused at the DATA stage, and ses.update_account_sending_enabled(Enabled=False)
   must make the account-pause refusal outrank that identity gate on both front doors.
4. Re-replay the checks listed in trace_ids.
