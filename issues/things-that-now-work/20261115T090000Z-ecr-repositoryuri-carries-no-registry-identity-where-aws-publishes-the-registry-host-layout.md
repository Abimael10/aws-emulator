Issue-Source: code-review-followup
Issue-Severity: LOW

# A repository's published address carries the registry identity AND an address a client can dial

## Operation
`CreateRepository`, `DescribeRepositories`, `DeleteRepository` — every Amazon ECR operation that
reports a `Repository` (`Repository$repositoryUri`), plus the `AWS::ECR::Repository`
`RepositoryUri` attribute CloudFormation publishes from it and the `ContainerDefinition$image`
Amazon ECS stores from that.

## Description
`Repository$repositoryUri` is read two ways at once, and both readings are requirements:

* a client PARSES it for the registry that holds the repository — which is what lets it recover
  the owning account and the Region and build an `ecr:` ARN, a `docker login` target or a
  compute service's image reference out of the one value it was handed. AWS publishes that as
  `aws_account_id.dkr.ecr.region.amazonaws.com/<name>`, the registry URL form the model states
  on `AuthorizationData$proxyEndpoint`;
* a container client DIALS it, so the authority has to resolve and answer on the machine the
  emulator runs on. AWS's own zone does not: it resolves at genuine AWS infrastructure, which
  would send the next client out of the emulated environment carrying the credential its
  `docker login` was issued.

The tree held those two as alternatives for a while — a bare `host:port` served one reading and
AWS's layout served the other — and it now serves both as ONE rule: the registry host layout
AWS publishes, spelled in the ZONE and on the PORT of the endpoint this emulator answers that
caller's registry requests at, composed in one place
(`services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state/registry_address.rs`,
`published_registry_authority`), with the registry's own `/v2` plane answering on the host it
publishes so a push or pull at the published address reaches the very repository it names. An
address inside the zone AWS serves its own endpoints in is still refused.

## Why this matters
The published address is the only value many consumers get: a stack output, a task definition's
image, a Lambda `ImageUri`. A caller that parses it for the registry recovers the account and
the Region from its leading labels, and a caller that dials it reaches this process — neither
reading is served at the other's expense.

## Steps to Reproduce
1. `CreateRepository` with `repositoryName` `demo` against an emulator served at
   `https://127.0.0.1:9297`.
2. Read `repository.repositoryUri`: `000000000000.dkr.ecr.<region>.localhost:9297/demo` — AWS's
   label layout, in the zone and on the port this emulator is reached at.
3. `DescribeRepositories` reports the same string, and the `registryId` and `repositoryArn`
   beside it name the same account and Region.

## Resolution
Implemented by `published_registry_authority` in that crate and pinned on three sides: the
crate's own `tests/all/repository_registry_uri.rs` and `tests/all/published_registry_address.rs`,
and the cross-service `core/router/tests/all/ecr_repository_uri_is_a_registry_authority_for_its_consumers.rs`,
which reads the address back through CloudFormation's published attribute, Amazon ECR's own
`DescribeRepositories` and the image Amazon ECS stored, over four served addresses (an IPv4
socket, an IPv6 socket, an operator-chosen host with a port and one with none) in four Regions
of three partitions.
