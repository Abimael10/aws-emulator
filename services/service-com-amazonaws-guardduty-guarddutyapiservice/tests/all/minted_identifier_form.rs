// SPDX-License-Identifier: CC0-1.0

//! The FORM a minted identifier is served in, read off the bytes the service answered
//! with and followed through every payload that names the resource.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handlers are exercised together.
//!
//! The rule under test: a minted identifier is served in the form its OWN member's
//! `@pattern` describes. The two members of this service whose pattern admits the
//! HYPHEN — `AssociationId`, `^[a-zA-Z0-9_-]{1,64}$`, `len 1..64`
//! (`smithy_query(shape="AssociationDetail,AssociationSummary")`), and
//! `InvestigationId`, `^[a-fA-F0-9\-]+$`, `len 1..64`
//! (`smithy_query(operation="CreateInvestigation")`) — carry the hyphen-grouped
//! 8-4-4-4-12 hexadecimal form, and those SAME bytes are what every payload naming the
//! resource carries, the `/association/<id>` suffix of its ARN included, so a value
//! read out of one payload is the handle the next call is addressed by. A minted id
//! whose member carries NO pattern (`DetectorId`, `IpSetId` — plain strings,
//! `smithy_query(operation="CreateDetector,CreateIPSet")`) is not re-grouped: it keeps
//! the bare hexadecimal form, which is why the rule is read off each member rather than
//! applied to every id this service mints.
//!
//! The group widths and the character classes here are the model's, cited above; the
//! digits of any one id are minted per run, so none is written into this suite.
//!
//! The recorded wire corpus is what settles which of the two forms a hyphen-admitting
//! member is served in, and it settles it for the association: replaying the recorded
//! association fixtures against this emulator, with nothing changed but the FORM the
//! create mints — the same per-run digits either way — diverges on `associationId`
//! while the bare form is served and passes every step once the grouped form is, so
//! the recorded bytes carry the grouping. No recorded value is reproduced here: a
//! recording's digits are one run's, and only the form is the requirement.

use serde_json::{json, Value};

use crate::custom_detection_rule_association_identity::{arn_association_id, associate};
use crate::detector_lifecycle::{
    call, call_bytes_in, call_in, enabled_detector, investigation_detector, new_router, Router,
};

/// `ListCustomDetectionRuleAssociations` is `GET /custom-detection-rule/association`
/// and the read of one association is
/// `GET /custom-detection-rule/rule/{RuleId}/association/{AssociationId}`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations,GetCustomDetectionRuleAssociation")`).
const ASSOCIATIONS_PATH: &str = "/custom-detection-rule/association";
/// See [`ASSOCIATIONS_PATH`].
const RULES_PATH: &str = "/custom-detection-rule/rule";

/// Every `AssociationMode` member (`smithy_query(shape="AssociationMode")`).
const MODES: [&str; 2] = ["DRY_RUN", "LIVE"];

/// Two Regions, so the form is asserted on an identity minted through more than one
/// endpoint.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-1"];

/// `MaxResults` is `range 1..100`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so one page this
/// size reports every association a case makes.
const MAX_RESULTS: i64 = 100;

/// The widths of the hyphen-separated groups a grouped id is served in, and the four
/// hyphens between them: the 8-4-4-4-12 form over the 32 hexadecimal characters this
/// service mints.
const GROUP_WIDTHS: [usize; 5] = [8, 4, 4, 4, 12];

/// The longest either hyphen-admitting id member may be: `len 1..64`
/// (`smithy_query(shape="AssociationDetail")`, `smithy_query(operation="CreateInvestigation")`).
const HYPHENATED_ID_MAX: usize = 64;

/// Why `value` is not an id in the hyphen-grouped form, or `None` when it is one.
///
/// Everything the form requires is checked here, so each assertion below names one
/// rule and one case: the group widths, the hexadecimal alphabet inside each group, and
/// the modelled length ceiling the whole value has to stay under.
fn not_grouped(value: &str) -> Option<String> {
    if value.len() > HYPHENATED_ID_MAX {
        return Some(format!(
            "longer than the modelled {HYPHENATED_ID_MAX} characters"
        ));
    }
    let groups: Vec<&str> = value.split('-').collect();
    if groups.len() != GROUP_WIDTHS.len() {
        return Some(format!(
            "expected {} hyphen-separated groups, found {}",
            GROUP_WIDTHS.len(),
            groups.len()
        ));
    }
    for (group, width) in groups.iter().zip(GROUP_WIDTHS) {
        if group.len() != width {
            return Some(format!("expected a {width}-character group, found {group:?}"));
        }
        if !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(format!("expected hexadecimal digits, found {group:?}"));
        }
    }
    None
}

/// Assert `value` is an id in the hyphen-grouped form.
fn assert_grouped(case: &str, member: &str, value: &str) {
    assert!(
        not_grouped(value).is_none(),
        "case {case:?}: expected {member} in the 8-4-4-4-12 grouped form, got {value:?} ({})",
        not_grouped(value).unwrap_or_default()
    );
}

/// Whether the bytes `raw` carry `member` with exactly the string `value`.
///
/// Read off the SERIALIZED body, because parsing collapses a member's JSON type, its
/// case and the whitespace around it — and the form of an id is exactly what a client
/// has to be able to copy out of the bytes. Whitespace BETWEEN tokens is tolerated
/// (it is a serializer's choice, not the service's answer); the member name and the
/// value are compared as written.
fn carries_string_member(raw: &str, member: &str, value: &str) -> bool {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(&format!("\"{member}\":\"{value}\""))
}

/// The `AssociationDetail` the read of one association reports, failing the test when
/// the read does not succeed — the round trip that proves a served id is a usable
/// `AssociationId` path label.
async fn read_association(router: &Router, region: &str, rule_id: &str, id: &str) -> Value {
    let (status, body) = call_in(
        router,
        region,
        "GET",
        &format!("{RULES_PATH}/{rule_id}/association/{id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "get-association {rule_id}/{id} in {region}: expected 200, got {status} {body:?}"
    );
    body["ruleAssociation"].clone()
}

// ---------------------------------------------------------------------------
// `AssociationId`: the member whose pattern admits the hyphen and whose value is
// also carried by the ARN that names the association.
// ---------------------------------------------------------------------------

/// Over every published rule, in both modes and through two Regions: the id the create
/// mints is in the grouped form, and the create body, the ARN suffix, the read-back and
/// the listing — parsed and as bytes — carry those same characters.
#[tokio::test]
async fn association_ids_are_served_in_the_hyphen_grouped_form() {
    let catalogue = crate::detector_lifecycle::published_rule_ids(&new_router()).await;

    for region in REGIONS {
        for mode in MODES {
            // One router per (Region, mode) pair: an account holds at most one
            // association per rule per Region, so each rule of the catalogue is
            // associated once here.
            let router = new_router();
            for rule_id in &catalogue {
                let case = format!("{rule_id} in {mode} through {region}");
                let created = associate(&router, region, rule_id, mode).await;
                let id = created["associationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: expected an associationId, got {created:?}"))
                    .to_string();
                assert_grouped(&case, "associationId", &id);

                let arn = created["arn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: expected an arn, got {created:?}"))
                    .to_string();
                assert_eq!(
                    arn_association_id(&arn),
                    Some(id.as_str()),
                    "case {case:?}: expected the arn to end in its own associationId {id:?}, got {arn:?}"
                );

                let read_back = read_association(&router, region, rule_id, &id).await;
                assert_eq!(
                    read_back["associationId"].as_str(),
                    Some(id.as_str()),
                    "case {case:?}: expected the read-back to echo {id:?}, got {read_back:?}"
                );
                assert_eq!(
                    read_back["arn"].as_str(),
                    Some(arn.as_str()),
                    "case {case:?}: expected the read-back to report {arn:?}, got {read_back:?}"
                );

                for (query, label) in [
                    (format!("?maxResults={MAX_RESULTS}"), "unfiltered listing"),
                    (
                        format!("?maxResults={MAX_RESULTS}&ruleId={rule_id}"),
                        "listing filtered by rule",
                    ),
                    (
                        format!("?maxResults={MAX_RESULTS}&mode={mode}"),
                        "listing filtered by mode",
                    ),
                ] {
                    let case = format!("{case}, {label}");
                    let (status, raw) =
                        call_bytes_in(&router, region, "GET", &format!("{ASSOCIATIONS_PATH}{query}"), None)
                            .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected 200, got {status} {raw:?}"
                    );
                    let listed: Value = serde_json::from_str(&raw)
                        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON body, got {raw:?}"));
                    let entry = listed["ruleAssociations"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .find(|entry| entry["arn"].as_str() == Some(arn.as_str()))
                        .unwrap_or_else(|| panic!("case {case:?}: expected an entry with arn {arn:?}, got {listed:?}"));
                    assert_eq!(
                        entry["associationId"].as_str(),
                        Some(id.as_str()),
                        "case {case:?}: expected the entry to advertise {id:?}, got {entry:?}"
                    );
                    assert!(
                        carries_string_member(&raw, "associationId", &id),
                        "case {case:?}: expected the bytes to carry \"associationId\":{id:?}, got {raw:?}"
                    );
                    // The form is part of the identity: the ungrouped characters of
                    // the same id are NOT what the service answered with.
                    let ungrouped = id.replace('-', "");
                    assert!(
                        !raw.contains(&ungrouped),
                        "case {case:?}: expected no ungrouped {ungrouped:?} in the bytes, got {raw:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `InvestigationId`: the other member whose pattern admits the hyphen, so the rule
// is asserted on more than the member the defect was found on.
// ---------------------------------------------------------------------------

/// `TriggerPrompt` is free text (`smithy_query(operation="CreateInvestigation")`), so
/// two prompts differing in length stand for the class of creates.
const PROMPTS: [&str; 2] = ["r", "an escalation of privilege on a role"];

/// Each investigation an account initiates is served under an id in the same grouped
/// form, and the read reached through that id reports it back.
#[tokio::test]
async fn investigation_ids_are_served_in_the_hyphen_grouped_form() {
    let router = new_router();
    let detector_id = investigation_detector(&router).await;

    for prompt in PROMPTS {
        let case = format!("investigation on a {}-character prompt", prompt.len());
        let (status, body) = call(
            &router,
            "POST",
            &format!("/detector/{detector_id}/investigation"),
            Some(json!({"triggerPrompt": prompt})),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        let id = body["investigationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an investigationId, got {body:?}"))
            .to_string();
        assert_grouped(&case, "investigationId", &id);

        let (status, raw) = call_bytes_in(
            &router,
            "us-east-1",
            "GET",
            &format!("/detector/{detector_id}/investigation/{id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read of {id:?} to answer 200, got {status} {raw:?}"
        );
        assert!(
            carries_string_member(&raw, "investigationId", &id),
            "case {case:?}: expected the bytes to carry \"investigationId\":{id:?}, got {raw:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The scope of the rule: an id whose member carries no pattern keeps its form.
// ---------------------------------------------------------------------------

/// `DetectorId` and `IpSetId` are plain strings with no `@pattern`
/// (`smithy_query(operation="CreateDetector,CreateIPSet")`), so neither is grouped:
/// each is served in the bare hexadecimal form this service mints, which is also the
/// form every path label built from it carries.
#[tokio::test]
async fn ids_whose_member_carries_no_pattern_keep_the_bare_hexadecimal_form() {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    assert!(
        detector_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "detectorId: expected bare hexadecimal characters, got {detector_id:?}"
    );

    // `CreateIPSet` takes the four required members of the set it makes
    // (`smithy_query(operation="CreateIPSet")`).
    let (status, body) = call(
        &router,
        "POST",
        &format!("/detector/{detector_id}/ipset"),
        Some(json!({
            "name": "bare-form-set",
            "format": "TXT",
            "location": "https://s3.amazonaws.com/bucket/list.txt",
            "activate": true
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "create-ipset: expected 200, got {status} {body:?}"
    );
    let ip_set_id = body["ipSetId"]
        .as_str()
        .unwrap_or_else(|| panic!("create-ipset: expected an ipSetId, got {body:?}"))
        .to_string();
    assert!(
        ip_set_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "ipSetId: expected bare hexadecimal characters, got {ip_set_id:?}"
    );

    // Both ids are still usable path labels in the form they were served in.
    let (status, body) = call(&router, "GET", &format!("/detector/{detector_id}"), None).await;
    assert_eq!(
        status, 200,
        "get-detector {detector_id}: expected 200, got {status} {body:?}"
    );
    let (status, body) = call(
        &router,
        "GET",
        &format!("/detector/{detector_id}/ipset/{ip_set_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "get-ipset {ip_set_id}: expected 200, got {status} {body:?}"
    );
}
