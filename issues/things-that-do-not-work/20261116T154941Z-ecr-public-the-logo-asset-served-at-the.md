Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr-public.catalog-logo-blob-is-stored-and-served-as-a-real-image
Issue-Shape-Id: com.amazonaws.ecrpublic#SpencerFrontendService
Issue-Key: ecr-public.catalog-logo-blob-is-stored-and-served-as-a-real-image|run#15|AssertionError|assert headers.get("Content-Type") == "image/png", (
Issue-Commit: 68169c86c7603a2902f84035dfc94fd02b30ddfc

# ECR Public: the logo asset served at the minted logoUrl carries no Content-Type, so the published PNG is served untyped

> _Note: this issue was identified against the code base at commit `68169c86c7603a2902f84035dfc94fd02b30ddfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRepositoryCatalogData

## Description
Behavioral replay of simparity-behavioral-aws-v1 (89/92 passed; ecr-public 88/91, so the emulator IS serving and ecr-public is bootstrapped as service-com-amazonaws-ecrpublic-spencerfrontendservice — this is not a bootstrap gap). Of the three ecr-public failures, two are marked OWNED by open issues (20261029T173115Z… the @pattern message, and 20261029T104628Z… PutRegistryCatalogData UnsupportedCommandException) and are excluded. ONE net-new root cause remains.

NOTE on an overlapping open issue: 20261107T170252Z-ecr-public-the-logoimageblob-accepted-by-putrepositorycatalogdata-is.md recorded the same check failing because the minted logoUrl answered 404 for its whole budget (its replay key is `_fetch_until_served#11|AssertionError|assert status == 200`). The emulator now serves the asset: the GET answers 200 and the body is byte-identical to the uploaded PNG with an IHDR reading 64x64, so every assertion that issue described as unverified up to the header check now passes, and the check fails one assertion later on the response header. The report therefore no longer binds this check to that issue, and the remaining divergence is filed here. If that issue is being worked, this cause is the next assertion in the same leg.

=== Cause 1 of 1: the hosted logo asset is served with no Content-Type header instead of the image media type of the stored blob ===
AWS: Smithy RepositoryCatalogDataInput.logoImageBlob is a blob, @length 0..512000, documented "The base64-encoded repository logo payload", and is INPUT-only — the output shape RepositoryCatalogData models no logoImageBlob and instead carries logoUrl -> ResourceUrl @length 0..2048, "The URL that contains the logo that's associated with the repository." The uploaded bytes therefore become a hosted web asset whose URL is the only way a client can observe them, and the hosting leg itself is outside the modeled API surface, so: recorded behavior only — an anonymous GET of the minted logoUrl answers HTTP 200 with the byte-identical payload AND a Content-Type naming the image's media type, `image/png` for a PNG upload. The header is what makes the URL usable as a web image, which is the member's entire documented purpose (it is rendered in the Gallery).
Emulator: the GET of the minted logoUrl now answers 200 and returns the uploaded bytes verbatim (length, PNG signature and IHDR all match), but the response carries NO Content-Type at all: the check reads `headers.get("Content-Type")` as `None` and fails with `AssertionError: the asset was served as None`. The bytes are stored and routed correctly; only the media type on the asset response is missing. Because the check aborts at that assertion, the legs after it stay unverified against the emulator: the undersized (32x32) logo being refused with InvalidParameterException while leaving logoUrl, description and the stored bytes intact, and an accepted replacement blob minting a DIFFERENT logoUrl that serves the new bytes without wiping the description.
Why: a client that uploads a logo and then renders or validates it relies on the media type, not on sniffing. Served untyped, a browser or image pipeline fetching the logoUrl gets an octet-stream/unknown body: the gallery preview shows a download rather than an image, and any publish pipeline that asserts the asset is an image (content-type check, HTML <img> embedding, CDN/image-processing step) fails against the emulator while succeeding on AWS. The call itself reports success and the bytes are right, so the defect surfaces only at the HTTP fetch, far from the operation that caused it.
Fix: the emulator's HTTP front door route that claims the minted logo path (the component that serves the asset persisted by the PutRepositoryCatalogData handler / repository-catalog state) must set the response Content-Type to the media type of the stored blob — `image/png` for the PNG the check uploads — alongside the verbatim bytes it already returns.
Traces: ecr-public.catalog-logo-blob-is-stored-and-served-as-a-real-image.

## Evidence
Replay: simparity-behavioral-aws-v1, total 89/92 passed; ecr-public 88/91 with 3 failures, 2 of which the report marked OWNED by open issues and are excluded here.

[mismatch] ecr-public.catalog-logo-blob-is-stored-and-served-as-a-real-image — CreateRepository with a fresh name, then PutRepositoryCatalogData with a hand-built 64x64 truecolour PNG as catalogData.logoImageBlob plus a description. GetRepositoryCatalogData returns catalogData without logoImageBlob, with the description round-tripped, and with a logoUrl that passes ctx.published_url_mismatch (so the URL is routed at the emulator's own published host — not an announced-address problem). The anonymous GET of that URL then answers 200 and the body passes every byte assertion (`body == good_png`, PNG signature b'\x89PNG\r\n\x1a\n', IHDR unpacking to (64, 64)). The failure is the next assertion, at check_ecr_public_catalog_logo_blob_is_stored_and_served_as_a_real_image.py line 145:
  AssertionError: the asset was served as None
  (the assertion is `headers.get("Content-Type") == "image/png"`; `None` means the header was absent from the 200 response.)

Expected: HTTP 200, body byte-identical to the uploaded PNG, and response header `Content-Type: image/png` — the fixed part is the header name and the `image/<subtype>` form; the subtype is a value derived from the blob the request uploaded (png here), not a literal from this run. Observed: HTTP 200 with the correct bytes and no Content-Type header. No AWS error code is involved: this is the asset-hosting leg, and the PutRepositoryCatalogData / GetRepositoryCatalogData API responses themselves are correctly shaped. Per-run values in this trace (repository name, asset hash in the URL, host and port) are not requirements.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-behavioral-aws-v1 ecr-public suite (run_behavioral_suite).
2. Replay ecr-public.catalog-logo-blob-is-stored-and-served-as-a-real-image.
3. By hand: against the us-east-1 ecr-public endpoint, CreateRepository with a fresh name, then PutRepositoryCatalogData with catalogData.logoImageBlob set to a valid 64x64 PNG and a description.
4. Read catalogData.logoUrl back from GetRepositoryCatalogData and issue an anonymous HTTP GET for it, routed at the emulator. Inspect the response headers: the body is the uploaded PNG, but there is no Content-Type header. AWS serves it as Content-Type: image/png.
5. With the header fixed, confirm the legs the assertion currently hides: PutRepositoryCatalogData with a 32x32 PNG must be refused with InvalidParameterException leaving logoUrl, the description and the served bytes unchanged; a different valid 64x64 PNG must mint a NEW logoUrl serving the new bytes while the description survives.
