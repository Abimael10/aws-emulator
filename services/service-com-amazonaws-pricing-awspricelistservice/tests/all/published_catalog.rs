// SPDX-License-Identifier: CC0-1.0

//! What the Price List catalog PUBLISHES, over tables of inputs rather than one
//! recorded request.
//!
//! The rules under test:
//!
//! * `GetAttributeValues` enumerates the whole published value set of an attribute, in
//!   one defined order, and carries a `NextToken` whenever that set outruns the
//!   requested page — so a client that pages sees every value exactly once.
//! * every value it publishes is a value some product of the same service carries, so a
//!   `GetProducts` filter built from the enumeration matches something.
//! * a filtered `GetProducts` query whose matched set outruns the requested page carries
//!   a `NextToken` too, and paging it yields that set once.
//! * `ListPriceLists` reports each Region's price list once, at the version in effect at
//!   the requested `EffectiveDate`, advertising exactly the formats the catalog serves a
//!   file for; every one of them resolves through `GetPriceListFileUrl`, and the flat
//!   (CSV) and nested (JSON) forms of a file carry the same priced dimensions.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_pricing_awspricelistservice::state::{
    render_price_list_file, PricingState,
};

use super::pricing_support::{
    all_attribute_values, all_offer_documents, attribute_values, call, column_of, csv_rows,
    list_member, offer_attribute, offer_dimension_values, offer_documents, ok, product_sku, router,
    router_with_engine, service_entry, split_https_url, JAN_1_2024, JUL_1_2024, MAX_PAGE,
};

/// Instance types the Price List API publishes `instanceType` values for, across the
/// families and generations the catalog prices: the source is the published instance-type
/// list, and none of these is a type the catalog's smaller seeds named.
const PUBLISHED_INSTANCE_TYPES: &[&str] = &[
    "a1.medium",
    "a1.2xlarge",
    "a1.metal",
    "c4.large",
    "c5.9xlarge",
    "c5a.24xlarge",
    "c5n.metal",
    "c6g.medium",
    "c6i.32xlarge",
    "d2.8xlarge",
    "g4dn.xlarge",
    "i3.metal",
    "m4.10xlarge",
    "m5.metal",
    "m5a.4xlarge",
    "m5n.large",
    "m6g.medium",
    "m6i.32xlarge",
    "p3.16xlarge",
    "r4.16xlarge",
    "r5.24xlarge",
    "r6g.medium",
    "t2.nano",
    "t3a.small",
    "t4g.nano",
    "x1.32xlarge",
    "x1e.xlarge",
    "z1d.3xlarge",
];

/// The `yyyymmddHHMMSS` stamp an instant names, for comparing a price-list version
/// against the `EffectiveDate` it was selected for.
fn version_stamp_at(epoch_secs: i64) -> String {
    ::aws_smithy_types::DateTime::from_secs(epoch_secs)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_digit)
        .collect()
}

/// The version segment of a price-list ARN (`…/<currency>/<version>/<region>`).
fn arn_version(arn: &str) -> String {
    arn.rsplit('/').nth(1).unwrap_or_default().to_string()
}

/// Whether `value` names a member of the family `family` names: the member's name is the
/// family's, extended with `.` and the size (`a1.large` of `a1`).
fn is_member_of(value: &str, family: &str) -> bool {
    value
        .strip_prefix(family)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `value` names a family that `values` also publishes members of.
fn names_a_family(value: &str, values: &[String]) -> bool {
    values.iter().any(|other| is_member_of(other, value))
}

#[tokio::test]
async fn an_attribute_enumeration_spans_pages_and_yields_every_value_once() {
    let router = router();

    let query = json!({ "ServiceCode": "AmazonEC2", "AttributeName": "instanceType" });
    let published = all_attribute_values(&router, &query).await;

    // The published value set, not a sample of it: every type the catalog prices is in
    // it, including ones no smaller seed named.
    for instance_type in PUBLISHED_INSTANCE_TYPES.iter().copied() {
        assert!(
            published.iter().any(|value| value == instance_type),
            "case {instance_type}: the published instance-type vocabulary must carry it, \
             got {} values",
            published.len()
        );
    }
    assert!(
        published.len() > usize::try_from(MAX_PAGE).expect("a small page"),
        "the published value set must outrun one page of {MAX_PAGE}, got {}",
        published.len()
    );

    // One defined order, with no value twice — so a page boundary always names the same
    // place in the set: ascending, except that a value which NAMES A FAMILY the service
    // also publishes members of (`a1` beside `a1.large`) is published behind that
    // family's members, which is where the API lists it.
    let distinct: BTreeSet<&String> = published.iter().collect();
    assert_eq!(
        distinct.len(),
        published.len(),
        "the values must be enumerated without duplicates, got {} distinct of {}",
        distinct.len(),
        published.len()
    );
    let families: Vec<String> = published
        .iter()
        .filter(|value| names_a_family(value, &published))
        .cloned()
        .collect();
    assert!(
        !families.is_empty(),
        "the published instance-type vocabulary must carry family-level values beside \
         the per-size ones, got {} values",
        published.len()
    );
    for family in &families {
        let family_at = published
            .iter()
            .position(|value| value == family)
            .expect("the family value is published");
        for (position, value) in published.iter().enumerate() {
            if is_member_of(value, family) {
                assert!(
                    position < family_at,
                    "case {family}: the family is published behind its members, but \
                     {value} is published at {position} after it at {family_at}"
                );
            }
        }
    }
    let sized: Vec<&String> = published
        .iter()
        .filter(|value| !families.contains(value))
        .collect();
    let mut ascending = sized.clone();
    ascending.sort();
    assert_eq!(
        sized, ascending,
        "read without the family-level values, the enumeration must be ascending"
    );

    // Rows differ in page size, including both published bounds of MaxResults on this
    // operation (1..10000, so the whole set in one page) and the largest page the other
    // listings take: each page is the next slice of the same set, and the token is
    // present for exactly as long as values remain.
    for page_size in [1_i64, 2, 5, 10, MAX_PAGE] {
        let mut walked: Vec<String> = Vec::new();
        let mut body = query.clone();
        body["MaxResults"] = json!(page_size);
        let mut pages = 0usize;
        loop {
            let page = ok(&router, "GetAttributeValues", &body).await;
            let values = attribute_values(&page);
            assert!(
                values.len() <= usize::try_from(page_size).expect("a small page"),
                "case MaxResults={page_size} page {pages}: a page must not exceed the \
                 page size, got {}",
                values.len()
            );
            walked.extend(values);
            pages += 1;
            let token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                token.is_some(),
                walked.len() < published.len(),
                "case MaxResults={page_size} page {pages}: expected a NextToken while \
                 {} of {} values remain, got {page}",
                published.len().saturating_sub(walked.len()),
                published.len()
            );
            match token {
                Some(token) => body["NextToken"] = json!(token),
                None => break,
            }
            assert!(
                pages <= published.len(),
                "case MaxResults={page_size}: paging must terminate"
            );
        }
        assert_eq!(
            walked, published,
            "case MaxResults={page_size}: paging must yield every value once, in order"
        );
    }

    // A value set smaller than the page carries no token at all: the token says more
    // remains, not that the operation is paginated.
    let small = ok(
        &router,
        "GetAttributeValues",
        &json!({
            "ServiceCode": "AmazonEC2",
            "AttributeName": "volumeType",
            "MaxResults": MAX_PAGE,
        }),
    )
    .await;
    assert!(
        small.get("NextToken").is_none_or(Value::is_null),
        "case volumeType/MaxResults={MAX_PAGE}: a complete page carries no NextToken, \
         got {small}"
    );
}

#[tokio::test]
async fn every_published_attribute_value_selects_a_priced_product() {
    let router = router();

    // Rows: every value of the attribute whose value set is the largest the catalog
    // publishes. A client that enumerates an attribute and filters on what it read must
    // get products back — so each value selects at least one, and every product it
    // selects carries that value and is priced.
    let published = all_attribute_values(
        &router,
        &json!({ "ServiceCode": "AmazonEC2", "AttributeName": "instanceType" }),
    )
    .await;
    assert!(
        !published.is_empty(),
        "the enumeration must publish values to filter on"
    );

    for instance_type in &published {
        let documents = all_offer_documents(
            &router,
            &json!({
                "ServiceCode": "AmazonEC2",
                "Filters": [
                    { "Type": "TERM_MATCH", "Field": "instanceType", "Value": instance_type },
                ],
            }),
        )
        .await;
        assert!(
            !documents.is_empty(),
            "case instanceType={instance_type}: an enumerated value must select at least \
             one product"
        );
        for document in &documents {
            assert_eq!(
                offer_attribute(document, "instanceType").as_deref(),
                Some(instance_type.as_str()),
                "case instanceType={instance_type}: a selected product must carry the \
                 filtered value, got {document}"
            );
            assert!(
                document.pointer("/terms/OnDemand").is_some(),
                "case instanceType={instance_type}: {} must be priced on demand",
                product_sku(document)
            );
        }
    }
}

#[tokio::test]
async fn a_filtered_query_longer_than_the_page_is_carried_on_a_next_token() {
    let router = router();

    // Rows differ in the filter and in how many products it selects, from a whole
    // service down to one dimension of it — each selecting more than one page of the
    // published catalog, which is what makes the token a requirement rather than an
    // option.
    let filters: &[Value] = &[
        json!([{ "Type": "TERM_MATCH", "Field": "ServiceCode", "Value": "AmazonEC2" }]),
        json!([{ "Type": "TERM_MATCH", "Field": "location", "Value": "US East (N. Virginia)" }]),
        json!([{ "Type": "TERM_MATCH", "Field": "operatingSystem", "Value": "Linux" }]),
        json!([
            { "Type": "TERM_MATCH", "Field": "instanceFamily", "Value": "General purpose" },
            { "Type": "TERM_MATCH", "Field": "tenancy", "Value": "Shared" },
        ]),
        json!([{ "Type": "CONTAINS", "Field": "usagetype", "Value": "BoxUsage" }]),
    ];

    for filter in filters {
        let case = filter.to_string();
        let query = json!({ "ServiceCode": "AmazonEC2", "Filters": filter });
        let matched: Vec<String> = all_offer_documents(&router, &query)
            .await
            .iter()
            .map(product_sku)
            .collect();
        let distinct: BTreeSet<&String> = matched.iter().collect();
        assert_eq!(
            distinct.len(),
            matched.len(),
            "case {case}: paging must not repeat a product"
        );
        assert!(
            matched.len() > usize::try_from(MAX_PAGE).expect("a small page"),
            "case {case}: the matched set must outrun one page of {MAX_PAGE}, got {}",
            matched.len()
        );

        // At the largest page the operation takes, the answer still reports that more
        // remains — and the token walks to exactly the matched set.
        for page_size in [1_i64, 10, MAX_PAGE] {
            let mut body = query.clone();
            body["MaxResults"] = json!(page_size);
            let first = ok(&router, "GetProducts", &body).await;
            assert!(
                first
                    .get("NextToken")
                    .and_then(Value::as_str)
                    .is_some_and(|token| !token.is_empty()),
                "case {case}/MaxResults={page_size}: expected a NextToken while products \
                 remain, got {first}"
            );
        }
    }
}

#[tokio::test]
async fn a_price_list_is_reported_at_the_version_in_effect_and_in_every_format_it_serves() {
    let (router, engine) = router_with_engine();

    // Rows differ in the service, the Region the price list covers and the currency its
    // files are denominated in, including a service priced with one usage type per
    // Region and the partition whose files are not denominated in USD.
    let cases: &[(&str, &str, &str)] = &[
        ("AmazonEC2", "us-east-1", "USD"),
        ("AmazonEC2", "eu-west-1", "USD"),
        ("AmazonEC2", "cn-north-1", "CNY"),
        ("AmazonS3", "us-east-1", "USD"),
        ("AWSLambda", "us-east-1", "USD"),
    ];

    for (service_code, region_code, currency_code) in cases.iter().copied() {
        // Rows over the EffectiveDate too: the instant selects a version, so each
        // instant reports the Region once, at the newest version it reaches.
        let mut by_date: BTreeMap<i64, String> = BTreeMap::new();
        for effective_date in [JAN_1_2024, JUL_1_2024] {
            let case = format!("{service_code}/{region_code}@{effective_date}");
            let listed = ok(
                &router,
                "ListPriceLists",
                &json!({
                    "ServiceCode": service_code,
                    "CurrencyCode": currency_code,
                    "RegionCode": region_code,
                    "EffectiveDate": effective_date,
                }),
            )
            .await;
            let entries = list_member(&listed, "PriceLists");
            assert_eq!(
                entries.len(),
                1,
                "case {case}: the Region's price list must be reported once, got {listed}"
            );
            let entry = &entries[0];
            let arn = entry
                .get("PriceListArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let version = arn_version(&arn);
            assert!(
                version.len() == 14 && version.chars().all(char::is_numeric),
                "case {case}: the ARN must name a published version, got {arn}"
            );
            assert!(
                version <= version_stamp_at(effective_date),
                "case {case}: a reported version must already be in effect at the \
                 requested date, got {version}"
            );
            by_date.insert(effective_date, version.clone());

            // FileFormats is a contract: exactly the formats the catalog serves a file
            // for, each resolvable through GetPriceListFileUrl and actually served.
            let formats: Vec<String> = list_member(entry, "FileFormats")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            assert!(
                formats.len() > 1,
                "case {case}: a price list is published in more than one format, got \
                 {formats:?}"
            );

            let mut skus_per_format: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            let mut dimensions_per_format: BTreeMap<String, usize> = BTreeMap::new();
            for file_format in &formats {
                let resolved = ok(
                    &router,
                    "GetPriceListFileUrl",
                    &json!({ "PriceListArn": arn, "FileFormat": file_format }),
                )
                .await;
                let url = resolved
                    .get("Url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    url.ends_with(&format!(".{file_format}")),
                    "case {case}/{file_format}: a listed format must resolve to a URL for \
                     it, got {url:?}"
                );
                let (_, path) = split_https_url(&url).unwrap_or_else(|| {
                    panic!("case {case}/{file_format}: not an https URL: {url:?}")
                });
                let state = engine.state::<PricingState>();
                let (content_type, body) = render_price_list_file(&state, &path)
                    .unwrap_or_else(|| {
                        panic!("case {case}/{file_format}: nothing is served at {path:?}")
                    });
                match file_format.as_str() {
                    "json" => {
                        assert_eq!(
                            content_type, "application/json",
                            "case {case}/{file_format}: wrong media type"
                        );
                        let file: Value = serde_json::from_str(&body).unwrap_or_else(|e| {
                            panic!("case {case}/{file_format}: must be JSON: {e}")
                        });
                        assert_eq!(
                            file.get("version").and_then(Value::as_str),
                            Some(version.as_str()),
                            "case {case}/{file_format}: the file must state the version \
                             its ARN names"
                        );
                        let skus: BTreeSet<String> = file
                            .get("products")
                            .and_then(Value::as_object)
                            .map(|products| products.keys().cloned().collect())
                            .unwrap_or_default();
                        let dimensions = json_dimensions(&file);
                        skus_per_format.insert(file_format.clone(), skus);
                        dimensions_per_format.insert(file_format.clone(), dimensions);
                    }
                    "csv" => {
                        assert_eq!(
                            content_type, "text/csv",
                            "case {case}/{file_format}: wrong media type"
                        );
                        let (header, rows) = csv_rows(&body);
                        assert!(
                            !rows.is_empty(),
                            "case {case}/{file_format}: the file must carry priced rows"
                        );
                        let skus: BTreeSet<String> = rows
                            .iter()
                            .filter_map(|row| column_of(&header, row, "SKU"))
                            .collect();
                        for row in &rows {
                            assert_eq!(
                                column_of(&header, row, "Currency").as_deref(),
                                Some(currency_code),
                                "case {case}/{file_format}: every row is denominated as \
                                 listed, got {row:?}"
                            );
                        }
                        skus_per_format.insert(file_format.clone(), skus);
                        dimensions_per_format.insert(file_format.clone(), rows.len());
                    }
                    other => {
                        panic!("case {case}: {other} is listed but no file is served for it")
                    }
                }
            }
            // Flat and nested are two forms of ONE price list: the same products, priced
            // over the same number of dimensions.
            let distinct_skus: BTreeSet<&BTreeSet<String>> = skus_per_format.values().collect();
            assert_eq!(
                distinct_skus.len(),
                1,
                "case {case}: every format of a price list must carry the same products, \
                 got {skus_per_format:?}"
            );
            let distinct_dimensions: BTreeSet<&usize> = dimensions_per_format.values().collect();
            assert_eq!(
                distinct_dimensions.len(),
                1,
                "case {case}: every format must price the same dimensions, got \
                 {dimensions_per_format:?}"
            );
        }

        // The later instant names a newer version: the ARN a client caches is keyed to
        // the date it asked about.
        assert!(
            by_date[&JUL_1_2024] > by_date[&JAN_1_2024],
            "case {service_code}/{region_code}: a later EffectiveDate must select a newer \
             version, got {by_date:?}"
        );
    }
}

/// How many price dimensions a price-list file's nested form carries: the file nests
/// `terms.<term type>.<SKU>.<offer term code>.priceDimensions.<rate code>`.
fn json_dimensions(file: &Value) -> usize {
    let mut dimensions = 0usize;
    let Some(by_term_type) = file.get("terms").and_then(Value::as_object) else {
        return dimensions;
    };
    for by_sku in by_term_type.values() {
        for by_term_code in by_sku.as_object().into_iter().flat_map(|skus| skus.values()) {
            for term in by_term_code
                .as_object()
                .into_iter()
                .flat_map(|terms| terms.values())
            {
                dimensions += term
                    .get("priceDimensions")
                    .and_then(Value::as_object)
                    .map_or(0, serde_json::Map::len);
            }
        }
    }
    dimensions
}

/// The `yyyymmddHHMMSS` stamp form of an RFC 3339 instant: its digits.
fn digits(rfc3339: &str) -> String {
    rfc3339.chars().filter(char::is_ascii_digit).collect()
}

/// One RFC 3339 instant as the epoch seconds the awsJson1_1 wire carries a timestamp as.
fn instant(rfc3339: &str) -> i64 {
    ::aws_smithy_types::DateTime::from_str(
        rfc3339,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .expect("a well-formed RFC 3339 instant")
    .secs()
}

/// The value of one `name,value` header line of a price-list file's CSV form, which
/// carries the file header in leading short records before the dimension columns.
fn csv_header_value(body: &str, name: &str) -> Option<String> {
    let mut reader = ::csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(false)
        .from_reader(body.as_bytes());
    reader
        .records()
        .filter_map(Result::ok)
        .take_while(|record| record.get(0) != Some("SKU"))
        .find(|record| record.get(0) == Some(name) && record.len() == 2)
        .and_then(|record| record.get(1).map(str::to_string))
}

/// One stored price-list version, copied out of the catalog so the assertions below run
/// with no lock held.
struct StoredVersion {
    arn: String,
    service_code: String,
    region_code: String,
    version: String,
    publication_date: String,
    effective_date: String,
    published_secs: i64,
    effective_secs: i64,
}

/// Every price-list version the catalog publishes.
fn stored_versions(engine: &std::sync::Arc<engine::InMemoryEngine>) -> Vec<StoredVersion> {
    let state = engine.state::<PricingState>();
    let guard = state.lock();
    guard
        .price_lists
        .values()
        .map(|record| StoredVersion {
            arn: record.arn.clone(),
            service_code: record.service_code.clone(),
            region_code: record.region_code.clone().unwrap_or_default(),
            version: record.version(),
            publication_date: record.publication_date_text(),
            effective_date: record.effective_date_text(),
            published_secs: record.published_epoch_secs(),
            effective_secs: record.effective_epoch_secs(),
        })
        .collect()
}

#[tokio::test]
async fn a_price_list_version_names_the_instant_it_was_published_not_its_effective_date() {
    let (router, engine) = router_with_engine();
    let published = stored_versions(&engine);

    // Rows differ in the service, the Region the price list covers, the currency its
    // files are denominated in and the instant the client asks about — both partitions,
    // a service priced with one usage type per Region, and dates inside each published
    // version's window, including one exactly ON a version's effective instant.
    let cases: &[(&str, &str, &str, &str)] = &[
        ("AmazonEC2", "us-east-1", "USD", "2024-07-15T09:41:03Z"),
        ("AmazonEC2", "eu-west-1", "USD", "2023-06-15T00:00:00Z"),
        ("AmazonEC2", "cn-north-1", "CNY", "2024-06-02T12:00:00Z"),
        ("AmazonS3", "eu-west-1", "USD", "2025-03-04T07:08:09Z"),
        ("AWSLambda", "us-east-1", "USD", "2023-01-01T00:00:00Z"),
        ("AmazonSNS", "ap-northeast-1", "USD", "2024-12-31T23:59:59Z"),
    ];

    for (service_code, region_code, currency_code, asked) in cases.iter().copied() {
        let case = format!("{service_code}/{region_code}/{currency_code}@{asked}");
        let listed = ok(
            &router,
            "ListPriceLists",
            &json!({
                "ServiceCode": service_code,
                "CurrencyCode": currency_code,
                "RegionCode": region_code,
                "EffectiveDate": instant(asked),
            }),
        )
        .await;
        let entries = list_member(&listed, "PriceLists");
        assert_eq!(
            entries.len(),
            1,
            "case {case}: the Region's price list must be reported once, got {listed}"
        );
        let arn = entries[0]
            .get("PriceListArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let version = arn_version(&arn);

        let stored = published
            .iter()
            .find(|stored| stored.arn == arn)
            .unwrap_or_else(|| panic!("case {case}: the listed ARN must be stored, got {arn}"));

        // The version a price list is addressed by is the stamp of the instant it was
        // PUBLISHED at: a publication instant carries a time of day, so the stamp's last
        // six digits are not the all-zero ones a date-derived stamp always has.
        assert_eq!(
            version,
            digits(&stored.publication_date),
            "case {case}: the ARN must name the publication instant {}, got {version}",
            stored.publication_date
        );
        assert_eq!(
            version.len(),
            14,
            "case {case}: a version is a yyyymmddHHMMSS stamp, got {version}"
        );
        assert!(
            version.chars().all(char::is_numeric),
            "case {case}: a version is all digits, got {version}"
        );
        assert_ne!(
            &version[8..],
            "000000",
            "case {case}: the version must carry the publication instant's time of day, \
             got {version}"
        );

        // And it is NOT the date the prices take effect on, nor the date the client
        // asked about: the effective date only selects which published version answers.
        assert_ne!(
            version,
            digits(&stored.effective_date),
            "case {case}: the version must not be the effective date {}",
            stored.effective_date
        );
        assert_ne!(
            version,
            digits(asked),
            "case {case}: the version must not be the requested instant"
        );
        assert_ne!(
            stored.published_secs, stored.effective_secs,
            "case {case}: a version's publication instant and its effective date are \
             separate attributes, both {}",
            stored.publication_date
        );

        // Every address of the file set carries that same version: the path
        // GetPriceListFileUrl hands back is the published offer-file layout
        // `/offers/v1.0/{partition}/{offerCode}/{version}/{regionCode}/index.{format}`.
        let partition = if currency_code == "CNY" { "cn" } else { "aws" };
        for file_format in list_member(&entries[0], "FileFormats")
            .iter()
            .filter_map(Value::as_str)
        {
            let resolved = ok(
                &router,
                "GetPriceListFileUrl",
                &json!({ "PriceListArn": arn, "FileFormat": file_format }),
            )
            .await;
            let url = resolved
                .get("Url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let (_, path) = split_https_url(&url).unwrap_or_else(|| {
                panic!("case {case}/{file_format}: not an https URL: {url:?}")
            });
            assert_eq!(
                path,
                format!(
                    "/offers/v1.0/{partition}/{service_code}/{version}/{region_code}/\
                     index.{file_format}"
                ),
                "case {case}/{file_format}: the URL must address the version its ARN \
                 names, got {path}"
            );

            // The file itself states the same version, and reports the publication
            // instant as its publication date rather than the effective date.
            let state = engine.state::<PricingState>();
            let (_, body) = render_price_list_file(&state, &path).unwrap_or_else(|| {
                panic!("case {case}/{file_format}: nothing is served at {path:?}")
            });
            let (file_version, file_published) = match file_format {
                "json" => {
                    let file: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
                        panic!("case {case}/{file_format}: must be JSON: {error}")
                    });
                    (
                        file.get("version")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        file.get("publicationDate")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                }
                "csv" => (
                    csv_header_value(&body, "Version").unwrap_or_default(),
                    csv_header_value(&body, "Publication Date").unwrap_or_default(),
                ),
                other => panic!("case {case}: {other} is listed but no file is served for it"),
            };
            assert_eq!(
                file_version, version,
                "case {case}/{file_format}: the file must state the version its ARN names"
            );
            assert_eq!(
                file_published, stored.publication_date,
                "case {case}/{file_format}: the file must report the instant it was \
                 published at"
            );
            assert_eq!(
                digits(&file_published),
                file_version,
                "case {case}/{file_format}: the file's version is its publication \
                 instant's stamp"
            );
            assert_ne!(
                file_published, stored.effective_date,
                "case {case}/{file_format}: the publication date must not be the \
                 effective date"
            );
        }
    }
}

#[tokio::test]
async fn every_published_version_has_its_own_publication_instant() {
    let (router, engine) = router_with_engine();
    let published = stored_versions(&engine);
    assert!(
        !published.is_empty(),
        "the catalog must publish price-list versions"
    );

    // Over the WHOLE seeded catalog, not one price list: every version is named by its
    // own publication instant, which is an attribute of the publication and is never
    // the date its prices take effect on.
    for stored in &published {
        let case = &stored.arn;
        assert_eq!(
            arn_version(&stored.arn),
            stored.version,
            "case {case}: the ARN must name the stored version"
        );
        assert_eq!(
            stored.version,
            digits(&stored.publication_date),
            "case {case}: the version is the publication instant's stamp, got {}",
            stored.version
        );
        assert_ne!(
            stored.version,
            digits(&stored.effective_date),
            "case {case}: the version must not be the effective date's stamp"
        );
        assert_ne!(
            &stored.version[8..],
            "000000",
            "case {case}: a publication instant carries a time of day, got {}",
            stored.version
        );
        assert!(
            stored.published_secs <= stored.effective_secs,
            "case {case}: a version is published before its prices take effect, got \
             published {} effective {}",
            stored.publication_date,
            stored.effective_date
        );
    }

    // Each price list is published in more than one version, and the versions order the
    // same way their effective dates do — so a newer version is also the later
    // publication, and no two share a publication instant.
    let mut by_price_list: BTreeMap<(String, String), Vec<&StoredVersion>> = BTreeMap::new();
    for stored in &published {
        by_price_list
            .entry((stored.service_code.clone(), stored.region_code.clone()))
            .or_default()
            .push(stored);
    }
    for ((service_code, region_code), mut versions) in by_price_list {
        let case = format!("{service_code}/{region_code}");
        versions.sort_by_key(|stored| stored.effective_secs);
        assert!(
            versions.len() > 1,
            "case {case}: a price list must exist in more than one version, got {}",
            versions.len()
        );
        for pair in versions.windows(2) {
            assert!(
                pair[0].published_secs < pair[1].published_secs,
                "case {case}: the newer version must be the later publication, got {} \
                 then {}",
                pair[0].publication_date,
                pair[1].publication_date
            );
            assert!(
                pair[0].version < pair[1].version,
                "case {case}: version stamps must order as the publications do, got {} \
                 then {}",
                pair[0].version,
                pair[1].version
            );
        }
    }

    // And what the API selects for an instant is one of those published versions, never
    // a stamp built from the instant asked about. Rows differ in the price list and in
    // where the instant falls: before a version takes effect, exactly on it, inside its
    // window and long after the newest one.
    let lists: &[(&str, &str, &str)] = &[
        ("AmazonEC2", "us-east-1", "USD"),
        ("AmazonEC2", "cn-north-1", "CNY"),
        ("AmazonS3", "ap-northeast-1", "USD"),
    ];
    let asked_instants = [
        "2023-02-14T03:21:44Z",
        "2024-05-31T23:59:59Z",
        "2024-06-01T00:00:00Z",
        "2026-11-09T20:56:22Z",
    ];
    for (service_code, region_code, currency_code) in lists.iter().copied() {
        let versions: BTreeSet<&str> = published
            .iter()
            .filter(|stored| {
                stored.service_code == service_code && stored.region_code == region_code
            })
            .map(|stored| stored.version.as_str())
            .collect();
        assert!(
            !versions.is_empty(),
            "case {service_code}/{region_code}: the catalog must publish this price list"
        );
        for asked in asked_instants {
            let case = format!("{service_code}/{region_code}@{asked}");
            let listed = ok(
                &router,
                "ListPriceLists",
                &json!({
                    "ServiceCode": service_code,
                    "CurrencyCode": currency_code,
                    "RegionCode": region_code,
                    "EffectiveDate": instant(asked),
                }),
            )
            .await;
            let entries = list_member(&listed, "PriceLists");
            assert_eq!(
                entries.len(),
                1,
                "case {case}: the Region's price list must be reported once, got {listed}"
            );
            let version = arn_version(
                entries[0]
                    .get("PriceListArn")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            assert!(
                versions.contains(version.as_str()),
                "case {case}: the reported version must be one the catalog published, \
                 got {version} of {versions:?}"
            );
            assert_ne!(
                version,
                digits(asked),
                "case {case}: the version must not be derived from the requested instant"
            );
            assert!(
                version <= version_stamp_at(instant(asked)),
                "case {case}: a version in effect at an instant was already published by \
                 then, got {version}"
            );
        }
    }
}

/// The head of the attribute-name list the Price List API publishes for a service, in its
/// published order: the dimensions of the offer document every service publishes, then the
/// dimensions that service's own products are priced along.
///
/// The source is the service's published `DescribeServices` answer, whose list opens with
/// these rather than with the alphabetical projection of the product rows. The rows cover
/// a service priced per instance type, one priced per storage class and two priced with
/// one usage type per Region.
const PUBLISHED_ATTRIBUTE_NAMES: &[(&str, &[&str])] = &[
    (
        "AmazonEC2",
        &[
            "termType",
            "productFamily",
            "servicecode",
            "location",
            "instanceType",
            "usagetype",
        ],
    ),
    (
        "AmazonS3",
        &[
            "termType",
            "productFamily",
            "servicecode",
            "location",
            "storageClass",
            "usagetype",
        ],
    ),
    (
        "AWSLambda",
        &[
            "termType",
            "productFamily",
            "servicecode",
            "location",
            "usagetype",
        ],
    ),
    (
        "AmazonSNS",
        &[
            "termType",
            "productFamily",
            "servicecode",
            "location",
            "usagetype",
        ],
    ),
];

#[tokio::test]
async fn describe_services_publishes_the_service_s_own_attribute_name_list() {
    let router = router();

    for (service_code, published_head) in PUBLISHED_ATTRIBUTE_NAMES.iter().copied() {
        let described = ok(
            &router,
            "DescribeServices",
            &json!({ "ServiceCode": service_code }),
        )
        .await;
        let entry = service_entry(&described, service_code)
            .unwrap_or_else(|| panic!("case {service_code}: expected the service, got {described}"));
        let names: Vec<String> = list_member(&entry, "AttributeNames")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let head: Vec<String> = published_head
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(
            names.get(..head.len()),
            Some(head.as_slice()),
            "case {service_code}: the list must open with the published dimensions in \
             their published order, got {names:?}"
        );

        // Not the alphabetical projection of the catalog's product rows: the list names
        // the dimensions the offer DOCUMENT carries, which no product attribute has a key
        // for, and it names them first.
        let documents = offer_documents(
            &ok(
                &router,
                "GetProducts",
                &json!({ "ServiceCode": service_code, "MaxResults": 1 }),
            )
            .await,
        );
        let document = documents
            .first()
            .unwrap_or_else(|| panic!("case {service_code}: expected a priced product"));
        for name in ["termType", "productFamily"] {
            assert!(
                offer_attribute(document, name).is_none(),
                "case {service_code}/{name}: the dimension is published as an attribute \
                 name although no product attribute carries that key, got {document}"
            );
        }
        let mut alphabetical = names.clone();
        alphabetical.sort();
        assert_ne!(
            names, alphabetical,
            "case {service_code}: the published list must be in the published order, not \
             sorted"
        );

        // Every published name resolves to a value set, and a value read off the list
        // selects products when a client filters on it — so iterating the list never
        // leads to a dead end.
        for name in &names {
            let values = all_attribute_values(
                &router,
                &json!({ "ServiceCode": service_code, "AttributeName": name }),
            )
            .await;
            assert!(
                !values.is_empty(),
                "case {service_code}/{name}: a published attribute name must resolve to \
                 the published value set, got {values:?}"
            );
            // Bounded: the first rows of the set are enough to show the vocabulary is
            // usable as a filter, and the whole set is covered by
            // `every_published_attribute_value_selects_a_priced_product`.
            for value in values.iter().take(3) {
                let selected = offer_documents(
                    &ok(
                        &router,
                        "GetProducts",
                        &json!({
                            "ServiceCode": service_code,
                            "Filters": [
                                { "Type": "TERM_MATCH", "Field": name, "Value": value },
                            ],
                            "MaxResults": 1,
                        }),
                    )
                    .await,
                );
                assert!(
                    !selected.is_empty(),
                    "case {service_code}/{name}={value}: a published value must select at \
                     least one product"
                );
                assert!(
                    offer_dimension_values(&selected[0], name)
                        .iter()
                        .any(|published| published == value),
                    "case {service_code}/{name}={value}: a selected product must publish \
                     the filtered value, got {}",
                    selected[0]
                );
            }
        }
    }
}

#[tokio::test]
async fn the_instance_type_vocabulary_publishes_a_family_behind_its_sizes() {
    let router = router();

    let published = all_attribute_values(
        &router,
        &json!({ "ServiceCode": "AmazonEC2", "AttributeName": "instanceType" }),
    )
    .await;

    // Rows: instance families the catalog prices, across generations and processors, and
    // beyond the one the published vocabulary was read against. A family is priced per
    // family rather than per size — that is what a dedicated host is — so the family's
    // own name is a published `instanceType` value, published behind the sizes it covers
    // and selecting the host product when a client filters on it.
    for family in ["a1", "t3", "m5", "c5", "r6g", "x1e"] {
        let family_at = published
            .iter()
            .position(|value| value == family)
            .unwrap_or_else(|| {
                panic!(
                    "case {family}: the published instance-type vocabulary must carry the \
                     family-level value, got {} values",
                    published.len()
                )
            });
        let last_member = published
            .iter()
            .rposition(|value| is_member_of(value, family))
            .unwrap_or_else(|| panic!("case {family}: expected sized values of the family"));
        assert_eq!(
            family_at,
            last_member + 1,
            "case {family}: the family must be published directly behind its sizes, got \
             it at {family_at} with its last size at {last_member}"
        );

        let selected = offer_documents(
            &ok(
                &router,
                "GetProducts",
                &json!({
                    "ServiceCode": "AmazonEC2",
                    "Filters": [
                        { "Type": "TERM_MATCH", "Field": "instanceType", "Value": family },
                    ],
                    "MaxResults": 5,
                }),
            )
            .await,
        );
        assert!(
            !selected.is_empty(),
            "case {family}: the family-level value must select the products priced per \
             family"
        );
        for document in &selected {
            assert_eq!(
                offer_attribute(document, "instanceType").as_deref(),
                Some(family),
                "case {family}: a selected product must be priced for the family itself, \
                 got {document}"
            );
            assert_eq!(
                document
                    .pointer("/product/productFamily")
                    .and_then(Value::as_str),
                Some("Dedicated Host"),
                "case {family}: what is priced per instance family is the dedicated host, \
                 got {document}"
            );
        }
    }
}

#[tokio::test]
async fn the_offer_document_s_own_dimensions_are_published_attributes() {
    let router = router();

    // The term type and the product family are dimensions of the offer DOCUMENT, so a
    // client enumerates and filters on them like any product attribute. Rows differ in
    // the service: one priced under reservations as well as on demand, and one priced on
    // demand only.
    let cases: &[(&str, &[&str])] = &[
        ("AmazonEC2", &["OnDemand", "Reserved"]),
        ("AmazonS3", &["OnDemand"]),
        ("AWSLambda", &["OnDemand"]),
    ];

    for &(service_code, expected_term_types) in cases {
        let term_types = all_attribute_values(
            &router,
            &json!({ "ServiceCode": service_code, "AttributeName": "termType" }),
        )
        .await;
        assert_eq!(
            term_types,
            expected_term_types
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<String>>(),
            "case {service_code}/termType: expected the term types the service's offers \
             are priced under, got {term_types:?}"
        );

        let families = all_attribute_values(
            &router,
            &json!({ "ServiceCode": service_code, "AttributeName": "productFamily" }),
        )
        .await;
        assert!(
            !families.is_empty(),
            "case {service_code}/productFamily: expected the families the service prices"
        );

        // Filtering on either dimension selects exactly the products that publish the
        // filtered value.
        for term_type in &term_types {
            let selected = offer_documents(
                &ok(
                    &router,
                    "GetProducts",
                    &json!({
                        "ServiceCode": service_code,
                        "Filters": [
                            { "Type": "TERM_MATCH", "Field": "termType", "Value": term_type },
                        ],
                        "MaxResults": 5,
                    }),
                )
                .await,
            );
            assert!(
                !selected.is_empty(),
                "case {service_code}/termType={term_type}: expected products priced under \
                 that term type"
            );
            for document in &selected {
                assert!(
                    document.pointer(&format!("/terms/{term_type}")).is_some(),
                    "case {service_code}/termType={term_type}: a selected product must be \
                     priced under that term type, got {document}"
                );
            }
        }
        for family in families.iter().take(3) {
            let selected = offer_documents(
                &ok(
                    &router,
                    "GetProducts",
                    &json!({
                        "ServiceCode": service_code,
                        "Filters": [
                            { "Type": "TERM_MATCH", "Field": "productFamily", "Value": family },
                        ],
                        "MaxResults": 5,
                    }),
                )
                .await,
            );
            assert!(
                !selected.is_empty(),
                "case {service_code}/productFamily={family}: expected products in that \
                 family"
            );
            for document in &selected {
                assert_eq!(
                    document
                        .pointer("/product/productFamily")
                        .and_then(Value::as_str),
                    Some(family.as_str()),
                    "case {service_code}/productFamily={family}: a selected product must \
                     be in the filtered family, got {document}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_published_attribute_is_matched_without_regard_to_case() {
    let router = router();

    // Rows: dimensions the service's own products are priced along and the two
    // dimensions of the offer document, each asked for under a casing other than the
    // published one — the same comparison this API makes on a filter VALUE. The same
    // dimension answers the same published vocabulary whichever casing names it, so a
    // client that asks for it under another casing discovers the values rather than an
    // empty set.
    let cases: &[(&str, &str, &str)] = &[
        ("AmazonEC2", "operatingSystem", "OperatingSystem"),
        ("AmazonEC2", "instanceType", "INSTANCETYPE"),
        ("AmazonEC2", "location", "Location"),
        ("AmazonEC2", "termType", "TermType"),
        ("AmazonEC2", "productFamily", "productfamily"),
        ("AmazonS3", "storageClass", "storageclass"),
    ];

    for (service_code, published, asked) in cases.iter().copied() {
        let expected = all_attribute_values(
            &router,
            &json!({ "ServiceCode": service_code, "AttributeName": published }),
        )
        .await;
        assert!(
            !expected.is_empty(),
            "case {service_code}/{published}: the published name must have a vocabulary"
        );
        let actual = all_attribute_values(
            &router,
            &json!({ "ServiceCode": service_code, "AttributeName": asked }),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {service_code}/{asked}: expected the vocabulary of {published}, got \
             {actual:?}"
        );

        // And a filter that names the dimension in that casing selects the same
        // products, so a value discovered under one casing is usable under the other.
        let value = &expected[0];
        for field in [published, asked] {
            let selected = offer_documents(
                &ok(
                    &router,
                    "GetProducts",
                    &json!({
                        "ServiceCode": service_code,
                        "Filters": [
                            { "Type": "TERM_MATCH", "Field": field, "Value": value },
                        ],
                        "MaxResults": 1,
                    }),
                )
                .await,
            );
            assert!(
                !selected.is_empty(),
                "case {service_code}/{field}={value}: the filter must select the products \
                 publishing that value"
            );
        }
    }

    // A name that differs from a published one by more than case still selects nothing:
    // the comparison ignores case, it does not guess a name.
    for asked in ["operating_system", "instanceTypes", "instanceType "] {
        let values = all_attribute_values(
            &router,
            &json!({ "ServiceCode": "AmazonEC2", "AttributeName": asked }),
        )
        .await;
        assert!(
            values.is_empty(),
            "case {asked}: a name the service does not publish must answer no values, \
             got {values:?}"
        );
    }
}

#[tokio::test]
async fn a_price_query_s_next_token_continues_that_same_filtered_set() {
    let router = router();

    // The filter set a cost-estimation client pins to price one platform of one instance
    // type across the Regions it is offered in: more products than a bounded page, so
    // the answer carries a token for exactly as long as products remain and the walk
    // covers that set once, in one order. Rows differ in the page size.
    let query = json!({
        "ServiceCode": "AmazonEC2",
        "Filters": [
            { "Type": "TERM_MATCH", "Field": "instanceType", "Value": "t3.nano" },
            { "Type": "TERM_MATCH", "Field": "operatingSystem", "Value": "Linux" },
            { "Type": "TERM_MATCH", "Field": "tenancy", "Value": "Shared" },
            { "Type": "TERM_MATCH", "Field": "capacitystatus", "Value": "Used" },
            { "Type": "TERM_MATCH", "Field": "preInstalledSw", "Value": "NA" },
        ],
    });
    let matched: Vec<String> = all_offer_documents(&router, &query)
        .await
        .iter()
        .map(product_sku)
        .collect();
    assert!(
        matched.len() > 1,
        "the filter set must select more than one product for this rule to span, got \
         {matched:?}"
    );

    for page_size in [1_i64, 2, 5] {
        let mut walked: Vec<String> = Vec::new();
        let mut body = query.clone();
        body["MaxResults"] = json!(page_size);
        loop {
            let page = ok(&router, "GetProducts", &body).await;
            walked.extend(offer_documents(&page).iter().map(product_sku));
            let token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                token.is_some(),
                walked.len() < matched.len(),
                "case MaxResults={page_size}: expected a NextToken while {} of {} \
                 products remain, got {page}",
                matched.len().saturating_sub(walked.len()),
                matched.len()
            );
            match token {
                Some(token) => body["NextToken"] = json!(token),
                None => break,
            }
            assert!(
                walked.len() <= matched.len(),
                "case MaxResults={page_size}: paging must terminate, got {} of {}",
                walked.len(),
                matched.len()
            );
        }
        assert_eq!(
            walked, matched,
            "case MaxResults={page_size}: the token must continue the same filtered set"
        );
    }

    // A token describes the query it was minted for: replaying it against another filter
    // set is refused rather than continuing an unrelated set.
    let first = ok(
        &router,
        "GetProducts",
        &{
            let mut body = query.clone();
            body["MaxResults"] = json!(1);
            body
        },
    )
    .await;
    let token = first
        .get("NextToken")
        .and_then(Value::as_str)
        .expect("the bounded page carries a token")
        .to_string();
    let (status, parsed) = call(
        &router,
        "GetProducts",
        &json!({
            "ServiceCode": "AmazonEC2",
            "Filters": [
                { "Type": "TERM_MATCH", "Field": "instanceType", "Value": "m5.large" },
            ],
            "NextToken": token,
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a token minted for another filter set must be refused, got {status} with {parsed}"
    );
    assert_eq!(
        parsed.get("__type").and_then(Value::as_str),
        Some("InvalidNextTokenException"),
        "a token minted for another filter set must be refused as an invalid token, got \
         {parsed}"
    );
}
