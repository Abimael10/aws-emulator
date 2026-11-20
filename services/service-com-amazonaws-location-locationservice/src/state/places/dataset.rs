// SPDX-License-Identifier: CC0-1.0

//! The place records the Places plane of this API geocodes against: the stand-in
//! for a geospatial data provider's own dataset.
//!
//! A place index resource holds no places of its own — `CreatePlaceIndex` stores a
//! provider name and nothing else, and the provider is what answers a search — so
//! the dataset lives beside the state module rather than in the store: every index
//! created against a provider searches the same records, which is what makes a
//! `PlaceId` handed out by one index resolvable through another index of the same
//! provider.
//!
//! Each record is a real place, written with its own address components as the
//! modelled `Place` members name them (`AddressNumber`, `Street`, `Neighborhood`,
//! `Municipality`, `SubMunicipality`, `SubRegion`, `Region`, `Country`,
//! `PostalCode`) and its WGS 84 `[longitude, latitude]` position. Nothing here is
//! derived from a request: a search REPORTS these records, and a record is only
//! ever read.

/// One place of the provider dataset.
///
/// The members mirror the modelled `Place` shape, so a search projects a record
/// without inventing a value for anything: a component the place does not have is
/// `None` here and absent from the response.
#[derive(Debug, Clone, Copy)]
pub struct PlaceRecord {
    /// The name of the point of interest, for a record that names one. A plain
    /// street address has none, and is reported by its address components alone.
    pub name: Option<&'static str>,
    /// The place's own name as other languages write it, as `(BCP 47 tag, name)`
    /// pairs.
    ///
    /// The `Language` member doc states what this is for: "You search for Athens,
    /// Greece, with the language parameter set to `en` […] If you set the language
    /// parameter to `el`, for Greek, then the result found will more likely be
    /// returned as Αθήνα". A place is findable by every one of these names too, so
    /// a query written in one language answers with a record named in another.
    pub names_by_language: &'static [(&'static str, &'static str)],
    /// `Place$AddressNumber` — "The numerical portion of an address".
    pub address_number: Option<&'static str>,
    /// `Place$Street` — "The name for a street or a road to identify a location."
    pub street: Option<&'static str>,
    /// `Place$Neighborhood` — "The name of a community district."
    pub neighborhood: Option<&'static str>,
    /// `Place$Municipality` — "A name for a local area, such as a city or town
    /// name."
    pub municipality: &'static str,
    /// `Place$SubMunicipality` — "An area that's part of a larger municipality."
    pub sub_municipality: Option<&'static str>,
    /// `Place$SubRegion` — "A county, or an area that's part of a larger region."
    pub sub_region: Option<&'static str>,
    /// `Place$Region` — "A name for an area or geographical division, such as a
    /// province or state name."
    pub region: Option<&'static str>,
    /// `Place$Country` — "A country/region specified using ISO 3166 3-digit
    /// country/region code."
    pub country: &'static str,
    /// `Place$PostalCode` — "A group of numbers and letters in a country-specific
    /// format".
    pub postal_code: Option<&'static str>,
    /// WGS 84 `[longitude, latitude]`, as the modelled `Position` orders them.
    pub position: [f64; 2],
    /// The Amazon Location categories that describe this place, drawn from
    /// [`CATEGORIES`].
    pub categories: &'static [&'static str],
    /// The IANA time zone the place lies in, and that zone's offset from UTC in
    /// seconds — the two members `TimeZone` models.
    ///
    /// The offset is the zone's STANDARD-time offset: the dataset carries no
    /// calendar of when a zone observes summer time, so the offset a record
    /// reports is the one its zone keeps outside such a period.
    pub time_zone: (&'static str, i32),
    /// `Place$UnitType` — "For addresses with a `UnitNumber`, the type of unit."
    pub unit_type: Option<&'static str>,
    /// `Place$UnitNumber` — "For addresses with multiple units, the unit
    /// identifier."
    pub unit_number: Option<&'static str>,
}

/// The Amazon Location categories the dataset uses, so a category filter is
/// matched against a closed set rather than against free text.
///
/// Source: the `Place$Categories` member doc, which points at the developer
/// guide's categories list for the values Amazon Location publishes; these are the
/// entries of that list the records below carry. A `FilterCategories` value
/// outside this set is not an error — it simply describes no place, so it narrows
/// a search to nothing.
pub const CATEGORIES: [&str; 8] = [
    "airport",
    "coffee_shop",
    "hotel",
    "market",
    "monument",
    "museum",
    "park",
    "tourist_attraction",
];

/// The English short name of each country the dataset covers, keyed by the ISO
/// 3166 alpha-3 code the records carry.
///
/// A search matches both forms, so a query naming a country by name finds the
/// same places as one naming it by code.
pub const COUNTRY_NAMES: [(&str, &str); 14] = [
    ("AUS", "Australia"),
    ("BRA", "Brazil"),
    ("CAN", "Canada"),
    ("DEU", "Germany"),
    ("FRA", "France"),
    ("GBR", "United Kingdom"),
    ("GRC", "Greece"),
    ("JPN", "Japan"),
    ("MEX", "Mexico"),
    ("MYS", "Malaysia"),
    ("SGP", "Singapore"),
    ("THA", "Thailand"),
    ("USA", "United States"),
    ("ZAF", "South Africa"),
];

/// The English short name of the country `alpha3` codes, when the dataset covers
/// it.
pub fn country_name(alpha3: &str) -> Option<&'static str> {
    COUNTRY_NAMES
        .iter()
        .find(|(code, _)| *code == alpha3)
        .map(|(_, name)| *name)
}

/// Every place of the provider dataset.
///
/// Ordered by country then municipality so the catalog reads as a gazetteer; the
/// order a search answers in is decided by relevance, never by this order.
pub const PLACES: [PlaceRecord; 23] = [
    PlaceRecord {
        name: Some("Sydney Opera House"),
        names_by_language: &[],
        address_number: None,
        street: Some("Bennelong Point"),
        neighborhood: Some("Circular Quay"),
        municipality: "Sydney",
        sub_municipality: None,
        sub_region: Some("Sydney"),
        region: Some("New South Wales"),
        country: "AUS",
        postal_code: Some("2000"),
        position: [151.2153, -33.8568],
        categories: &["tourist_attraction"],
        time_zone: ("Australia/Sydney", 36_000),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Christ the Redeemer"),
        names_by_language: &[("pt", "Cristo Redentor")],
        address_number: None,
        street: Some("Parque Nacional da Tijuca"),
        neighborhood: Some("Alto da Boa Vista"),
        municipality: "Rio de Janeiro",
        sub_municipality: None,
        sub_region: Some("Rio de Janeiro"),
        region: Some("Rio de Janeiro"),
        country: "BRA",
        postal_code: Some("22241-330"),
        position: [-43.2105, -22.9519],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("America/Sao_Paulo", -10_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("CN Tower"),
        names_by_language: &[],
        address_number: Some("290"),
        street: Some("Bremner Boulevard"),
        neighborhood: Some("Entertainment District"),
        municipality: "Toronto",
        sub_municipality: None,
        sub_region: Some("Toronto Division"),
        region: Some("Ontario"),
        country: "CAN",
        postal_code: Some("M5V 3L9"),
        position: [-79.3871, 43.6426],
        categories: &["tourist_attraction"],
        time_zone: ("America/Toronto", -18_000),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Canada Place"),
        names_by_language: &[],
        address_number: Some("999"),
        street: Some("Canada Place"),
        neighborhood: Some("Downtown"),
        municipality: "Vancouver",
        sub_municipality: None,
        sub_region: Some("Metro Vancouver"),
        region: Some("British Columbia"),
        country: "CAN",
        postal_code: Some("V6C 3T4"),
        position: [-123.1117, 49.2888],
        categories: &["tourist_attraction"],
        time_zone: ("America/Vancouver", -28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Brandenburg Gate"),
        names_by_language: &[("de", "Brandenburger Tor")],
        address_number: Some("1"),
        street: Some("Pariser Platz"),
        neighborhood: Some("Mitte"),
        municipality: "Berlin",
        sub_municipality: None,
        sub_region: Some("Berlin"),
        region: Some("Berlin"),
        country: "DEU",
        postal_code: Some("10117"),
        position: [13.3777, 52.5163],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("Europe/Berlin", 3_600),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Eiffel Tower"),
        names_by_language: &[("fr", "Tour Eiffel")],

        address_number: Some("5"),
        street: Some("Avenue Anatole France"),
        neighborhood: Some("7th Arrondissement"),
        municipality: "Paris",
        sub_municipality: None,
        sub_region: Some("Paris"),
        region: Some("Île-de-France"),
        country: "FRA",
        postal_code: Some("75007"),
        position: [2.2945, 48.8584],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("Europe/Paris", 3_600),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Louvre Museum"),
        names_by_language: &[("fr", "Musée du Louvre")],
        address_number: None,
        street: Some("Rue de Rivoli"),
        neighborhood: Some("1st Arrondissement"),
        municipality: "Paris",
        sub_municipality: None,
        sub_region: Some("Paris"),
        region: Some("Île-de-France"),
        country: "FRA",
        postal_code: Some("75001"),
        position: [2.3376, 48.8606],
        categories: &["museum", "tourist_attraction"],
        time_zone: ("Europe/Paris", 3_600),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Tower Bridge"),
        names_by_language: &[],
        address_number: None,
        street: Some("Tower Bridge Road"),
        neighborhood: Some("Southwark"),
        municipality: "London",
        sub_municipality: None,
        sub_region: Some("Greater London"),
        region: Some("England"),
        country: "GBR",
        postal_code: Some("SE1 2UP"),
        position: [-0.0754, 51.5055],
        categories: &["tourist_attraction"],
        time_zone: ("Europe/London", 0),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Acropolis of Athens"),
        names_by_language: &[("el", "Ακρόπολη Αθηνών")],
        address_number: None,
        street: Some("Dionysiou Areopagitou"),
        neighborhood: Some("Plaka"),
        municipality: "Athens",
        sub_municipality: None,
        sub_region: Some("Central Athens"),
        region: Some("Attica"),
        country: "GRC",
        postal_code: Some("10558"),
        position: [23.7257, 37.9715],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("Europe/Athens", 7_200),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Athens"),
        names_by_language: &[("el", "Αθήνα")],
        address_number: None,
        street: None,
        neighborhood: None,
        municipality: "Athens",
        sub_municipality: None,
        sub_region: Some("Central Athens"),
        region: Some("Attica"),
        country: "GRC",
        postal_code: None,
        position: [23.7275, 37.9838],
        categories: &[],
        time_zone: ("Europe/Athens", 7_200),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Tokyo Tower"),
        names_by_language: &[("ja", "東京タワー")],
        address_number: Some("4-2-8"),
        street: Some("Shibakoen"),
        neighborhood: Some("Minato"),
        municipality: "Tokyo",
        sub_municipality: Some("Minato City"),
        sub_region: Some("Tokyo"),
        region: Some("Tokyo"),
        country: "JPN",
        postal_code: Some("105-0011"),
        position: [139.7454, 35.6586],
        categories: &["tourist_attraction"],
        time_zone: ("Asia/Tokyo", 32_400),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Museo Nacional de Antropología"),
        names_by_language: &[("en", "National Museum of Anthropology")],
        address_number: Some("1"),
        street: Some("Avenida Paseo de la Reforma"),
        neighborhood: Some("Chapultepec"),
        municipality: "Mexico City",
        sub_municipality: None,
        sub_region: None,
        region: Some("Ciudad de México"),
        country: "MEX",
        postal_code: Some("11560"),
        position: [-99.1862, 19.4260],
        categories: &["museum", "tourist_attraction"],
        time_zone: ("America/Mexico_City", -21_600),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Petronas Twin Towers"),
        names_by_language: &[("ms", "Menara Berkembar Petronas")],
        address_number: None,
        street: Some("Jalan Ampang"),
        neighborhood: Some("Kuala Lumpur City Centre"),
        municipality: "Kuala Lumpur",
        sub_municipality: None,
        sub_region: None,
        region: Some("Kuala Lumpur"),
        country: "MYS",
        postal_code: Some("50088"),
        position: [101.7117, 3.1578],
        categories: &["tourist_attraction"],
        time_zone: ("Asia/Kuala_Lumpur", 28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Marina Bay Sands"),
        names_by_language: &[],
        address_number: Some("10"),
        street: Some("Bayfront Avenue"),
        neighborhood: Some("Marina Bay"),
        municipality: "Singapore",
        sub_municipality: None,
        sub_region: None,
        region: None,
        country: "SGP",
        postal_code: Some("018956"),
        position: [103.8607, 1.2834],
        categories: &["hotel", "tourist_attraction"],
        time_zone: ("Asia/Singapore", 28_800),
        unit_type: Some("Tower"),
        unit_number: Some("3"),
    },
    PlaceRecord {
        name: Some("Grand Palace"),
        names_by_language: &[("th", "Phra Borom Maha Ratcha Wang")],
        address_number: None,
        street: Some("Na Phra Lan Road"),
        neighborhood: Some("Phra Nakhon"),
        municipality: "Bangkok",
        sub_municipality: None,
        sub_region: None,
        region: Some("Bangkok"),
        country: "THA",
        postal_code: Some("10200"),
        position: [100.4917, 13.7500],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("Asia/Bangkok", 25_200),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Space Needle"),
        names_by_language: &[],
        address_number: Some("400"),
        street: Some("Broad Street"),
        neighborhood: Some("Lower Queen Anne"),
        municipality: "Seattle",
        sub_municipality: None,
        sub_region: Some("King County"),
        region: Some("Washington"),
        country: "USA",
        postal_code: Some("98109"),
        position: [-122.3493, 47.6205],
        categories: &["tourist_attraction"],
        time_zone: ("America/Los_Angeles", -28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Pike Place Market"),
        names_by_language: &[],
        address_number: Some("85"),
        street: Some("Pike Street"),
        neighborhood: Some("Downtown"),
        municipality: "Seattle",
        sub_municipality: None,
        sub_region: Some("King County"),
        region: Some("Washington"),
        country: "USA",
        postal_code: Some("98101"),
        position: [-122.3406, 47.6097],
        categories: &["market", "tourist_attraction"],
        time_zone: ("America/Los_Angeles", -28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Starbucks Reserve Roastery"),
        names_by_language: &[],
        address_number: Some("1124"),
        street: Some("Pike Street"),
        neighborhood: Some("Capitol Hill"),
        municipality: "Seattle",
        sub_municipality: None,
        sub_region: Some("King County"),
        region: Some("Washington"),
        country: "USA",
        postal_code: Some("98101"),
        position: [-122.3275, 47.6142],
        categories: &["coffee_shop"],
        time_zone: ("America/Los_Angeles", -28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: None,
        names_by_language: &[],
        address_number: Some("2001"),
        street: Some("8th Avenue"),
        neighborhood: Some("Belltown"),
        municipality: "Seattle",
        sub_municipality: None,
        sub_region: Some("King County"),
        region: Some("Washington"),
        country: "USA",
        postal_code: Some("98121"),
        position: [-122.3376, 47.6150],
        categories: &[],
        time_zone: ("America/Los_Angeles", -28_800),
        unit_type: Some("Suite"),
        unit_number: Some("1200"),
    },
    PlaceRecord {
        name: Some("Seattle-Tacoma International Airport"),
        names_by_language: &[],
        address_number: Some("17801"),
        street: Some("International Boulevard"),
        neighborhood: None,
        municipality: "SeaTac",
        sub_municipality: None,
        sub_region: Some("King County"),
        region: Some("Washington"),
        country: "USA",
        postal_code: Some("98158"),
        position: [-122.3088, 47.4502],
        categories: &["airport"],
        time_zone: ("America/Los_Angeles", -28_800),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Empire State Building"),
        names_by_language: &[],
        address_number: Some("20"),
        street: Some("West 34th Street"),
        neighborhood: Some("Midtown"),
        municipality: "New York",
        sub_municipality: Some("Manhattan"),
        sub_region: Some("New York County"),
        region: Some("New York"),
        country: "USA",
        postal_code: Some("10001"),
        position: [-73.9857, 40.7484],
        categories: &["tourist_attraction"],
        time_zone: ("America/New_York", -18_000),
        unit_type: Some("Floor"),
        unit_number: Some("86"),
    },
    PlaceRecord {
        name: Some("Statue of Liberty"),
        names_by_language: &[],
        address_number: Some("1"),
        street: Some("Liberty Island"),
        neighborhood: None,
        municipality: "New York",
        sub_municipality: Some("Manhattan"),
        sub_region: Some("New York County"),
        region: Some("New York"),
        country: "USA",
        postal_code: Some("10004"),
        position: [-74.0445, 40.6892],
        categories: &["monument", "tourist_attraction"],
        time_zone: ("America/New_York", -18_000),
        unit_type: None,
        unit_number: None,
    },
    PlaceRecord {
        name: Some("Table Mountain"),
        names_by_language: &[("af", "Tafelberg")],
        address_number: None,
        street: Some("Tafelberg Road"),
        neighborhood: Some("Gardens"),
        municipality: "Cape Town",
        sub_municipality: None,
        sub_region: Some("City of Cape Town"),
        region: Some("Western Cape"),
        country: "ZAF",
        postal_code: Some("8001"),
        position: [18.4041, -33.9625],
        categories: &["park", "tourist_attraction"],
        time_zone: ("Africa/Johannesburg", 7_200),
        unit_type: None,
        unit_number: None,
    },
];
