Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-algorithm-duplicate-and-missing-errors
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-algorithm-duplicate-and-missing-errors sagemaker-algorithm-lifecycle sagemaker-model-package-create-and-verify sagemaker-model-package-metric-numeric-precision-preserved
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|DescribeAlgorithm|2xx->2xx|$.TrainingSpecification.TrainingImageDigest
Issue-Key: sagemaker|DescribeModelPackage|2xx->2xx|$.CreatedBy.IamIdentity.Arn
Issue-Key: sagemaker|DescribeModelPackage|2xx->2xx|$.InferenceSpecification.Containers[*].ImageDigest
Issue-Commit: 242873f3341a0220639d323ce35a2b91fad7ef25

# sagemaker algorithm and model-package describes report an invented ECR image digest instead of the referenced image's digest

> _Note: this issue was identified against the code base at commit `242873f3341a0220639d323ce35a2b91fad7ef25`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlgorithm

## Description
Two net-new wire divergence clusters in the sagemaker emulator share one fault: a Describe response field that AWS derives by resolving the referenced Amazon ECR container image (and, in the second cluster, the authenticated caller) is instead filled with a value the emulator synthesizes locally. Both steps answer 200, so a client sees a well-formed registry entry carrying a digest that identifies no image in the registry. TWO causes must be closed.

Cause 1 of 2: DescribeAlgorithm reports a TrainingSpecification.TrainingImageDigest that is not the digest of the training image the algorithm references (fixture sagemaker-algorithm-duplicate-and-missing-errors, step task#4).
AWS: Smithy (sagemaker) DescribeAlgorithmOutput member TrainingSpecification targets TrainingSpecification; its member TrainingImage targets ContainerImage (@length 0..255, @pattern ^[\S]+$) and is documented as "The Amazon ECR registry path of the Docker image that contains the training algorithm", and its member TrainingImageDigest targets ImageDigest (@length 0..72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$), documented as the hash "that identifies the Docker image used for training". So the value is a property of the referenced ECR image, not of the algorithm record: its fixed text is the literal prefix `sha256:` followed by the 64 lowercase hex characters of that image's content digest, which is the variable part and is taken from the registry entry the request's TrainingImage points at. The recording shows the registered algorithm's image resolving to `sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e`, and the same digest is recorded for the same image in the model-package cluster below — i.e. AWS reports one digest per image, stably, across operations and fixtures.
Emulator: answers a different 64-hex digest for the same image (`sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a`). The value is well-formed and stable, and the emulator returns that same wrong digest for the same image under DescribeModelPackage too, so the digest is being derived from the emulator's own inputs (the stored algorithm/model-package record or a local hash of the request) rather than resolved from the ECR image the request named.
Why: the image digest is how a client pins an immutable image. Tooling compares TrainingImageDigest against the digest it obtained from ECR (DescribeImages / the registry manifest) before it will launch a training job, and reproducibility and supply-chain checks pull `registry/repository@<digest>` built from this field. Against the emulator that comparison always fails and the pinned pull reference points at a manifest that does not exist, so a client that enforces digest pinning cannot use a registered algorithm at all.
Fix: the sagemaker crate's algorithm registration/Describe path — where TrainingSpecification.TrainingImageDigest is populated, resolve the digest of the ECR image named by TrainingSpecification.TrainingImage (the same digest the emulator's registry reports for that image) instead of computing one from the algorithm record.
Traces: sagemaker-algorithm-duplicate-and-missing-errors#task#4 (representative); also sagemaker-algorithm-lifecycle#task#3.

Cause 2 of 2: DescribeModelPackage reports the same invented container digest and additionally names a randomly generated IAM principal as the creator (fixture sagemaker-model-package-create-and-verify, step task#3).
AWS: Smithy (sagemaker) DescribeModelPackageOutput member InferenceSpecification carries Containers, a list of ModelPackageContainerDefinition whose member Image targets ContainerImage ("The Amazon Elastic Container Registry (Amazon ECR) path where inference code is stored") and whose member ImageDigest targets ImageDigest (@length 0..72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$) — again the digest of that referenced image, `sha256:` plus the registry's 64 hex characters. Output member CreatedBy targets UserContext ("Information about the user who created or modified a SageMaker resource"), whose IamIdentity targets IamIdentity, whose Arn member is "The Amazon Resource Name (ARN) of the IAM identity" — the identity whose credentials signed the CreateModelPackage request. Its fixed text is the ARN template `arn:aws:iam::<account-id>:user/<user-name>`, with the account id and user name taken from the request's SigV4 credential; the recording's caller signs as the user `emulator`, so the recorded value is `arn:aws:iam::000000000000:user/emulator`.
Emulator: InferenceSpecification.Containers[0].ImageDigest is the same locally derived `sha256:a9a77d47...` value seen in cause 1 rather than the image's recorded `sha256:8ebddbc1...`, and CreatedBy.IamIdentity.Arn is `arn:aws:iam::000000000000:user/EMULATOR<hex>` — a per-resource invented principal name with no relation to the signing credential (neighbouring clusters show different `EMULATOR<hex>` names for the same caller).
Why: a client pinning the inference container by digest, or attributing/filtering registry entries by comparing CreatedBy.IamIdentity.Arn with its own identity (sts:GetCallerIdentity) or a known user/role ARN, gets a mismatch every time: the digest resolves to no manifest, and two packages created by the same principal report different creators, so digest pinning, ownership filters and least-privilege checks all mis-classify.
Fix: the sagemaker crate's model-package registration/Describe path — resolve ModelPackageContainerDefinition.ImageDigest from the referenced ECR image (shared with the algorithm path in cause 1), and populate UserContext.IamIdentity from the request's authenticated SigV4 principal when the model package is recorded. The open issue 20261113T193514Z-sagemaker-resource-provenance-and-export-artifact-response-fields.md describes the same invented-principal fault for DescribeModelPackageGroup and DescribeHub and does NOT own this DescribeModelPackage cluster, which the replay reports unowned.
Traces: sagemaker-model-package-create-and-verify#task#3 (representative); also sagemaker-model-package-metric-numeric-precision-preserved#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, 497 steps, 394 passed, score 79.3%). Two unowned, non-stub divergence clusters, both service=sagemaker:

1) sagemaker DescribeAlgorithm, sagemaker-algorithm-duplicate-and-missing-errors#task#4 — expected status 200, got 200; $.TrainingSpecification.TrainingImageDigest: value_mismatch (expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e', got 'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a'). affected_count 2: also sagemaker-algorithm-lifecycle#task#3. owned_by: [] (unowned), stub_path: "".

2) sagemaker DescribeModelPackage, sagemaker-model-package-create-and-verify#task#3 — expected status 200, got 200; $.InferenceSpecification.Containers[0].ImageDigest: value_mismatch (expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e', got 'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a'); $.CreatedBy.IamIdentity.Arn: value_mismatch (expected 'arn:aws:iam::000000000000:user/emulator', got 'arn:aws:iam::000000000000:user/EMULATORCCDF3E18977C'). affected_count 2: also sagemaker-model-package-metric-numeric-precision-preserved#task#4. owned_by: [] (unowned), stub_path: "".

Cross-cluster evidence that the digest is derived locally rather than resolved: both clusters expect the SAME digest (sha256:8ebddbc1...) for the referenced image and both receive the SAME wrong digest (sha256:a9a77d47...), stably, across four fixtures.

Model evidence (Smithy, com.amazonaws.sagemaker): TrainingSpecification.TrainingImage -> ContainerImage ("Amazon ECR registry path of the Docker image that contains the training algorithm"); TrainingSpecification.TrainingImageDigest -> ImageDigest (@length 0..72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$); ModelPackageContainerDefinition.Image -> ContainerImage, .ImageDigest -> ImageDigest (same constraints); UserContext.IamIdentity -> IamIdentity, IamIdentity.Arn -> String ("The Amazon Resource Name (ARN) of the IAM identity").

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.

Cause 1: replay fixture sagemaker-algorithm-duplicate-and-missing-errors, step task#4 (CreateAlgorithm with a TrainingSpecification.TrainingImage pointing at an ECR image, then DescribeAlgorithm) and read $.TrainingSpecification.TrainingImageDigest — it is a digest the emulator derived itself (sha256:a9a77d47...), not the digest the referenced image resolves to in the registry (sha256:8ebddbc1...). Same via sagemaker-algorithm-lifecycle#task#3.

Cause 2: replay fixture sagemaker-model-package-create-and-verify, step task#3 (CreateModelPackage with an InferenceSpecification container image, signed as user 'emulator', then DescribeModelPackage) and read $.InferenceSpecification.Containers[0].ImageDigest (same locally derived digest) and $.CreatedBy.IamIdentity.Arn (arn:aws:iam::<account>:user/EMULATOR<hex> instead of the signing principal's ARN). Same via sagemaker-model-package-metric-numeric-precision-preserved#task#4.

To confirm the digest is image-scoped and not resource-scoped, compare the digest the two Describes return for the same image path: AWS returns one digest for both, the emulator returns its own single wrong value for both.
