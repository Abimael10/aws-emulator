// SPDX-License-Identifier: CC0-1.0

//! What is IN the two service-owned reference catalogs: the health-checker
//! addresses `GetCheckerIpRanges` publishes and the locations the geolocation
//! catalog holds.
//!
//! The sibling suites assert the SHAPE of both catalogs (a well-formed, sorted,
//! de-duplicated CIDR block; one well-formed kind per location) and the country
//! NAMING of a handful of rows. This one asserts the two rules a synthesized
//! table satisfies and a published one does not:
//!
//! 1. A published single checker address is published in its own right, not
//!    computed from the retired regional block that holds it: the addresses sit
//!    at DIFFERENT offsets inside their blocks, and at least one of them sits on
//!    no sub-block boundary at all, so no formula over the blocks can produce the
//!    set. A client allow-lists these addresses, so an address the service does
//!    not publish is an address no checker ever sends from.
//! 2. The geolocation catalog is the SERVICE's own table of the locations it
//!    places traffic in, not a generic country list: it carries the spelling the
//!    service publishes rather than the one a country has since been renamed to,
//!    it carries a code the standard leaves to its users where the service places
//!    traffic in it, it does NOT carry a location the service cannot place
//!    traffic in (which `GetGeoLocation` therefore refuses), and its subdivisions
//!    are not one country's: every country the service places traffic by
//!    subdivision in has its own subdivisions listed immediately after it, each
//!    resolvable on its own, under the code and the name the SERVICE publishes
//!    for it and not the one a standard it is mostly drawn from assigns, with no
//!    row for a code the service publishes no location for.
//!
//! Every test drives the service through the real router, using the same harness
//! the hosted-zone suites use, so what is asserted is the HTTP/restXml behaviour a
//! Route 53 client sees.

use std::collections::{BTreeMap, BTreeSet};

use smithy_server_runtime::MultiServiceRouter;

use crate::checker_ip_ranges::{parse_cidr, reported_blocks};
use crate::geo_locations::{entries, get, list, location_query};
use crate::hosted_zone_records::router;
use crate::reference_catalogs::within;

/// The single checker address the service publishes inside each retired regional
/// block, as `(retired block, published address)`.
///
/// Source: the Route 53 health-checks entry of the published `ip-ranges.json`
/// that `GetCheckerIpRanges`'s own documentation points at ("we recommend that
/// you download ip-ranges.json"). Written out here rather than read from the
/// crate under test, and including the rows no report cited
/// (`54.183.255.160/32`, `54.228.16.32/32`, `54.232.40.96/32`,
/// `54.244.52.224/32`, `54.251.31.160/32`, `107.23.255.32/32`): each one is the
/// address that publication carries for its block, and the offsets inside the
/// blocks differ from row to row.
const PUBLISHED_CHECKER_ADDRESSES: &[(&str, &str)] = &[
    ("54.183.255.128/26", "54.183.255.160/32"),
    ("54.228.16.0/26", "54.228.16.32/32"),
    ("54.232.40.64/26", "54.232.40.96/32"),
    ("54.241.32.64/26", "54.241.32.64/32"),
    ("54.243.31.192/26", "54.243.31.192/32"),
    ("54.244.52.192/26", "54.244.52.224/32"),
    ("54.245.168.0/26", "54.245.168.0/32"),
    ("54.248.220.0/26", "54.248.220.0/32"),
    ("54.250.253.192/26", "54.250.253.192/32"),
    ("54.251.31.128/26", "54.251.31.160/32"),
    ("54.252.79.128/26", "54.252.79.128/32"),
    ("54.252.254.192/26", "54.252.254.243/32"),
    ("54.255.254.192/26", "54.255.254.192/32"),
    ("107.23.255.0/26", "107.23.255.32/32"),
    ("176.34.159.192/26", "176.34.159.192/32"),
    ("177.71.207.128/26", "177.71.207.128/32"),
];

/// The country name the service publishes, as `(country code, published name)`,
/// beside the name that country is called by the current standard.
///
/// Source: the service's own geolocation catalog, whose naming is neither the ISO
/// standard's current English spelling nor the current CLDR one. The rows span
/// the two directions it departs from them — a short form where the standard
/// carries a long one and a long form where it carries a short one — and most of
/// them are codes no report cited (`MM`, `SZ`, `TL`, `PN`, `UM`, `XK`).
const PUBLISHED_COUNTRY_SPELLINGS: &[(&str, &str, &str)] = &[
    ("CD", "Congo", "Congo - Kinshasa"),
    ("CG", "Republic of the Congo", "Congo - Brazzaville"),
    ("CI", "Ivory Coast", "Côte d'Ivoire"),
    ("CZ", "Czech Republic", "Czechia"),
    ("FM", "Federated States of Micronesia", "Micronesia"),
    ("JO", "Hashemite Kingdom of Jordan", "Jordan"),
    ("KR", "Republic of Korea", "South Korea"),
    ("MD", "Republic of Moldova", "Moldova"),
    ("MK", "Macedonia", "North Macedonia"),
    ("MM", "Myanmar [Burma]", "Myanmar"),
    ("PN", "Pitcairn Islands", "Pitcairn"),
    ("SZ", "Swaziland", "Eswatini"),
    ("TL", "East Timor", "Timor-Leste"),
    ("UM", "U.S. Minor Outlying Islands", "United States Minor Outlying Islands"),
    ("XK", "Kosovo", "Kosovo"),
];

/// Codes the standard assigns to a territory the service publishes no geolocation
/// for, so the catalog carries no location for them.
///
/// Source: the service's own geolocation catalog, which is not the standard's
/// assigned set: it holds a location only where the service can place a query in
/// it.
const UNPLACED_COUNTRY_CODES: &[&str] = &["BV", "CX", "EH", "HM"];

/// How many subdivisions the catalog holds for each country that has any, as
/// `(country code, count)`.
///
/// Source: the service's own geolocation catalog, which carries 51 rows for the
/// states and the federal district of the United States (the two-letter
/// abbreviations the United States Postal Service publishes, which
/// `GeoLocationDetails$SubdivisionCode` points at) and 27 for the regions of
/// Ukraine — the other country `GeoLocation$CountryCode` singles out ("Route 53
/// also supports the country code UA for Ukraine").
const SUBDIVISION_COUNTS: &[(&str, usize)] = &[("UA", 27), ("US", 51)];

/// Subdivisions the catalog has to hold, as `(country code, subdivision code)`.
///
/// Source: the subdivision codes the service's own catalog publishes for the two
/// countries above, which `GeoLocationDetails$SubdivisionCode` sends a client to
/// this listing for ("For a list of all supported subdivision codes, use the
/// ListGeoLocations API") rather than to one standard. The rows span both
/// countries and both kinds of row Ukraine's set has — a region and a city with
/// special status — and none of them is a subdivision any report cited.
const PUBLISHED_SUBDIVISIONS: &[(&str, &str)] = &[
    ("US", "DC"),
    ("US", "NY"),
    ("US", "WY"),
    ("UA", "05"),
    ("UA", "30"),
    ("UA", "77"),
];

/// Subdivisions the catalog publishes, as `(country code, subdivision code,
/// published name)`.
///
/// Source: the same catalog, read for the NAME each code is published under. The
/// rows span both countries that have subdivisions and the three kinds of row the
/// set holds — a state abbreviation, a numeric region code, and a territory the
/// service publishes under a code of its own where ISO 3166-2 numbers the same
/// territory differently (`11` and `20` here, against that standard's `43` and
/// `40`) — and most of them are codes no report cited.
const PUBLISHED_SUBDIVISION_NAMES: &[(&str, &str, &str)] = &[
    ("US", "AK", "Alaska"),
    ("US", "DC", "District of Columbia"),
    ("US", "HI", "Hawaii"),
    ("UA", "09", "Luhanska oblast"),
    ("UA", "11", "Crimea"),
    ("UA", "20", "Sevastopol"),
    ("UA", "30", "Kyiv"),
    ("UA", "32", "Kyivska oblast"),
    ("UA", "35", "Kirovohradska oblast"),
];

/// Codes the catalog publishes NO subdivision for, as `(country code,
/// subdivision code)`.
///
/// Source: the same catalog, read for what it leaves out. The rows span the two
/// ways such a code arises: a code ISO 3166-2 assigns a territory the service
/// publishes under a different code of its own (`40` and `43`), and a United
/// States Postal Service possession abbreviation the service publishes as a
/// country of its own rather than as a subdivision of another (`PR`).
const UNPUBLISHED_SUBDIVISIONS: &[(&str, &str)] = &[("UA", "40"), ("UA", "43"), ("US", "PR")];

/// Every location the catalog reports, read through one `ListGeoLocations` call.
async fn catalog(router: &MultiServiceRouter) -> Vec<crate::geo_locations::Entry> {
    let reply = list(router, "").await;
    entries(&reply.body)
}

#[tokio::test]
async fn every_published_checker_address_is_reported_and_sits_where_the_source_publishes_it() {
    // One row per retired block, including the six no report cited: the address
    // the publication carries for that block is the address the operation
    // reports, and nothing else inside the block is reported.
    let router = router();
    let reported = reported_blocks(&router).await;

    for (block, address) in PUBLISHED_CHECKER_ADDRESSES {
        assert!(
            reported.iter().any(|entry| entry == address),
            "case {block:?}: expected the published address {address:?} among the reported \
             ranges, got {reported:?}"
        );
        let inside: Vec<&String> = reported
            .iter()
            .filter(|entry| within(block, entry))
            .collect();
        assert_eq!(
            inside.len(),
            1,
            "case {block:?}: exactly one published address sits in a retired block, got {inside:?}"
        );
    }
}

#[tokio::test]
async fn the_published_checker_addresses_are_not_one_formula_over_their_blocks() {
    // The rule that tells a published table from a synthesized one, read off the
    // REPORTED ranges rather than off the table above: the offsets of the
    // addresses the service reports inside their blocks are not all the same, and
    // at least one of them is on no sub-block boundary, so no rule of the form
    // "the nth address of each block" can produce the answer. One row per retired
    // block.
    let router = router();
    let reported = reported_blocks(&router).await;

    let mut offsets: BTreeMap<String, u32> = BTreeMap::new();
    for (block, _) in PUBLISHED_CHECKER_ADDRESSES {
        let Some((network, _)) = parse_cidr(block) else {
            panic!("case {block:?}: a retired block is IPv4 CIDR text");
        };
        let inside: Vec<&String> = reported
            .iter()
            .filter(|entry| within(block, entry))
            .collect();
        let [held] = inside.as_slice() else {
            panic!(
                "case {block:?}: expected exactly one reported address inside the block, got \
                 {inside:?}"
            );
        };
        let Some((address, _)) = parse_cidr(held) else {
            panic!("case {block:?}: the reported {held:?} is IPv4 CIDR text");
        };
        offsets.insert((*block).to_string(), address - network);
    }

    let distinct: BTreeSet<u32> = offsets.values().copied().collect();
    assert!(
        distinct.len() > 1,
        "case {:?}: the reported addresses sit at different offsets inside their blocks, got the \
         single offset {distinct:?} across {offsets:?}",
        "offsets across the retired blocks"
    );
    let unaligned: Vec<(&String, &u32)> = offsets.iter().filter(|(_, at)| **at % 32 != 0).collect();
    assert!(
        !unaligned.is_empty(),
        "case {:?}: at least one reported address sits on no sub-block boundary, got the offsets \
         {offsets:?}",
        "an address off every sub-block boundary"
    );
}

#[tokio::test]
async fn the_catalog_carries_the_published_country_spelling_and_not_the_current_one() {
    // One row per country whose published name differs from the name the current
    // standard calls it: the listing and the resolver both carry the published
    // spelling, and the current name is carried by no location at all.
    let router = router();
    let listed = catalog(&router).await;
    let countries: BTreeMap<String, String> = listed
        .iter()
        .filter(|entry| entry.subdivision_code.is_none())
        .filter_map(|entry| {
            entry
                .country_code
                .clone()
                .zip(entry.country_name.clone())
        })
        .collect();
    let all_names: BTreeSet<&String> = countries.values().collect();

    for (code, published, current) in PUBLISHED_COUNTRY_SPELLINGS {
        let listed_name = countries.get(*code).map(String::as_str);
        assert_eq!(
            listed_name,
            Some(*published),
            "case {code:?}: the listing carries the published name, expected {published:?}, got \
             {listed_name:?}"
        );
        if published != current {
            assert!(
                !all_names.iter().any(|name| name.as_str() == *current),
                "case {code:?}: the name the current standard uses ({current:?}) is carried by no \
                 location, got it among the listed names"
            );
        }

        let resolved = get(&router, &location_query(code, "")).await;
        assert_eq!(
            resolved.status, 200,
            "case {code:?}: expected 200 from GetGeoLocation, got {} with body {}",
            resolved.status, resolved.body
        );
        let resolved_name = resolved.first("CountryName");
        assert_eq!(
            resolved_name,
            Some(*published),
            "case {code:?}: the resolver carries the listing's name, expected {published:?}, got \
             {resolved_name:?}"
        );
    }
}

#[tokio::test]
async fn a_location_the_service_places_no_traffic_in_is_neither_listed_nor_resolved() {
    // One row per assigned country code the catalog does not carry: it is in no
    // listed location, and the resolver refuses it the way an unsupported
    // location is refused rather than answering with an entry.
    let router = router();
    let listed = catalog(&router).await;

    for case in UNPLACED_COUNTRY_CODES {
        let held: Vec<&crate::geo_locations::Entry> = listed
            .iter()
            .filter(|entry| entry.country_code.as_deref() == Some(*case))
            .collect();
        assert!(
            held.is_empty(),
            "case {case:?}: the catalog carries no location for a code the service places no \
             traffic in, got {held:?}"
        );

        let refused = get(&router, &location_query(case, "")).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404 from GetGeoLocation, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "NoSuchGeoLocation",
            "case {case:?}: expected the NoSuchGeoLocation code, got body {}",
            refused.body
        );
    }
}

#[tokio::test]
async fn every_country_with_subdivisions_carries_them_right_after_itself() {
    // The subdivisions are not one country's: one row per country that has any,
    // each asserted on the COUNT the standard assigns it and on the run of
    // entries the listing carries immediately after that country.
    let router = router();
    let listed = catalog(&router).await;

    for (country_code, expected_count) in SUBDIVISION_COUNTS {
        let at = listed
            .iter()
            .position(|entry| {
                entry.country_code.as_deref() == Some(*country_code)
                    && entry.subdivision_code.is_none()
            })
            .unwrap_or_else(|| {
                panic!("case {country_code:?}: the catalog has to carry the country itself")
            });
        let run: Vec<&crate::geo_locations::Entry> = listed
            .iter()
            .skip(at + 1)
            .take_while(|entry| entry.subdivision_code.is_some())
            .collect();
        assert_eq!(
            run.len(),
            *expected_count,
            "case {country_code:?}: expected {expected_count} subdivision(s) immediately after \
             the country, got {}",
            run.len()
        );
        for entry in &run {
            assert_eq!(
                entry.country_code.as_deref(),
                Some(*country_code),
                "case {country_code:?}: a subdivision in the run belongs to its own country, got \
                 {entry:?}"
            );
            let code = entry.subdivision_code.clone().unwrap_or_default();
            assert!(
                (1..=3).contains(&code.chars().count()),
                "case {country_code:?}: a subdivision code is 1 to 3 characters, got {code:?}"
            );
            assert!(
                entry
                    .subdivision_name
                    .as_deref()
                    .is_some_and(|name| !name.is_empty()),
                "case {country_code:?}: a subdivision carries its own name, got {entry:?}"
            );
        }

        // The whole catalog holds no subdivision of this country outside that run,
        // so a client that reads the run reads all of them.
        let everywhere = listed
            .iter()
            .filter(|entry| {
                entry.country_code.as_deref() == Some(*country_code)
                    && entry.subdivision_code.is_some()
            })
            .count();
        assert_eq!(
            everywhere, *expected_count,
            "case {country_code:?}: every subdivision of the country sits in that one run, got \
             {everywhere} of them in the catalog"
        );
    }
}

#[tokio::test]
async fn every_published_subdivision_is_listed_and_resolves_on_its_own() {
    // One row per published subdivision, spanning both countries that have them
    // and both kinds of row Ukraine's set holds: the listing carries it and
    // `GetGeoLocation` answers with the very entry the listing carried.
    let router = router();
    let listed = catalog(&router).await;

    for (country_code, subdivision_code) in PUBLISHED_SUBDIVISIONS {
        let entry = listed
            .iter()
            .find(|entry| {
                entry.country_code.as_deref() == Some(*country_code)
                    && entry.subdivision_code.as_deref() == Some(*subdivision_code)
            })
            .cloned();
        let case = format!("{country_code}/{subdivision_code}");
        let Some(entry) = entry else {
            panic!("case {case:?}: the catalog has to carry this subdivision");
        };

        let resolved = get(&router, &location_query(country_code, subdivision_code)).await;
        assert_eq!(
            resolved.status, 200,
            "case {case:?}: expected 200 from GetGeoLocation, got {} with body {}",
            resolved.status, resolved.body
        );
        assert_eq!(
            resolved.first("SubdivisionCode"),
            Some(*subdivision_code),
            "case {case:?}: the resolver answers about the subdivision asked for, got {:?}",
            resolved.first("SubdivisionCode")
        );
        assert_eq!(
            resolved.first("SubdivisionName"),
            entry.subdivision_name.as_deref(),
            "case {case:?}: the resolver carries the listing's subdivision name, got {:?}",
            resolved.first("SubdivisionName")
        );
        assert_eq!(
            resolved.first("CountryName"),
            entry.country_name.as_deref(),
            "case {case:?}: the resolver carries the listing's country name, got {:?}",
            resolved.first("CountryName")
        );
    }
}

#[tokio::test]
async fn every_published_subdivision_carries_the_code_and_name_the_service_publishes() {
    // One row per published subdivision, across both countries and all three
    // kinds of row: the listing carries that code exactly once, under the name
    // the service publishes for it, and the resolver answers about it with the
    // same name.
    let router = router();
    let listed = catalog(&router).await;

    for (country_code, subdivision_code, published_name) in PUBLISHED_SUBDIVISION_NAMES {
        let case = format!("{country_code}/{subdivision_code}");
        let held: Vec<&crate::geo_locations::Entry> = listed
            .iter()
            .filter(|entry| {
                entry.country_code.as_deref() == Some(*country_code)
                    && entry.subdivision_code.as_deref() == Some(*subdivision_code)
            })
            .collect();
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: the catalog publishes this subdivision exactly once, got {held:?}"
        );
        let listed_name = held[0].subdivision_name.as_deref();
        assert_eq!(
            listed_name,
            Some(*published_name),
            "case {case:?}: expected the published name {published_name:?}, got {listed_name:?}"
        );

        let resolved = get(&router, &location_query(country_code, subdivision_code)).await;
        assert_eq!(
            resolved.status, 200,
            "case {case:?}: expected 200 from GetGeoLocation, got {} with body {}",
            resolved.status, resolved.body
        );
        let resolved_name = resolved.first("SubdivisionName");
        assert_eq!(
            resolved_name,
            Some(*published_name),
            "case {case:?}: the resolver carries the published name, got {resolved_name:?}"
        );
    }
}

#[tokio::test]
async fn a_subdivision_code_the_service_publishes_no_location_for_is_neither_listed_nor_resolved() {
    // One row per absent code: no listed location carries it, and the resolver
    // refuses it as an unsupported location instead of answering with an entry.
    let router = router();
    let listed = catalog(&router).await;

    for (country_code, subdivision_code) in UNPUBLISHED_SUBDIVISIONS {
        let case = format!("{country_code}/{subdivision_code}");
        let held: Vec<&crate::geo_locations::Entry> = listed
            .iter()
            .filter(|entry| {
                entry.country_code.as_deref() == Some(*country_code)
                    && entry.subdivision_code.as_deref() == Some(*subdivision_code)
            })
            .collect();
        assert!(
            held.is_empty(),
            "case {case:?}: the catalog carries no subdivision for a code the service publishes \
             none for, got {held:?}"
        );

        let refused = get(&router, &location_query(country_code, subdivision_code)).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404 from GetGeoLocation, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "NoSuchGeoLocation",
            "case {case:?}: expected the NoSuchGeoLocation code, got body {}",
            refused.body
        );
        let expected_message = format!(
            "No geo location for location: [country = {country_code}, subdivision = \
             {subdivision_code}]"
        );
        let message = refused.first("Error/Message");
        assert_eq!(
            message,
            Some(expected_message.as_str()),
            "case {case:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn no_country_publishes_one_subdivision_name_under_two_codes() {
    // Read off the whole listing rather than off the tables above: a catalog that
    // carried a territory under the service's own code AND under another table's
    // code would report one name twice inside one country, which is how a merged
    // or doubly-sourced table shows up. One case per country that has
    // subdivisions.
    let router = router();
    let listed = catalog(&router).await;

    let mut by_country: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for entry in &listed {
        let (Some(country_code), Some(code), Some(name)) = (
            entry.country_code.clone(),
            entry.subdivision_code.clone(),
            entry.subdivision_name.clone(),
        ) else {
            continue;
        };
        by_country
            .entry(country_code)
            .or_default()
            .push((code, name));
    }
    assert!(
        !by_country.is_empty(),
        "case {:?}: the catalog publishes subdivisions for at least one country, got none",
        "the subdivisions of the whole catalog"
    );

    for (country_code, rows) in &by_country {
        for (code, name) in rows {
            let twice: Vec<&(String, String)> = rows
                .iter()
                .filter(|(other_code, other_name)| other_name == name && other_code != code)
                .collect();
            assert!(
                twice.is_empty(),
                "case {country_code:?}: {name:?} is published under one code only, got it under \
                 {code:?} and {twice:?}"
            );
            let same_code = rows.iter().filter(|(other, _)| other == code).count();
            assert_eq!(
                same_code, 1,
                "case {country_code:?}: the code {code:?} is published once, got it {same_code} \
                 times"
            );
        }
    }
}
