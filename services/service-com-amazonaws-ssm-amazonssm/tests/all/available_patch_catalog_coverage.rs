// SPDX-License-Identifier: CC0-1.0

//! What the published patch catalogue COVERS, read through `DescribeAvailablePatches`.
//!
//! `all/available_patches.rs` pins the read's own rules — which catalogue a `PRODUCT` key
//! selects, how each filter narrows, how a page resumes — and this suite reaches the
//! emulator through that file's helpers rather than a second harness of its own. What it
//! pins is the rule about the catalogue BEHIND that read: every value the service publishes
//! for a patch property is carried by patches in the catalogue, and the properties vary
//! independently of one another, so a read filtered on any published value — or on a pair of
//! values of two different properties — reports the entries carrying them rather than an
//! empty list; and every entry it reports carries each modelled `Patch` member the catalogue
//! publishes a value for.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so what a read
//! reports is what the wire reports.

use std::collections::BTreeSet;

use crate::available_patches::{all_patches, member, published_values, request};
use crate::parameter_store::router;

/// The one operating system whose patches carry a Knowledge Base id, a Microsoft Security
/// Response Center severity and a product family (`Patch$KbNumber`, `Patch$MsrcSeverity`:
/// "Applies to Windows patches only").
const WINDOWS: &str = "WINDOWS";

/// The published patch set an operating system's own patches belong to, and the one its
/// vendor's applications belong to ("`PATCH_SET` Sample values: `OS` | `APPLICATION`",
/// `DescribeAvailablePatchesRequest$Filters`).
const PATCH_SETS: [&str; 2] = ["OS", "APPLICATION"];

/// Where the article of a patch its vendor identifies by a Knowledge Base id is published:
/// "the URL where more information can be obtained about the patch" (`Patch$ContentUrl`) is
/// that article, addressed by the digits of the id.
const KB_ARTICLE_URL: &str = "https://support.microsoft.com/en-us/kb/";

/// Every product the catalogue publishes is a product it holds patches for: a read naming
/// one reports that product's patches and nothing else's.
///
/// The rows are the Windows releases and Microsoft applications the patch docs name a
/// product by (`Patch$Product`: "For example, `WindowsServer2016`";
/// `DescribeAvailablePatchesRequest$Filters`: `PRODUCT` sample values `WindowsServer2012 |
/// Office 2010 | MicrosoftDefenderAntivirus`), spanning both patch sets and the client,
/// server and application product lines — among them entries no recorded read named.
#[tokio::test]
async fn every_published_product_holds_patches() {
    let router = router();

    let published: BTreeSet<String> = published_values(&router, WINDOWS, "PRODUCT", Some("OS"))
        .await
        .into_iter()
        .chain(published_values(&router, WINDOWS, "PRODUCT", Some("APPLICATION")).await)
        .collect();

    for product in [
        "Windows7",
        "Windows8",
        "Windows8.1",
        "Windows11",
        "WindowsServer2008",
        "WindowsServer2008R2",
        "WindowsServer2022",
        "Office 2010",
        "MicrosoftDefenderAntivirus",
    ] {
        assert!(
            published.contains(product),
            "case {product}: the catalogue publishes the product, got {published:?}"
        );
        let patches = all_patches(&router, product, &request(&[("PRODUCT", &[product])])).await;
        assert!(
            !patches.is_empty(),
            "case {product}: the published product holds patches, got an empty catalogue"
        );
        let reported: BTreeSet<&str> = patches
            .iter()
            .filter_map(|patch| member(patch, "Product"))
            .collect();
        assert_eq!(
            reported,
            BTreeSet::from([product]),
            "case {product}: every reported patch applies to the product searched for, got \
             {reported:?}"
        );
    }
}

/// Every product the catalogue publishes holds patches, walked over the WHOLE published
/// product list rather than a table of named rows: no published product reports nothing.
#[tokio::test]
async fn no_published_product_reports_an_empty_catalogue() {
    let router = router();

    for patch_set in PATCH_SETS {
        for product in published_values(&router, WINDOWS, "PRODUCT", Some(patch_set)).await {
            let case = format!("{patch_set} {product}");
            let body = request(&[("PRODUCT", &[product.as_str()])]);
            let patches = all_patches(&router, &case, &body).await;
            assert!(
                !patches.is_empty(),
                "case {case}: the published product holds patches, got an empty catalogue"
            );
        }
    }
}

/// Two patch properties vary independently: every pair of a published classification and a
/// published severity is carried by some patch, so a read filtered on both reports the
/// patches carrying them rather than an empty list.
///
/// The rows are the whole cross product of the two published value lists
/// (`DescribeAvailablePatchesRequest$Filters`: `CLASSIFICATION` sample values `ServicePacks
/// | SecurityUpdates | DefinitionUpdates`, `MSRC_SEVERITY` sample values `ServicePacks |
/// Important | Moderate`), read from the catalogue itself.
#[tokio::test]
async fn every_pair_of_published_properties_is_carried_by_some_patch() {
    let router = router();

    let classifications = published_values(&router, WINDOWS, "CLASSIFICATION", None).await;
    let severities = published_values(&router, WINDOWS, "MSRC_SEVERITY", None).await;
    assert!(
        !classifications.is_empty() && !severities.is_empty(),
        "the catalogue publishes classifications and severities, got {classifications:?} and \
         {severities:?}"
    );

    for classification in &classifications {
        for severity in &severities {
            let case = format!("{classification} at {severity}");
            let patches = all_patches(
                &router,
                &case,
                &request(&[
                    ("CLASSIFICATION", &[classification.as_str()]),
                    ("MSRC_SEVERITY", &[severity.as_str()]),
                ]),
            )
            .await;
            assert!(
                !patches.is_empty(),
                "case {case}: the published pair is carried by some patch, got an empty \
                 catalogue"
            );
            for patch in &patches {
                assert_eq!(
                    (
                        member(patch, "Classification"),
                        member(patch, "MsrcSeverity")
                    ),
                    (Some(classification.as_str()), Some(severity.as_str())),
                    "case {case}: every reported patch carries both values, got {patch}"
                );
            }
        }
    }
}

/// One row of the member table below: the case it is, the filters its read carries, and
/// whether its patches are the ones whose vendor identifies a patch by a Knowledge Base id.
type MemberRow = (
    &'static str,
    &'static [(&'static str, &'static [&'static str])],
    bool,
);

/// Every entry the catalogue reports carries the modelled members it publishes a value for:
/// its description, and — for the one operating system whose vendor identifies a patch by a
/// Knowledge Base id — the article that id names. No entry is related to a numbered Microsoft
/// Security Response Center bulletin, because the catalogue publishes its updates one article
/// at a time, so none names one.
///
/// The rows are one read per kind of catalogue: the Windows patches a read naming no product
/// reports, a Microsoft application's, and a Linux one's, whose patches are package updates
/// published through advisories instead.
#[tokio::test]
async fn every_reported_patch_carries_the_members_its_catalogue_publishes() {
    let router = router();

    let rows: &[MemberRow] = &[
        ("the Windows catalogue", &[], true),
        (
            "a Microsoft application",
            &[("PRODUCT", &["Office 2010"])],
            true,
        ),
        (
            "a Linux catalogue",
            &[("PRODUCT", &["AmazonLinux2018.03"])],
            false,
        ),
    ];

    for (case, filters, windows) in rows {
        let patches = all_patches(&router, case, &request(filters)).await;
        assert!(
            !patches.is_empty(),
            "case {case}: the read reports the catalogue's patches, got an empty list"
        );
        for patch in &patches {
            let product = member(patch, "Product")
                .unwrap_or_else(|| panic!("case {case}: a patch names its product, got {patch}"));
            let description = member(patch, "Description").unwrap_or_else(|| {
                panic!("case {case}: every patch carries its description, got {patch}")
            });
            assert!(
                description.contains(product),
                "case {case}: the description says what the patch applies to, got \
                 {description:?}"
            );
            assert!(
                patch.get("MsrcNumber").is_none(),
                "case {case}: no patch of this catalogue names a bulletin, got {patch}"
            );
            let kb_number = member(patch, "KbNumber");
            assert_eq!(
                kb_number.is_some(),
                *windows,
                "case {case}: a Knowledge Base id is carried by the Windows patches alone, \
                 got {patch}"
            );
            match kb_number {
                Some(kb) => {
                    let expected =
                        format!("{KB_ARTICLE_URL}{}", kb.strip_prefix("KB").unwrap_or(kb));
                    assert_eq!(
                        member(patch, "ContentUrl"),
                        Some(expected.as_str()),
                        "case {case}: the article is the one the Knowledge Base id names, got \
                         {patch}"
                    );
                }
                None => assert!(
                    patch.get("ContentUrl").is_none(),
                    "case {case}: a patch its vendor publishes no article for names none, got \
                     {patch}"
                ),
            }
        }
    }
}
