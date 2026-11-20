Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatchomni-domain-and-space-singleton-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatchomni-domain-and-space-singleton-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|CreateDomain|2xx->2xx|$.domain.customEndpointUrls[*]
Issue-Key: cloudwatchomni|CreateDomain|2xx->2xx|$.domain.domainEndpointUrl
Issue-Key: cloudwatchomni|UpdateDomain|2xx->2xx|$.domain.customEndpointUrls[*]
Issue-Key: cloudwatchomni|UpdateDomain|2xx->2xx|$.domain.domainEndpointUrl
Issue-Commit: c2a9aa425461796da61883782981cd948eb4a841

# cloudwatchomni CreateDomain and UpdateDomain publish domain endpoint URLs on a host the emulator does not serve

> _Note: this issue was identified against the code base at commit `c2a9aa425461796da61883782981cd948eb4a841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
TWO net-new causes: the Domain envelope returned by CreateDomain and by UpdateDomain advertises domainEndpointUrl and customEndpointUrls[*] on the public cloudwatch-omni.global.app.aws suffix instead of on the authority the emulator actually serves, so the URLs the emulator hands a client are off-emulator and reach nothing it produced. Both write paths need the same projection fixed; each carries its own replay key below.

Related but NOT covering these: open issue 20261112T102024Z-cloudwatchomni-publishes-domain-endpoint-urls-on-a-cloudwatch.md reports exactly this fault on GetDomain and is keyed on cloudwatchomni|GetDomain|2xx->2xx|$.domain.domainEndpointUrl / customEndpointUrls[*]; its replay keys do not cover the CreateDomain and UpdateDomain responses below, which diverge on the same two members through the create/update code paths. If the endpoint composition is genuinely shared, one edit may close all of them — but these two steps must be re-replayed to show it.

Cause 1 of 2: CreateDomain mints the new domain's endpoint URLs on a foreign host (fixture cloudwatchomni-domain-and-space-singleton-lifecycle, step task#2).
  AWS: Smithy CreateDomain returns the required `domain` member (Domain); Domain.domainEndpointUrl is a required String documented "The HTTPS endpoint URL for accessing the domain" and Domain.customEndpointUrls holds "Additional endpoint URLs derived from the domain name". The model does not fix the hostname, so the host is recorded behavior only: against AWS the created domain's endpoint URL names a host that actually serves that domain, i.e. a URL the caller can open to reach the resource it just created. The URL SHAPE is the requirement — an https URL whose leftmost label is the domain id (domainEndpointUrl) or the domain name (each custom endpoint) — while the concrete host, the generated domain id and the generated name in any recording are per-run values.
  Emulator: CreateDomain answers 200 with $.domain.domainEndpointUrl = 'https://d-<id>.cloudwatch-omni.global.app.aws' and $.domain.customEndpointUrls[0] = 'https://<domain-name>.cloudwatch-omni.global.app.aws'; the replay comparator returns host_not_own on both members — the emulator announces an address other than the one it is reached on, and never substitutes its own data listener's authority.
  Why: a client that follows the endpoint URL returned by the create call leaves the emulator entirely — the request fails to resolve or egresses toward real AWS, where this emulator-local domain does not exist. Every flow that chains off a freshly created domain (opening the domain endpoint, hitting a custom endpoint, handing the URL to a browser or deep-link flow) is unreachable, and the two endpoint members of the create response cannot be trusted at all.
  Fix: the Domain projection/serializer in the cloudwatchomni service crate (services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend) that composes domainEndpointUrl and customEndpointUrls — build both from the emulator's own served authority (the host:port the request arrived on, or the configured public base URL) while keeping AWS's URL shape, instead of hard-coding the cloudwatch-omni.global.app.aws suffix. Call it from the CreateDomain response path.
  Traces: cloudwatchomni-domain-and-space-singleton-lifecycle#task#2 (CreateDomain, 200, $.domain.domainEndpointUrl and $.domain.customEndpointUrls[0] both host_not_own).

Cause 2 of 2: UpdateDomain re-emits the same foreign-host endpoint URLs in its response (fixture cloudwatchomni-domain-and-space-singleton-lifecycle, step task#7).
  AWS: Smithy UpdateDomain is @idempotent and also returns the required `domain` member, so the same Domain contract applies: the endpoint URLs in the update response name a host that serves the domain.
  Emulator: UpdateDomain answers 200 with $.domain.domainEndpointUrl and $.domain.customEndpointUrls[0] on the cloudwatch-omni.global.app.aws suffix; both are host_not_own.
  Why: the update response is the authoritative post-change view of the domain, and clients that cache endpoint URLs refresh them from it — so even an emulator that fixed only the read path would re-poison a client's cache here. The URLs point off-emulator for the same reason as cause 1.
  Fix: route UpdateDomain's response through the same corrected Domain projection, so every write path (create, update) and read path agrees on the served authority.
  Traces: cloudwatchomni-domain-and-space-singleton-lifecycle#task#7 (UpdateDomain, 200, both endpoint members host_not_own).

## Evidence
Fixture cloudwatchomni-domain-and-space-singleton-lifecycle, both steps verdict mismatch with expected status 200 and actual status 200 — the divergence is in the endpoint members only.

Cause 1 — step task#2, CreateDomain. Replay detail:
  $.domain.domainEndpointUrl: host_not_own (expected '<shaped-not-asserted>', got 'https://d-cy5xn789rb9xgj6plvuxmdq3q.cloudwatch-omni.global.app.aws')
  $.domain.customEndpointUrls[0]: host_not_own (expected 'https://sim2real-probe-cwo-dom-9e839b1f.cloudwatch-omni.global.app.aws', got 'https://sim2real-probe-cwo-dom-9e839b1f.cloudwatch-omni.global.app.aws')

Cause 2 — step task#7, UpdateDomain. Replay detail:
  $.domain.domainEndpointUrl: host_not_own (expected 'https://d-cy5xn789rb9xgj6plvuxmdq3q.cloudwatch-omni.global.app.aws', got 'https://d-cy5xn789rb9xgj6plvuxmdq3q.cloudwatch-omni.global.app.aws')
  $.domain.customEndpointUrls[0]: host_not_own (expected 'https://sim2real-probe-cwo-dom-bfd57c5b.cloudwatch-omni.global.app.aws', got 'https://sim2real-probe-cwo-dom-bfd57c5b.cloudwatch-omni.global.app.aws')

Note the verdict is host_not_own rather than value_mismatch: the recorded string and the emulator's string coincide, and that is the defect — the emulator reproduced the recording's public AWS host instead of announcing the authority it is reached on. The domain ids, generated domain names and the emulator's port in the lines above are per-run values and are not part of the requirement; the requirement is that the announced host is the one serving the response, with AWS's URL shape preserved (leftmost label = domain id for domainEndpointUrl, domain name for each custom endpoint).

Smithy cited: CreateDomain input name (required, @length 3..63, @pattern ^[a-z0-9]+(-[a-z0-9]+)*$), identityProviders (required), clientToken (@idempotencyToken), output domain (Domain, required); UpdateDomain @idempotent, output domain (Domain, required); com.amazonaws.cloudwatchomni#Domain.domainEndpointUrl required String "The HTTPS endpoint URL for accessing the domain", Domain.customEndpointUrls "Additional endpoint URLs derived from the domain name". The hostname is not modeled — recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1 — fixture 'cloudwatchomni-domain-and-space-singleton-lifecycle' step task#2: call CreateDomain with a lowercase-hyphenated name and a custom endpoint. Inspect the 200 body's $.domain.domainEndpointUrl and $.domain.customEndpointUrls[0].
3. Observe both are https URLs under the public suffix cloudwatch-omni.global.app.aws rather than under the emulator's own served authority; an HTTPS request to either does not reach the emulator that minted it.
4. Cause 2 — step task#7: call UpdateDomain on that domain and inspect the same two members of the 200 body; they carry the same foreign host.
5. Expected in both cases: the URLs are rooted at the host:port this emulator serves, so a request to them reaches this emulator's domain endpoint, while keeping AWS's URL shape (domain id / domain name as the leftmost label).
6. Re-replay both steps after fixing — a fix applied only to the GetDomain read path (open issue 20261112T102024Z) leaves these two write-path steps diverging.
