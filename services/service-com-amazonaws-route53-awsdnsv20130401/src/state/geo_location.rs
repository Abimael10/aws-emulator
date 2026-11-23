// SPDX-License-Identifier: CC0-1.0

//! The geographic locations Route 53 supports for geolocation routing, as the
//! API reports them.
//!
//! `ListGeoLocations` reports this catalog and `GetGeoLocation` resolves one
//! entry of it, so both read the SAME table: a location the listing carries is a
//! location the getter resolves, and one neither holds is a location the service
//! "doesn't support" (`NoSuchGeoLocation`).
//!
//! ## What a location is, and where the codes come from
//!
//! A location is one of three kinds, which is exactly what `GetGeoLocation`'s own
//! documentation describes asking for — "whether a continent is supported …
//! whether a country is supported … whether a subdivision of a country is
//! supported" — and each kind fills only the members of `GeoLocationDetails`
//! that name it:
//!
//! * a CONTINENT carries `ContinentCode`/`ContinentName`. The codes are the seven
//!   `GeoLocation$ContinentCode` documents: "AF: Africa AN: Antarctica AS: Asia
//!   EU: Europe OC: Oceania NA: North America SA: South America", which is also
//!   the order [`CONTINENTS`] lists them in.
//! * a COUNTRY carries `CountryCode`/`CountryName`. `GeoLocation$CountryCode`:
//!   "Amazon Route 53 uses the two-letter country codes that are specified in ISO
//!   standard 3166-1 alpha-2", so [`COUNTRIES`] is that standard's assigned codes
//!   beside the one-character `*` the same member documents as the default
//!   location ("the default resource record set", the location queries that match
//!   no other location are answered from). The NAMES are the service's own, not
//!   the standard's English spelling — see [`COUNTRIES`].
//! * a SUBDIVISION carries its country's code and name PLUS
//!   `SubdivisionCode`/`SubdivisionName`. A geolocation record set's own
//!   `SubdivisionCode` admits only a state of the United States, spelled with
//!   the United States Postal Service's "Appendix B: Two-Letter State and
//!   Possession Abbreviations", but the catalog is wider than that one country,
//!   which is why `GeoLocationDetails$SubdivisionCode` sends a client to this
//!   listing for the codes ("For a list of all supported subdivision codes, use
//!   the ListGeoLocations API") — see [`SUBDIVISIONS`].
//!
//! Nothing here is reached over the network and nothing is derived from a
//! request: the table is the published catalog, and what matters about it is that
//! every entry is one of those three kinds, that no code appears twice, and that
//! two reads agree — never that a particular country is in it.

use crate::ops::types::shared_types::GeoLocationDetails;

/// The continents Route 53 supports, as `(code, name)`.
///
/// Source: `GeoLocation$ContinentCode`'s own documentation, which enumerates the
/// seven supported continents; the rows are in the order it enumerates them, and
/// that is the order the continent part of the catalog is reported in.
pub const CONTINENTS: &[(&str, &str)] = &[
    ("AF", "Africa"),
    ("AN", "Antarctica"),
    ("AS", "Asia"),
    ("EU", "Europe"),
    ("OC", "Oceania"),
    ("NA", "North America"),
    ("SA", "South America"),
];

/// The code of the DEFAULT location — the one country code that is not a country.
///
/// `GeoLocation$CountryCode` documents it as the location a geolocation record
/// set covers when no other location matches the query; it is one character
/// long, which is why `GeoLocationCountryCode` admits a length of 1.
pub const DEFAULT_COUNTRY_CODE: &str = "*";

/// The countries Route 53 supports, as `(code, name)`.
///
/// Source of the CODES: the two-letter codes of ISO 3166-1 alpha-2, which is the
/// standard `GeoLocation$CountryCode` names, as the SERVICE's own geolocation
/// table carries them — which is not the standard's assigned set verbatim:
///
/// * the `*` row is the default location `GeoLocation$CountryCode` documents;
/// * a code the standard leaves to its users is carried when the service places
///   traffic in it (`XK`, Kosovo, which has no assigned alpha-2 code);
/// * a code the service publishes no location for is NOT carried: the catalog
///   holds a location only where the service can place a query in it, so a code
///   it leaves out is one no geolocation record set can ever match.
///
/// A code the table leaves out is a location `GetGeoLocation` refuses with
/// `NoSuchGeoLocation` and `ChangeResourceRecordSets` refuses as unsupported,
/// which is what "Route 53 doesn't support the specified geographic location"
/// means — so this set is the one place either answer comes from.
///
/// Source of the NAMES: the service's own country naming, which is NOT the ISO
/// standard's own English spelling and NOT the current CLDR one either. It keeps
/// a name's non-ASCII letters rather than folding them to ASCII (`Åland`, `Saint
/// Barthélemy`, `Curaçao`, `Réunion`, `São Tomé and Príncipe`), it brackets a
/// parenthetical with square brackets (`Cocos [Keeling] Islands`), and for the
/// rest it keeps the spelling the service has published all along rather than
/// the one a country has since been renamed to: an older short form
/// (`Macedonia`, `Ivory Coast`, `Czech Republic`, `Bonaire`, `Cape Verde`) where
/// the current standard carries a longer or newer one, and a long form
/// (`Republic of Korea`, `Hashemite Kingdom of Jordan`, `Republic of Moldova`,
/// `Federated States of Micronesia`, `Republic of the Congo`) where the current
/// standard carries a short one. A client displays `CountryName` to a user and
/// compares it against what Route 53 returns, so a folded, modernised or
/// re-worded name is a name AWS does not use.
///
/// The rows are spelled out in code order, but the ORDER the wire carries is
/// computed by [`catalog`] rather than being a property of this table.
pub const COUNTRIES: &[(&str, &str)] = &[
    (DEFAULT_COUNTRY_CODE, "Default"),
    ("AD", "Andorra"),
    ("AE", "United Arab Emirates"),
    ("AF", "Afghanistan"),
    ("AG", "Antigua and Barbuda"),
    ("AI", "Anguilla"),
    ("AL", "Albania"),
    ("AM", "Armenia"),
    ("AO", "Angola"),
    ("AQ", "Antarctica"),
    ("AR", "Argentina"),
    ("AS", "American Samoa"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("AW", "Aruba"),
    ("AX", "Åland"),
    ("AZ", "Azerbaijan"),
    ("BA", "Bosnia and Herzegovina"),
    ("BB", "Barbados"),
    ("BD", "Bangladesh"),
    ("BE", "Belgium"),
    ("BF", "Burkina Faso"),
    ("BG", "Bulgaria"),
    ("BH", "Bahrain"),
    ("BI", "Burundi"),
    ("BJ", "Benin"),
    ("BL", "Saint Barthélemy"),
    ("BM", "Bermuda"),
    ("BN", "Brunei"),
    ("BO", "Bolivia"),
    ("BQ", "Bonaire"),
    ("BR", "Brazil"),
    ("BS", "Bahamas"),
    ("BT", "Bhutan"),
    ("BW", "Botswana"),
    ("BY", "Belarus"),
    ("BZ", "Belize"),
    ("CA", "Canada"),
    ("CC", "Cocos [Keeling] Islands"),
    ("CD", "Congo"),
    ("CF", "Central African Republic"),
    ("CG", "Republic of the Congo"),
    ("CH", "Switzerland"),
    ("CI", "Ivory Coast"),
    ("CK", "Cook Islands"),
    ("CL", "Chile"),
    ("CM", "Cameroon"),
    ("CN", "China"),
    ("CO", "Colombia"),
    ("CR", "Costa Rica"),
    ("CU", "Cuba"),
    ("CV", "Cape Verde"),
    ("CW", "Curaçao"),
    ("CY", "Cyprus"),
    ("CZ", "Czech Republic"),
    ("DE", "Germany"),
    ("DJ", "Djibouti"),
    ("DK", "Denmark"),
    ("DM", "Dominica"),
    ("DO", "Dominican Republic"),
    ("DZ", "Algeria"),
    ("EC", "Ecuador"),
    ("EE", "Estonia"),
    ("EG", "Egypt"),
    ("ER", "Eritrea"),
    ("ES", "Spain"),
    ("ET", "Ethiopia"),
    ("FI", "Finland"),
    ("FJ", "Fiji"),
    ("FK", "Falkland Islands"),
    ("FM", "Federated States of Micronesia"),
    ("FO", "Faroe Islands"),
    ("FR", "France"),
    ("GA", "Gabon"),
    ("GB", "United Kingdom"),
    ("GD", "Grenada"),
    ("GE", "Georgia"),
    ("GF", "French Guiana"),
    ("GG", "Guernsey"),
    ("GH", "Ghana"),
    ("GI", "Gibraltar"),
    ("GL", "Greenland"),
    ("GM", "Gambia"),
    ("GN", "Guinea"),
    ("GP", "Guadeloupe"),
    ("GQ", "Equatorial Guinea"),
    ("GR", "Greece"),
    ("GS", "South Georgia and the South Sandwich Islands"),
    ("GT", "Guatemala"),
    ("GU", "Guam"),
    ("GW", "Guinea-Bissau"),
    ("GY", "Guyana"),
    ("HK", "Hong Kong"),
    ("HN", "Honduras"),
    ("HR", "Croatia"),
    ("HT", "Haiti"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IM", "Isle of Man"),
    ("IN", "India"),
    ("IO", "British Indian Ocean Territory"),
    ("IQ", "Iraq"),
    ("IR", "Iran"),
    ("IS", "Iceland"),
    ("IT", "Italy"),
    ("JE", "Jersey"),
    ("JM", "Jamaica"),
    ("JO", "Hashemite Kingdom of Jordan"),
    ("JP", "Japan"),
    ("KE", "Kenya"),
    ("KG", "Kyrgyzstan"),
    ("KH", "Cambodia"),
    ("KI", "Kiribati"),
    ("KM", "Comoros"),
    ("KN", "Saint Kitts and Nevis"),
    ("KP", "North Korea"),
    ("KR", "Republic of Korea"),
    ("KW", "Kuwait"),
    ("KY", "Cayman Islands"),
    ("KZ", "Kazakhstan"),
    ("LA", "Laos"),
    ("LB", "Lebanon"),
    ("LC", "Saint Lucia"),
    ("LI", "Liechtenstein"),
    ("LK", "Sri Lanka"),
    ("LR", "Liberia"),
    ("LS", "Lesotho"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("LV", "Latvia"),
    ("LY", "Libya"),
    ("MA", "Morocco"),
    ("MC", "Monaco"),
    ("MD", "Republic of Moldova"),
    ("ME", "Montenegro"),
    ("MF", "Saint Martin"),
    ("MG", "Madagascar"),
    ("MH", "Marshall Islands"),
    ("MK", "Macedonia"),
    ("ML", "Mali"),
    ("MM", "Myanmar [Burma]"),
    ("MN", "Mongolia"),
    ("MO", "Macao"),
    ("MP", "Northern Mariana Islands"),
    ("MQ", "Martinique"),
    ("MR", "Mauritania"),
    ("MS", "Montserrat"),
    ("MT", "Malta"),
    ("MU", "Mauritius"),
    ("MV", "Maldives"),
    ("MW", "Malawi"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("MZ", "Mozambique"),
    ("NA", "Namibia"),
    ("NC", "New Caledonia"),
    ("NE", "Niger"),
    ("NF", "Norfolk Island"),
    ("NG", "Nigeria"),
    ("NI", "Nicaragua"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NP", "Nepal"),
    ("NR", "Nauru"),
    ("NU", "Niue"),
    ("NZ", "New Zealand"),
    ("OM", "Oman"),
    ("PA", "Panama"),
    ("PE", "Peru"),
    ("PF", "French Polynesia"),
    ("PG", "Papua New Guinea"),
    ("PH", "Philippines"),
    ("PK", "Pakistan"),
    ("PL", "Poland"),
    ("PM", "Saint Pierre and Miquelon"),
    ("PN", "Pitcairn Islands"),
    ("PR", "Puerto Rico"),
    ("PS", "Palestine"),
    ("PT", "Portugal"),
    ("PW", "Palau"),
    ("PY", "Paraguay"),
    ("QA", "Qatar"),
    ("RE", "Réunion"),
    ("RO", "Romania"),
    ("RS", "Serbia"),
    ("RU", "Russia"),
    ("RW", "Rwanda"),
    ("SA", "Saudi Arabia"),
    ("SB", "Solomon Islands"),
    ("SC", "Seychelles"),
    ("SD", "Sudan"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("SH", "Saint Helena"),
    ("SI", "Slovenia"),
    ("SJ", "Svalbard and Jan Mayen"),
    ("SK", "Slovakia"),
    ("SL", "Sierra Leone"),
    ("SM", "San Marino"),
    ("SN", "Senegal"),
    ("SO", "Somalia"),
    ("SR", "Suriname"),
    ("SS", "South Sudan"),
    ("ST", "São Tomé and Príncipe"),
    ("SV", "El Salvador"),
    ("SX", "Sint Maarten"),
    ("SY", "Syria"),
    ("SZ", "Swaziland"),
    ("TC", "Turks and Caicos Islands"),
    ("TD", "Chad"),
    ("TF", "French Southern Territories"),
    ("TG", "Togo"),
    ("TH", "Thailand"),
    ("TJ", "Tajikistan"),
    ("TK", "Tokelau"),
    ("TL", "East Timor"),
    ("TM", "Turkmenistan"),
    ("TN", "Tunisia"),
    ("TO", "Tonga"),
    ("TR", "Turkey"),
    ("TT", "Trinidad and Tobago"),
    ("TV", "Tuvalu"),
    ("TW", "Taiwan"),
    ("TZ", "Tanzania"),
    ("UA", "Ukraine"),
    ("UG", "Uganda"),
    ("UM", "U.S. Minor Outlying Islands"),
    ("US", "United States"),
    ("UY", "Uruguay"),
    ("UZ", "Uzbekistan"),
    ("VA", "Vatican City"),
    ("VC", "Saint Vincent and the Grenadines"),
    ("VE", "Venezuela"),
    ("VG", "British Virgin Islands"),
    ("VI", "U.S. Virgin Islands"),
    ("VN", "Vietnam"),
    ("VU", "Vanuatu"),
    ("WF", "Wallis and Futuna"),
    ("WS", "Samoa"),
    ("XK", "Kosovo"),
    ("YE", "Yemen"),
    ("YT", "Mayotte"),
    ("ZA", "South Africa"),
    ("ZM", "Zambia"),
    ("ZW", "Zimbabwe"),
];

/// The subdivisions Route 53 supports, as `(country code, subdivision code,
/// subdivision name)`.
///
/// A geolocation record set's own `SubdivisionCode` takes "the two-letter code
/// for a state of the United States", from the abbreviations the United States
/// Postal Service publishes in "Appendix B: Two-Letter State and Possession
/// Abbreviations" — but the CATALOG is wider than that one country, which is why
/// `GeoLocationDetails$SubdivisionCode` sends a client here for it ("For a list
/// of all supported subdivision codes, use the ListGeoLocations API") instead of
/// to Appendix B alone. The other country the service places traffic by
/// subdivision in is the one `GeoLocation$CountryCode` singles out beside the
/// standard ("Route 53 also supports the country code UA for Ukraine"), whose
/// rows carry that country's own subdivision codes and names — mostly the numeric
/// codes ISO 3166-2 assigns its regions, which is why
/// `GeoLocationSubdivisionCode` admits up to three characters rather than exactly
/// two, beside that same standard's romanised names for them.
///
/// What this table is, though, is the SERVICE's published catalog and not the
/// standard it is mostly drawn from: where the service publishes a territory
/// under a code and a name of its own, THAT is the row carried here, and the
/// standard's code for the same territory is carried by no row at all — the
/// catalog holds a location only where the service places traffic in it, so a
/// code the service publishes no location for is one `GetGeoLocation` refuses and
/// one no geolocation record set can match. The two rows that depart from the
/// standard's numbering are the peninsula territories the service publishes as
/// `11`/`Crimea` and `20`/`Sevastopol`, which that standard instead numbers `43`
/// and `40`.
///
/// The table is keyed by country rather than hard-wired to one, so a country the
/// service starts placing traffic by subdivision in is rows here and no change
/// to either handler.
pub const SUBDIVISIONS: &[(&str, &str, &str)] = &[
    ("UA", "05", "Vinnytska oblast"),
    ("UA", "07", "Volynska oblast"),
    ("UA", "09", "Luhanska oblast"),
    ("UA", "11", "Crimea"),
    ("UA", "12", "Dnipropetrovska oblast"),
    ("UA", "14", "Donetska oblast"),
    ("UA", "18", "Zhytomyrska oblast"),
    ("UA", "20", "Sevastopol"),
    ("UA", "21", "Zakarpatska oblast"),
    ("UA", "23", "Zaporizka oblast"),
    ("UA", "26", "Ivano-Frankivska oblast"),
    ("UA", "30", "Kyiv"),
    ("UA", "32", "Kyivska oblast"),
    ("UA", "35", "Kirovohradska oblast"),
    ("UA", "46", "Lvivska oblast"),
    ("UA", "48", "Mykolaivska oblast"),
    ("UA", "51", "Odeska oblast"),
    ("UA", "53", "Poltavska oblast"),
    ("UA", "56", "Rivnenska oblast"),
    ("UA", "59", "Sumska oblast"),
    ("UA", "61", "Ternopilska oblast"),
    ("UA", "63", "Kharkivska oblast"),
    ("UA", "65", "Khersonska oblast"),
    ("UA", "68", "Khmelnytska oblast"),
    ("UA", "71", "Cherkaska oblast"),
    ("UA", "74", "Chernihivska oblast"),
    ("UA", "77", "Chernivetska oblast"),
    ("US", "AK", "Alaska"),
    ("US", "AL", "Alabama"),
    ("US", "AR", "Arkansas"),
    ("US", "AZ", "Arizona"),
    ("US", "CA", "California"),
    ("US", "CO", "Colorado"),
    ("US", "CT", "Connecticut"),
    ("US", "DC", "District of Columbia"),
    ("US", "DE", "Delaware"),
    ("US", "FL", "Florida"),
    ("US", "GA", "Georgia"),
    ("US", "HI", "Hawaii"),
    ("US", "IA", "Iowa"),
    ("US", "ID", "Idaho"),
    ("US", "IL", "Illinois"),
    ("US", "IN", "Indiana"),
    ("US", "KS", "Kansas"),
    ("US", "KY", "Kentucky"),
    ("US", "LA", "Louisiana"),
    ("US", "MA", "Massachusetts"),
    ("US", "MD", "Maryland"),
    ("US", "ME", "Maine"),
    ("US", "MI", "Michigan"),
    ("US", "MN", "Minnesota"),
    ("US", "MO", "Missouri"),
    ("US", "MS", "Mississippi"),
    ("US", "MT", "Montana"),
    ("US", "NC", "North Carolina"),
    ("US", "ND", "North Dakota"),
    ("US", "NE", "Nebraska"),
    ("US", "NH", "New Hampshire"),
    ("US", "NJ", "New Jersey"),
    ("US", "NM", "New Mexico"),
    ("US", "NV", "Nevada"),
    ("US", "NY", "New York"),
    ("US", "OH", "Ohio"),
    ("US", "OK", "Oklahoma"),
    ("US", "OR", "Oregon"),
    ("US", "PA", "Pennsylvania"),
    ("US", "RI", "Rhode Island"),
    ("US", "SC", "South Carolina"),
    ("US", "SD", "South Dakota"),
    ("US", "TN", "Tennessee"),
    ("US", "TX", "Texas"),
    ("US", "UT", "Utah"),
    ("US", "VA", "Virginia"),
    ("US", "VT", "Vermont"),
    ("US", "WA", "Washington"),
    ("US", "WI", "Wisconsin"),
    ("US", "WV", "West Virginia"),
    ("US", "WY", "Wyoming"),
];

/// One location as a request names it: exactly one of the three forms
/// `GetGeoLocation`'s documented syntax admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location<'a> {
    /// A continent, named by its code alone.
    Continent(&'a str),
    /// A country, named by its code alone.
    Country(&'a str),
    /// A subdivision of a country, which takes both codes: a subdivision code
    /// says nothing on its own ("you must include both startcountrycode and
    /// startsubdivisioncode").
    Subdivision {
        /// The country the subdivision belongs to.
        country_code: &'a str,
        /// The subdivision's own code.
        subdivision_code: &'a str,
    },
}

/// A continent entry: the two members that name a continent, and no other.
fn continent_entry(code: &str, name: &str) -> GeoLocationDetails {
    GeoLocationDetails {
        continent_code: Some(code.to_string()),
        continent_name: Some(name.to_string()),
        ..GeoLocationDetails::default()
    }
}

/// A country entry: the two members that name a country, and no other.
fn country_entry(code: &str, name: &str) -> GeoLocationDetails {
    GeoLocationDetails {
        country_code: Some(code.to_string()),
        country_name: Some(name.to_string()),
        ..GeoLocationDetails::default()
    }
}

/// A subdivision entry: the subdivision's own code and name, plus the country it
/// is a subdivision OF — a subdivision is reported inside its country, so the
/// entry carries both ("the codes and full continent, country, and subdivision
/// names for the specified geolocation code").
fn subdivision_entry(
    country_code: &str,
    country_name: &str,
    subdivision_code: &str,
    subdivision_name: &str,
) -> GeoLocationDetails {
    GeoLocationDetails {
        country_code: Some(country_code.to_string()),
        country_name: Some(country_name.to_string()),
        subdivision_code: Some(subdivision_code.to_string()),
        subdivision_name: Some(subdivision_name.to_string()),
        ..GeoLocationDetails::default()
    }
}

/// The supported locations in the order the API reports them.
///
/// `ListGeoLocations`'s own documentation states that order: "Countries are
/// listed first, and continents are listed last. If Amazon Route 53 supports
/// subdivisions for a country (for example, states or provinces), the
/// subdivisions for that country are listed in alphabetical order immediately
/// after the corresponding country." So: every country, in ascending code order,
/// each one followed straight away by its own subdivisions in ascending code
/// order, and then the continents, in the order
/// `GeoLocation$ContinentCode` enumerates them.
///
/// A code is listed once: a repeated row in [`COUNTRIES`] or [`SUBDIVISIONS`]
/// collapses here, so no caller can see one location as two.
pub fn catalog() -> Vec<GeoLocationDetails> {
    let mut countries: Vec<(&str, &str)> = COUNTRIES.to_vec();
    countries.sort_unstable_by_key(|(code, _)| *code);
    countries.dedup_by(|(left, _), (right, _)| left == right);

    let mut catalog = Vec::with_capacity(COUNTRIES.len() + SUBDIVISIONS.len() + CONTINENTS.len());
    for (country_code, country_name) in countries {
        catalog.push(country_entry(country_code, country_name));

        let mut subdivisions: Vec<(&str, &str)> = SUBDIVISIONS
            .iter()
            .filter(|(code, _, _)| *code == country_code)
            .map(|(_, code, name)| (*code, *name))
            .collect();
        subdivisions.sort_unstable_by_key(|(code, _)| *code);
        subdivisions.dedup_by(|(left, _), (right, _)| left == right);
        for (subdivision_code, subdivision_name) in subdivisions {
            catalog.push(subdivision_entry(
                country_code,
                country_name,
                subdivision_code,
                subdivision_name,
            ));
        }
    }
    for (continent_code, continent_name) in CONTINENTS {
        catalog.push(continent_entry(continent_code, continent_name));
    }
    catalog
}

/// Whether `held` is the code `asked` names.
///
/// A geolocation code is an identifier rather than a label — the same two letters
/// select the same location however a client spells them — so the comparison is
/// case-insensitive, and the CATALOG's spelling is what the response carries.
fn same_code(held: Option<&String>, asked: &str) -> bool {
    held.is_some_and(|held| held.eq_ignore_ascii_case(asked))
}

/// Whether `entry` is the location `wanted` names.
///
/// Each kind is matched on the members that NAME it and on the absence of the
/// finer one: a request for the country `US` names the country entry, not the
/// `US`/`NY` subdivision that carries the same country code.
fn is(entry: &GeoLocationDetails, wanted: Location<'_>) -> bool {
    match wanted {
        Location::Continent(code) => same_code(entry.continent_code.as_ref(), code),
        Location::Country(code) => {
            same_code(entry.country_code.as_ref(), code) && entry.subdivision_code.is_none()
        }
        Location::Subdivision {
            country_code,
            subdivision_code,
        } => {
            same_code(entry.country_code.as_ref(), country_code)
                && same_code(entry.subdivision_code.as_ref(), subdivision_code)
        }
    }
}

/// Where in `catalog` the location `wanted` sits, or `None` when the catalog
/// holds no such location.
///
/// One function for both handlers: the position is what `ListGeoLocations`
/// resumes a page at, and that the position EXISTS is what `GetGeoLocation`
/// answers with the entry rather than `NoSuchGeoLocation`.
pub fn position(catalog: &[GeoLocationDetails], wanted: Location<'_>) -> Option<usize> {
    catalog.iter().position(|entry| is(entry, wanted))
}

/// The entry of `catalog` the location `wanted` names, or `None` when the catalog
/// holds no such location — which is what "Route 53 doesn't support the specified
/// geographic location" means.
pub fn find<'a>(
    catalog: &'a [GeoLocationDetails],
    wanted: Location<'_>,
) -> Option<&'a GeoLocationDetails> {
    catalog.iter().find(|entry| is(entry, wanted))
}
