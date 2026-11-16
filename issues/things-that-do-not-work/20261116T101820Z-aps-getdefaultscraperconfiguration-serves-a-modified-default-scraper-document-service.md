Issue-Source: trace-sweep-divergence
Issue-Trace: amp-scraper-error-paths-and-defaults
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: amp-scraper-error-paths-and-defaults
Issue-Shape-Id: com.amazonaws.amp#AmazonPrometheusService
Issue-Key: aps|GetDefaultScraperConfiguration|2xx->2xx|$.configuration
Issue-Commit: f42da0d4917d7016500e6277673593e18f63e86f

# GetDefaultScraperConfiguration serves a modified default scraper document: service-account token members dropped and job members re-ordered/re-indented

> _Note: this issue was identified against the code base at commit `f42da0d4917d7016500e6277673593e18f63e86f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDefaultScraperConfiguration

## Description
Cause 1 of 1: the byte-for-byte document returned in `GetDefaultScraperConfigurationResponse$configuration` is not the document AWS serves. Two members are missing and two jobs' members are ordered and indented differently.

AWS: Smithy (services/service-com-amazonaws-amp-amazonprometheusservice/smithy-model/com-amazonaws-amp-amazonprometheusservice.json) models GetDefaultScraperConfiguration as `@readonly`, GET /scraperconfiguration, 200, input with no members, and output with exactly one member, `configuration` (target `Blob`, **required**), documented as "The configuration file. Base 64 encoded." — the Prometheus scrape configuration Amazon EKS uses when it creates a scraper for you (model doc links to "Scraper configuration" in the Amazon Managed Service for Prometheus User Guide). Because the operation takes no input and names no workspace or scraper, the blob is a fixed service-wide document: AWS returns the same bytes on every call, and the recorded response is that document. As AWS serves it, the `cadvisor` job carries `authorization.credentials_file: /var/run/secrets/kubernetes.io/serviceaccount/token`, and the `kubernetes-apiservers` job carries `bearer_token_file: /var/run/secrets/kubernetes.io/serviceaccount/token` as its FIRST mapping key (before `job_name`); in that job and in `kube-proxy`, the sequence items under `kubernetes_sd_configs:` and `relabel_configs:` are indented to the same column as their parent key (`    - role: endpoints`, `    - action: keep`), not nested one level deeper.

Emulator: the handler returns a hand-maintained constant that deliberately omits both credential members and nests those sequence items two columns deeper. Replay of `amp-scraper-error-paths-and-defaults` step `task#1` answered HTTP 200 (status matches) but `$.configuration` was a value_mismatch: the decoded blob lacks the `authorization:`/`credentials_file:` block in the `cadvisor` job, lacks `bearer_token_file:` in the `kubernetes-apiservers` job, emits `job_name: kubernetes-apiservers` as that job's first key, and writes `      - role: endpoints` / `      - action: keep` (6 spaces) where the served document writes them with 4. The omission is intentional in the source: the handler's own doc comment states the two members are "the ONE deliberate omission" because "emulator source may not carry a credential path, in any form" — so this is a known, self-documented departure from the served document, and `tests/all/default_scraper_configuration.rs` pins the gap rather than closing it.

Why: a client's whole purpose for this call is to take the blob and hand it to Prometheus (or to a scraper create call) unchanged. Without `authorization.credentials_file` the cAdvisor job has no bearer credential for the kubelet, and without `bearer_token_file` the `kubernetes-apiservers` job has none for the API server, so both jobs are rejected by the cluster and two of the four default scrape jobs silently collect nothing — the client gets a config that looks valid and scrapes half of what AWS's default scrapes. Any caller that compares, hashes, diffs or round-trips the document against AWS's default (a drift check, a committed golden, a config-management diff) also sees a permanent difference, and the member-ordering/indentation changes make even a whitespace-insensitive comparison of the two blobs non-identical at the byte level the member is compared at.

Fix: the operation's handler — specifically the served document constant it returns, which must reproduce the published default scraper configuration exactly, including both service-account token member references and the published key order and sequence-item indentation of the `kubernetes-apiservers` and `kube-proxy` jobs. The constant is the whole answer (the handler reads neither input nor state), so no state or serializer change is involved; the serializer correctly base64-encodes the bytes. The repo-local rule that forbids a credential path in source, and the test that pins the omission, have to be reconciled with serving the real document (e.g. by sourcing the document as data rather than as a source literal) rather than by keeping the blob wrong.

Traces: amp-scraper-error-paths-and-defaults#task#1 (divergence_key service=aps|GetDefaultScraperConfiguration|status=200->200|keys=$.configuration). Handler and constant: services/service-com-amazonaws-amp-amazonprometheusservice/src/ops/get_default_scraper_configuration.rs lines 96-149 (the constant, with the omission documented at lines 82-92) and lines 216-230 (the handler returning it).

## Evidence
Replay step amp-scraper-error-paths-and-defaults#task#1, verdict=mismatch, expected_status=200, actual_status=200, diverging key $.configuration (value_mismatch).

Expected (recorded) blob, decoded — relevant fragments:
  - job_name: cadvisor
    scheme: https
    authorization:
      credentials_file: /var/run/secrets/kubernetes.io/serviceaccount/token
    kubernetes_sd_configs:
      - role: node
...
  - bearer_token_file: /var/run/secrets/kubernetes.io/serviceaccount/token
    job_name: kubernetes-apiservers
    kubernetes_sd_configs:
    - role: endpoints
    relabel_configs:
    - action: keep
      regex: default;kubernetes;https
      source_labels:
      - __meta_kubernetes_namespace
      - __meta_kubernetes_service_name
      - __meta_kubernetes_endpoint_port_name
    scheme: https

Actual (emulator) blob, same fragments (from the constant the handler returns):
  - job_name: cadvisor
    scheme: https
    kubernetes_sd_configs:
      - role: node          <-- no authorization/credentials_file block at all
...
  - job_name: kubernetes-apiservers   <-- no bearer_token_file; job_name is first key
    kubernetes_sd_configs:
      - role: endpoints               <-- 6-space item indent, served document uses 4
    relabel_configs:
      - action: keep                  <-- 6-space item indent, served document uses 4

So: status and envelope agree; the required `configuration` member's bytes do not. Two members present in every AWS response are absent, and the apiservers/kube-proxy jobs differ in key order and sequence-item indentation. Nothing here is a per-run value — the document is fixed and input-independent, so the difference is reproducible on every call.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `amp-scraper-error-paths-and-defaults`, step `task#1` (GET /scraperconfiguration against the aps endpoint; the request has no input members and no auth-relevant state).
2. Read the 200 response's `configuration` member and base64-decode it.
3. Search the decoded YAML for `credentials_file` and for `bearer_token_file`: both are absent, where the recorded AWS response carries each pointing at /var/run/secrets/kubernetes.io/serviceaccount/token (in the `cadvisor` job's `authorization` block and as the `kubernetes-apiservers` job's first key respectively).
4. Compare the `kubernetes-apiservers` and `kube-proxy` jobs: the emulator indents the items under `kubernetes_sd_configs:` and `relabel_configs:` two columns deeper than the served document, and emits `job_name` as the apiservers job's first key.
5. Diff the decoded blob against the recorded one — it differs on every call, since the handler returns a fixed constant and reads neither input nor state.
