Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only|run#6|AssertionError|assert id_label == domain_id, f"the domain endpoint's first label is {id_label!r}, expected the domain id"
Issue-Commit: 2aaadc19d9353b323ddc8d1ceaf32931386905fe

# cloudwatchomni Domain endpoint URLs carry the identity in the PATH again: domainEndpointUrl's leftmost host label is the serving host, not the domain id

> _Note: this issue was identified against the code base at commit `2aaadc19d9353b323ddc8d1ceaf32931386905fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
Cause 1 of 1: every `Domain` envelope this service announces (CreateDomain, GetDomain, UpdateDomain all project through one helper) publishes `domainEndpointUrl` and each `customEndpointUrls` entry as `https://<serving-authority>/_aws/cloudwatch-omni/domain/<domainId>` and `https://<serving-authority>/_aws/cloudwatch-omni/domain-name/<name>` — the resource identity sits in the PATH and the host is the bare authority the request was reached on. AWS puts the identity in the LEFTMOST HOST LABEL of both members. This is a regression of fixed issue 20261122T221118Z-cloudwatchomni-domain-endpoint-urls-carry-the-domain-identity.md, which closed on exactly this shape defect (identity in the path rather than as the leftmost label); the still-open issue 20261125T083155Z-cloudwatchomni-getdomain-announces-domain-endpoint-urls-on-a.md owns the complementary half of the pairing (the SUFFIX the label is attached to must be an authority this emulator answers on) and explicitly records that the leftmost-label part 'is now correct' — it no longer is, so that issue does not cover this.

  AWS: Smithy com.amazonaws.cloudwatchomni#Domain declares `domainEndpointUrl` (String, REQUIRED) documented "The HTTPS endpoint URL for accessing the domain" and `customEndpointUrls` (StringList) documented "Additional endpoint URLs derived from the domain name"; `Domain.domainId` is required with @pattern ^d-[0-9a-z]{1,25}$ and `Domain.name` is @length 3..63 @pattern ^[a-z0-9]+(-[a-z0-9]+)*$ — i.e. both identities are already single DNS labels. The model fixes no hostname, so the host SHAPE is recorded behavior only: real CloudWatch Omni answers an https URL whose first host label is the domain id for `domainEndpointUrl` and the domain NAME for each `customEndpointUrls` entry, both sharing ONE host suffix, and a rename moves only the name-labelled custom endpoint while the id-labelled endpoint, the ARN and createdAt stay unchanged. The concrete suffix, the generated id/name and any port in one run are per-environment values and are NOT the requirement; the requirement is the pairing: identity as the leftmost label, one suffix shared by both members, rooted on an address that actually serves the domain.

  Emulator: CreateDomain answers 200 but with `domainEndpointUrl` = `https://<host>:<port>/_aws/cloudwatch-omni/domain/<domainId>` — parsing the URL's hostname and splitting on the first '.' yields a leftmost label that is part of the serving host (the replay saw '127', the first octet of the loopback literal) instead of the domain id, and the identity appears only as a path segment. `customEndpointUrls[0]` is built the same way (`/_aws/cloudwatch-omni/domain-name/<name>`), so the custom endpoint is not "derived from the domain name" in its host either. The check never gets past the first assertion, so the rename behavior it also covers (the custom endpoint must follow the new name while the id endpoint, ARN and createdAt do not move) is unverified behind the same defect.

  Why: both members are documented addresses a client OPENS — a browser or deep-link flow dials them, a probe fetches them, a TLS/virtual-host matcher pins the leftmost label, and a client recovers which domain an endpoint belongs to by reading that label. Against this emulator every one of those reads the serving host (or a path segment it does not know to look in) as the domain identity, so endpoint-derived routing, per-domain host matching and any assertion that the name endpoint tracks a rename while the id endpoint does not are all wrong, even though the call returns 200.

  Fix: the shared Domain endpoint projection in the cloudwatchomni service crate — the helpers that mint the two members and the inverse reader/route that serves them, and the single `Domain` projection every create/read/update answer passes through. Compose each member with the identity as the leftmost host label (domain id for `domainEndpointUrl`, domain name for each `customEndpointUrls` entry) over one shared suffix, keeping the authority one this emulator answers on as 20261125T083155Z requires, and move whatever serves those addresses with it so what is advertised is what is answered.

  Traces: cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check `cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only`, verdict mismatch (HTTP status was the expected 200 — the divergence is purely in the two endpoint members):

  AssertionError: the domain endpoint's first label is '127', expected the domain id
  (check_cloudwatchomni_domain_rename_moves_its_custom_endpoint_url_only.py line 86:
   `assert id_label == domain_id`, where id_label = urlsplit(created['domainEndpointUrl']).hostname.partition('.')[0])

Expected (AWS): urlsplit(domainEndpointUrl).hostname.partition('.') == (<Domain.domainId>, <shared-suffix>) and urlsplit(customEndpointUrls[0]).hostname.partition('.') == (<Domain.name>, <same shared-suffix>), with exactly one customEndpointUrls entry; after UpdateDomain(name=<new>) the custom endpoint's leftmost label becomes <new> while domainEndpointUrl, domainArn and createdAt are unchanged.
Actual (emulator): hostname is the bare serving authority's host, so its leftmost label is a fragment of that host ('127' for the loopback literal), and the identity appears only as a path segment: `https://<host>:<port>/_aws/cloudwatch-omni/domain/<domainId>` / `.../domain-name/<name>`. The literal host, port, generated domain id and generated name in this run are per-environment values and are not the requirement.

Smithy cited (services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/smithy-model/com-amazonaws-cloudwatchomni-cloudwatchomnifrontend.json): Domain.domainEndpointUrl (String, required, "The HTTPS endpoint URL for accessing the domain"), Domain.customEndpointUrls (StringList, "Additional endpoint URLs derived from the domain name"), Domain.domainId (required, @pattern ^d-[0-9a-z]{1,25}$), Domain.name (@length 3..63, @pattern ^[a-z0-9]+(-[a-z0-9]+)*$). The hostname itself is not modeled — recorded behavior only.

Emulator source (file/line citations only): services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/state/domain_endpoint.rs:73-78 `domain_endpoint_path`, :88-93 `domain_name_endpoint_path`, :111-122 `announced_authority`, :135-143 `announced_endpoint_url`, :148-153 `domain_endpoint_url`, :163-170 `custom_endpoint_urls`, :180-189 `announced_domain` (the one projection every Domain answer returns through), :213-234 `domain_endpoint_route` (the inverse reader that must move with the shape); services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/create_domain.rs:221-225 where CreateDomain fills the two members. The module docs at domain_endpoint.rs:14-31 state the deliberate choice to carry identity in the path, and the unit tests at :304-423 encode it — both must change with the fix.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite.
2. Run check `cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only`, or by hand: CreateDomain(name=<a name matching ^[a-z0-9]+(-[a-z0-9]+)*$>, identityProviders=['IAM'], clientToken=<token>).
3. Read the 200 body's $.domain.domainEndpointUrl and split its hostname on the first '.': the leftmost label is a piece of the serving host (e.g. '127' for a loopback-literal authority), not $.domain.domainId; the domain id appears only in the path segment after /_aws/cloudwatch-omni/domain/.
4. Same for $.domain.customEndpointUrls[0]: its leftmost host label is not $.domain.name.
5. Then UpdateDomain(domainId, name=<new name>) and GetDomain(domainId): the custom endpoint must carry <new name> as its leftmost host label over the same suffix as domainEndpointUrl, while domainEndpointUrl, domainArn and createdAt are unchanged. This part of the check is currently unreachable behind step 3.
6. Expected: both members are https URLs whose leftmost host label is the identity (domain id / domain name) over ONE shared suffix, rooted on an authority this emulator answers on (see open issue 20261125T083155Z for that half).
7. Re-replay cloudwatchomni.domain-rename-moves-its-custom-endpoint-url-only after the fix.
