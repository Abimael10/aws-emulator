// SPDX-License-Identifier: CC0-1.0

//! `GetBlueprints` — WHICH published form the end-user licence agreement of an image takes.
//!
//! The rule under test: `licenseUrl` is the agreement AWS publishes for THAT image, in whichever
//! of the published forms carries it — an agreement document on AWS's licence host, or the usage
//! section of the image's own AWS Marketplace listing — so the form is the entry's own and is NOT
//! derived from another column of the row. Two images of one `type`, one `platform`, and even two
//! releases of one family, can be published differently, which is what makes the column carried
//! per entry rather than computed from the row.
//!
//! The per-row pages and agreements, the `<product>/<agreement>.txt` shape of a document, and the
//! rule that usage terms are read from the entry's own listing are asserted by the sibling suite
//! `published_catalog_text_and_links.rs`; this suite adds the catalog-wide classification rule
//! that no reading of an entry's other columns predicts its form, over a table of rows that
//! sibling does not carry.
//!
//! The harness, the paged catalog walk and the member readers are that sibling suite's, reused
//! rather than copied.

use std::collections::{BTreeMap, BTreeSet};

use crate::instance_lifecycle::Emulator;
use crate::published_catalog_text_and_links::{
    blueprint_catalog, image_of, text, LICENSE_AGREEMENT_PREFIX, LISTING_USAGE_TERMS_FRAGMENT,
};

use serde_json::Value;

/// The form the agreement AWS publishes for an image takes.
#[derive(Debug, PartialEq, Eq)]
enum Licence {
    /// A `<product>/<agreement>.txt` document on AWS's licence host.
    Document,
    /// The usage terms published on the image's OWN Marketplace listing.
    ListingUsageTerms,
}

/// The form each of these images reports, chosen to span the class where the sibling suite's
/// per-row table does not reach: an application image on Linux/Unix, a retired (inactive)
/// operating-system entry, an older Windows release, and the previous release of a family whose
/// current release is published the other way.
///
/// SOURCE: `amazon_linux_2` is the form a recorded `GetBlueprints` answer reports (its own
/// listing's usage section, where the current Amazon Linux release carries an agreement
/// document). The other three rows are this catalog's own published form for those images, so
/// they are DRIFT PROTECTION over the class rather than evidence: what carries the rule is the
/// catalog-wide classification asserted below.
const EXPECTED_LICENCES: &[(&str, Licence)] = &[
    ("amazon_linux_2", Licence::ListingUsageTerms),
    ("wordpress", Licence::Document),
    ("debian_10", Licence::Document),
    ("windows_server_2016", Licence::Document),
];

/// The form of the licence `entry` reports, or `None` where it is neither published form — a
/// document on AWS's licence host, or the usage section of the entry's OWN listing.
fn licence_of(entry: &Value) -> Option<Licence> {
    let license_url = text(entry, "licenseUrl");
    if license_url.starts_with(LICENSE_AGREEMENT_PREFIX) {
        return Some(Licence::Document);
    }
    license_url
        .strip_suffix(LISTING_USAGE_TERMS_FRAGMENT)
        .filter(|listing| *listing == text(entry, "productUrl"))
        .map(|_| Licence::ListingUsageTerms)
}

/// The family an image belongs to: the first word of its id, which names the software the family
/// installs (`windows`, `ubuntu`, `amazon`).
fn family_of(blueprint_id: &str) -> String {
    blueprint_id
        .split('_')
        .next()
        .unwrap_or(blueprint_id)
        .to_string()
}

#[tokio::test]
async fn the_licence_form_an_image_reports_is_its_own_and_not_its_row_s() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for (blueprint_id, want) in EXPECTED_LICENCES {
        let reported = image_of(&catalog, blueprint_id);
        let got = licence_of(&reported);
        assert_eq!(
            got.as_ref(),
            Some(want),
            "case {blueprint_id}: expected the agreement AWS publishes for THIS image as a \
             {want:?}, got {reported:?}"
        );
    }

    // Both published forms are reported by the catalog, so neither is a form it never serves.
    let forms: BTreeSet<String> = catalog
        .iter()
        .filter_map(|entry| licence_of(entry).map(|form| format!("{form:?}")))
        .collect();
    assert_eq!(
        forms.len(),
        2,
        "case the whole catalog: both published licence forms are reported, got {forms:?}"
    );

    // And no column of the row decides the form: for `type`, for `platform` and for the family
    // alike, some value of that column carries BOTH forms — which a form computed from the row
    // rather than published per entry could not do.
    for column in ["type", "platform", "family"] {
        let mut forms: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for entry in &catalog {
            let Some(form) = licence_of(entry) else {
                continue;
            };
            let value = if column == "family" {
                family_of(&text(entry, "blueprintId"))
            } else {
                text(entry, column)
            };
            forms.entry(value).or_default().insert(format!("{form:?}"));
        }
        assert!(
            forms.values().any(|forms| forms.len() > 1),
            "case {column}: the licence form is the image's own, so one {column} carries both \
             forms, got {forms:?}"
        );
    }
}
