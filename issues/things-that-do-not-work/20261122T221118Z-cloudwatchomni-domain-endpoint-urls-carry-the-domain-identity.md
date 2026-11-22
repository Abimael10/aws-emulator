Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only|run#6|AssertionError|assert id_label == domain_id, f"the domain endpoint's first label is {id_label!r}, expected the domain id"
Issue-Commit: 5d63e2aee7050ea48e4d122e3079b6f7ce94d0b2

# cloudwatchomni Domain endpoint URLs carry the domain identity in the PATH, not as the leftmost host label, so domainEndpointUrl/customEndpointUrls have the wrong URL shape

> _Note: this issue was identified against the code base at commit `5d63e2aee7050ea48e4d122e3079b6f7ce94d0b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
Cause 1 of 1: every Domain envelope the service projects (CreateDomain, GetDomain, UpdateDomain — all three go through one projection) composes `domainEndpointUrl` and each `customEndpointUrls` entry as `https://<emulator-authority>/<path-with-the-identity>`, so the URL's leftmost host label is a fragment of the emulator's own authority instead of the domain's identity. Failing check: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only.

  AWS: Smithy com.amazonaws.cloudwatchomni#Domain.domainEndpointUrl is a REQUIRED String documented "The HTTPS endpoint URL for accessing the domain"; Domain.customEndpointUrls is a StringList documented "Additional endpoint URLs derived from the domain name"; Domain.domainId is required with @pattern ^d-[0-9a-z]{1,25}$ and Domain.name has @length 3..63 / @pattern ^[a-z0-9]+(-[a-z0-9]+)*$. The model does not fix a hostname, so the host SUFFIX is recorded behavior only — but the URL SHAPE is the requirement AWS's answers exhibit and the behavioral check asserts: each of these is an `https` URL whose LEFTMOST host label is the resource identity — the domain id for `domainEndpointUrl`, the domain name for each `customEndpointUrls` entry — and both URLs share one host suffix after that first label. A domain carrying a name publishes exactly one custom endpoint. On rename only the name-derived label moves: the id-labelled `domainEndpointUrl`, `domainArn` and `createdAt` are unchanged, and the renamed domain's custom endpoint's first label becomes the new name while its suffix stays the suffix the id endpoint uses. The identity lives in the HOST, never in a path.

  Emulator: CreateDomain answers 200, but the `domain.domainEndpointUrl` it returns is of the form `https://<host>:<port>/_aws/cloudwatch-omni/domain/<domainId>` and each `domain.customEndpointUrls[0]` is `https://<host>:<port>/_aws/cloudwatch-omni/domain-name/<name>`. Splitting the URL's hostname on '.' therefore yields a first label taken from the emulator's own address rather than from the resource, and the replay fails on the very first assertion with `AssertionError: the domain endpoint's first label is '127', expected the domain id` ('127' is the leading octet of the per-run loopback address the emulator is reached on — a per-run value; the DEFECT is that the first label is an address fragment at all instead of the `d-…` domain id). Because the identity was moved into the path, the id-vs-name distinction and the shared-suffix relation the check asserts next (custom endpoint's first label == name, both suffixes equal, rename moves only the custom endpoint) cannot hold either: every domain of every name projects the SAME hostname and differs only below the path.

  Why: a client that reads these members treats them as DNS-addressed per-domain endpoints — it parses the leftmost label to recover which domain an endpoint belongs to, pins or matches a TLS/virtual-host name, derives a custom endpoint from a domain name, or re-reads the URL after a rename to confirm only the name-derived endpoint moved. Against this emulator all of that reads an address fragment: the domain id and name are not recoverable from the host, two different domains (and the id and name endpoints of one domain) are indistinguishable by host, and a rename appears to change nothing in the host at all. Any code that round-trips name → custom endpoint URL, or host label → domain, silently resolves to the wrong resource or to none.

  Fix: the shared Domain endpoint projection in the cloudwatchomni service crate — `services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/state/domain_endpoint.rs` (`domain_endpoint_url`, `custom_endpoint_urls`, `announced_domain`), which is what `ops/create_domain.rs` and the get/update paths call. It must keep AWS's URL shape — identity as the LEFTMOST HOST LABEL, one shared host suffix for both members — rather than placing the identity in a path under `/_aws/cloudwatch-omni/domain{,-name}/…`; the inverse route reader `domain_endpoint_route` and whatever `crate::service` serves for these addresses must be moved to match, so what is advertised is still what the emulator answers.

  Related, NOT covering this: open issue 20261120T040030Z-cloudwatchomni-createdomain-and-updatedomain-publish-domain-endpoint-urls.md and fixed issue 20261112T102024Z-cloudwatchomni-publishes-domain-endpoint-urls-on-a-cloudwatch.md both concern the HOST these URLs are rooted on (a foreign cloudwatch-omni.global.app.aws suffix). The host is no longer foreign; what diverges now is the URL's STRUCTURE — the identity label was relocated from the host into the path — which neither of those issues describes or keys.

  Traces: behavioral check cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only (group domains), assertion at line 86 of check_cloudwatchomni_domain_rename_moves_its_custom_endpoint_url_only.py.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, verdict `mismatch`, check `cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only`:

  File "check_cloudwatchomni_domain_rename_moves_its_custom_endpoint_url_only.py", line 86, in run
    assert id_label == domain_id, f"the domain endpoint's first label is {id_label!r}, expected the domain id"
  AssertionError: the domain endpoint's first label is '127', expected the domain id

The check splits `urlsplit(url).hostname` on the first '.': expected first label = the `domainId` returned by the same CreateDomain response (shape `^d-[0-9a-z]{1,25}$`), got an address fragment. Status was 200 on every call; the divergence is purely in the two endpoint members of the `domain` envelope.

Emulator source confirming the shape (file/line citations only, not a patch):
  services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/state/domain_endpoint.rs:61-66  domain_endpoint_path -> "/{EMULATOR_PATH_NAMESPACE}/{DOMAIN_PLANE}/domain/{domain_id}"
  ...:74-79   domain_name_endpoint_path -> "/{EMULATOR_PATH_NAMESPACE}/{DOMAIN_PLANE}/domain-name/{name}"
  ...:113-122 domain_endpoint_url -> format!("https://{announced_authority}{domain_endpoint_path}")
  ...:130-138 custom_endpoint_urls -> format!("https://{announced_authority}{domain_name_endpoint_path}")
  ...:148-157 announced_domain — the single projection CreateDomain/GetDomain/UpdateDomain all return through
  services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/create_domain.rs:221-225 — the create path calling both helpers

Smithy cited (services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/smithy-model/com-amazonaws-cloudwatchomni-cloudwatchomnifrontend.json): Domain.domainEndpointUrl (String, required, "The HTTPS endpoint URL for accessing the domain"), Domain.customEndpointUrls (StringList, "Additional endpoint URLs derived from the domain name"), Domain.domainId (required, @pattern ^d-[0-9a-z]{1,25}$), Domain.name (@length 3..63, @pattern ^[a-z0-9]+(-[a-z0-9]+)*$). The hostname itself is not modeled — recorded/asserted behavior only; the requirement asserted is the label placement and the shared suffix, not any one run's host, port, domain id or generated name.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite (run_behavioral_suite); the check reproduces deterministically across runs.
2. Call CreateDomain with a clientToken, a name matching ^[a-z0-9]+(-[a-z0-9]+)*$ and identityProviders=["IAM"].
3. Read `domain.domainEndpointUrl` from the 200 body and take the hostname's first '.'-separated label. Observed: an octet of the emulator's own address (e.g. '127'), with the domain id pushed into the path `/_aws/cloudwatch-omni/domain/<domainId>`. Expected: the label equals the returned `domainId`.
4. Read `domain.customEndpointUrls` — expected exactly one entry whose first host label is the domain's `name` and whose remaining host suffix is identical to the suffix of `domainEndpointUrl`. Observed: the same hostname as the id endpoint, with the name pushed into the path `/_aws/cloudwatch-omni/domain-name/<name>`, so neither the name label nor the shared-suffix relation is observable.
5. Call UpdateDomain(domainId, name=<new-name>) then GetDomain(domainId): expected the single custom endpoint's first host label to become <new-name> while `domainEndpointUrl`, `domainArn` and `createdAt` are byte-identical to the create response. Observed: the custom endpoint's hostname is unchanged (only its path segment moves), so the rename is not visible in the host at all.
6. Re-replay check cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only after the fix; it must pass end to end, not just past line 86.
