// SPDX-License-Identifier: CC0-1.0

//! The Places plane of this API: the provider dataset a place index geocodes
//! against, and the one statement of how a free-form query is matched against it,
//! how a match is ranked, how it is narrowed, and how a record is reported.
//!
//! Kept beside the state module rather than in one operation's file because every
//! Places operation of this service reaches the same records and has to agree with
//! the others about them: a `PlaceId` a text search hands out is what `GetPlace`
//! resolves, and the two must project one record identically.
//!
//! What a provider reports about a place differs by provider, and the modelled
//! `Place` members say which: `TimeZone` is "Returned only when using HERE or Grab
//! as the selected partner", `UnitType` "only for a place index that uses Esri as
//! a data provider", `UnitNumber` "only for a place index that uses Esri or Grab",
//! and `SubMunicipality` is "supported by Esri and OpenData". So a record is
//! projected THROUGH a provider, and the provider is the one the place index
//! resource was created with.

use super::{PROVIDER_ESRI, PROVIDER_GRAB, PROVIDER_HERE};

/// The place records themselves: the stand-in for the provider's own dataset.
mod dataset;
pub use dataset::{country_name, PlaceRecord, CATEGORIES, COUNTRY_NAMES, PLACES};

/// How many results a text search returns when the request names no `MaxResults`.
///
/// The model's `SearchPlaceIndexForTextRequest$MaxResults` doc: "The default: 50".
pub const DEFAULT_SEARCH_RESULT_LIMIT: usize = 50;

/// The most results a search can be asked for: the modelled
/// `PlaceIndexSearchResultLimit` range, `1..50`.
pub const MAX_SEARCH_RESULT_LIMIT: i32 = 50;

/// The member a refusal about the requested bias position names.
pub const BIAS_POSITION_MEMBER: &str = "BiasPosition";

/// The member a refusal about the requested place identifier names, as the model
/// spells it on `GetPlaceRequest`.
pub const PLACE_ID_MEMBER: &str = "PlaceId";

/// The member a refusal about the requested bounding box names.
pub const FILTER_BBOX_MEMBER: &str = "FilterBBox";

/// The `ValidationException` sentence a search naming both a bias position and a
/// bounding box is refused with.
///
/// The model states the rule on both members —
/// `SearchPlaceIndexForTextRequest$FilterBBox`: "`FilterBBox` and `BiasPosition`
/// are mutually exclusive. Specifying both options results in an error." — so the
/// sentence names the two members and nothing else. It carries no request value:
/// both members are `@sensitive`.
pub const MUTUALLY_EXCLUSIVE_BIAS_AND_BBOX: &str =
    "'BiasPosition' and 'FilterBBox' are mutually exclusive.";

/// The `ValidationException` sentence a bounding box whose corners are the wrong
/// way round is refused with.
///
/// The `FilterBBox` doc fixes the order: "The first pair of numbers represents the
/// X and Y coordinates (longitude and latitude, respectively) of the southwest
/// corner of the bounding box; the second pair of numbers represents […] the
/// northeast corner". It carries no request value, for the reason above.
pub const INVERTED_BOUNDING_BOX: &str =
    "'FilterBBox' must give the southwest corner before the northeast corner.";

/// The least share of a query's own terms a place must account for to be a result
/// at all.
///
/// A geocode of a piece of text answers the places that text NAMES, so a place
/// that happens to share one word of a longer query — a street type, a category —
/// is not one of them: a result has to account for at least as much of the query
/// as it leaves unaccounted for. The share a place does account for is its
/// relevance, so this is the floor of the relevance a reported result carries.
const MINIMUM_SHARE_OF_QUERY: f64 = 0.5;

/// The mean radius of the Earth in metres (IUGG mean radius, 6 371 008.8 m), which
/// the great-circle arc a `Distance` reports is measured on.
const EARTH_RADIUS_METRES: f64 = 6_371_008.8;

/// The `[west, south, east, north]` box a `BoundingBox` carries, or the reason it
/// is not a box of WGS 84 corners.
///
/// `BoundingBox` is `len 4..4` (refused at the request boundary) and puts no
/// `range` on its members, so the coordinate domain and the corner order are this
/// service's own checks.
pub fn wgs84_bounding_box(bounding_box: &[f64]) -> Result<[f64; 4], String> {
    let [west, south, east, north] = match bounding_box {
        [west, south, east, north] => [*west, *south, *east, *north],
        other => {
            return Err(format!(
                "FilterBBox must hold exactly 4 values, [west, south, east, north]; got {}",
                other.len()
            ))
        }
    };
    // Each corner is a `Position` of the same domain, so the two corners are
    // checked by the check every position of this service goes through.
    super::wgs84_position(&[west, south])?;
    super::wgs84_position(&[east, north])?;
    if west > east || south > north {
        return Err(INVERTED_BOUNDING_BOX.to_string());
    }
    Ok([west, south, east, north])
}

/// What a search narrows its results to: the optional filters a request carries.
///
/// Each one is "an optional parameter that limits the search results" in its own
/// member doc, so an absent filter narrows nothing.
#[derive(Debug, Default, Clone)]
pub struct Narrowing<'a> {
    /// `FilterCountries`: "only places that are in a specified list of countries.
    /// Valid values include ISO 3166 3-digit country codes."
    pub countries: Option<&'a [String]>,
    /// `FilterBBox`: "only places that are within the provided bounding box", as
    /// `[west, south, east, north]`.
    pub bounding_box: Option<[f64; 4]>,
    /// `FilterCategories`: "If you include more than one category, the results
    /// will include results that match any of the categories listed."
    pub categories: Option<&'a [String]>,
}

/// One record a query matched, with what the response reports about the match.
#[derive(Debug, Clone)]
pub struct RankedPlace {
    /// The matched record.
    pub record: &'static PlaceRecord,
    /// `SearchForTextResult$Relevance`: "The relative confidence in the match for
    /// a result among the results returned. For example, if more fields for an
    /// address match […] the relevance score is closer to 1." It is the share of
    /// the query's own terms this record accounted for, so a query every part of
    /// which named something about the place scores 1.
    pub relevance: f64,
    /// `SearchForTextResult$Distance`: "The distance in meters of a great-circle
    /// arc between the bias position specified and the result. `Distance` will be
    /// returned only if a bias position was specified in the query."
    pub distance: Option<f64>,
}

/// The records `text` geocodes to, narrowed by `narrowing` and biased toward
/// `bias`, in the order a response lists them: "Search results are returned in
/// order of highest to lowest relevance."
///
/// A record is a result when the query accounted for at least
/// [`MINIMUM_SHARE_OF_QUERY`] of its own terms about it; how much it accounted for
/// is the relevance. Two equally relevant records are ordered by how near they are
/// to the bias position when the request named one — the member's "preference for
/// places that are closer to a specified position" — and otherwise by their own
/// label, so one query always answers in one order.
pub fn search_text(
    text: &str,
    narrowing: &Narrowing<'_>,
    bias: Option<[f64; 2]>,
) -> Vec<RankedPlace> {
    let query = terms(text);
    let mut ranked: Vec<RankedPlace> = PLACES
        .iter()
        .filter(|record| admitted(record, narrowing))
        .filter_map(|record| {
            let relevance = relevance_of(record, &query)?;
            Some(RankedPlace {
                record,
                relevance,
                distance: bias.map(|from| great_circle_metres(from, record.position)),
            })
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .relevance
            .total_cmp(&left.relevance)
            .then_with(|| match (left.distance, right.distance) {
                (Some(near), Some(far)) => near.total_cmp(&far),
                _ => ::std::cmp::Ordering::Equal,
            })
            .then_with(|| label(left.record, None).cmp(&label(right.record, None)))
    });
    ranked
}

/// The record one of `data_source`'s place identifiers names, or `None` when that
/// provider never minted it.
///
/// A `PlaceId` is derived from the record AND the provider ([`place_id`]), so this
/// lookup is what makes the identifier's documented scope hold: "A PlaceId is
/// valid only if all of the following are the same in the original search request
/// and the call to `GetPlace`: Customer Amazon Web Services account, Amazon Web
/// Services Region, Data provider specified in the place index resource" — the
/// INDEX is not among them, so any index of the same provider resolves it, and an
/// index of another provider resolves none of its identifiers.
pub fn record_of_place_id(identifier: &str, data_source: &str) -> Option<&'static PlaceRecord> {
    PLACES
        .iter()
        .find(|record| place_id(record, data_source) == identifier)
}

/// The `PlaceId` `data_source` reports for `record`.
///
/// Derived from the provider and the record's own identity, so it is the same
/// identifier on every read of that place through that provider and a different
/// one through another provider. Opaque by construction: `PlaceId` is modelled as
/// a plain string with no documented form, and nothing may be authorised on it.
pub fn place_id(record: &PlaceRecord, data_source: &str) -> String {
    ::service_common::stable_hex_id(&[
        data_source,
        &label(record, None),
        &format!("{:.6},{:.6}", record.position[0], record.position[1]),
    ])
}

/// The `PlaceId` a TEXT search reports for `record` on a `data_source` index, if
/// that provider reports one at all.
///
/// The `SearchForTextResult$PlaceId` doc: "For `SearchPlaceIndexForText`
/// operations, the `PlaceId` is returned only by place indexes that use HERE or
/// Grab as a data provider."
pub fn text_search_place_id(record: &PlaceRecord, data_source: &str) -> Option<String> {
    (data_source == PROVIDER_HERE || data_source == PROVIDER_GRAB)
        .then(|| place_id(record, data_source))
}

/// The modelled `Place` `data_source` reports for `record`, with names in
/// `language` where the record has them.
pub fn projected_place(
    record: &PlaceRecord,
    data_source: &str,
    language: Option<&str>,
) -> crate::ops::types::Place {
    crate::ops::types::Place {
        label: Some(label(record, language)),
        geometry: crate::ops::types::PlaceGeometry {
            point: Some(record.position.to_vec()),
        },
        address_number: record.address_number.map(str::to_string),
        street: record.street.map(str::to_string),
        neighborhood: record.neighborhood.map(str::to_string),
        municipality: Some(record.municipality.to_string()),
        sub_region: record.sub_region.map(str::to_string),
        region: record.region.map(str::to_string),
        country: Some(record.country.to_string()),
        postal_code: record.postal_code.map(str::to_string),
        // "False if the Place is a known place": every record of the dataset is a
        // place the provider holds, never a position interpolated between two of
        // them.
        interpolated: Some(false),
        time_zone: (data_source == PROVIDER_HERE || data_source == PROVIDER_GRAB).then(|| {
            crate::ops::types::TimeZone {
                name: record.time_zone.0.to_string(),
                offset: Some(record.time_zone.1),
            }
        }),
        unit_type: record
            .unit_type
            .filter(|_| data_source == PROVIDER_ESRI)
            .map(str::to_string),
        unit_number: record
            .unit_number
            .filter(|_| data_source == PROVIDER_ESRI || data_source == PROVIDER_GRAB)
            .map(str::to_string),
        categories: (!record.categories.is_empty())
            .then(|| record.categories.iter().map(|c| (*c).to_string()).collect()),
        // The dataset's categories are all Amazon Location categories, so no
        // record carries one that is unmapped.
        supplemental_categories: None,
        sub_municipality: record
            .sub_municipality
            .filter(|_| data_source == PROVIDER_ESRI)
            .map(str::to_string),
    }
}

/// `Place$Label`: "The full name and address of the point of interest such as a
/// city, region, or country. For example, `123 Any Street, Any Town, USA`."
///
/// Assembled from the record's own components in that order — the name, the street
/// line, the municipality, the region, the postal code, then the country — so a
/// place with no name or no street reads as its address alone, and nothing is
/// invented for a component the place does not have.
pub fn label(record: &PlaceRecord, language: Option<&str>) -> String {
    let street_line = match (record.address_number, record.street) {
        (Some(number), Some(street)) => Some(format!("{number} {street}")),
        (None, Some(street)) => Some(street.to_string()),
        // A number with no street names no line of an address.
        (_, None) => None,
    };
    [
        named(record, language).map(str::to_string),
        street_line,
        Some(record.municipality.to_string()),
        record.region.map(str::to_string),
        record.postal_code.map(str::to_string),
        Some(record.country.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

/// The name `language` asks for, falling back to the record's own name.
///
/// The `Language` member doc: "The preferred language used to return results […]
/// This setting affects the languages used in the results, but not the results
/// themselves […] If the data provider does not have a value" for the language,
/// the result keeps the name it is written under.
fn named(record: &PlaceRecord, language: Option<&str>) -> Option<&'static str> {
    let requested = language.and_then(|tag| {
        let tag = primary_subtag(tag);
        record
            .names_by_language
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(tag))
            .map(|(_, name)| *name)
    });
    requested.or(record.name)
}

/// The primary language subtag of a BCP 47 tag: `en` of `en-US`.
///
/// A record's names are keyed by language, so a request asking for a regional
/// variant is answered with the language's own name rather than with nothing.
fn primary_subtag(tag: &str) -> &str {
    tag.split(['-', '_']).next().unwrap_or(tag)
}

/// Whether `record` survives the request's filters.
fn admitted(record: &PlaceRecord, narrowing: &Narrowing<'_>) -> bool {
    if let Some(countries) = narrowing.countries {
        if !countries
            .iter()
            .any(|country| country.eq_ignore_ascii_case(record.country))
        {
            return false;
        }
    }
    if let Some([west, south, east, north]) = narrowing.bounding_box {
        let [longitude, latitude] = record.position;
        if longitude < west || longitude > east || latitude < south || latitude > north {
            return false;
        }
    }
    if let Some(categories) = narrowing.categories {
        if !categories.iter().any(|requested| {
            record
                .categories
                .iter()
                .any(|held| held.eq_ignore_ascii_case(requested.trim()))
        }) {
            return false;
        }
    }
    true
}

/// How relevant `record` is to `query`, or `None` when the query accounted for too
/// little of it to be a geocode of that text.
///
/// The share of the query's terms the record accounted for: a query whose every
/// term named something about the place scores 1, and one that also named
/// something else scores lower, which is the ordering
/// `SearchForTextResult$Relevance` describes.
fn relevance_of(record: &PlaceRecord, query: &[String]) -> Option<f64> {
    if query.is_empty() {
        return None;
    }
    let components = searchable_terms(record);
    let matched = query
        .iter()
        .filter(|term| components.iter().any(|held| held == *term))
        .count();
    let share = matched as f64 / query.len() as f64;
    (share >= MINIMUM_SHARE_OF_QUERY).then_some(share)
}

/// Every term a query can name this record by: its names, its address components,
/// its country code and the country's name, its postal code and its categories.
fn searchable_terms(record: &PlaceRecord) -> Vec<String> {
    let mut text: Vec<&str> = Vec::new();
    text.extend(record.name);
    text.extend(record.names_by_language.iter().map(|(_, name)| *name));
    text.extend(record.address_number);
    text.extend(record.street);
    text.extend(record.neighborhood);
    text.push(record.municipality);
    text.extend(record.sub_municipality);
    text.extend(record.sub_region);
    text.extend(record.region);
    text.push(record.country);
    text.extend(country_name(record.country));
    text.extend(record.postal_code);
    text.extend(record.categories.iter().copied());
    text.into_iter().flat_map(terms).collect()
}

/// The terms a piece of text carries: its words, lowercased, with everything that
/// is not a letter or a digit read as a separator.
///
/// One rule for the query and for the dataset, so a postal code written
/// `105-0011` and a street written `8th Avenue` are matched the same way from
/// either side.
fn terms(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The box that fully contains `positions`, as `[west, south, east, north]`, or
/// `None` when there are none.
///
/// `SearchPlaceIndexForTextSummary$ResultBBox`: "The bounding box that fully
/// contains all search results." Computed from the results themselves, so a box
/// is never reported for an answer with no results and is contained within the
/// request's own `FilterBBox` whenever one narrowed them.
pub fn enclosing_box(positions: impl IntoIterator<Item = [f64; 2]>) -> Option<Vec<f64>> {
    let mut bounds: Option<[f64; 4]> = None;
    for [longitude, latitude] in positions {
        bounds = Some(match bounds {
            None => [longitude, latitude, longitude, latitude],
            Some([west, south, east, north]) => [
                west.min(longitude),
                south.min(latitude),
                east.max(longitude),
                north.max(latitude),
            ],
        });
    }
    bounds.map(|box_| box_.to_vec())
}

/// The length in metres of the great-circle arc between two WGS 84 positions.
///
/// `SearchForTextResult$Distance` is that arc: "A great-circle arc is the shortest
/// path on a sphere, in this case the Earth. This returns the shortest distance
/// between two locations."
pub fn great_circle_metres(from: [f64; 2], to: [f64; 2]) -> f64 {
    let [from_longitude, from_latitude] = from;
    let [to_longitude, to_latitude] = to;
    let from_latitude = from_latitude.to_radians();
    let to_latitude = to_latitude.to_radians();
    let delta_latitude = to_latitude - from_latitude;
    let delta_longitude = (to_longitude - from_longitude).to_radians();
    let haversine = (delta_latitude / 2.0).sin().powi(2)
        + from_latitude.cos() * to_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METRES * haversine.sqrt().clamp(0.0, 1.0).asin()
}

/// The coordinate domain the dataset's own positions lie in, asserted so a record
/// added to it cannot carry a coordinate no request could name.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{MAX_LATITUDE, MAX_LONGITUDE};

    #[test]
    fn every_record_lies_in_the_wgs84_domain() {
        for record in &PLACES {
            let [longitude, latitude] = record.position;
            assert!(
                longitude.abs() <= MAX_LONGITUDE && latitude.abs() <= MAX_LATITUDE,
                "record {:?} is outside the WGS 84 domain at {:?}",
                label(record, None),
                record.position
            );
        }
    }

    #[test]
    fn every_record_carries_a_known_country_and_categories() {
        for record in &PLACES {
            assert!(
                country_name(record.country).is_some(),
                "record {:?} names the country {:?}, which the dataset has no name for",
                label(record, None),
                record.country
            );
            for category in record.categories {
                assert!(
                    CATEGORIES.contains(category),
                    "record {:?} carries the category {category:?}, which is not published",
                    label(record, None)
                );
            }
        }
    }

    #[test]
    fn a_place_id_is_scoped_to_its_provider() {
        for record in &PLACES {
            let here = place_id(record, PROVIDER_HERE);
            let esri = place_id(record, PROVIDER_ESRI);
            assert_ne!(
                here, esri,
                "record {:?}: one identifier cannot stand for two providers",
                label(record, None)
            );
            assert_eq!(
                record_of_place_id(&here, PROVIDER_HERE).map(|found| label(found, None)),
                Some(label(record, None)),
                "record {:?}: its own provider must resolve its identifier",
                label(record, None)
            );
            assert!(
                record_of_place_id(&here, PROVIDER_ESRI).is_none(),
                "record {:?}: another provider must resolve none of them",
                label(record, None)
            );
        }
    }
}
