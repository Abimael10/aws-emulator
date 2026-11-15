Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.replicated-image-is-byte-identical-in-the-destination-region
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.replicated-image-is-byte-identical-in-the-destination-region|run#26|Timeout|time.sleep(5.0)
Issue-Commit: da3e7884167b3f933d6f1406ab01004ac62fd391

# ECR cross-Region replication delivers nothing to the destination Region: the replicated tag never resolves there, so the check exhausts its budget

> _Note: this issue was identified against the code base at commit `da3e7884167b3f933d6f1406ab01004ac62fd391`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutReplicationConfiguration

## Description
One cause, in ECR. Note: the open issue 20261024T014332Z-ecr-asynchronous-effects-never-occur-and-several-enforcement.md names this same check in its trace list, but under a different replay key (an AssertionError on the destination repository's ARN Region at `run#31`); the check now fails earlier and on a different signature (the replay harness does not treat it as owned), so this is filed separately and that issue does not cover this symptom.

Cause 1 of 1: a registry replication rule is accepted and echoed, but no image is ever replicated into the destination Region, so a destination-Region client can never observe the replicated image.

AWS: Smithy: PutReplicationConfiguration(replicationConfiguration: ReplicationConfiguration required) -> ReplicationConfiguration; DescribeRegistry echoes it; DescribeImageReplicationStatus returns replicationStatuses: ImageReplicationStatusList (region, registryId, status, failureCode). Recorded behavior for the end-to-end effect: after a rule whose destinations name {region: <dest>, registryId: <account>} and whose repositoryFilters are [{filter: <prefix>, filterType: PREFIX_MATCH}] is in place BEFORE a push, ECR auto-creates a same-named repository in the destination Region and makes the pushed tag resolvable there within minutes — DescribeImages(repositoryName=<name>, imageIds=[{imageTag: <tag>}]) in the destination Region returns an imageDetails entry whose imageDigest equals the source digest, BatchGetImage there returns the identical manifest bytes, and GetDownloadUrlForLayer there serves a blob hashing to the source layer digest. A repository that matches no filter is NOT replicated: DescribeRepositories in the destination Region refuses it with HTTP 400, code RepositoryNotFoundException and the message whose fixed text is "The repository with name '<repositoryName>' does not exist in the registry with id '<registryId>'" — the quoted repository name comes from the request and the registry id from the caller's account, nothing else in the text varies.

Emulator: PutReplicationConfiguration and DescribeRegistry answer (the check's pre-flight assertion that DescribeRegistry echoes exactly one rule naming the destination Region passes, so the registry is configured), the source-side uploads and PutImage succeed, and then the destination-Region read never produces the image: the check's bounded poll of ecr_dest.describe_images(repositoryName=<replicated name>, imageIds=[{imageTag: 'replicated'}]) — 5 s cadence, 240 s budget, breaking only on success and swallowing only RepositoryNotFoundException/ImageNotFoundException — never breaks, and the run is cut off by the harness's 60 s cap with the verdict "Check timed out after 60.0s". Hypothesis: that destination poll is where the budget is spent — it is the only unbounded wait reached before any destination-side assertion in this check, and every preceding step is a single fast control-plane call that would have failed fast rather than hung.

Why: every cross-Region replication workflow is unobservable and unusable. A client that pushes to a source Region and then pulls, scans, signs or deploys from the destination Region waits forever and then sees RepositoryNotFoundException/ImageNotFoundException for content AWS would have delivered; a disaster-recovery or multi-Region rollout path built on replication cannot be exercised at all, and a test that asserts the replica is byte-identical (same manifest digest, same layer bytes) can never reach its assertions.

Fix: the replication engine behind PutReplicationConfiguration — on PutImage/CompleteLayerUpload, every rule whose repositoryFilters select the repository must create the same-named repository in each destination registry/Region and copy the manifest bytes and layer blobs verbatim (no re-serialisation, so the digest is preserved), and must leave a repository that matches no filter absent in the destination so the destination-Region RepositoryNotFoundException refusal still holds. This belongs in the ECR registry/replication state and the PutImage handler, not in the destination-Region read handlers.

Traces: ecr.replicated-image-is-byte-identical-in-the-destination-region.

## Evidence
Cause 1 — check ecr.replicated-image-is-byte-identical-in-the-destination-region, verdict mismatch, detail "Check timed out after 60.0s".
Expected: within the check's DEST_TIMEOUT_S=240 s poll, ecr_dest.describe_images(repositoryName=<prefix-matched repo>, imageIds=[{imageTag: 'replicated'}]) returns imageDetails[0] with imageDigest == the source manifest digest and imageTags == ['replicated']; ecr_dest.batch_get_image returns failures == [] and the identical manifest bytes; ecr_dest.get_download_url_for_layer serves HTTP 200 whose body hashes to the source layer digest; and DescribeRepositories in the destination Region for the unfiltered control repository refuses with {"Code": "RepositoryNotFoundException", "Message": "The repository with name '<unfiltered name>' does not exist in the registry with id '<account>'"}.
Actual: nothing is ever replicated — the destination poll keeps catching RepositoryNotFoundException/ImageNotFoundException and never breaks, so the check never reaches any of the above assertions and is cut off at the harness's 60 s cap. The check's own pre-flight assertions passed first: DescribeRegistry echoed exactly one rule whose destinations[0].region is the destination Region, and PutImage at the source minted the content-addressed digest of the pushed manifest bytes.

## Steps to Reproduce
1. ecr.put_replication_configuration(replicationConfiguration={'rules': [{'destinations': [{'region': <a Region other than the client's>, 'registryId': <account>}], 'repositoryFilters': [{'filter': <prefix>, 'filterType': 'PREFIX_MATCH'}]}]}) and confirm ecr.describe_registry() echoes the rule.
2. ecr.create_repository(repositoryName=<prefix>...) (matches the filter) and a second repository whose name does not start with that prefix (the control).
3. In each repository, push a single-layer image: initiate_layer_upload / upload_layer_part / complete_layer_upload for the gzipped layer and for the config blob, then put_image(imageManifest=<docker v2 manifest>, imageManifestMediaType='application/vnd.docker.distribution.manifest.v2+json', imageTag='replicated').
4. With a client bound to the destination Region, poll describe_images(repositoryName=<prefix repo>, imageIds=[{'imageTag': 'replicated'}]). Observed: it keeps answering RepositoryNotFoundException/ImageNotFoundException indefinitely. Expected: the imageDetails entry appears with the source digest within minutes.
5. Then (unreachable today) batch_get_image and get_download_url_for_layer in the destination Region for the same digests, and describe_repositories in the destination Region for the control repository, which must still be refused with RepositoryNotFoundException.
