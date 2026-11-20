Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr-public.describe-registries-alias-record-is-fully-populated ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless ecr-public.server-side-repository-uri-server-assigned
Issue-Shape-Id: com.amazonaws.ecrpublic#SpencerFrontendService
Issue-Key: ecr-public.describe-registries-alias-record-is-fully-populated|run#21|AssertionError|assert _public_ecr_host(host) and path == alias, (registry["registryUri"], alias)
Issue-Key: ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless|_assert_repository_shape#11|AssertionError|assert _public_ecr_host(host) and path == f"{alias}/{name}", uri
Issue-Key: ecr-public.server-side-repository-uri-server-assigned|run#7|AssertionError|assert _public_ecr_host(host), f"expected a public.ecr.<domain> host before the path, got {uri!r}"
Issue-Commit: e1cfc0c2c76597b7a2bdd77e91655798e6b82e43

# ecr-public: registryUri and repositoryUri are minted as the emulator's bare host:port with a constant 'emulator' segment instead of a public.ecr.<domain>/<alias> registry authority (2 causes)

> _Note: this issue was identified against the code base at commit `e1cfc0c2c76597b7a2bdd77e91655798e6b82e43`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRegistries

## Description
Two related-but-distinct members of the ecr-public surface carry a URI that is not an ECR Public registry authority at all. Both are 200 responses — these are body-shape divergences, not status or error divergences. Note: open issue 20261117T002111Z-ecr-public-iam-ecr-public-describerepositories-createrepository-report-a-repositoryuri-that.md describes the repositoryUri shape for the composed service `ecr-public__iam` and carries only that check's replay key; it does not carry any of the three checks below, and it does not cover Registry$registryUri at all.

Cause 1 of 2: Repository$repositoryUri (CreateRepository, DescribeRepositories, DeleteRepository echo) has no public.ecr authority and no registry alias.
Failing checks: ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless, ecr-public.server-side-repository-uri-server-assigned.
AWS: Smithy (com.amazonaws.ecrpublic) Repository$repositoryUri (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." For the public registry AWS composes it as `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>`: the labels `public` and `ecr` are FIXED text, the remaining labels are the partition's DNS suffix for the public registry, the first path segment is the account's public-registry alias (RegistryAlias$name, pattern ^[a-z][a-z0-9]+(?:[._-][a-z0-9]+)*$, length 2..50 — specifically the alias flagged RegistryAlias$primaryRegistryAlias, which the model says "is displayed in both the repository URL and the image URI used in the docker pull commands"), and the remaining segments are exactly Repository$repositoryName as the request spelled it, including any `a/b/c` namespacing the RepositoryName pattern allows. The owning account appears only in Repository$registryId (^[0-9]{12}$), never in the URI, and no region appears in it. That label/segment structure is the requirement; the concrete DNS suffix, alias and repository name of any one run are values from the account's registry state and the request.
Emulator: repositoryUri comes back as the socket address the emulator is bound to plus a constant first path segment, e.g. `127.0.0.1:34697/emulator/sim2real-behav-ecrpub-repo-<id>` and `127.0.0.1:45875/emulator/sim2real-behav-ecrpub-uri-<id>` (the host:port and the generated name are per-run values; the defect is that there are no `public`,`ecr` leading labels, no domain, and the alias position is a literal `emulator`).
Why: repositoryUri is the one field a client feeds to `docker push`/`docker pull`, and tooling parses it to recover the registry alias and to tell a public registry URI apart from a private `<acct>.dkr.ecr.<region>` one. A client relying on AWS's shape gets an authority that is not a registry name, cannot recover the alias, cannot distinguish public from private, and gets a value that changes with the emulator's listening port on every run.

Cause 2 of 2: Registry$registryUri (DescribeRegistries) has the same defect, so the alias record a client reads back cannot be joined to a pull URI.
Failing check: ecr-public.describe-registries-alias-record-is-fully-populated.
AWS: Smithy Registry$registryUri (target Url, @required) — "The URI of a public registry. The URI contains a universal prefix and the registry alias." i.e. `public.ecr.<dns-suffix>/<registryAlias>` — the universal prefix is the fixed `public.ecr.` labels plus the partition DNS suffix, and the single path segment is the primary alias's name; the account's registry is global, so no region or account id appears. A repository's repositoryUri is then exactly `<registryUri>/<repositoryName>`.
Emulator: DescribeRegistries answers 200 with registryUri = `127.0.0.1:37413/emulator` — bare host:port as the authority and the constant `emulator` as the alias segment (the alias record itself reports name `emulator`, which does satisfy RegistryAliasName, so the divergence is the missing `public.ecr.<domain>` authority, not the alias name).
Why: a client that builds pull URIs from DescribeRegistries, or that validates a repositoryUri against the registry's own registryUri, gets a value with no registry authority; nothing in the response identifies the registry as an ECR Public registry.

Fix: the ecr-public URI composition shared by the Repository serializer/state (CreateRepository, DescribeRepositories, DeleteRepository) and the DescribeRegistries handler must build the documented `public.ecr.<dns-suffix>` authority — the fixed `public`/`ecr` labels followed by the environment's own domain, which the emulator must also answer on — and must take the alias segment from the account's primary registry alias in state rather than a constant, with registryUri = `<authority>/<alias>` and repositoryUri = `<registryUri>/<repositoryName>` (nested names preserved verbatim).

Traces: ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless, ecr-public.server-side-repository-uri-server-assigned (Cause 1); ecr-public.describe-registries-alias-record-is-fully-populated (Cause 2).

## Evidence
Suite: 85/92 passed; service ecr-public 85/91 (93%), 6 failed, of which these three are neither ALREADY FILED nor OWNED by an open issue.

[mismatch] ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless
  check_..._repository_uri_and_arn_are_alias_scoped_and_regionless.py line 73, in _assert_repository_shape:
    assert _public_ecr_host(host) and path == f"{alias}/{name}", uri
  AssertionError: 127.0.0.1:34697/emulator/sim2real-behav-ecrpub-repo-852bee81f68e
  (asserted on CreateRepository's own response; the later ARN/region and RepositoryNotFoundException-message assertions were never reached)

[mismatch] ecr-public.server-side-repository-uri-server-assigned
  check_..._server_side_repository_uri_server_assigned.py line 25, in run:
    assert _public_ecr_host(host)
  AssertionError: expected a public.ecr.<domain> host before the path, got '127.0.0.1:45875/emulator/sim2real-behav-ecrpub-uri-6275825d8bff'

[mismatch] ecr-public.describe-registries-alias-record-is-fully-populated
  check_..._describe_registries_alias_record_is_fully_populated.py line 84, in run:
    assert _public_ecr_host(host) and path == alias, (registry["registryUri"], alias)
  AssertionError: ('127.0.0.1:37413/emulator', 'emulator')
  (the alias-segment half of that conjunction holds — path == alias == 'emulator' — so the failure is the authority)

The checks' shared definition of the required host is:
  def _public_ecr_host(host): labels = host.split('.'); return labels[:2] == ['public','ecr'] and len(labels) > 2 and all(labels[2:])
so only the leading `public.ecr.` labels plus a non-empty domain are required; the domain itself is allowed to be the environment's.

Expected (AWS): HTTP 200; $.registries[0].registryUri == 'public.ecr.<dns-suffix>/<primaryAlias>' and $.repository.repositoryUri == 'public.ecr.<dns-suffix>/<primaryAlias>/<repositoryName>'.
Actual (emulator): HTTP 200; registryUri == '127.0.0.1:<port>/emulator', repositoryUri == '127.0.0.1:<port>/emulator/<repositoryName>'.
No status-code or error-code divergence. Ports, generated repository names and any one run's alias value are per-run values; the label/segment structure is the requirement.

Everything the checks assert before these points passed: registryId is a 12-digit account, registryArn is arn:aws:ecr-public::<acct>:registry/<acct>, verified is boolean, exactly one ACTIVE primary alias is present with all four RegistryAlias members, and DescribeRegistries is stable across calls.

## Steps to Reproduce
1. Against the emulator in us-east-1: `aws ecr-public create-repository --repository-name <name> --region us-east-1` and inspect $.repository.repositoryUri.
   -> AWS: authority `public.ecr.<dns-suffix>` (fixed `public`,`ecr` labels then a real domain), path `<primaryAlias>/<name>`.
   -> Emulator: authority `127.0.0.1:<port>`, path `emulator/<name>`.
2. `aws ecr-public describe-repositories --repository-names <name> --region us-east-1` — same field, same defect.
3. `aws ecr-public describe-registries --region us-east-1` and inspect $.registries[0].registryUri.
   -> AWS: `public.ecr.<dns-suffix>/<primaryAlias>`.
   -> Emulator: `127.0.0.1:<port>/emulator`.
4. Verify the join AWS guarantees: repositoryUri == registryUri + '/' + repositoryName, with the alias segment taken from the alias flagged primaryRegistryAlias.
5. Re-replay checks ecr-public.repository-uri-and-arn-are-alias-scoped-and-regionless, ecr-public.server-side-repository-uri-server-assigned and ecr-public.describe-registries-alias-record-is-fully-populated.
