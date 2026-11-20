Issue-Source: trace-sweep-divergence
Issue-Trace: api-image-layer-upload-and-push-workflow
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: api-image-layer-upload-and-push-workflow api-multi-repository-isolation api-repository-creation-catalog-lifecycle api-repository-policy-management api-repository-uri-arn-alias-scoped-regionless api-resource-tagging-workflow api-repository-deletion-and-non-empty-error-paths ecr-public-describe-repositories-max-results-pagination api-registry-level-operations-and-authorization
Issue-Shape-Id: com.amazonaws.ecrpublic#SpencerFrontendService
Issue-Key: ecr-public|CreateRepository|2xx->2xx|$.repository.repositoryUri
Issue-Key: ecr-public|DeleteRepository|2xx->2xx|$.repository.repositoryUri
Issue-Key: ecr-public|DescribeRepositories|2xx->2xx|$.repositories[*].repositoryUri
Issue-Key: ecr-public|DescribeRegistries|2xx->2xx|$.registries[*].registryUri
Issue-Commit: bdca4f331e3f84a5ee858acb1e0644b3d6ffabda

# ecr-public: repository and registry URIs are minted from the emulator's local listener instead of the public.ecr registry authority and alias

> _Note: this issue was identified against the code base at commit `bdca4f331e3f84a5ee858acb1e0644b3d6ffabda`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Every ECR Public response that carries a container-registry URI is built from the socket the emulator happens to be listening on rather than from the public registry's authority and the account's registry alias. The wire replay isolates five distinct response paths (5 causes, 25 affected steps across 7 fixtures); all are HTTP 200 with the correct status and no error code, so each is a body-shape divergence in one field.

Shared AWS rule (cited once, applies to all five causes). Smithy (ecr-public) Repository$repositoryUri -> Url: "The URI for the repository. You can use this URI for container image push and pull operations."; Registry$registryUri -> Url (required): "The URI of a public registry. The URI contains a universal prefix and the registry alias." For the public registry AWS composes these as registryUri = public.ecr.<dns-suffix> / <registryAlias> and repositoryUri = public.ecr.<dns-suffix> / <registryAlias> / <repositoryName>. What is REQUIRED is that structure: the fixed leading DNS labels `public` and `ecr`, then the partition's real DNS suffix for the public registry (no port), then a first path segment that is the registry's own alias (Registry$aliases / RegistryAlias$name, pattern ^[a-z][a-z0-9]+(?:[._-][a-z0-9]+)*$ - the primary alias, NOT the 12-digit account id, which travels separately in registryId with pattern ^[0-9]{12}$), then, for a repository, a final segment equal to Repository$repositoryName exactly as the request spelled it. The alias and repository name of any one recording are values from the request and the account's registry state, not part of the requirement.

Shared emulator behavior. In place of that authority the emulator emits `public.ecr.localhost:<its own listener port>` and a constant `emulator` path segment where the alias belongs: e.g. `public.ecr.localhost:43015/emulator/<repositoryName>`. The port is whatever the emulator bound in that run and varies between fixtures in one replay (43015, 38171, 41253), and the announced host is not even the address the emulator is reached on (the data listener is a different port), so the replay classifies it host_not_own: the emulator announces an address it does not answer on.

Shared client impact (Why). repositoryUri is the single field a client uses to push and pull: `docker login public.ecr.<suffix>` then `docker push <repositoryUri>:<tag>`. A client relying on AWS's shape gets an authority that is not a registry name, cannot tell the repository belongs to the PUBLIC registry rather than a private `<acct>.dkr.ecr.<region>` one, cannot recover the alias in order to hand an unauthenticated consumer a pull URI, and gets a value that changes with the emulator's port on every run - so nothing derived from it is reproducible, and any client that dials the URI reaches nothing.

Shared fix location. The ecr-public component that composes registry/repository URIs - the Repository and Registry serializers and the registry state that owns the account's alias, shared by CreateRepository, DeleteRepository, DescribeRepositories and DescribeRegistries - must mint them from the public-registry authority plus the alias held in registry state, not from the request's Host header or the bound listener, and the authority it announces must be one the emulator answers on.

Cause 1 of 5: CreateRepository's own response. $.repository.repositoryUri is returned as the local-listener form (`public.ecr.localhost:<port>/emulator/<repositoryName>`) instead of `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>`. 8 affected steps. Fixture api-image-layer-upload-and-push-workflow, step setup#1.

Cause 2 of 5: DeleteRepository's echoed repository. The deleted repository is echoed back with the same defective $.repository.repositoryUri, so the delete confirmation never names the real registry either; this path composes the URI independently of the create path and must be fixed with it. 8 affected steps. Fixture api-image-layer-upload-and-push-workflow, step teardown#17.

Cause 3 of 5: DescribeRepositories over a multi-repository page. Every element's $.repositories[i].repositoryUri carries the same defective authority and `emulator` alias segment. The recorded response additionally orders the page differently from the emulator: AWS returned the frontend repository at index 0 and the backend repository at index 1, while the emulator returned them swapped (recorded behavior only - the model states no ordering for DescribeRepositories$repositories - so the requirement here is the recording's ordering of the page, which the URI diff exposes element by element). 2 affected steps. Fixture api-multi-repository-isolation, step task#11.

Cause 4 of 5: DescribeRepositories over a single-repository/paginated page (including the maxResults pages). $.repositories[0].repositoryUri is again the local-listener form; this is the read-back path the pagination fixtures exercise, distinct from the multi-element page above. 5 affected steps. Fixture api-multi-repository-isolation, step task#14.

Cause 5 of 5: DescribeRegistries. $.registries[0].registryUri is returned as `public.ecr.localhost:41253/emulator`, i.e. the listener authority plus the constant `emulator` segment, instead of the documented universal prefix plus the registry alias (`public.ecr.<dns-suffix>/<registryAlias>`). The recording does not pin the exact value here (expected is recorded as <shaped-not-asserted>), so the requirement is the documented structure: fixed `public`.`ecr` labels, a real DNS suffix with no port, and the account's alias as the only path segment. 2 affected steps. Fixture api-registry-level-operations-and-authorization, step task#3.

Related, does NOT cover this: open issue 20261117T002111Z-ecr-public-iam-ecr-public-describerepositories-createrepository-report-a-repositoryuri-that.md reports the same field from the BEHAVIORAL corpus (bare 127.0.0.1:<port>/emulator form) for DescribeRepositories/CreateRepository only. It carries a behavioral check key, not these wire replay keys, and does not reach DeleteRepository, the multi-element page ordering or DescribeRegistries$registryUri, so all five causes here are filed with their own fixture steps.

Traces: api-image-layer-upload-and-push-workflow#setup#1 (CreateRepository), api-image-layer-upload-and-push-workflow#teardown#17 (DeleteRepository), api-multi-repository-isolation#task#11 (DescribeRepositories multi-element page), api-multi-repository-isolation#task#14 (DescribeRepositories single/paginated page), api-registry-level-operations-and-authorization#task#3 (DescribeRegistries).

## Evidence
Replay of simparity-wire-aws-v1: 165 steps, 140 passed, 25 mismatched, 0 errored, score 84.8%; all 25 mismatches are these 5 ecr-public clusters. Every one is expected_status 200 / actual_status 200 - no status or error-code divergence - with the diff confined to a URI field, verdict `mismatch`, stub_path empty and owned_by empty.

Cause 1 - CreateRepository (api-image-layer-upload-and-push-workflow#setup#1, 8 steps: also api-multi-repository-isolation#setup#1, api-multi-repository-isolation#setup#2, api-repository-creation-catalog-lifecycle#setup#1, api-repository-policy-management#setup#1, api-repository-uri-arn-alias-scoped-regionless#task#2, api-repository-uri-arn-alias-scoped-regionless#task#4, api-resource-tagging-workflow#setup#1):
  $.repository.repositoryUri: host_not_own
  expected 'public.ecr.aws/a6g2m9v2/sim2real-probe-ecr-public-push-repo-02c1a31d'
  actual   'public.ecr.localhost:43015/emulator/sim2real-probe-ecr-public-push-repo-02c1a31d'
  (the alias 'a6g2m9v2' and the repo name are this recording's values; the requirement is public.ecr.<dns-suffix>/<alias>/<repositoryName>.)

Cause 2 - DeleteRepository (api-image-layer-upload-and-push-workflow#teardown#17, 8 steps: also api-multi-repository-isolation#task#12, api-multi-repository-isolation#teardown#15, api-repository-creation-catalog-lifecycle#teardown#8, api-repository-deletion-and-non-empty-error-paths#task#14, api-repository-deletion-and-non-empty-error-paths#task#23, api-repository-policy-management#teardown#11, api-resource-tagging-workflow#teardown#11):
  $.repository.repositoryUri: host_not_own, same expected vs actual pair as cause 1.

Cause 3 - DescribeRepositories, multi-element page (api-multi-repository-isolation#task#11, 2 steps: also ecr-public-describe-repositories-max-results-pagination#task#5):
  $.repositories[0].repositoryUri: expected 'public.ecr.aws/a6g2m9v2/<...>-frontend-repo-27ed4f43', got 'public.ecr.localhost:38171/emulator/<...>-backend-repo-27ed4f43'
  $.repositories[1].repositoryUri: expected 'public.ecr.aws/a6g2m9v2/<...>-backend-repo-27ed4f43', got 'public.ecr.localhost:38171/emulator/<...>-frontend-repo-27ed4f43'
  -> both the authority/alias defect AND a page whose two elements are in the opposite order from the recording.

Cause 4 - DescribeRepositories, single/paginated page (api-multi-repository-isolation#task#14, 5 steps: also api-repository-creation-catalog-lifecycle#task#2, api-repository-deletion-and-non-empty-error-paths#task#12, ecr-public-describe-repositories-max-results-pagination#task#3, ecr-public-describe-repositories-max-results-pagination#task#4):
  $.repositories[0].repositoryUri: host_not_own
  expected 'public.ecr.aws/a6g2m9v2/<...>-backend-repo-27ed4f43'
  actual   'public.ecr.localhost:38171/emulator/<...>-backend-repo-27ed4f43'

Cause 5 - DescribeRegistries (api-registry-level-operations-and-authorization#task#3, 2 steps: also api-repository-uri-arn-alias-scoped-regionless#task#1):
  $.registries[0].registryUri: host_not_own
  expected '<shaped-not-asserted>' (structure asserted, value not pinned)
  actual   'public.ecr.localhost:41253/emulator'

Cross-cutting evidence that the host is the emulator's own and not a registry: the emulator reported its data listener on port 37211, yet the URIs it minted announce ports 43015, 38171 and 41253 in the same replay - the authority tracks whatever socket served the call, which is why the replay verdict is host_not_own rather than a value diff. Ports, generated repository names and the alias string are per-run values; the fixed parts of the requirement are the leading `public` and `ecr` labels, a real DNS suffix with no port, the alias as first path segment, and the repository name as the last.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture api-image-layer-upload-and-push-workflow.
2. Step setup#1 (CreateRepository): inspect $.repository.repositoryUri. Expected a `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>` authority; observed `public.ecr.localhost:<emulator port>/emulator/<repositoryName>`. (Cause 1)
3. Step teardown#17 (DeleteRepository) of the same fixture: the echoed $.repository.repositoryUri shows the identical defect. (Cause 2)
4. Replay api-multi-repository-isolation and read step task#11 (DescribeRepositories over two repositories): every $.repositories[i].repositoryUri carries the local-listener authority, and the two elements come back in the opposite order from the recording. (Cause 3)
5. Step task#14 of the same fixture (and ecr-public-describe-repositories-max-results-pagination task#3/task#4): $.repositories[0].repositoryUri shows the same authority defect on the single/paginated read path. (Cause 4)
6. Replay api-registry-level-operations-and-authorization and read step task#3 (DescribeRegistries): $.registries[0].registryUri is `public.ecr.localhost:<port>/emulator` instead of the universal public-registry prefix plus the account's registry alias. (Cause 5)
7. Compare the announced host against the emulator's actual data listener port to confirm it announces an address it does not answer on.
8. After fixing, re-replay all five steps named above (and their clustered members) to confirm every cause is closed.
