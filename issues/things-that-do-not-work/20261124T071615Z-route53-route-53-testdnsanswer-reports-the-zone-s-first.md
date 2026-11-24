Issue-Source: trace-sweep-divergence
Issue-Trace: route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type
Issue-Step: task#6
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|TestDNSAnswer|2xx->2xx|$.TestDNSAnswerResponse.Nameserver
Issue-Commit: 71f89b78277d5e005fd5668f9db41a590997ed19

# Route 53 TestDNSAnswer reports the zone's first delegation-set hostname as Nameserver instead of the name server that answered

> _Note: this issue was identified against the code base at commit `71f89b78277d5e005fd5668f9db41a590997ed19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestDNSAnswer

## Description
Cause 1 of 1: TestDNSAnswer's required `Nameserver` output member does not carry a value of the class AWS returns there.

AWS: Smithy (route53, com-amazonaws-route53-awsdnsv20130401) models TestDNSAnswer -> TestDNSAnswerResponse.Nameserver as a required string target `Nameserver`, @length 0..255, documented as "The Amazon Route 53 name server used to respond to the request." (operation http GET /2013-04-01/testdnsanswer, 200). The model does not state the value's form, so for the form this is recorded behavior only: the fixture's recorded AWS value is shape-classified by the replay harness and deliberately not asserted literally (expected '<shaped-not-asserted>'), i.e. the literal is environment-specific but its CLASS is the requirement — and the emulator's value is not of that class. Hypothesis: AWS answers with the address/identity of the Route 53 edge name server that actually served the simulated query (which is not one of the zone's four delegated `ns-<n>.awsdns-<n>.<tld>` delegation-set host names), and which of the four is reported varies per query.

Emulator: it answers 200 with `$.TestDNSAnswerResponse.Nameserver` set to the hosted zone's FIRST minted delegation-set host name — the replay reports `value_mismatch (expected '<shaped-not-asserted>', got 'ns-1179.awsdns-35.com')` on two independent fixtures. The handler hard-codes the first element of the zone's delegation set (`services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/test_dns_answer.rs:784`, `nameserver: zone.name_servers.first().cloned().unwrap_or_default()`), so the field is a deterministic restatement of state the caller already holds from CreateHostedZone/GetHostedZone rather than a report of who answered. The `ns-1179.awsdns-35.com` label itself is a per-run minted value and is NOT the requirement; the requirement is the class of value the member carries.

Why: a client that uses TestDNSAnswer as a diagnostic — to learn which Route 53 name server served a query, to correlate an answer with a particular edge, or to distinguish per-name-server divergence while a change propagates — reads a value of the wrong kind and always the same one for a given zone. Any assertion over the shape of that field (an address vs. a delegation host name) fails against the emulator, and per-name-server diagnostics are unobservable because all four name servers of a zone are collapsed to the first.

Fix: the TestDNSAnswer handler's output projection (`src/ops/test_dns_answer.rs`, the `nameserver` field of `TestDNSAnswerOutput`) together with whatever state the service keeps for the responding resolver/name-server identity — it must report the name server that served the simulated query in the class AWS reports it, not echo `zone.name_servers.first()`. Status (200), the other output members and the rest of the body are correct and must not change.

Traces: route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type#task#6 (representative) and route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#12 — the same divergence in the same cluster (divergence_key `service=route53|TestDNSAnswer|status=200->200|keys=$.TestDNSAnswerResponse.Nameserver`). No open issue carries this replay key: the nearest open reports (20261122T001500Z-route53-a-hosted-zone-s-delegated-nameservers-resolve-nowhere.md and 20261117T145616Z-route53-7-net-new-causes-the-authoritative-dns.md) are about the delegated name servers being unreachable for real DNS, not about which value TestDNSAnswer's Nameserver member reports.

## Evidence
Replay of fixture route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type, step task#6 (TestDNSAnswer, GET /2013-04-01/testdnsanswer):
  verdict: mismatch; expected_status 200, actual_status 200 (status agrees)
  detail: $.TestDNSAnswerResponse.Nameserver: value_mismatch (expected '<shaped-not-asserted>', got 'ns-1179.awsdns-35.com')
Same divergence at route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#12 (affected_count 2).

Expected: TestDNSAnswerResponse.Nameserver carries a value of the class the recorded AWS response carries for "the Amazon Route 53 name server used to respond to the request" (the harness shape-normalizes the literal, so the class is what is asserted).
Actual: the hosted zone's first delegation-set host name, e.g. 'ns-1179.awsdns-35.com' (the digits are per-run; the invariant defect is that it is the zone's own first delegated host name, emitted for every query against that zone).

## Steps to Reproduce
1. CreateHostedZone for a public zone and note DelegationSet.NameServers.
2. ChangeResourceRecordSets to UPSERT a record in that zone (e.g. an A record).
3. GET /2013-04-01/testdnsanswer?hostedzoneid=<zone id>&recordname=<record name>&recordtype=A.
4. Read $.TestDNSAnswerResponse.Nameserver: the emulator returns the first entry of the zone's DelegationSet.NameServers (identically for every query against the zone), whereas the recorded AWS response carries a value of a different class for the name server that answered.
