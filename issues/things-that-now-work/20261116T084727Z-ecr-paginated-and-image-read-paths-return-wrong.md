Issue-Source: trace-sweep-divergence
Issue-Trace: ecr-repository-pagination-and-scoping-conflict
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecr-repository-pagination-and-scoping-conflict ecr-image-describe-pagination-and-params
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr|DescribeImages|2xx->2xx|$.imageDetails[*]
Issue-Related: 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md
Issue-Commit: a1915270e40223bfb23a5a7de86fe74aa15a3df0

# ECR paginated and image read paths return wrong record contents: second page repeats an entry, imageSizeInBytes is miscomputed

> _Note: this issue was identified against the code base at commit `a1915270e40223bfb23a5a7de86fe74aa15a3df0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
2 net-new causes. Both are read-path content defects: the records the emulator serves are not the records AWS serves for the same state — one in the page window DescribeRepositories computes from a nextToken, one in the per-image size DescribeImages reports.

Cause 1 of 2: DescribeRepositories' continuation page starts one entry too early, so the last entry of the previous page is served twice and the last repository in the set is never served.
AWS: Smithy: ecr DescribeRepositories is @paginated(inputToken: nextToken, outputToken: nextToken, items: repositories, pageSize: maxResults). AWS's contract for a paginated list is that consecutive pages partition the result set: the page fetched with the nextToken returned by the previous call begins at the first element AFTER the elements already returned, so no element is repeated and none is dropped; iterating until nextToken is absent yields every repository exactly once.
Emulator: the second page answers 200 but its window is shifted back by one — its first element is the element that already closed the previous page, and the set's final element never appears. In the recorded run the emulator's page is [<repo A>, <repo B>] where AWS's is [<repo B>, <repo C>] (recorded names: got 'sim2real-probe-ecr-pag-57e211c7' then 'sim2real-probe-ecr-pag-9ef181b1' where the recording has 'sim2real-probe-ecr-pag-9ef181b1' then 'sim2real-probe-ecr-pag-ae27ec07'); the generated repository names are per-run values, the off-by-one window is the defect. Hypothesis: the offset encoded in / decoded from nextToken counts pages or is applied before the previous page's last element is consumed, rather than being the count of elements already emitted.
Why: a client paging DescribeRepositories gets a duplicate and silently loses a repository — a sweep that deletes, re-tags or audits every repository skips one and processes another twice, with no error to notice.
Fix: the DescribeRepositories handler's pagination (nextToken encode/decode and the slice it selects) — the token must denote the number of elements already returned over a stable ordering.
Traces: ecr-repository-pagination-and-scoping-conflict#task#4. (The same step also reports `host_not_own` on the repositoryUri authority; that authority fault is a separate bundle, filed as 'ECR announces a repositoryUri host the emulator does not serve'.)

Cause 2 of 2: DescribeImages reports an imageSizeInBytes that is not the image's layer size, and that varies between images whose layers are the same size.
AWS: Smithy: ecr ImageDetail.imageSizeInBytes — "The size, in bytes, of the image in the repository. If the image is a manifest list, this will be the max size of all manifests in the list." The value is derived from the pushed manifest's layer sizes (the compressed blobs), so two images pushed with equal-sized layers report the same imageSizeInBytes; it is deterministic from the push, not from the run. Recorded behavior confirms it: all images pushed by the fixture report an identical size.
Emulator: DescribeImages answers 200 but the imageSizeInBytes of the second and third images in the repository differ from the first and from each other, by small per-image amounts (recorded run: 155 for every image in the recording vs 156 and 157 from the emulator, while the first image's value matched). The per-element comparison therefore reports $.imageDetails[1] and $.imageDetails[2] as missing/extra. Hypothesis: the emulator measures the serialized manifest document (or the manifest plus per-image metadata), whose length varies with each image's digests/tags, instead of summing the manifest's layer `size` fields.
Why: a client that uses imageSizeInBytes to size a cache, enforce an image-size budget, or compare two tags of the same image sees sizes that neither match the registry's blobs nor agree between identical-sized images, so size-based lifecycle or admission logic diverges from AWS.
Fix: the ECR image state / DescribeImages serializer — compute imageSizeInBytes once at PutImage time from the manifest's layer sizes (max over manifests for a manifest list) and report that stored value.
Traces: ecr-image-describe-pagination-and-params#task#12.

## Evidence
Cause 1 — DescribeRepositories, fixture ecr-repository-pagination-and-scoping-conflict step task#4: expected HTTP 200, actual HTTP 200; divergence `$.repositories[0].repositoryUri` expected '...pag-9ef181b1' got '...pag-57e211c7' and `$.repositories[1].repositoryUri` expected '...pag-ae27ec07' got '...pag-9ef181b1' — i.e. the emulator's page begins one element earlier than AWS's, repeating the previous page's last entry and dropping the set's last entry. The literal names are per-run; the shifted window is the requirement violated.
Cause 2 — DescribeImages, fixture ecr-image-describe-pagination-and-params step task#12: expected HTTP 200, actual HTTP 200; `$.imageDetails[1]` and `$.imageDetails[2]` reported missing/extra. The only non-per-run field that differs is imageSizeInBytes: the recording reports the same size for every pushed image (155), the emulator reports 156 for the 'v2' image and 157 for the 'v3' image while imageDetails[0] matched. imagePushedAt also differs but is a per-run clock value and is not part of this finding (element [0] matched despite it).
No status, error code or error message divergence in either cause.

## Steps to Reproduce
Cause 1:
1. CreateRepository three times so the account holds more repositories than one page.
2. Call DescribeRepositories with a small maxResults, note the names returned and the nextToken.
3. Call DescribeRepositories again with that nextToken: the first element of the second page is the last element of the first page, and the final repository of the set never appears in any page.
4. Replay fixture ecr-repository-pagination-and-scoping-conflict step task#4.
Cause 2:
1. CreateRepository, then PutImage three times with V2 manifests whose layer blobs are the same size, under tags v1/v2/v3.
2. Call DescribeImages for the repository and compare imageDetails[*].imageSizeInBytes: AWS reports the same size for all three; the emulator reports values that drift by one or two bytes per image.
3. Replay fixture ecr-image-describe-pagination-and-params step task#12.
