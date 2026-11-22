// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the AWS Outposts PATCH update operations, driven over the wire.
//!
//! Every case here goes through the real front door — `MultiServiceRouter::dispatch`
//! with a restJson1 request, exactly as a signed `aws outposts` call arrives — so what
//! it asserts is the emulator's HTTP behaviour, not a handler called directly. The
//! request harness is the lifecycle suite's, reused rather than rebuilt.
//!
//! The rule under test, stated once and then exercised over a SET of inputs:
//!
//! > An update operation applies exactly the members its request carries to the stored
//! > resource — replacing the stored value, including one the resource had no value for
//! > — leaves every member the request omits exactly as it was, and answers the record
//! > as it now stands, which is what every later read answers too. A request carrying no
//! > updatable member at all writes nothing, down to a nested structure the resource has
//! > none of: it reads back exactly as it did before the call.
//!
//! The rows differ along which members the request carries (none, one, several, all),
//! which value each carries (every value the member's modelled enum declares, for the
//! rack properties), the length of a free-text member (the modelled minimum and
//! maximum), the identifier form the request names the resource by (bare id and full
//! ARN), and whether the member had a value before the call.
//!
//! An identifier that names nothing is refused 404 `NotFoundException` and one that is
//! no identifier at all 400 `ValidationException`, on each of the three update routes.

use serde_json::{json, Value};

use crate::error_classification::{reported_tags, tag_map};
use super::site_and_outpost_lifecycle::{
    address, install_hardware, query_value, refusal, segment, text, Outposts, ABSENT_OUTPOST_ID,
    ABSENT_SITE_ID, OUTPOST_ARN_PREFIX, SITE_ARN_PREFIX,
};

/// Every rack property `RackPhysicalProperties` declares, with every value its member's
/// modelled enum admits.
///
/// Source: `smithy_query(query="shape", shape="PowerDrawKva,PowerPhase,PowerConnector,
/// PowerFeedDrop,UplinkGbps,UplinkCount,FiberOpticCableType,OpticalStandard,
/// MaximumSupportedWeightLbs")` against this service's model — the same nine members
/// `UpdateSiteRackPhysicalProperties` takes.
pub(crate) const RACK_PROPERTY_VALUES: &[(&str, &[&str])] = &[
    (
        "PowerDrawKva",
        &["POWER_5_KVA", "POWER_10_KVA", "POWER_15_KVA", "POWER_30_KVA"],
    ),
    ("PowerPhase", &["SINGLE_PHASE", "THREE_PHASE"]),
    (
        "PowerConnector",
        &["L6_30P", "IEC309", "AH530P7W", "AH532P6W", "CS8365C"],
    ),
    ("PowerFeedDrop", &["ABOVE_RACK", "BELOW_RACK"]),
    (
        "UplinkGbps",
        &["UPLINK_1G", "UPLINK_10G", "UPLINK_40G", "UPLINK_100G"],
    ),
    (
        "UplinkCount",
        &[
            "UPLINK_COUNT_1",
            "UPLINK_COUNT_2",
            "UPLINK_COUNT_3",
            "UPLINK_COUNT_4",
            "UPLINK_COUNT_5",
            "UPLINK_COUNT_6",
            "UPLINK_COUNT_7",
            "UPLINK_COUNT_8",
            "UPLINK_COUNT_12",
            "UPLINK_COUNT_16",
        ],
    ),
    ("FiberOpticCableType", &["SINGLE_MODE", "MULTI_MODE"]),
    (
        "OpticalStandard",
        &[
            "OPTIC_10GBASE_SR",
            "OPTIC_10GBASE_IR",
            "OPTIC_10GBASE_LR",
            "OPTIC_40GBASE_SR",
            "OPTIC_40GBASE_ESR",
            "OPTIC_40GBASE_IR4_LR4L",
            "OPTIC_40GBASE_LR4",
            "OPTIC_100GBASE_SR4",
            "OPTIC_100GBASE_CWDM4",
            "OPTIC_100GBASE_LR4",
            "OPTIC_100G_PSM4_MSA",
            "OPTIC_1000BASE_LX",
            "OPTIC_1000BASE_SX",
        ],
    ),
    (
        "MaximumSupportedWeightLbs",
        &[
            "NO_LIMIT",
            "MAX_1400_LBS",
            "MAX_1600_LBS",
            "MAX_1800_LBS",
            "MAX_2000_LBS",
        ],
    ),
];

/// What the rule says the resource must look like after one update: the record as it
/// was, with every member the request carried written over it and every member it
/// omitted left alone.
pub(crate) fn overlay(base: &Value, patch: &Value) -> Value {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    for (member, value) in patch.as_object().into_iter().flatten() {
        merged.insert(member.clone(), value.clone());
    }
    Value::Object(merged)
}

/// `PATCH /outposts/{OutpostId}`, returning the `Outpost` it answered with.
async fn update_outpost(outposts: &Outposts, identifier: &str, patch: &Value) -> Value {
    outposts
        .ok(
            "PATCH",
            &format!("/outposts/{}", segment(identifier)),
            Some(patch),
        )
        .await["Outpost"]
        .clone()
}

/// `PATCH /sites/{SiteId}`, returning the `Site` it answered with.
async fn update_site(outposts: &Outposts, identifier: &str, patch: &Value) -> Value {
    outposts
        .ok(
            "PATCH",
            &format!("/sites/{}", segment(identifier)),
            Some(patch),
        )
        .await["Site"]
        .clone()
}

/// `PATCH /sites/{SiteId}/rackPhysicalProperties`, returning the `Site` it answered with.
async fn update_rack_properties(outposts: &Outposts, identifier: &str, patch: &Value) -> Value {
    outposts
        .ok(
            "PATCH",
            &format!("/sites/{}/rackPhysicalProperties", segment(identifier)),
            Some(patch),
        )
        .await["Site"]
        .clone()
}

/// The rule for `UpdateOutpost`: each row's members land on the stored Outpost, the rest
/// of it is untouched, and `GetOutpost` answers the updated record by either identifier
/// form.
///
/// The rows differ along which of the three members the request carries, the value each
/// carries (including `Description` at the modelled `length(min: 0)` — the empty string
/// — and `Name` at the modelled `length(max: 255)`), and the identifier form the request
/// uses. They are applied in sequence to ONE Outpost, so a row that left a previous
/// row's write behind, or that wrote a member it never named, shows up immediately.
#[tokio::test]
async fn update_outpost_applies_present_members_and_reads_back() {
    let outposts = Outposts::new();
    let site = outposts
        .create_site(&json!({
            "Name": "update site",
            "OperatingAddress": address("US", "WA", "Seattle"),
            // Hardware is ordered for this site's Outpost below, and an order can only
            // be placed at a site whose details are complete
            // ([`crate::precondition_gates`]).
            "ShippingAddress": address("US", "WA", "Kirkland"),
        }))
        .await;
    let created = outposts
        .create_outpost(&json!({
            "Name": "before",
            "SiteId": text(&site, "SiteId"),
            "Description": "created description",
            "AvailabilityZone": "us-east-1a",
            "Tags": { "Env": "dev" },
            "SupportedHardwareType": "RACK",
        }))
        .await;
    let outpost_id = text(&created, "OutpostId");
    let outpost_arn = text(&created, "OutpostArn");
    // An Outpost reaches its settled lifecycle status when hardware ordered for it has
    // been installed; this suite is about what an update writes, so it has that install
    // behind it first and then reads the settled record as its baseline.
    install_hardware(&outposts, &outpost_id).await;
    let created = outposts.get_outpost(&outpost_id).await;

    let cases: Vec<(&str, &str, Value)> = vec![
        ("no updatable member, by id", outpost_id.as_str(), json!({})),
        (
            "description only",
            outpost_id.as_str(),
            json!({ "Description": "re-described" }),
        ),
        ("name only", outpost_id.as_str(), json!({ "Name": "renamed" })),
        (
            "hardware type only",
            outpost_id.as_str(),
            json!({ "SupportedHardwareType": "SERVER" }),
        ),
        (
            "every member at once, named by ARN",
            outpost_arn.as_str(),
            json!({
                "Name": "renamed again",
                "Description": "described again",
                "SupportedHardwareType": "RACK",
            }),
        ),
        (
            "description cleared to the modelled empty string",
            outpost_id.as_str(),
            json!({ "Description": "" }),
        ),
        (
            "name at the modelled 255-character maximum",
            outpost_id.as_str(),
            json!({ "Name": "n".repeat(255) }),
        ),
    ];

    let mut expected = created;
    for (case, identifier, patch) in &cases {
        expected = overlay(&expected, patch);
        let answered = update_outpost(&outposts, identifier, patch).await;
        assert_eq!(
            answered, expected,
            "case {case:?}: UpdateOutpost must answer the updated record, got {answered:?}"
        );
        assert_eq!(
            outposts.get_outpost(&outpost_id).await,
            expected,
            "case {case:?}: GetOutpost by id must answer the updated record"
        );
        assert_eq!(
            outposts.get_outpost(&outpost_arn).await,
            expected,
            "case {case:?}: GetOutpost by ARN must answer the updated record"
        );
    }
}

/// The rule for `UpdateSite`: each row's members land on the stored site, every other
/// member of it — including the operating-address projection, the tags and the rack
/// properties no update member names — is untouched, and `GetSite` answers the updated
/// record by either identifier form.
#[tokio::test]
async fn update_site_applies_present_members_and_reads_back() {
    let outposts = Outposts::new();
    let created = outposts
        .create_site(&json!({
            "Name": "before",
            "Description": "created description",
            "Notes": "created notes",
            "Tags": { "Env": "dev", "team": "platform" },
            "OperatingAddress": address("US", "WA", "Seattle"),
            "RackPhysicalProperties": { "PowerDrawKva": "POWER_5_KVA" },
        }))
        .await;
    let site_id = text(&created, "SiteId");
    let site_arn = text(&created, "SiteArn");

    let cases: Vec<(&str, &str, Value)> = vec![
        ("no updatable member, by id", site_id.as_str(), json!({})),
        ("name only", site_id.as_str(), json!({ "Name": "renamed" })),
        (
            "description only",
            site_id.as_str(),
            json!({ "Description": "re-described" }),
        ),
        (
            "notes only, with the newline the modelled pattern allows",
            site_id.as_str(),
            json!({ "Notes": "badge required\nescort to the cage" }),
        ),
        (
            "every member at once, named by ARN",
            site_arn.as_str(),
            json!({
                "Name": "renamed again",
                "Description": "described again",
                "Notes": "noted again",
            }),
        ),
        (
            "name at the modelled 1000-character maximum",
            site_id.as_str(),
            json!({ "Name": "n".repeat(1000) }),
        ),
        (
            "description at the modelled 1-character minimum",
            site_id.as_str(),
            json!({ "Description": "x" }),
        ),
    ];

    let mut expected = created;
    for (case, identifier, patch) in &cases {
        expected = overlay(&expected, patch);
        let answered = update_site(&outposts, identifier, patch).await;
        assert_eq!(
            answered, expected,
            "case {case:?}: UpdateSite must answer the updated record, got {answered:?}"
        );
        assert_eq!(
            outposts.get_site(&site_id).await,
            expected,
            "case {case:?}: GetSite by id must answer the updated record"
        );
        assert_eq!(
            outposts.get_site(&site_arn).await,
            expected,
            "case {case:?}: GetSite by ARN must answer the updated record"
        );
    }
}

/// Which keys a row of [`update_site_answers_the_tags_the_site_carries`] removes from
/// the site before that row's update runs.
enum Remove {
    /// Nothing: the row only adds or replaces tags.
    Nothing,
    /// Exactly the keys named, whether the site carries them or not.
    Keys(&'static [&'static str]),
    /// Every key the site carries at that point — the row that leaves it untagged
    /// again, so the rule is exercised on a tag map that BECAME empty as well as on
    /// one that was never filled.
    Every,
}

/// `POST /tags/{ResourceArn}`: how a row applies tags to the site between updates.
async fn tag_site(outposts: &Outposts, arn: &str, tags: &Value) {
    outposts
        .no_content(
            "POST",
            &format!("/tags/{}", segment(arn)),
            Some(&json!({ "Tags": tags })),
        )
        .await;
}

/// `DELETE /tags/{ResourceArn}?tagKeys=…`, with one `tagKeys` parameter per key —
/// which is how a list-typed `@httpQuery` member travels. Each key is percent-encoded,
/// because the modelled `TagKey` pattern admits characters a query string reads
/// otherwise.
async fn untag_site(outposts: &Outposts, arn: &str, keys: &[String]) {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", query_value(key)))
        .collect();
    outposts
        .no_content(
            "DELETE",
            &format!("/tags/{}?{}", segment(arn), query.join("&")),
            None,
        )
        .await;
}

/// The `Site` `ListSites` reports for `site_id`, if it reports one.
async fn listed_site(outposts: &Outposts, site_id: &str) -> Option<Value> {
    outposts.list_sites("").await["Sites"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|site| text(site, "SiteId") == site_id)
}

/// The rule for what an update REPORTS about a member it does not take: a site's tags
/// are part of the site, so the `Site` `UpdateSite` answers reports the tags the site
/// carries AT THAT MOMENT — the ones it was created with, the ones an earlier
/// `TagResource` applied, and none an `UntagResource` has removed — which is the same
/// map `GetSite` and `ListSites` report right after the call. The update writes only
/// the three members it takes, so no row's tags change across it, and the map is
/// reported even when the site carries no tags at all.
///
/// The rows differ along how the site's tags came to be what they are (created with
/// them, added, replaced, one removed, a key it never carried removed, and every key
/// removed), the SIZE of the map (none, one, several), the form of the keys and values
/// (the character classes the modelled `TagKey` pattern `^(?!aws:)[a-zA-Z+-=._:/]+$`
/// admits, and a value carrying the site's own ARN), and which of the three members
/// the update itself carries (one, several, none). They are applied in sequence to the
/// SAME site — once to a site created with tags and once to a site created without —
/// so a row that dropped a previous row's tags shows up at once.
#[tokio::test]
async fn update_site_answers_the_tags_the_site_carries() {
    let outposts = Outposts::new();
    let starting: [(&str, Value); 2] = [
        ("no tags at create", json!(null)),
        ("tags at create", json!({ "Env": "prod", "cost.center": "4711" })),
    ];

    for (start, at_create) in &starting {
        let mut request = json!({
            "Name": format!("tag reporting site: {start}"),
            "Description": "created description",
            "Notes": "created notes",
        });
        if at_create.is_object() {
            request["Tags"] = at_create.clone();
        }
        let created = outposts.create_site(&request).await;
        let site_id = text(&created, "SiteId");
        let site_arn = text(&created, "SiteArn");
        let mut expected_tags = tag_map(at_create);

        let rows: Vec<(&str, Value, Remove, Value)> = vec![
            (
                "the tags the site was created with",
                json!(null),
                Remove::Nothing,
                json!({ "Name": "renamed" }),
            ),
            (
                "one tag whose value is the site's own ARN",
                json!({ "resourceArn": site_arn.clone() }),
                Remove::Nothing,
                json!({ "Description": "re-described" }),
            ),
            (
                "several tags spanning the modelled key character classes",
                json!({
                    "a+b": "plus",
                    "a-b": "dash",
                    "a.b": "dot",
                    "a_b": "underscore",
                    "a:b": "colon",
                    "a/b": "slash",
                    "A1": "alnum",
                }),
                Remove::Nothing,
                json!({ "Notes": "noted again" }),
            ),
            (
                "a stored key at a new value",
                json!({ "resourceArn": "replaced" }),
                Remove::Nothing,
                json!({}),
            ),
            (
                "one key removed",
                json!(null),
                Remove::Keys(&["a+b"]),
                json!({ "Name": "renamed again", "Notes": "noted once more" }),
            ),
            (
                "a key the site does not carry removed",
                json!(null),
                Remove::Keys(&["never-applied"]),
                json!({ "Description": "re-described again" }),
            ),
            (
                "one key added and another removed before the same update",
                json!({ "owner": "sre" }),
                Remove::Keys(&["a.b"]),
                json!({ "Name": "renamed once more", "Description": "d", "Notes": "n" }),
            ),
            (
                "every key removed",
                json!(null),
                Remove::Every,
                json!({ "Name": "renamed last" }),
            ),
        ];

        for (row, applied, remove, patch) in &rows {
            let case = format!("{start}: {row}");
            if applied.is_object() {
                tag_site(&outposts, &site_arn, applied).await;
                expected_tags.extend(tag_map(applied));
            }
            let removed: Vec<String> = match remove {
                Remove::Nothing => Vec::new(),
                Remove::Keys(keys) => keys.iter().map(|key| (*key).to_string()).collect(),
                Remove::Every => expected_tags.keys().cloned().collect(),
            };
            if !removed.is_empty() {
                untag_site(&outposts, &site_arn, &removed).await;
                for key in &removed {
                    expected_tags.remove(key);
                }
            }

            // The record as it stands with those tags on it: what the update must
            // answer with only the members its own request carries changed.
            let before = outposts.get_site(&site_id).await;
            assert_eq!(
                reported_tags(&before),
                expected_tags,
                "case {case:?}: the tag calls must leave the site carrying exactly \
                 those tags, got {before:?}"
            );

            let answered = update_site(&outposts, &site_id, patch).await;
            let expected = overlay(&before, patch);
            assert_eq!(
                answered, expected,
                "case {case:?}: UpdateSite must answer the site as it now stands, \
                 tags included, got {answered:?}"
            );
            assert_eq!(
                reported_tags(&answered),
                expected_tags,
                "case {case:?}: UpdateSite must report the tags the site carries, \
                 got {answered:?}"
            );
            assert!(
                answered.get("Tags").is_some_and(Value::is_object),
                "case {case:?}: the tag map is reported even when the site carries no \
                 tags, got {answered:?}"
            );
            assert_eq!(
                outposts.get_site(&site_arn).await,
                expected,
                "case {case:?}: GetSite must answer the same site the update answered"
            );
            let listed = listed_site(&outposts, &site_id).await;
            assert_eq!(
                listed.as_ref(),
                Some(&expected),
                "case {case:?}: ListSites must report the same site, got {listed:?}"
            );
        }

        assert!(
            expected_tags.is_empty(),
            "case {start:?}: the rows end with every key removed, so the site must \
             carry no tags, got {expected_tags:?}"
        );
    }
}

/// The rule for `UpdateSiteRackPhysicalProperties`, over EVERY value EVERY one of the
/// nine rack properties declares: setting one property alone stores that value, leaves
/// the other eight as they were, and `GetSite` answers it.
///
/// The site is created with NO `RackPhysicalProperties` at all, so the first row also
/// covers a property the resource had no value for — the case the structure itself has
/// to appear for.
#[tokio::test]
async fn update_rack_properties_stores_every_modelled_value_one_at_a_time() {
    let outposts = Outposts::new();
    let created = outposts
        .create_site(&json!({ "Name": "rack site" }))
        .await;
    let site_id = text(&created, "SiteId");
    assert!(
        created.get("RackPhysicalProperties").is_none(),
        "a site created with no rack properties reports none, got {created:?}"
    );

    let mut expected_properties = json!({});
    for (member, values) in RACK_PROPERTY_VALUES {
        for value in *values {
            let case = format!("{member} = {value}");
            let patch = json!({ *member: value });
            expected_properties = overlay(&expected_properties, &patch);
            let expected = overlay(&created, &json!({ "RackPhysicalProperties": expected_properties }));

            let answered = update_rack_properties(&outposts, &site_id, &patch).await;
            assert_eq!(
                answered, expected,
                "case {case:?}: the update must answer the site with only that property changed, got {answered:?}"
            );
            assert_eq!(
                outposts.get_site(&site_id).await,
                expected,
                "case {case:?}: GetSite must answer the updated rack properties"
            );
        }
    }
}

/// The same rule in its many-members-at-once and omitted-members forms, over a site
/// whose rack properties were set at creation: a request carrying several properties
/// writes exactly those, and one carrying none changes nothing.
#[tokio::test]
async fn update_rack_properties_leaves_omitted_properties_alone() {
    let outposts = Outposts::new();
    // Every property the modelled structure declares, at its first modelled value, so
    // each row below overwrites a property that already had one.
    let at_create: Value = RACK_PROPERTY_VALUES
        .iter()
        .map(|(member, values)| ((*member).to_string(), json!(values[0])))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let created = outposts
        .create_site(&json!({
            "Name": "fully specified rack",
            "RackPhysicalProperties": at_create,
            "OperatingAddress": address("IE", "Leinster", "Dublin"),
        }))
        .await;
    let site_id = text(&created, "SiteId");
    let site_arn = text(&created, "SiteArn");

    let cases: Vec<(&str, &str, Value)> = vec![
        ("no property at all, by id", site_id.as_str(), json!({})),
        (
            "two properties",
            site_id.as_str(),
            json!({ "PowerPhase": "THREE_PHASE", "PowerConnector": "CS8365C" }),
        ),
        (
            "the uplink pair, named by ARN",
            site_arn.as_str(),
            json!({ "UplinkGbps": "UPLINK_100G", "UplinkCount": "UPLINK_COUNT_16" }),
        ),
        (
            "all nine properties at once",
            site_id.as_str(),
            RACK_PROPERTY_VALUES
                .iter()
                .map(|(member, values)| {
                    (
                        (*member).to_string(),
                        json!(values[values.len() - 1]),
                    )
                })
                .collect::<serde_json::Map<String, Value>>()
                .into(),
        ),
    ];

    let mut expected_properties = at_create;
    for (case, identifier, patch) in &cases {
        expected_properties = overlay(&expected_properties, patch);
        let expected = overlay(
            &created,
            &json!({ "RackPhysicalProperties": expected_properties }),
        );
        let answered = update_rack_properties(&outposts, identifier, patch).await;
        assert_eq!(
            answered, expected,
            "case {case:?}: only the properties the request carried may change, got {answered:?}"
        );
        assert_eq!(
            outposts.get_site(&site_id).await,
            expected,
            "case {case:?}: GetSite must answer the updated rack properties"
        );
    }
}

/// The no-op form of the rule: a request carrying no rack property writes NOTHING, so
/// the site it names reads back exactly as it did before the call — down to whether it
/// has a `RackPhysicalProperties` structure at all.
///
/// The rows differ along what the site's rack properties are BEFORE the call, which is
/// what the rule generalises over here: no structure at all, a structure with no
/// property in it, one property, and every property the structure declares. Each row is
/// read back through `GetSite` and `ListSites` as well as the update's own answer, and
/// the call is repeated, since a request that changes nothing cannot start changing
/// something on a later try.
#[tokio::test]
async fn update_rack_properties_carrying_no_property_changes_nothing() {
    let outposts = Outposts::new();
    // Every property at its last modelled value, so the richest row differs from the
    // one-property row in more than its count.
    let every_property: Value = RACK_PROPERTY_VALUES
        .iter()
        .map(|(member, values)| ((*member).to_string(), json!(values[values.len() - 1])))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let cases: Vec<(&str, Option<Value>)> = vec![
        ("no rack properties at all", None),
        ("a rack properties structure with nothing in it", Some(json!({}))),
        (
            "one rack property",
            Some(json!({ "PowerDrawKva": "POWER_15_KVA" })),
        ),
        ("every rack property", Some(every_property)),
    ];

    for (case, at_create) in &cases {
        let mut request = serde_json::Map::new();
        request.insert("Name".to_string(), json!(format!("no-op rack: {case}")));
        if let Some(properties) = at_create {
            request.insert("RackPhysicalProperties".to_string(), properties.clone());
        }
        let created = outposts.create_site(&Value::Object(request)).await;
        let site_id = text(&created, "SiteId");
        let site_arn = text(&created, "SiteArn");
        assert_eq!(
            created.get("RackPhysicalProperties"),
            at_create.as_ref(),
            "case {case:?}: a created site reports exactly the rack properties it was created with, got {created:?}"
        );

        for attempt in 1..=2 {
            let answered = update_rack_properties(&outposts, &site_id, &json!({})).await;
            assert_eq!(
                answered, created,
                "case {case:?} attempt {attempt}: a request carrying no property must answer the site unchanged, got {answered:?}"
            );
            let read = outposts.get_site(&site_arn).await;
            assert_eq!(
                read, created,
                "case {case:?} attempt {attempt}: GetSite must answer the site unchanged, got {read:?}"
            );
            let listed = outposts.list_sites("").await["Sites"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|site| text(site, "SiteId") == site_id);
            assert_eq!(
                listed.as_ref(),
                Some(&created),
                "case {case:?} attempt {attempt}: ListSites must report the site unchanged, got {listed:?}"
            );
        }
    }
}

/// An update whose identifier names nothing is 404 `NotFoundException`, and one whose
/// identifier is no identifier at all is 400 `ValidationException` — on every update
/// route, and for the bare-id and ARN forms alike. Nothing is created to satisfy it.
#[tokio::test]
async fn updates_of_absent_or_malformed_identifiers_are_refused() {
    let outposts = Outposts::new();
    let absent_outpost_arn = format!("{OUTPOST_ARN_PREFIX}{ABSENT_OUTPOST_ID}");
    let absent_site_arn = format!("{SITE_ARN_PREFIX}{ABSENT_SITE_ID}");

    let cases = vec![
        refusal(
            "UpdateOutpost on an id nothing holds",
            "PATCH",
            format!("/outposts/{ABSENT_OUTPOST_ID}"),
            404,
            "NotFoundException",
        )
        .with_body(json!({ "Description": "no such Outpost" })),
        refusal(
            "UpdateOutpost on an ARN nothing holds",
            "PATCH",
            format!("/outposts/{}", segment(&absent_outpost_arn)),
            404,
            "NotFoundException",
        )
        .with_body(json!({ "Name": "no such Outpost" })),
        refusal(
            "UpdateSite on an id nothing holds",
            "PATCH",
            format!("/sites/{ABSENT_SITE_ID}"),
            404,
            "NotFoundException",
        )
        .with_body(json!({ "Name": "no such site" })),
        refusal(
            "UpdateSite on an ARN nothing holds",
            "PATCH",
            format!("/sites/{}", segment(&absent_site_arn)),
            404,
            "NotFoundException",
        )
        .with_body(json!({ "Notes": "no such site" })),
        refusal(
            "UpdateSiteRackPhysicalProperties on an id nothing holds",
            "PATCH",
            format!("/sites/{ABSENT_SITE_ID}/rackPhysicalProperties"),
            404,
            "NotFoundException",
        )
        .with_body(json!({ "PowerPhase": "THREE_PHASE" })),
        refusal(
            "UpdateOutpost on a value that is no identifier",
            "PATCH",
            "/outposts/not-an-outpost-id".to_string(),
            400,
            "ValidationException",
        )
        .with_body(json!({ "Name": "nope" })),
        refusal(
            "UpdateOutpost on a site id",
            "PATCH",
            format!("/outposts/{ABSENT_SITE_ID}"),
            400,
            "ValidationException",
        )
        .with_body(json!({ "Name": "nope" })),
        refusal(
            "UpdateSite on a value that is no identifier",
            "PATCH",
            "/sites/not-a-site-id".to_string(),
            400,
            "ValidationException",
        )
        .with_body(json!({ "Name": "nope" })),
        refusal(
            "UpdateSite on an id whose hex is too short",
            "PATCH",
            "/sites/os-0000000000000000".to_string(),
            400,
            "ValidationException",
        )
        .with_body(json!({ "Name": "nope" })),
        refusal(
            "UpdateSiteRackPhysicalProperties on a value that is no identifier",
            "PATCH",
            "/sites/nope/rackPhysicalProperties".to_string(),
            400,
            "ValidationException",
        )
        .with_body(json!({ "PowerPhase": "THREE_PHASE" })),
        refusal(
            "UpdateSiteRackPhysicalProperties on an id whose suffix is not hex",
            "PATCH",
            "/sites/os-zzzzzzzzzzzzzzzzz/rackPhysicalProperties".to_string(),
            400,
            "ValidationException",
        )
        .with_body(json!({ "PowerPhase": "THREE_PHASE" })),
    ];

    outposts.expect_refusals(&cases).await;

    // A refused update created nothing: the store is still empty.
    for (method, path) in [
        ("GET", format!("/sites/{ABSENT_SITE_ID}")),
        ("GET", format!("/outposts/{ABSENT_OUTPOST_ID}")),
    ] {
        let reply = outposts.call(method, &path, None).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (404, "NotFoundException"),
            "case {method} {path}: a refused update must not have created the resource, got {} {:?}",
            reply.status,
            reply.body
        );
    }
}
