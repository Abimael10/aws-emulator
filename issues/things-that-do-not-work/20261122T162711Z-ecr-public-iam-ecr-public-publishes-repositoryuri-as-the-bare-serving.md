Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action
Issue-Key: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action|run#30|AssertionError|assert _public_ecr_host(host) and path.count("/") >= 1, (
Issue-Commit: 8b5969fac7c1c7431c4928bdfe595aafe4e06568

# ECR Public publishes repositoryUri as the bare serving host:port, not public.ecr.<domain>/<alias>/<name>

> _Note: this issue was identified against the code base at commit `8b5969fac7c1c7431c4928bdfe595aafe4e06568`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: the `repositoryUri` ECR Public reports for a repository omits the `public.ecr.<domain>` authority form, publishing the raw address the emulator is reached on instead.

AWS: Smithy (services/service-com-amazonaws-ecrpublic-spencerfrontendservice/smithy-model/com-amazonaws-ecrpublic-spencerfrontendservice.json) documents Repository.repositoryUri only as "The URI for the repository. You can use this URI for container image push and pull operations", so the structure is recorded behavior only: a public registry's repository URI is `public.ecr.<domain>/<alias>/<name>` — the fixed service labels `public` and `ecr` lead the authority, the rest of the authority is the target's own domain, and the path is the registry alias followed by the repository name. The call itself is a 200 on both CreateRepository and DescribeRepositories; no error code is involved. The variable parts are the domain (the target's own), the registry alias, and the repository name from the request; the fixed part is the leading `public.ecr.` labels and the `<alias>/<name>` path shape.

Emulator: CreateRepository succeeds and the repository is then read back by a different principal through DescribeRepositories, which reports `repositoryUri` = `127.0.0.1:37317/n7v9k4n5/sim2real-behav-ecrpub-iamrepo-<suffix>`. The alias and name path segments are right; the authority is the bare serving host and port with no `public.ecr.` labels at all. create_repository.rs builds it from `crate::state::published_gallery_authority(ctx)` + `store.registry_uri(...)` (services/service-com-amazonaws-ecrpublic-spencerfrontendservice/src/ops/create_repository.rs:299-305), i.e. it publishes the request's own Host verbatim. (The specific host and port are environment-dependent and are NOT the requirement; the missing `public.ecr.` service labels are.)

Why: the repositoryUri is the one value a client feeds to `docker push` / `docker pull` and to registry tooling. Tooling that recognises an ECR Public registry by the `public.ecr.*` authority — credential helpers, registry-type detection, image-reference parsers that require a dotted registry authority to tell a registry from a Docker Hub namespace — misroutes or rejects the URI the emulator hands back, and any assertion that a public repository's URI is a public-gallery address fails. Everything else in the authorization flow is correct, so the failure surfaces only at the point a client would try to use the address.

Fix: the ECR Public state's URI publication helper (`published_gallery_authority` / `registry_uri` in services/service-com-amazonaws-ecrpublic-spencerfrontendservice/src/state.rs), which both CreateRepository and DescribeRepositories read, must prepend the `public.ecr.` service labels to the published authority (keeping the target's own domain, so the address still reaches the emulator) rather than emitting the bare host:port.

Traces: failing check `ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action`; the assertion is at check line 258 (`assert _public_ecr_host(host) and path.count("/") >= 1`). Construction site: services/service-com-amazonaws-ecrpublic-spencerfrontendservice/src/ops/create_repository.rs:299-305.

## Evidence
Check: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action [mismatch]

All prior beats pass: the scoped IAM user's read-only policy permits DescribeRepositories and GetAuthorizationToken; CreateRepository is refused AccessDeniedException with the exact message `User: <user arn> is not authorized to perform: ecr-public:CreateRepository on resource: arn:aws:ecr-public::<account>:repository/<repo> because no identity-based policy allows the ecr-public:CreateRepository action`; the repository is absent (RepositoryNotFoundException); after adding ecr-public:CreateRepository the identical call succeeds and DescribeRepositories reports the repositoryName and registryId correctly.

Then, on DescribeRepositories of the created repository:

Expected (AWS): repositoryUri of the form `public.ecr.<domain>/<alias>/<name>` — authority begins with the labels `public`, `ecr`, then the target's own domain; path is `<registry alias>/<repository name>`.

Actual (emulator): repositoryUri == '127.0.0.1:37317/n7v9k4n5/sim2real-behav-ecrpub-iamrepo-6c1919433037'

AssertionError: the repository's URI is '127.0.0.1:37317/n7v9k4n5/sim2real-behav-ecrpub-iamrepo-6c1919433037'; an ECR Public repository is public.ecr.<domain>/<alias>/<name>

The host/port, alias and name suffix shown are per-run values; the divergence is the absence of the fixed `public.ecr.` labels in the authority.

## Steps to Reproduce
1. Against the ecr-public endpoint (us-east-1), call CreateRepository(repositoryName='<some-name>') as a principal allowed ecr-public:CreateRepository.
2. Read the repository back with DescribeRepositories(repositoryNames=['<some-name>']).
3. Split repositories[0].repositoryUri on the first '/': the authority is the emulator's bare host:port (e.g. '127.0.0.1:37317') instead of 'public.ecr.<domain>'. AWS's authority starts with the labels 'public' and 'ecr' followed by the target's own domain, with path '<alias>/<name>'.
4. The same wrong authority is reported by CreateRepository's own response (ops/create_repository.rs builds it from published_gallery_authority(ctx)).
