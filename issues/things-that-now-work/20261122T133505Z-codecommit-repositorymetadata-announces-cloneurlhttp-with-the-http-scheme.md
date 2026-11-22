Issue-Source: trace-sweep-divergence
Issue-Trace: codecommit-repository-lifecycle-and-metadata
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codecommit-repository-lifecycle-and-metadata
Issue-Shape-Id: com.amazonaws.codecommit#CodeCommit_20150413
Issue-Key: codecommit|BatchGetRepositories|2xx->2xx|$.repositories[*].cloneUrlHttp
Issue-Key: codecommit|BatchGetRepositories|2xx->2xx|$.repositories[*].cloneUrlSsh
Issue-Commit: 27c72d454eafc35a12aac6fa1d715ada36555a31

# CodeCommit repositoryMetadata announces cloneUrlHttp with the http scheme instead of an https CodeCommit clone URL

> _Note: this issue was identified against the code base at commit `27c72d454eafc35a12aac6fa1d715ada36555a31`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetRepositories

## Description
One net-new codecommit wire cause: every operation that returns a RepositoryMetadata announces the repository's HTTPS clone URL under the plain `http` scheme, so the value is not an HTTPS URL at all. BatchGetRepositories is the operation the replay caught it on; the value comes from the shared announced-git-endpoint state, so GetRepository/CreateRepository/UpdateRepositoryName report the same thing.

Cause 1 of 1: cloneUrlHttp is an http:// URL where AWS always returns an https:// one.
AWS: Smithy com.amazonaws.codecommit#BatchGetRepositories takes repositoryNames (@required RepositoryNameList) and returns repositories (RepositoryMetadataList of RepositoryMetadata) plus repositoriesNotFound/errors. RepositoryMetadata.cloneUrlHttp (target CloneUrlHttp) is documented 'The URL to use for cloning the repository over HTTPS' and RepositoryMetadata.cloneUrlSsh 'The URL to use for cloning the repository over SSH'. The model states the scheme of each field and nothing further, so the rest is recorded behavior: AWS fills them with the service's regional CodeCommit git origin, of the shape 'https://git-codecommit.<region>.amazonaws.com/v1/repos/<repositoryName>' and 'ssh://git-codecommit.<region>.amazonaws.com/v1/repos/<repositoryName>'. Of that template the FIXED parts are the scheme (`https` for cloneUrlHttp, `ssh` for cloneUrlSsh) and the '/v1/repos/' path prefix; the VARIABLE parts are the region and the repository name. The authority an emulator serves git on is an environment value, so the host and port in the recording are NOT the requirement here — the scheme is, because cloneUrlHttp is contractually the HTTPS endpoint and clients drive AWS's documented credential flows off it.
Emulator: the response is HTTP 200 with the right shape, the right repositoryName ordering and a correct '/v1/repos/<repositoryName>' path, but cloneUrlHttp comes back as 'http://<emulator-host>:<port>/v1/repos/<repositoryName>' — plain http, no TLS — while cloneUrlSsh is 'ssh://<emulator-host>/v1/repos/<repositoryName>' (correct scheme, environment authority). So the only non-environmental divergence is cloneUrlHttp's scheme: the field named and documented as the HTTPS clone URL never carries https.
Why: a client relying on AWS's behavior treats cloneUrlHttp as an HTTPS origin. `aws codecommit credential-helper` only answers a credential query whose protocol is https and signs the HTTPS clone URL, so following AWS's documented clone instructions against this value yields no credentials at all; a client or CI policy that refuses to send a git credential (or any secret) over a non-TLS origin refuses to use the URL; and any caller that string-checks or re-derives the region from the announced https origin gets nothing it can parse. Pushing or cloning with a user name and password over this URL sends the secret in the clear.
Fix: the announced git endpoint in the codecommit service's state (the git-endpoint construction that `ensure_git_endpoint` publishes and that the RepositoryMetadata mapper copies into cloneUrlHttp) must publish the HTTP clone URL under the `https` scheme on an origin the emulator actually serves git TLS on, rather than hard-coding the `http` scheme; cloneUrlSsh already carries the right scheme and needs no change.
Related but NOT covering this divergence: open issue 20261120T182842Z-codecommit-git-transport-authenticates-nobody-any-password-and.md (cause 3 of 3) and 20261025T040000Z-codecommit-announced-clone-url-is-served-with-an-untrusted-certificate.md both discuss the announced clone URL, and 20261109T165809Z-cloudformation-codecommit-3-net-new-causes-aws-codecommit.md covers the CFN GetAtt clone-URL attributes; all three are behavioral findings and none carries this cluster's wire replay key (the replay reports owned_by empty and stub_path empty for it), so re-replaying this fixture step is not gated by any of them.
Traces: codecommit-repository-lifecycle-and-metadata#task#10 (representative).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true ('/__admin/health answered 200 and the :34107 data listener accepts'), bootstrap_signal=false, 50 fixtures replayed, 403 steps, 394 passed, 9 mismatched, 3 divergence clusters, score 97.8%. All three clusters are service=codecommit; two are owned_by open issue 20261117T170442Z-codecommit-response-fidelity-listfilecommithistory-re-dates-stored-commits.md (ListFileCommitHistory revisionDag dates, TestRepositoryTriggers branch-limit message) and are skipped. This is the remaining cluster: divergence_key 'service=codecommit|BatchGetRepositories|status=200->200|keys=$.repositories[0].cloneUrlHttp,$.repositories[0].cloneUrlSsh,$.repositories[1].cloneUrlHttp,$.repositories[1].cloneUrlSsh', fixture codecommit-repository-lifecycle-and-metadata, step task#10, verdict mismatch, affected_count 1 (the cluster's only member is the representative), stub_path empty, owned_by empty.

Status 200 expected and observed; the body diverges on the clone-URL members of both returned repositories:
- $.repositories[*].cloneUrlHttp: value_mismatch — expected 'https://git-codecommit.<region>.amazonaws.com/v1/repos/<repositoryName>' (the recording captured region us-west-2 and the two per-run repository names), got 'http://<emulator-host>:<port>/v1/repos/<repositoryName>' (the recording captured 127.0.0.1:41041). Scheme http vs https; path prefix '/v1/repos/' and the repository-name suffix agree.
- $.repositories[*].cloneUrlSsh: value_mismatch — expected 'ssh://git-codecommit.<region>.amazonaws.com/v1/repos/<repositoryName>', got 'ssh://<emulator-host>/v1/repos/<repositoryName>'. Scheme and path agree; only the authority differs.

The host, port, region and the generated repository names in the recorded strings are per-run/environment values and are explicitly NOT the requirement being filed; the filed requirement is the scheme of cloneUrlHttp (https, per the member's Smithy documentation 'The URL to use for cloning the repository over HTTPS') together with the '<scheme>://<origin>/v1/repos/<repositoryName>' shape. Model facts read from services/service-com-amazonaws-codecommit-codecommit-20150413/smithy-model/com-amazonaws-codecommit-codecommit-20150413.json (operation BatchGetRepositories; shape RepositoryMetadata, members cloneUrlHttp/cloneUrlSsh).

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and point botocore at it: cc = boto3.client('codecommit').
2. Create two repositories: cc.create_repository(repositoryName=R1); cc.create_repository(repositoryName=R2).
3. Call cc.batch_get_repositories(repositoryNames=[R2, R1]) and read repositories[*]['cloneUrlHttp'] and repositories[*]['cloneUrlSsh'].
4. Expect cloneUrlHttp to be an https:// URL of the form 'https://<codecommit git origin>/v1/repos/<repositoryName>' (AWS: https://git-codecommit.<region>.amazonaws.com/v1/repos/<repositoryName>) and cloneUrlSsh to be the ssh:// form of the same path. Observe cloneUrlHttp returned with the 'http' scheme (e.g. 'http://127.0.0.1:<port>/v1/repos/<repositoryName>'); the host and port are expected to be the emulator's own, the scheme is not.
5. Call cc.get_repository(repositoryName=R1) and confirm the same cloneUrlHttp scheme is announced there, i.e. the defect is in the shared announced git endpoint rather than in BatchGetRepositories' serializer.
6. Confirm the URL is not TLS: an https:// request to the announced authority is not served, which is what makes `git -c credential.helper='!aws codecommit credential-helper $@' clone <cloneUrlHttp>` obtain no user name.
7. Re-replay fixture codecommit-repository-lifecycle-and-metadata step task#10; it must pass (with host/port normalisation, as the corpus already applies to environment authorities).
