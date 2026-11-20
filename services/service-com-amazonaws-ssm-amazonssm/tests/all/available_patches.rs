// SPDX-License-Identifier: CC0-1.0

//! `DescribeAvailablePatches`, end to end: the patches Patch Manager publishes as eligible
//! to be included in a patch baseline.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router. Nothing reaches
//! into the state struct, so what a read reports is what the wire reports.
//!
//! The rule under test is that the read reports the patches published for the operating
//! systems it supports — the ones whose products its `PRODUCT` key names, and the Windows
//! patches when it names no product — narrowed to the patches meeting EVERY filter it
//! carries, paged by `MaxResults` and resumed at the patch a page stopped on, and refuses a
//! filter it cannot search on. The suites are tables whose rows differ along what the rule
//! generalises over: the product named (each supported operating system, an unsupported
//! one's product, a product nobody publishes, the wildcard, none at all), the filter key,
//! the number of filters, the page size and the kind of unserviceable request.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// The operation under test.
const OPERATION: &str = "DescribeAvailablePatches";

/// The read of the same published catalogue as PROPERTY LISTS, which this suite takes the
/// products and the property values of its rows from rather than hard-coding a catalogue
/// beside the one the service publishes.
const PROPERTIES_OPERATION: &str = "DescribePatchProperties";

/// The `PatchSet` value whose products are an operating system's own: "`PATCH_SET` Sample
/// values: `OS` | `APPLICATION`"
/// (`smithy_query(query="doc", shape="DescribeAvailablePatchesRequest", member="Filters")`).
const PATCH_SET_OS: &str = "OS";

/// The `PatchSet` value whose products are the Microsoft applications, from the same doc.
const PATCH_SET_APPLICATION: &str = "APPLICATION";

/// The published floor of this read's `MaxResults`
/// (`smithy_query(query="operation", operation="DescribeAvailablePatches")`: range 1..100).
const MAX_RESULTS_MIN: i64 = 1;

/// The published ceiling of the same member.
const MAX_RESULTS_MAX: i64 = 100;

/// The filter value that "matches all values" of its key (`PatchFilter`).
const WILDCARD: &str = "*";

/// Every key this read searches on, in the order the two lists of the member's own doc
/// publish them (`DescribeAvailablePatchesRequest$Filters`: the Windows keys `PATCH_SET`,
/// `PRODUCT`, `PRODUCT_FAMILY`, `MSRC_SEVERITY`, `CLASSIFICATION`, `PATCH_ID`, then the
/// Linux keys `PRODUCT`, `NAME`, `SEVERITY`, `EPOCH`, `VERSION`, `RELEASE`, `ARCH`,
/// `REPOSITORY`, `ADVISORY_ID`, `CVE_ID`, `BUGZILLA_ID`). Spelled out here rather than read
/// from the crate, so the vocabulary itself is what the assertions check.
const FILTER_KEYS: &[&str] = &[
    "PATCH_SET",
    "PRODUCT",
    "PRODUCT_FAMILY",
    "MSRC_SEVERITY",
    "CLASSIFICATION",
    "PATCH_ID",
    "NAME",
    "SEVERITY",
    "EPOCH",
    "VERSION",
    "RELEASE",
    "ARCH",
    "REPOSITORY",
    "ADVISORY_ID",
    "CVE_ID",
    "BUGZILLA_ID",
];

/// The keys of that vocabulary a request may search on without naming a product, which are
/// the ones the doc lists for Windows patches.
const WINDOWS_FILTER_KEYS: &[&str] = &[
    "PATCH_SET",
    "PRODUCT",
    "PRODUCT_FAMILY",
    "MSRC_SEVERITY",
    "CLASSIFICATION",
    "PATCH_ID",
];

/// What a `NextToken` this read never issued is refused with: fixed text for this class,
/// interpolating nothing from the request. Spelled out here rather than read from the
/// crate, so the wording itself is what the assertion checks.
const INVALID_NEXT_TOKEN_MESSAGE: &str = "The NextToken value is not valid";

/// What a `Filters` entry naming no key at all is refused with — both members of a
/// `PatchOrchestratorFilter` are optional in the model, so such an entry reaches the
/// service and names no patch property to search on.
const FILTER_KEY_MISSING_MESSAGE: &str = "A Filters entry carries no filter key to search on";

/// What a request searching on a key only Linux patches carry, without saying WHICH Linux
/// patches, is refused with: the rule the member's own doc states ("When specifying filters
/// for Linux patches, you must specify a key-pair for PRODUCT", illustrated there with a
/// command that fails for naming `CVE_ID` alone and the same command succeeding once it
/// names a product too).
const LINUX_NEEDS_PRODUCT_MESSAGE: &str =
    "When specifying filters for Linux patches, you must specify a key-pair for PRODUCT";

/// What a `Filters` entry naming a key but no value is refused with.
fn filter_value_missing_message(key: &str) -> String {
    format!("Filter key '{key}' carries no value to search for")
}

/// What a `Filters` entry naming a key this read does not search on is refused with: the
/// key it carried and the vocabulary it may ask for instead.
fn invalid_filter_key_message(key: &str) -> String {
    format!(
        "The following filter key is not valid: {key}. Valid filter keys include: [{}].",
        FILTER_KEYS.join(", ")
    )
}

/// How many pages a walk may take before the test gives up: every read below reports fewer
/// patches than this at the smallest page size it serves, so a walk that reaches it is
/// looping rather than paging.
const MAX_PAGES: usize = 300;

/// The request body of one read: one `Filters` entry per pair, and none at all for an empty
/// table row.
fn request(filters: &[(&str, &[&str])]) -> Value {
    if filters.is_empty() {
        return json!({});
    }
    let entries: Vec<Value> = filters
        .iter()
        .map(|(key, values)| json!({ "Key": key, "Values": values }))
        .collect();
    json!({ "Filters": entries })
}

/// One read at [`REGION`]'s endpoint, returning `(status, body)`.
async fn describe(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, OPERATION, body).await
}

/// The patches a response reports, asserting the shape the model gives them: a list of
/// `Patch` structures, each naming the patch it is about.
fn reported_patches(case: &str, response: &Value) -> Vec<Value> {
    if response["Patches"].is_null() {
        // A read that selects nothing reports no patches at all, which is how this
        // protocol may spell an empty list.
        return Vec::new();
    }
    let reported = response["Patches"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: a read reports a Patches list, got {response}"));
    for patch in reported {
        assert!(
            patch.get("Id").and_then(Value::as_str).is_some(),
            "case {case}: every patch names itself through Id, got {patch}"
        );
        assert!(
            patch.get("Title").and_then(Value::as_str).is_some(),
            "case {case}: every patch carries its title, got {patch}"
        );
        assert!(
            patch.get("ReleaseDate").is_some(),
            "case {case}: every patch carries the date it was released, got {patch}"
        );
        assert!(
            patch.get("Product").and_then(Value::as_str).is_some(),
            "case {case}: every patch carries the product it applies to, got {patch}"
        );
    }
    reported.clone()
}

/// The value `patch` carries for the member `key`, or `None` where it carries none.
fn member<'a>(patch: &'a Value, key: &str) -> Option<&'a str> {
    patch.get(key).and_then(Value::as_str)
}

/// The id of one reported patch.
fn id_of(case: &str, patch: &Value) -> String {
    member(patch, "Id")
        .unwrap_or_else(|| panic!("case {case}: a reported patch names itself, got {patch}"))
        .to_string()
}

/// The distinct values the reported patches carry for `key`.
fn values_of(patches: &[Value], key: &str) -> BTreeSet<String> {
    patches
        .iter()
        .filter_map(|patch| member(patch, key).map(str::to_string))
        .collect()
}

/// The values one patch holds in the LIST member `key` — the members the model gives a list
/// (`Patch$AdvisoryIds`, `Patch$CVEIds`, `Patch$BugzillaIds`).
fn listed(patch: &Value, key: &str) -> Vec<String> {
    patch
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The distinct values the reported patches hold in that list member.
fn listed_values(patches: &[Value], key: &str) -> BTreeSet<String> {
    patches
        .iter()
        .flat_map(|patch| listed(patch, key))
        .collect()
}

/// Every patch a read of `body` reports, walked in pages of `page_size`.
///
/// Asserts the paging rule as it walks: a page carries at most `page_size` patches, a page
/// that hands back a token is full, and the token is the id of the patch the next page
/// starts at.
async fn walked(
    router: &MultiServiceRouter,
    case: &str,
    body: &Value,
    page_size: i64,
) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut page_request = body.clone();
        page_request["MaxResults"] = json!(page_size);
        if let Some(token) = &token {
            page_request["NextToken"] = json!(token);
        }
        let (status, response) = describe(router, &page_request).await;
        assert_eq!(
            status, 200,
            "case {case}: a page must be served, got {status} {response}"
        );
        let page = reported_patches(case, &response);
        assert!(
            i64::try_from(page.len()).expect("a page length fits an i64") <= page_size,
            "case {case}: a page carries at most {page_size} patches, got {}",
            page.len()
        );
        match response["NextToken"].as_str() {
            Some(next) => {
                assert_eq!(
                    i64::try_from(page.len()).expect("a page length fits an i64"),
                    page_size,
                    "case {case}: a page that hands back a token is full, got {} patches",
                    page.len()
                );
                collected.extend(page);
                tokens.push(next.to_string());
                token = Some(next.to_string());
            }
            None => {
                collected.extend(page);
                let ids: Vec<String> = collected
                    .iter()
                    .map(|patch| id_of(case, patch))
                    .collect();
                let distinct: BTreeSet<&String> = ids.iter().collect();
                assert_eq!(
                    distinct.len(),
                    ids.len(),
                    "case {case}: a walk reports each patch once, got {} ids for {} \
                     patches",
                    distinct.len(),
                    ids.len()
                );
                for (page_index, handed_out) in tokens.iter().enumerate() {
                    let page = usize::try_from(page_size).expect("a page size fits a usize");
                    let resumes_at = (page_index + 1) * page;
                    assert_eq!(
                        Some(handed_out.as_str()),
                        ids.get(resumes_at).map(String::as_str),
                        "case {case}: the token of page {page_index} is the patch the next \
                         page starts at, got {handed_out} against {:?}",
                        ids.get(resumes_at)
                    );
                }
                return collected;
            }
        }
    }
    panic!("case {case}: the walk did not reach a last page in {MAX_PAGES} pages")
}

/// Every patch a read of `body` reports, walked at the largest page the read serves.
async fn all_patches(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    walked(router, case, body, MAX_RESULTS_MAX).await
}

/// The values the published patch metadata of `operating_system`'s `patch_set` carries for
/// `property`, read through `DescribePatchProperties` — the sibling read of the SAME
/// published catalogue, so the rows below name no product or property value this service
/// does not publish.
async fn published_values(
    router: &MultiServiceRouter,
    operating_system: &str,
    property: &str,
    patch_set: Option<&str>,
) -> BTreeSet<String> {
    let case = format!("{operating_system} {property} {patch_set:?}");
    let mut body = json!({ "OperatingSystem": operating_system, "Property": property });
    if let Some(patch_set) = patch_set {
        body["PatchSet"] = json!(patch_set);
    }
    let mut collected = BTreeSet::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut page_request = body.clone();
        if let Some(token) = &token {
            page_request["NextToken"] = json!(token);
        }
        let (status, response) =
            call_in_region(router, REGION, PROPERTIES_OPERATION, &page_request).await;
        assert_eq!(
            status, 200,
            "case {case}: the published values must be served, got {status} {response}"
        );
        let entries = response["Properties"].as_array().unwrap_or_else(|| {
            panic!("case {case}: a property read reports a Properties list, got {response}")
        });
        for entry in entries {
            collected.insert(
                member(entry, "Name")
                    .unwrap_or_else(|| {
                        panic!("case {case}: every entry carries its value, got {entry}")
                    })
                    .to_string(),
            );
        }
        match response["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return collected,
        }
    }
    panic!("case {case}: the property walk did not reach a last page")
}

/// A `PRODUCT` key says which published catalogue a read reports: the operating systems
/// that publish the products it names, and nothing at all for a product no operating system
/// this read supports publishes.
#[tokio::test]
async fn the_product_searched_for_selects_the_catalogue_a_read_reports() {
    let router = router();

    // The products of the three operating systems this read supports ("currently,
    // DescribeAvailablePatches supports only the Amazon Linux 1, Amazon Linux 2, and
    // Windows Server operating systems"), each read from the published metadata rather
    // than hard-coded here.
    let amazon_linux = published_values(&router, "AMAZON_LINUX", "PRODUCT", None).await;
    let amazon_linux_2 = published_values(&router, "AMAZON_LINUX_2", "PRODUCT", None).await;
    let windows_os = published_values(&router, "WINDOWS", "PRODUCT", Some(PATCH_SET_OS)).await;
    let windows_applications =
        published_values(&router, "WINDOWS", "PRODUCT", Some(PATCH_SET_APPLICATION)).await;

    // One row per supported operating system, each naming a product of it the issue did
    // not cite, and in Windows's case one product of each patch set.
    let supported: Vec<(&str, String, bool)> = vec![
        (
            "an Amazon Linux 1 product",
            first(&amazon_linux, "AMAZON_LINUX publishes products"),
            false,
        ),
        (
            "an Amazon Linux 2 product",
            first(&amazon_linux_2, "AMAZON_LINUX_2 publishes products"),
            false,
        ),
        (
            "a Windows Server product",
            first(&windows_os, "WINDOWS publishes OS products"),
            true,
        ),
        (
            "a Microsoft application product",
            first(&windows_applications, "WINDOWS publishes application products"),
            true,
        ),
    ];

    for (case, product, windows) in &supported {
        let body = request(&[("PRODUCT", &[product.as_str()])]);
        let patches = all_patches(&router, case, &body).await;
        assert!(
            !patches.is_empty(),
            "case {case}: {product} is a published product, so patches are available for \
             it, got {patches:?}"
        );
        assert_eq!(
            values_of(&patches, "Product"),
            BTreeSet::from([product.clone()]),
            "case {case}: every reported patch applies to {product}, got {:?}",
            values_of(&patches, "Product")
        );
        for patch in &patches {
            // "The Microsoft Knowledge Base ID of the patch. Applies to Windows patches
            // only" (`Patch$KbNumber`), and the vendor is named where the catalogue
            // publishes one.
            assert_eq!(
                member(patch, "KbNumber").is_some(),
                *windows,
                "case {case}: a Knowledge Base id applies to Windows patches only, got \
                 {patch}"
            );
            assert_eq!(
                member(patch, "Vendor"),
                windows.then_some("Microsoft"),
                "case {case}: the vendor is the one that publishes the patch, got {patch}"
            );
            // "The name of the patch. Applies to Linux-based managed nodes only"
            // (`Patch$Name`), and the same for the rest of the package properties: a Linux
            // patch is a package update, a Windows one is not.
            for package_member in ["Name", "Version", "Release", "Arch", "Repository"] {
                assert_eq!(
                    patch.get(package_member).is_some(),
                    !*windows,
                    "case {case}: {package_member} applies to Linux patches only, got \
                     {patch}"
                );
            }
        }
    }

    // A product of an operating system this read does not support, and one nobody
    // publishes, name no catalogue — so the read is served and reports nothing.
    for (case, product) in [
        ("a product of an unsupported operating system", "Ubuntu22.04"),
        ("a product of another unsupported one", "Suse15"),
        ("a product nobody publishes", "NoSuchProduct17"),
    ] {
        let body = request(&[("PRODUCT", &[product])]);
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case}: the read is served, got {status} {response}"
        );
        assert!(
            reported_patches(case, &response).is_empty(),
            "case {case}: {product} names no supported catalogue, so no patch is \
             available for it, got {response}"
        );
    }

    // The wildcard "matches all values", so it names every catalogue this read supports.
    let every = all_patches(
        &router,
        "the wildcard product",
        &request(&[("PRODUCT", &[WILDCARD])]),
    )
    .await;
    let reported = values_of(&every, "Product");
    let mut supported_products = BTreeSet::new();
    supported_products.extend(amazon_linux.iter().cloned());
    supported_products.extend(amazon_linux_2.iter().cloned());
    supported_products.extend(windows_os.iter().cloned());
    supported_products.extend(windows_applications.iter().cloned());
    assert_eq!(
        reported, supported_products,
        "case the wildcard product: the read covers every supported catalogue, got \
         {reported:?} against {supported_products:?}"
    );

    // A read changes nothing: the same request answers the same patches.
    let again = all_patches(
        &router,
        "the wildcard product again",
        &request(&[("PRODUCT", &[WILDCARD])]),
    )
    .await;
    assert_eq!(
        again, every,
        "case the wildcard product again: a read reports the same patches, got {} against \
         {} patches",
        again.len(),
        every.len()
    );
}

/// The first value of a published set, which the rows above search on.
fn first(values: &BTreeSet<String>, why: &str) -> String {
    values
        .iter()
        .next()
        .unwrap_or_else(|| panic!("{why}"))
        .clone()
}

/// A read naming no product at all reports the Windows catalogue, which is the one set of
/// patches that can be asked for without naming a product.
#[tokio::test]
async fn a_read_naming_no_product_reports_the_windows_patches() {
    let router = router();

    let patches = all_patches(&router, "no filters", &request(&[])).await;
    assert!(
        !patches.is_empty(),
        "case no filters: the Windows catalogue is published, so patches are reported"
    );

    let mut windows_products = published_values(&router, "WINDOWS", "PRODUCT", Some(PATCH_SET_OS))
        .await;
    windows_products.extend(
        published_values(&router, "WINDOWS", "PRODUCT", Some(PATCH_SET_APPLICATION)).await,
    );
    assert_eq!(
        values_of(&patches, "Product"),
        windows_products,
        "case no filters: the read covers every published Windows product and no other, \
         got {:?} against {windows_products:?}",
        values_of(&patches, "Product")
    );

    // Every reported patch is a Windows one: it carries the members that apply to Windows
    // patches only.
    for patch in &patches {
        assert!(
            member(patch, "KbNumber").is_some(),
            "case no filters: a Windows patch carries a Knowledge Base id, got {patch}"
        );
        assert!(
            member(patch, "MsrcSeverity").is_some(),
            "case no filters: a Windows patch carries an MSRC severity, got {patch}"
        );
    }

    // The classifications and severities reported are the ones published for Windows.
    for (property, key) in [
        ("CLASSIFICATION", "Classification"),
        ("MSRC_SEVERITY", "MsrcSeverity"),
    ] {
        let reported = values_of(&patches, key);
        let published = published_values(&router, "WINDOWS", property, Some(PATCH_SET_OS)).await;
        assert!(
            reported.is_subset(&published),
            "case no filters: every reported {key} is published for Windows, got \
             {reported:?} against {published:?}"
        );
    }
}

/// Each filter key narrows the read by the patch property it names, and several filters
/// hold together: a patch is reported when EVERY filter accepts it.
#[tokio::test]
async fn each_filter_key_narrows_by_the_property_it_names() {
    let router = router();

    let windows = all_patches(&router, "the whole Windows catalogue", &request(&[])).await;
    let classification = first(
        &values_of(&windows, "Classification"),
        "the Windows catalogue carries classifications",
    );
    let msrc_severity = first(
        &values_of(&windows, "MsrcSeverity"),
        "the Windows catalogue carries MSRC severities",
    );
    let product_family = first(
        &values_of(&windows, "ProductFamily"),
        "the Windows catalogue carries product families",
    );
    let patch_id = first(
        &values_of(&windows, "KbNumber"),
        "a Windows patch is named by its Knowledge Base id",
    );

    // One row per key that searches on a member the response carries, so the patches
    // selected can be compared against the subset of the whole catalogue carrying that
    // value — the rule itself, rather than a count.
    for (case, key, value, response_member) in [
        (
            "by classification",
            "CLASSIFICATION",
            classification.as_str(),
            "Classification",
        ),
        (
            "by MSRC severity",
            "MSRC_SEVERITY",
            msrc_severity.as_str(),
            "MsrcSeverity",
        ),
        (
            "by product family",
            "PRODUCT_FAMILY",
            product_family.as_str(),
            "ProductFamily",
        ),
        ("by patch id", "PATCH_ID", patch_id.as_str(), "KbNumber"),
    ] {
        let selected = all_patches(&router, case, &request(&[(key, &[value])])).await;
        let expected: Vec<Value> = windows
            .iter()
            .filter(|patch| member(patch, response_member) == Some(value))
            .cloned()
            .collect();
        assert!(
            !expected.is_empty(),
            "case {case}: {value} is a value the catalogue publishes, so the row selects \
             patches"
        );
        assert_eq!(
            selected, expected,
            "case {case}: the read reports exactly the patches whose {response_member} is \
             {value}, got {} against {} patches",
            selected.len(),
            expected.len()
        );
    }

    // The patch set is a property of the patch rather than a member of it, so what it
    // selects is checked against the products each set publishes.
    for (case, patch_set) in [
        ("by the operating system's own patch set", PATCH_SET_OS),
        ("by the application patch set", PATCH_SET_APPLICATION),
    ] {
        let selected = all_patches(&router, case, &request(&[("PATCH_SET", &[patch_set])])).await;
        let published = published_values(&router, "WINDOWS", "PRODUCT", Some(patch_set)).await;
        assert_eq!(
            values_of(&selected, "Product"),
            published,
            "case {case}: the read covers the products of that patch set, got {:?} against \
             {published:?}",
            values_of(&selected, "Product")
        );
    }

    // Several filters hold together, and a Linux read says which Linux patches through its
    // own product key: "aws ssm describe-available-patches --filters
    // Key=PRODUCT,Values=AmazonLinux2018.03 Key=CVE_ID,Values=CVE-2018-3615" is the shape
    // the member's doc gives a Linux read.
    let linux_product = first(
        &published_values(&router, "AMAZON_LINUX_2", "PRODUCT", None).await,
        "AMAZON_LINUX_2 publishes products",
    );
    let linux = all_patches(
        &router,
        "a Linux catalogue",
        &request(&[("PRODUCT", &[linux_product.as_str()])]),
    )
    .await;
    let severity = first(
        &values_of(&linux, "Severity"),
        "an Amazon Linux patch carries a severity",
    );
    let narrowed = all_patches(
        &router,
        "a Linux catalogue by severity",
        &request(&[
            ("PRODUCT", &[linux_product.as_str()]),
            ("SEVERITY", &[severity.as_str()]),
        ]),
    )
    .await;
    let expected: Vec<Value> = linux
        .iter()
        .filter(|patch| member(patch, "Severity") == Some(severity.as_str()))
        .cloned()
        .collect();
    assert_eq!(
        narrowed, expected,
        "case a Linux catalogue by severity: both filters hold, got {} against {} patches",
        narrowed.len(),
        expected.len()
    );

    // Two filters no patch satisfies together select nothing, and the read says so rather
    // than dropping one of them: an application product family and an operating system
    // product.
    let impossible = request(&[
        ("PRODUCT_FAMILY", &[product_family.as_str()]),
        ("PRODUCT", &[linux_product.as_str()]),
    ]);
    let (status, response) = describe(&router, &impossible).await;
    assert_eq!(
        status, 200,
        "case filters nothing satisfies: the read is served, got {status} {response}"
    );
    assert!(
        reported_patches("filters nothing satisfies", &response).is_empty(),
        "case filters nothing satisfies: no patch meets both filters, got {response}"
    );

    // Every key only Linux patches carry searches on a package property the catalogue
    // publishes for them, so what it selects is the subset of that catalogue carrying the
    // value — and the patches report those properties, which "apply to Linux-based managed
    // nodes only".
    for (case, key, response_member) in [
        ("by package name", "NAME", "Name"),
        ("by package version", "VERSION", "Version"),
        ("by package release", "RELEASE", "Release"),
        ("by package architecture", "ARCH", "Arch"),
        ("by package repository", "REPOSITORY", "Repository"),
    ] {
        let value = first(
            &values_of(&linux, response_member),
            "an Amazon Linux patch is a package",
        );
        let selected = all_patches(
            &router,
            case,
            &request(&[
                ("PRODUCT", &[linux_product.as_str()]),
                (key, &[value.as_str()]),
            ]),
        )
        .await;
        let expected: Vec<Value> = linux
            .iter()
            .filter(|patch| member(patch, response_member) == Some(value.as_str()))
            .cloned()
            .collect();
        assert!(
            !expected.is_empty(),
            "case {case}: {value} is a published {response_member}, so the row selects \
             patches"
        );
        assert_eq!(
            selected, expected,
            "case {case}: the read reports exactly the patches whose {response_member} is \
             {value}, got {} against {} patches",
            selected.len(),
            expected.len()
        );
    }

    // The advisory a security patch is published through, and the vulnerability and bug it
    // addresses, are list members, so a filter searching on one selects every patch whose
    // list holds the value.
    for (case, key, response_member) in [
        ("by advisory", "ADVISORY_ID", "AdvisoryIds"),
        ("by vulnerability", "CVE_ID", "CVEIds"),
        ("by bug", "BUGZILLA_ID", "BugzillaIds"),
    ] {
        let value = first(
            &listed_values(&linux, response_member),
            "an Amazon Linux security patch is published through an advisory",
        );
        let selected = all_patches(
            &router,
            case,
            &request(&[
                ("PRODUCT", &[linux_product.as_str()]),
                (key, &[value.as_str()]),
            ]),
        )
        .await;
        let expected: Vec<Value> = linux
            .iter()
            .filter(|patch| listed(patch, response_member).contains(&value))
            .cloned()
            .collect();
        assert!(
            !expected.is_empty(),
            "case {case}: {value} is a published {response_member}, so the row selects \
             patches"
        );
        assert_eq!(
            selected, expected,
            "case {case}: the read reports exactly the patches whose {response_member} \
             hold {value}, got {} against {} patches",
            selected.len(),
            expected.len()
        );
        // Only a security patch is published through an advisory.
        for patch in &selected {
            assert_eq!(
                member(patch, "Classification"),
                Some("Security"),
                "case {case}: an advisory publishes a security patch, got {patch}"
            );
        }
    }

    // Every published package carries the epoch the catalogue publishes for it, and no
    // package carries another, so one value selects the whole catalogue and the other none.
    for (case, epoch, selects_all) in [
        ("the published epoch", "0", true),
        ("an epoch no package carries", "1", false),
    ] {
        let selected = all_patches(
            &router,
            case,
            &request(&[
                ("PRODUCT", &[linux_product.as_str()]),
                ("EPOCH", &[epoch]),
            ]),
        )
        .await;
        let expected = if selects_all { linux.clone() } else { Vec::new() };
        assert_eq!(
            selected, expected,
            "case {case}: an EPOCH of {epoch} selects {} of the catalogue's patches, got {}",
            expected.len(),
            selected.len()
        );
    }

    // The wildcard "matches all values" of its key, so it narrows nothing.
    let wildcarded = all_patches(
        &router,
        "the wildcard classification",
        &request(&[("CLASSIFICATION", &[WILDCARD])]),
    )
    .await;
    assert_eq!(
        wildcarded, windows,
        "case the wildcard classification: a wildcard selects every patch of the \
         catalogue, got {} against {} patches",
        wildcarded.len(),
        windows.len()
    );
}

/// A page carries at most `MaxResults` patches and the token resumes at the patch the page
/// stopped on, so walking a read in pages of any size covers the same patches in the same
/// order as the largest page does.
#[tokio::test]
async fn paging_covers_the_same_patches_in_the_same_order() {
    let router = router();

    let linux_product = first(
        &published_values(&router, "AMAZON_LINUX", "PRODUCT", None).await,
        "AMAZON_LINUX publishes products",
    );
    let body = request(&[("PRODUCT", &[linux_product.as_str()])]);
    let whole = all_patches(&router, "one product unpaged", &body).await;
    // Both published bounds of `MaxResults` and sizes in between, so a page smaller than,
    // equal to and larger than the catalogue's own length is covered.
    for page_size in [MAX_RESULTS_MIN, 3, 7, MAX_RESULTS_MAX] {
        let case = format!("one product at MaxResults={page_size}");
        let walked = walked(&router, &case, &body, page_size).await;
        assert_eq!(
            walked, whole,
            "case {case}: paging covers the same patches in the same order, got {} against \
             {} patches",
            walked.len(),
            whole.len()
        );
    }

    // The Windows catalogue is longer than one page, so its walk is the case where the
    // read hands out tokens of its own accord.
    let windows = all_patches(&router, "the Windows catalogue unpaged", &request(&[])).await;
    assert!(
        windows.len() > usize::try_from(MAX_RESULTS_MAX).expect("the ceiling fits a usize"),
        "case the Windows catalogue: it is longer than one page, got {} patches",
        windows.len()
    );
    let at_ninety_seven = walked(&router, "the Windows catalogue at 97", &request(&[]), 97).await;
    assert_eq!(
        at_ninety_seven, windows,
        "case the Windows catalogue at 97: paging covers the same patches, got {} against \
         {} patches",
        at_ninety_seven.len(),
        windows.len()
    );

    // The first page of a bounded read hands back the id of the patch the next page starts
    // at, and that page resumes there.
    let mut first_page = body.clone();
    first_page["MaxResults"] = json!(2);
    let (status, response) = describe(&router, &first_page).await;
    assert_eq!(
        status, 200,
        "case the first page: it must be served, got {status} {response}"
    );
    let token = response["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case the first page: it hands back a token, got {response}"))
        .to_string();
    assert_eq!(
        Some(token.as_str()),
        whole.get(2).map(|patch| id_of("the first page", patch)).as_deref(),
        "case the first page: the token is the patch the next page starts at, got {token}"
    );
    let mut second_page = body.clone();
    second_page["MaxResults"] = json!(2);
    second_page["NextToken"] = json!(token);
    let (status, response) = describe(&router, &second_page).await;
    assert_eq!(
        status, 200,
        "case the resumed page: it must be served, got {status} {response}"
    );
    assert_eq!(
        reported_patches("the resumed page", &response),
        whole[2..4].to_vec(),
        "case the resumed page: it starts at the token's patch"
    );
}

/// A request this read cannot serve is refused, and the refusal reports no patches: a
/// filter naming no key or no value, a key outside this read's vocabulary, a Linux-only key
/// without a product, a `MaxResults` outside the published range and a `NextToken` this
/// read never issued.
#[tokio::test]
async fn an_unserviceable_request_is_refused() {
    let router = router();

    // A filter entry has to name the key it searches on and a value to search for.
    for (case, body, expected) in [
        (
            "a filter naming no key",
            json!({ "Filters": [{ "Values": ["WindowsServer2019"] }] }),
            FILTER_KEY_MISSING_MESSAGE.to_string(),
        ),
        (
            "a filter naming an empty key",
            json!({ "Filters": [{ "Key": "", "Values": ["WindowsServer2019"] }] }),
            // The model gives `PatchOrchestratorFilter$Key` a minimum length of 1, so an
            // empty key is refused by the boundary walk before the read runs, in the
            // sentence that walk words a constraint failure with.
            "1 validation error detected: Value '' at 'filters.1.member.key' failed to \
             satisfy constraint: Member must have length greater than or equal to 1"
                .to_string(),
        ),
        (
            "a filter naming no value",
            json!({ "Filters": [{ "Key": "PRODUCT" }] }),
            filter_value_missing_message("PRODUCT"),
        ),
        (
            "a filter naming an empty value list",
            json!({ "Filters": [{ "Key": "CLASSIFICATION", "Values": [] }] }),
            filter_value_missing_message("CLASSIFICATION"),
        ),
    ] {
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            expected,
            "case {case}: the refusal says what the entry is missing, got {response}"
        );
        assert!(
            response["Patches"].is_null(),
            "case {case}: a refusal reports no patches, got {response}"
        );
    }

    // A key outside this read's vocabulary is refused rather than silently matching
    // nothing: the keys of a sibling patch read, a property name of another read, and the
    // same key in the wrong case — a filter key is a name, not a prose label.
    for (case, key) in [
        ("a key of the baseline listing", "OPERATING_SYSTEM"),
        ("a key of the node listing", "InstanceId"),
        ("a key in the wrong case", "product"),
        ("a key nothing declares", "NOT_A_KEY"),
    ] {
        let body = request(&[(key, &["anything"])]);
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            invalid_filter_key_message(key),
            "case {case}: the refusal names the key and the vocabulary, got {response}"
        );
    }

    // Every key only Linux patches carry says nothing about WHICH Linux patches on its
    // own, and is served as soon as the request names a product too.
    let linux_product = first(
        &published_values(&router, "AMAZON_LINUX_2", "PRODUCT", None).await,
        "AMAZON_LINUX_2 publishes products",
    );
    for key in FILTER_KEYS
        .iter()
        .filter(|key| !WINDOWS_FILTER_KEYS.contains(key))
    {
        let (status, response) = describe(&router, &request(&[(*key, &["anything"])])).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case the Linux-only key {key} alone: expected 400 ValidationException, got \
             {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            LINUX_NEEDS_PRODUCT_MESSAGE,
            "case the Linux-only key {key} alone: the refusal states the rule, got \
             {response}"
        );

        let with_product = request(&[
            ("PRODUCT", &[linux_product.as_str()]),
            (*key, &["anything"]),
        ]);
        let (status, response) = describe(&router, &with_product).await;
        assert_eq!(
            status, 200,
            "case the Linux-only key {key} with a product: it is served, got {status} \
             {response}"
        );
    }

    // A `MaxResults` outside the published range is refused, and each published bound is
    // itself served.
    for (case, max_results) in [
        ("one below the published floor", MAX_RESULTS_MIN - 1),
        ("one past the published ceiling", MAX_RESULTS_MAX + 1),
    ] {
        let mut body = request(&[]);
        body["MaxResults"] = json!(max_results);
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case a MaxResults {case}: expected 400 ValidationException, got {status} \
             {response}"
        );
    }
    for max_results in [MAX_RESULTS_MIN, MAX_RESULTS_MAX] {
        let mut body = request(&[]);
        body["MaxResults"] = json!(max_results);
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            status, 200,
            "case MaxResults={max_results} is inside the published range: it must be \
             served, got {status} {response}"
        );
    }

    // A token is the patch the next page starts at, so one naming no patch of THIS
    // result — including the id of a patch another catalogue holds — was never issued here.
    let linux = all_patches(
        &router,
        "a Linux catalogue",
        &request(&[("PRODUCT", &[linux_product.as_str()])]),
    )
    .await;
    let foreign = id_of(
        "a Linux catalogue",
        linux.first().unwrap_or_else(|| panic!("the catalogue is not empty")),
    );
    for (case, token) in [
        ("an id nothing published", "not-a-published-patch"),
        ("an id of another catalogue", foreign.as_str()),
    ] {
        let mut body = request(&[]);
        body["NextToken"] = json!(token);
        let (status, response) = describe(&router, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case a NextToken that is {case}: expected 400 ValidationException, got \
             {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            INVALID_NEXT_TOKEN_MESSAGE,
            "case a NextToken that is {case}: the refusal reports the token is not valid, \
             got {response}"
        );
        assert!(
            response["Patches"].is_null(),
            "case a NextToken that is {case}: a refusal reports no patches, got {response}"
        );
    }
}
