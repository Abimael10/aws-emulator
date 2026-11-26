// SPDX-License-Identifier: CC0-1.0

//! `GetBlueprints` — the AWS page and the end-user licence agreement a published image is linked
//! to, as ONE pair belonging to that image.
//!
//! The rule under test: every row of the published blueprint catalog reports in `productUrl` the
//! AWS page published for THAT image — its own Marketplace listing, or the product page AWS keeps
//! for an operating system of its own — and in `licenseUrl` the agreement published for THAT
//! image in the form AWS serves it: that SAME listing, whole or at its usage section, or that
//! image's own agreement document on AWS's licence host. So a row never carries a neighbour's
//! listing id, and never a neighbour's licence form; and the catalog text a row reports is the
//! copy published for that image, with its own release named in it.
//!
//! The cases are tables spanning what the rule generalises over — every licence form, both
//! `type` values, both platforms, an image offered through a Marketplace listing and one AWS
//! publishes a product page of its own for, two editions of one product tier published two
//! different ways, a retired entry, and images the per-row tables elsewhere do not name — and each
//! table is backed by invariants asserted over whatever the catalog holds, which is what catches a
//! pair that crosses on a row no table here writes out.
//!
//! The harness, the paged catalog walk and the member readers are the sibling suite
//! `published_catalog_text_and_links.rs`'s, reused rather than copied.

use std::collections::BTreeMap;

use crate::instance_lifecycle::Emulator;
use crate::published_catalog_text_and_links::{
    blueprint_catalog, image_of, text, AWS_PRODUCT_PAGE_PREFIX, LICENSE_AGREEMENT_PREFIX,
    LISTING_USAGE_TERMS_FRAGMENT, MARKETPLACE_LISTING_PATH,
};

/// Which form the agreement a row reports takes, and the document it names where a recorded
/// `GetBlueprints` answer names one.
enum Agreement {
    /// A `<product>/<agreement>.txt` document on AWS's licence host: the recorded path where the
    /// recording names it, and otherwise any document of that form, so a row the recording does
    /// not reach is still held to the published shape.
    Document(Option<&'static str>),
    /// The usage terms published on the image's OWN listing, which the row's `licenseUrl` names as
    /// that listing with [`LISTING_USAGE_TERMS_FRAGMENT`].
    ListingUsageTerms,
    /// The image's OWN listing itself, which the row's `licenseUrl` names as that page: which
    /// part of a listing carries an image's terms is published per image.
    Listing,
}

/// One published image, the AWS page it is linked to and the agreement published for it.
///
/// `page` is the exact page where a recorded answer names it, and `None` where the row only has to
/// carry a Marketplace listing of its own.
struct Pair {
    blueprint_id: &'static str,
    page: Option<&'static str>,
    agreement: Agreement,
}

/// The one agreement document published for the Windows Server operating-system images: the
/// FAMILY's, so every release of it reports this document.
const WINDOWS_SERVER_AGREEMENT: &str =
    "b0e459f5-2c38-4939-ad2a-d9784ecab690/0e7c7387-f24a-45b5-9206-e5ab5ff5fd9c.txt";

/// SOURCE: the `productUrl` and `licenseUrl` values a recorded `GetBlueprints` answer reports for
/// the rows it names them for. The rows that name no page (`page: None`) and no document are held
/// to the published FORM instead: which form an image carries is published per image, so a row the
/// recording does not reach still has to carry one of them, whole and its own.
const PAIRS: &[Pair] = &[
    // The Windows Server releases: one listing each — at a `prodview-` product view, or at the
    // product code an older listing is published under — and the family's one agreement document.
    Pair {
        blueprint_id: "windows_server_2025",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-g76csvka4r3by"),
        agreement: Agreement::Document(Some(WINDOWS_SERVER_AGREEMENT)),
    },
    Pair {
        blueprint_id: "windows_server_2019",
        page: Some("https://aws.amazon.com/marketplace/pp/B07QZ4XZ8F"),
        agreement: Agreement::Document(Some(WINDOWS_SERVER_AGREEMENT)),
    },
    // Both editions of the Windows SQL Server tier: the terms of one are on its listing, the
    // other's are an agreement document of its own, so the form is not read off the row.
    Pair {
        blueprint_id: "windows_server_2022_sql_2022_express",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-c2jz4lr4h2yc6"),
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "windows_server_2022_sql_2019_express",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-xbikutlmywslu"),
        agreement: Agreement::Document(Some(
            "5c071384-f68c-4519-9040-342072d7e3de/1402cd01-4590-4cf4-bd59-092f727456ca.txt",
        )),
    },
    // The current Amazon Linux release: a product page AWS keeps of its own, and an agreement
    // document — an openly licensed operating system carries one too.
    Pair {
        blueprint_id: "amazon_linux_2023",
        page: Some("https://aws.amazon.com/linux/amazon-linux-2023"),
        agreement: Agreement::Document(Some(
            "f37c8255-1ff9-48bd-b5da-b5046f4fee68/17864b80-f5b6-4f0a-aa26-7346ef135ebc.txt",
        )),
    },
    // The previous release of that family: its own listing, with the terms on it.
    Pair {
        blueprint_id: "amazon_linux_2",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-s4zvkzmlirbga"),
        agreement: Agreement::ListingUsageTerms,
    },
    // The Marketplace-listed Linux/Unix releases the recording names the listing of.
    Pair {
        blueprint_id: "ubuntu_22_04",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-goqtn6wuqa2sc"),
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "ubuntu_20_04",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-g5rooj5oqzrw4"),
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "debian_12",
        page: Some("https://aws.amazon.com/marketplace/pp/prodview-kweb77e4ra73a"),
        agreement: Agreement::Listing,
    },
    // Rows no per-row table names: another operating-system family, two application stacks, and a
    // retired entry. Each has to carry a listing of its own and the terms on it.
    Pair {
        blueprint_id: "opensuse_15",
        page: None,
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "nginx",
        page: None,
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "cpanel_whm_almalinux",
        page: None,
        agreement: Agreement::ListingUsageTerms,
    },
    Pair {
        blueprint_id: "ubuntu_18_04",
        page: None,
        agreement: Agreement::ListingUsageTerms,
    },
];

/// Whether `agreement` is a `<product>/<agreement>.txt` document path: the product the agreement
/// is published for, and the document served for it.
fn is_document_path(agreement: &str) -> bool {
    let parts: Vec<&str> = agreement.split('/').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].ends_with(".txt")
        && parts[1].len() > ".txt".len()
}

/// The family an image belongs to: the first word of its id, which names the software the family
/// installs.
fn family_of(blueprint_id: &str) -> &str {
    blueprint_id.split('_').next().unwrap_or(blueprint_id)
}

#[tokio::test]
async fn the_page_and_the_agreement_a_published_image_reports_are_that_image_s_pair() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for want in PAIRS {
        let case = want.blueprint_id;
        let reported = image_of(&catalog, case);
        let product_url = text(&reported, "productUrl");
        match want.page {
            Some(page) => assert_eq!(
                product_url, page,
                "case {case}: expected the AWS page published for THIS image, {page:?}, got \
                 {reported:?}"
            ),
            None => assert!(
                product_url
                    .starts_with(&format!("{AWS_PRODUCT_PAGE_PREFIX}{MARKETPLACE_LISTING_PATH}")),
                "case {case}: expected this image's own AWS Marketplace listing, got \
                 {product_url:?}"
            ),
        }

        let license_url = text(&reported, "licenseUrl");
        match want.agreement {
            Agreement::ListingUsageTerms => assert_eq!(
                license_url,
                format!("{product_url}{LISTING_USAGE_TERMS_FRAGMENT}"),
                "case {case}: expected the terms on THIS image's own listing, got {license_url:?}"
            ),
            Agreement::Listing => assert_eq!(
                license_url, product_url,
                "case {case}: expected THIS image's own listing page, got {license_url:?}"
            ),
            Agreement::Document(document) => {
                let path = license_url
                    .strip_prefix(LICENSE_AGREEMENT_PREFIX)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case}: expected the agreement document published for this \
                             image under {LICENSE_AGREEMENT_PREFIX}, got {license_url:?}"
                        )
                    });
                match document {
                    Some(document) => assert_eq!(
                        path, document,
                        "case {case}: expected the agreement document published for THIS image, \
                         got {path:?}"
                    ),
                    None => assert!(
                        is_document_path(path),
                        "case {case}: expected a <product>/<agreement>.txt document, got {path:?}"
                    ),
                }
            }
        }
    }
}

#[tokio::test]
async fn no_published_image_carries_another_image_s_listing_or_agreement() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;
    assert!(
        catalog.len() > 20,
        "case the whole catalog: expected the published images, got {} rows",
        catalog.len()
    );

    // One listing belongs to one image, so no two rows report the same page.
    let mut listings: BTreeMap<String, String> = BTreeMap::new();
    // An agreement DOCUMENT is published for a product, which the releases of one family share —
    // so the rows reporting one document are all of one family, and never two families.
    let mut documents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for blueprint in &catalog {
        let case = text(blueprint, "blueprintId");
        let product_url = text(blueprint, "productUrl");
        assert!(
            product_url.starts_with(AWS_PRODUCT_PAGE_PREFIX)
                && product_url.len() > AWS_PRODUCT_PAGE_PREFIX.len(),
            "case {case}: expected the AWS page published for this image, got {product_url:?}"
        );
        if let Some(owner) = listings.insert(product_url.clone(), case.clone()) {
            panic!(
                "case {case}: expected this image's own listing, but {owner} reports \
                 {product_url:?} too"
            );
        }

        let license_url = text(blueprint, "licenseUrl");
        if license_url == product_url {
            // The terms are this row's listing page itself: the pair agrees.
            continue;
        }
        match license_url.strip_suffix(LISTING_USAGE_TERMS_FRAGMENT) {
            // The terms on a listing are read off THIS row's listing: the pair agrees.
            Some(listing) => assert_eq!(
                listing, product_url,
                "case {case}: expected the terms on its own listing {product_url:?}, got \
                 {license_url:?}"
            ),
            None => {
                let path = license_url
                    .strip_prefix(LICENSE_AGREEMENT_PREFIX)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case}: expected either its own listing, whole or at its usage \
                             section, or an agreement document on AWS's licence host, got \
                             {license_url:?}"
                        )
                    });
                assert!(
                    is_document_path(path),
                    "case {case}: expected a <product>/<agreement>.txt document, got {path:?}"
                );
                documents.entry(path.to_string()).or_default().push(case);
            }
        }
    }

    for (document, images) in &documents {
        let families: Vec<&str> = images
            .iter()
            .map(|blueprint_id| family_of(blueprint_id))
            .collect();
        assert!(
            families.windows(2).all(|pair| pair[0] == pair[1]),
            "case {document}: an agreement document is one family's, got {images:?}"
        );
    }
    assert!(
        !documents.is_empty() && documents.len() < catalog.len(),
        "case the whole catalog: expected both published licence forms to be reported, got \
         {documents:?} documents over {} rows",
        catalog.len()
    );
}

/// The sentences a recorded `GetBlueprints` answer reports in the copy published for a Windows
/// Server release, and which copy each release carries.
///
/// SOURCE: the recorded answer's paragraphs. One release is reported with the copy of its own
/// Marketplace listing — opening on what Windows Server is an environment for, and closing by
/// naming that release on the Marketplace — and the others with the Lightsail family paragraph,
/// each naming the release it installs. The sentence written out per row is the one that
/// distinguishes the two copies, so a row carrying the other copy, or a neighbour's release word,
/// fails.
struct WindowsCopy {
    blueprint_id: &'static str,
    release: &'static str,
    sentence: &'static str,
}

const WINDOWS_COPY: &[WindowsCopy] = &[
    WindowsCopy {
        blueprint_id: "windows_server_2025",
        release: "2025",
        sentence: "For business IT applications, Lightsail runs Windows-based solutions in a \
                   secure, easily managed, and performant cloud environment.",
    },
    WindowsCopy {
        blueprint_id: "windows_server_2022",
        release: "2022",
        sentence: "This blueprint allows you to run compatible solutions that are based on \
                   Windows on Amazon Web Services (AWS), providing high-performance, reliable, \
                   and cost-effective cloud computing.",
    },
    WindowsCopy {
        blueprint_id: "windows_server_2019",
        release: "2019",
        sentence: "For business IT applications, Lightsail runs Windows-based solutions in a \
                   secure, easily managed, and performant cloud environment.",
    },
    WindowsCopy {
        blueprint_id: "windows_server_2016",
        release: "2016",
        sentence: "For business IT applications, Lightsail runs Windows-based solutions in a \
                   secure, easily managed, and performant cloud environment.",
    },
];

#[tokio::test]
async fn a_windows_release_reports_the_copy_published_for_it_naming_its_own_release() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for want in WINDOWS_COPY {
        let case = want.blueprint_id;
        let description = text(&image_of(&catalog, case), "description");
        assert!(
            description.contains(&format!("Windows Server {}", want.release)),
            "case {case}: expected the copy to name Windows Server {}, got {description:?}",
            want.release
        );
        assert!(
            description.contains(want.sentence),
            "case {case}: expected the published copy's wording {:?}, got {description:?}",
            want.sentence
        );
        // And the copy is this release's: no other release's number appears in it.
        for other in WINDOWS_COPY {
            assert!(
                other.release == want.release || !description.contains(other.release),
                "case {case}: expected only its own release named, got {description:?}"
            );
        }
    }
}
