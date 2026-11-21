Issue-Source: trace-sweep-divergence
Issue-Trace: appstream-catalog-describe-images
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appstream-catalog-describe-images
Issue-Shape-Id: com.amazonaws.appstream#PhotonAdminProxyService
Issue-Key: appstream|DescribeImages|2xx->4xx|$.Images
Issue-Key: appstream|DescribeImages|2xx->4xx|status
Issue-Commit: 8abe98066d21e850555e9c7202212c4bed572c90

# appstream DescribeImages answers HTTP 400 with no Images instead of the published image catalog

> _Note: this issue was identified against the code base at commit `8abe98066d21e850555e9c7202212c4bed572c90`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeImages

## Description
Cause 1 of 1: every recorded DescribeImages read of the AppStream image catalog is refused with HTTP 400 and no `Images` member, instead of being answered 200 with the images visible to the account.

AWS: Smithy (services/service-com-amazonaws-appstream-photonadminproxyservice/smithy-model/com-amazonaws-appstream-photonadminproxyservice.json) — DescribeImages input {Names: StringList, Arns: ArnList, Type: VisibilityType (PRIVATE|PUBLIC|SHARED), NextToken: String @length min 1, MaxResults: DescribeImagesMaxResults @range 0..25}, output DescribeImagesResult {Images: ImageList, NextToken}, paginated (inputToken NextToken / outputToken NextToken / pageSize MaxResults), modeled errors InvalidParameterCombinationException (httpError 400) and ResourceNotFoundException (httpError 404). Operation doc: "Retrieves a list that describes one or more specified images, if the image names or image ARNs are provided. Otherwise, all images in the account are described." So a read that addresses images AWS publishes — by `Names`, by `Arns`, or by naming none at all — is a success: HTTP 200 carrying one modeled `Image` per addressed image, each with the accountless published ARN form arn:aws:appstream:<region>::image/<name>, Visibility PUBLIC, State AVAILABLE, ImageBuilderSupported true, ImageType NATIVE, its Platform, SupportedInstanceFamilies, CreatedTime and PublicBaseImageReleasedDate (Smithy: Image shape). A 4xx is reserved for the two modeled failures: both Names and Arns in one request (InvalidParameterCombinationException, 400) and a name/ARN addressing no image at all (ResourceNotFoundException, modeled httpError 404). Recorded behavior only: the recorded 200 bodies also carry members the model does not declare on Image — DynamicApplicationCatalogEnabled, RootVolumeConfig {VolumeSizeInGb, VolumeType}, appStreamRecipeArn, appStreamRecipeReleaseTimestamp — so the projection is expected to carry them too; the particular release dates, recipe versions and timestamps in one recording are data, not requirements.

Emulator: all eight recorded steps of this read answer HTTP 400 and the response carries no `Images` member at all (replay detail: "status: expected 200, got 400" / "$.Images: missing"). The read path is therefore unusable for the recorded catalog queries: not one image is enumerable.

Hypothesis: the store seeds a single hard-coded published batch — state/images.rs `PUBLISHED_RELEASE_DATES` is `&["2024-06-17"]`, and `seed_image_catalog()` mints names only as `AppStream-[Graphics-<accel>-]<os>-06-17-2024` — while the recording addresses images of later published batches (names ending `-02-18-2026`, `-04-16-2026`). A `Names`/`Arns` read naming an image the seed does not hold takes the handler's not-found branch, so a catalog read of real published images is refused rather than answered. Second, related observation grounded in the model: that branch's refusal is being served at HTTP 400, whereas Smithy gives ResourceNotFoundException httpError 404 (the handler module's own doc comment asserts "ResourceNotFoundException (HTTP 400)", contradicting the model), so even the genuine not-found case is reported under the wrong status.

Why: DescribeImages is how a client discovers which image to launch a fleet or an image builder from. A client that lists or looks up the published base images gets a hard 400 and no catalog, so image selection, fleet creation and image-builder creation cannot proceed at all against the emulator — and because the refusal is a 400 rather than the modeled 404, retry/branching logic that distinguishes "no such image" from "bad request" takes the wrong path even where a not-found answer would be correct.

Fix: this service's image state and the DescribeImages read. In services/service-com-amazonaws-appstream-photonadminproxyservice/src/state/images.rs the seeded catalog must cover the published base-image batches a client can actually address (the list of release batches is already the extension point), rather than one frozen batch; and in src/ops/describe_images.rs the name/ARN branch must answer the modeled ResourceNotFoundException status (404) for an image genuinely absent, leaving an addressable published image to be rendered 200 with the full Image projection, including the members the recordings carry beyond the modeled shape.

Traces: appstream-catalog-describe-images#task#1 (representative) and the same read at task#2, task#3, task#4, task#5, task#6, task#7, task#8 — one cluster, eight steps, all HTTP 400.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the booted emulator (emulator_up: true, /__admin/health 200). Divergence cluster service=appstream|DescribeImages|status=200->400|keys=$.Images,status, verdict mismatch, 8 affected steps (appstream-catalog-describe-images#task#1 .. #task#8), owned_by: [] and stub_path: "" (not owned by any open issue and not a stub).

Expected (recorded AWS response), step task#1: HTTP 200 with Images = [ {Name/DisplayName 'AppStream-Graphics-G4dn-RHEL8-02-18-2026', Arn 'arn:aws:appstream:us-west-2::image/AppStream-Graphics-G4dn-RHEL8-02-18-2026', Visibility 'PUBLIC', State 'AVAILABLE', ImageType 'NATIVE', ImageBuilderSupported true, Platform 'RHEL8', InstanceFamily 'GRAPHICS_G4', SupportedInstanceFamilies ['Graphics G4'], Byol false, ManagedSoftwareIncluded false, DynamicAppProvidersEnabled 'DISABLED', ImageSharedWithOthers 'FALSE', LatestAppstreamAgentVersion 'FALSE', RootVolumeConfig {VolumeSizeInGb 200, VolumeType 'gp2'}, Applications [], ImageErrors [], CreatedTime/PublicBaseImageReleasedDate set}, {the same for 'AppStream-Graphics-G4dn-RHEL8-04-16-2026'}, ... ] — the published catalog.
Actual (emulator): HTTP 400, and `$.Images` missing from the body entirely. Same outcome on all eight steps.

Model evidence: DescribeImages output Images: ImageList, SUCCESS_STATUS 200; errors InvalidParameterCombinationException httpError 400, ResourceNotFoundException httpError 404.
Code evidence for the hypothesis: services/service-com-amazonaws-appstream-photonadminproxyservice/src/state/images.rs line 316 `const PUBLISHED_RELEASE_DATES: &[&str] = &["2024-06-17"];` and lines 337-350 (name built as `AppStream-[Graphics-<accel>-]<os>-<MM-DD-YYYY>`), with src/ops/describe_images.rs lines 192-235 returning ResourceNotFoundException for a Name/Arn the seed does not hold, and that file's lines 8-9 documenting the refusal as "HTTP 400" against the model's 404.

NOT filed from this sweep, for the record: the other cluster in this corpus (appstream-asvpce-traj-access-endpoints-request-validation#task#4, CreateStack) differs from the recording only in the per-run Java identity hashes inside an otherwise character-identical ValidationException message, which is not a requirement.

## Steps to Reproduce
1. git clone the project repo and boot the emulator built from that checkout.
2. Replay the wire fixture appstream-catalog-describe-images, step task#1 (and task#2 through task#8, which are the same read).
3. Each step issues an AppStream DescribeImages addressing the published base-image catalog; observe HTTP 400 with no Images member in the body instead of HTTP 200 carrying one Image per addressed published image.
4. Equivalent by hand: call DescribeImages with Type='PUBLIC' and with Names/Arns naming a currently published base image (for example a 'AppStream-Graphics-G4dn-RHEL8-<MM-DD-YYYY>' name from a batch later than 06-17-2024). Observe the 4xx refusal rather than the image record.
5. Confirm the status rule separately: call DescribeImages with Names naming an image that genuinely does not exist and observe the refusal is served as HTTP 400, while Smithy models ResourceNotFoundException with httpError 404.
