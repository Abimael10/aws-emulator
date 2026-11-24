// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for the IDENTITY a listing entry of this service
//! carries, driven through the real `MultiServiceRouter` so every assertion
//! reads the JSON a client receives.
//!
//! The rule under test: every entry of a resource listing identifies the stored
//! resource it summarizes — its own identifier and its ARN, scoped to the
//! caller's region, the emulator's account and the domain that holds it — so a
//! client that enumerated a resource can go on to read, tag and address it
//! through the entry alone, and a summary member that targets a list is reported
//! as a list, empty rather than absent, for a resource that holds none.
//!
//! The rows span the class the rule generalises over rather than the one listing
//! a divergence was reported on: every resource listing this service serves
//! (`ListDomains`, `ListFields`, `ListTemplates`, `ListLayouts` and
//! `ListCaseRules`), each in two signing regions — the region an ARN is scoped
//! to — and each entry located by its `name`, never by its position.
//!
//! Where a summary carries a member the two halves of the rule reach, the row
//! carries it too: the names one identity is reported under, which have to agree
//! (a template summary's `templateId`/`id` and `templateArn`/`arn`), and a
//! member targeting a list that the resource holds nothing in, which has to be
//! reported as `[]` rather than left out (a template's
//! `tagPropagationConfigurations`). What those members hold for a resource that
//! is not freshly created is the subject of `template_summary_wire_form.rs`,
//! which reads the same projection after updates and across page boundaries.
//!
//! The suite then consumes what the entries report: the identity an entry
//! reported resolves through the resource's OWN read, which answers the same id
//! and the same ARN, and the ARN addresses that resource for tagging, so a tag
//! written through the listed ARN is the tag the resource reports.

use serde_json::{json, Value};

// The crate's shared harness: one emulator behind the production router, and the
// request plumbing the other suites drive this service's lifecycle with.
use crate::case_lifecycle::create_case_rule;
use crate::harness::{domain, listed_tags, string_at, tag_resource, template, Emulator};
use crate::layout_lifecycle::{create_layout, text_field, top_panel};

/// The emulator's account id, as every ARN this service mints carries it.
const ACCOUNT: &str = "000000000000";

/// The regions a row is driven in: an ARN is scoped to the region the caller
/// signs for, so every expectation below is computed from the region rather
/// than fixed.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-3"];

/// How one resource kind is read back on its own, once a listing has reported
/// its identity.
#[derive(Debug, Clone, Copy)]
enum Read {
    /// `GetDomain`.
    Domain,
    /// `BatchGetField`, whose single requested field must resolve.
    Field,
    /// `GetTemplate`.
    Template,
    /// `GetLayout`.
    Layout,
    /// `BatchGetCaseRule`, whose single requested rule must resolve.
    CaseRule,
}

/// One resource kind's listing: the operation that enumerates it, where its
/// entries sit in the answer, the members an entry reports its identity under,
/// and the stored resource one of those entries must describe.
struct Listing {
    /// The listing operation, which is also the case name.
    operation: &'static str,
    /// The route the emulator serves the listing on.
    path: String,
    /// The output member holding the summaries.
    items: &'static str,
    /// The summary member carrying the resource's own identifier.
    id_member: &'static str,
    /// The summary member carrying its Amazon Resource Name.
    arn_member: &'static str,
    /// Every `@required` string member of the summary shape, which an entry
    /// therefore reports for every resource. The member lists come from
    /// `smithy_query(query="shape",
    /// shape="DomainSummary,FieldSummary,LayoutSummary,CaseRuleSummary,TemplateSummary")`.
    required: &'static [&'static str],
    /// The identifier the create minted for the resource under test.
    id: String,
    /// The resource path its ARN addresses it by, after the account field.
    arn_resource: String,
    /// Every pair of names the wire form reports ONE identity under: the
    /// modelled member and the other name a summary also carries it as. Empty
    /// for a kind whose summary reports its identity under the modelled names
    /// alone.
    aliases: &'static [(&'static str, &'static str)],
    /// Every summary member that targets a list and that the resource under
    /// test holds nothing in, so an entry reports it as the empty list rather
    /// than leaving it out — which is what lets a client iterate it zero times
    /// instead of handling a null.
    empty_lists: &'static [&'static str],
    /// The name the create gave it — how a row finds its own entry, so no
    /// assertion depends on where in the page it landed.
    name: String,
    /// How the resource is read back through the identity its entry reported.
    read: Read,
}

impl Listing {
    /// The ARN an entry must report for this resource when the caller signs for
    /// `region`: the general form of every ARN this service mints, with the
    /// region, the account and the resource path filled in.
    fn expected_arn(&self, region: &str) -> String {
        format!("arn:aws:cases:{region}:{ACCOUNT}:{}", self.arn_resource)
    }
}

/// The route `ListTemplates` is served on for `domain_id`.
fn templates_path(domain_id: &str) -> String {
    format!("/domains/{domain_id}/templates-list")
}

/// One resource of every kind a listing of this service enumerates, created in
/// one fresh domain, as the listings that enumerate them.
async fn one_of_every_kind(emulator: &Emulator) -> (String, Vec<Listing>) {
    let domain_name = "listed-domain";
    let domain_id = domain(emulator, domain_name).await;
    let field_id = text_field(emulator, &domain_id, "listed-field").await;
    // The shared harness create, which names no `tagPropagationConfigurations`:
    // the stored template propagates no tags, which is the shape its summary has
    // to report as the empty list.
    let template_id = template(
        emulator,
        &domain_id,
        "listed-template",
        std::slice::from_ref(&field_id),
    )
    .await;
    let (layout_id, _) = create_layout(
        emulator,
        "a layout to enumerate",
        &domain_id,
        "listed-layout",
        &top_panel(std::slice::from_ref(&field_id)),
    )
    .await;
    // The case-rule create the case suite drives, with the simplest rule the
    // model admits: a `required` condition list that is empty.
    let (case_rule_id, _) = create_case_rule(
        emulator,
        "a case rule to enumerate",
        &domain_id,
        "listed-case-rule",
        None,
        &json!({ "required": { "defaultValue": true, "conditions": [] } }),
    )
    .await;

    let listings = vec![
        Listing {
            operation: "ListDomains",
            path: "/domains-list".to_string(),
            items: "domains",
            id_member: "domainId",
            arn_member: "domainArn",
            required: &["domainId", "domainArn", "name"],
            aliases: &[],
            empty_lists: &[],
            id: domain_id.clone(),
            arn_resource: format!("domain/{domain_id}"),
            name: domain_name.to_string(),
            read: Read::Domain,
        },
        Listing {
            operation: "ListFields",
            path: format!("/domains/{domain_id}/fields-list"),
            items: "fields",
            id_member: "fieldId",
            arn_member: "fieldArn",
            required: &["fieldId", "fieldArn", "name", "type", "namespace"],
            aliases: &[],
            empty_lists: &[],
            id: field_id.clone(),
            arn_resource: format!("domain/{domain_id}/field/{field_id}"),
            name: "listed-field".to_string(),
            read: Read::Field,
        },
        Listing {
            operation: "ListTemplates",
            path: templates_path(&domain_id),
            items: "templates",
            id_member: "templateId",
            arn_member: "templateArn",
            required: &["templateId", "templateArn", "name", "status"],
            // A template summary carries its identity under the modelled pair
            // and under the `id`/`arn` names the wire form also reports it by.
            aliases: &[("templateId", "id"), ("templateArn", "arn")],
            // `tagPropagationConfigurations` targets a list (`@length(max: 1)`,
            // `smithy_query(query="shape", shape="TemplateSummary")`), and this
            // template was created without one.
            empty_lists: &["tagPropagationConfigurations"],
            id: template_id.clone(),
            arn_resource: format!("domain/{domain_id}/template/{template_id}"),
            name: "listed-template".to_string(),
            read: Read::Template,
        },
        Listing {
            operation: "ListLayouts",
            path: format!("/domains/{domain_id}/layouts-list"),
            items: "layouts",
            id_member: "layoutId",
            arn_member: "layoutArn",
            required: &["layoutId", "layoutArn", "name"],
            aliases: &[],
            empty_lists: &[],
            id: layout_id.clone(),
            arn_resource: format!("domain/{domain_id}/layout/{layout_id}"),
            name: "listed-layout".to_string(),
            read: Read::Layout,
        },
        Listing {
            operation: "ListCaseRules",
            path: format!("/domains/{domain_id}/rules-list/"),
            items: "caseRules",
            id_member: "caseRuleId",
            arn_member: "caseRuleArn",
            required: &["caseRuleId", "caseRuleArn", "name", "ruleType"],
            aliases: &[],
            empty_lists: &[],
            id: case_rule_id.clone(),
            // The `case-rule` path segment a rule's ARN addresses it by
            // (`state::case_rules`), which is also the segment the tagging
            // operations resolve a rule ARN through.
            arn_resource: format!("domain/{domain_id}/case-rule/{case_rule_id}"),
            name: "listed-case-rule".to_string(),
            read: Read::CaseRule,
        },
    ];
    (domain_id, listings)
}

/// Every entry the listing `operation` reports at `path` under `items`,
/// following its pages to exhaustion so the answer is the whole listing and not
/// only its first page. Each of these operations carries `nextToken` in the
/// query string, so the cursor travels there rather than in the body.
async fn entries(
    emulator: &Emulator,
    operation: &str,
    path: &str,
    items: &str,
) -> Vec<Value> {
    let mut reported: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let requested = match &token {
            Some(token) => format!("{path}?nextToken={token}"),
            None => path.to_string(),
        };
        let answer = emulator.ok(operation, "POST", &requested, json!({})).await;
        let page = answer
            .get(items)
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {operation:?}: the answer reports no {items} list, got {answer:?}")
            })
            .clone();
        reported.extend(page);
        token = answer
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return reported;
        }
    }
}

/// The one entry of `listing` whose `name` is the resource under test, which
/// must be there exactly once.
async fn entry_of(emulator: &Emulator, listing: &Listing) -> Value {
    let reported = entries(emulator, listing.operation, &listing.path, listing.items).await;
    let matching: Vec<&Value> = reported
        .iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(listing.name.as_str()))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {:?}: expected exactly one entry named {:?}, got {reported:?}",
        listing.operation,
        listing.name
    );
    matching[0].clone()
}

/// The resource `listing` describes, read back through the identity `id` — which
/// every caller of this function takes from the listing entry and nowhere else.
async fn read_back(emulator: &Emulator, listing: &Listing, domain_id: &str, id: &str) -> Value {
    let case = listing.operation;
    match listing.read {
        Read::Domain => {
            emulator
                .ok(case, "POST", &format!("/domains/{id}"), json!({}))
                .await
        }
        Read::Template => {
            emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/templates/{id}"),
                    json!({}),
                )
                .await
        }
        Read::Layout => {
            emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/layouts/{id}"),
                    json!({}),
                )
                .await
        }
        Read::Field => {
            let answer = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/fields-batch"),
                    json!({ "fields": [{ "id": id }] }),
                )
                .await;
            only_resolved(&answer, "fields", case)
        }
        Read::CaseRule => {
            let answer = emulator
                .ok(
                    case,
                    "POST",
                    &format!("/domains/{domain_id}/rules-batch"),
                    json!({ "caseRules": [{ "id": id }] }),
                )
                .await;
            only_resolved(&answer, "caseRules", case)
        }
    }
}

/// The single record a batch read reports under `member`, which must have
/// resolved: a batch that answered an error for the identity a listing reported
/// would fail here.
fn only_resolved(answer: &Value, member: &str, case: &str) -> Value {
    let errors = answer
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        errors.is_empty(),
        "case {case:?}: the batch read must resolve the listed identity, got errors {errors:?}"
    );
    let records = answer
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the answer reports no {member} list: {answer:?}"));
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected exactly one record back, got {records:?}"
    );
    records[0].clone()
}

#[tokio::test]
async fn every_listing_entry_identifies_the_resource_it_summarizes() {
    for region in REGIONS {
        let emulator = Emulator::in_region(region);
        let (_, listings) = one_of_every_kind(&emulator).await;
        for listing in &listings {
            let entry = entry_of(&emulator, listing).await;
            let case = listing.operation;

            // The identifier the entry reports is the one the create minted for
            // the resource the entry names: not an absent member, and not
            // another resource's.
            assert_eq!(
                entry.get(listing.id_member).and_then(Value::as_str),
                Some(listing.id.as_str()),
                "case {case:?}: the entry must report {} {:?}, got {entry:?}",
                listing.id_member,
                listing.id
            );

            // The ARN is the general form this service mints, scoped to the
            // caller's region, the emulator's account and the resource path.
            let expected = listing.expected_arn(region);
            assert_eq!(
                entry.get(listing.arn_member).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: the entry must report {} {expected:?}, got {entry:?}",
                listing.arn_member
            );

            // Every member the summary shape requires is reported, and none of
            // them empty: an SDK that enforces them deserializes the whole page
            // instead of failing on it.
            for member in listing.required {
                let value = entry.get(*member).and_then(Value::as_str);
                assert!(
                    value.is_some_and(|value| !value.is_empty()),
                    "case {case:?}: the entry must report a non-empty {member}, got {entry:?}"
                );
            }

            // Where the wire form reports one identity under two names, both
            // are projected from the same stored record, so they are reported
            // and they agree.
            for (modelled, also) in listing.aliases {
                let reported = entry.get(*also).and_then(Value::as_str);
                assert_eq!(
                    entry.get(*modelled).and_then(Value::as_str),
                    reported,
                    "case {case:?}: {modelled} and {also} must report one identity, \
                     got {entry:?}"
                );
                assert!(
                    reported.is_some_and(|value| !value.is_empty()),
                    "case {case:?}: the entry must report a non-empty {also}, got {entry:?}"
                );
            }

            // A summary member targeting a list is reported as a list, empty
            // rather than absent, for a resource that holds nothing in it.
            for member in listing.empty_lists {
                let reported = entry.get(*member);
                assert_eq!(
                    reported,
                    Some(&json!([])),
                    "case {case:?}: {member} must be reported as the empty list, got {entry:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_identity_a_listing_entry_reports_resolves_to_its_resource() {
    for region in REGIONS {
        let emulator = Emulator::in_region(region);
        let (domain_id, listings) = one_of_every_kind(&emulator).await;
        for listing in &listings {
            let entry = entry_of(&emulator, listing).await;
            let case = listing.operation;

            // Everything below is driven by what the ENTRY reported: this is the
            // list-then-use chain a client performs, and it has nothing else to
            // go on.
            let listed_id = string_at(&entry, listing.id_member, case);
            let listed_arn = string_at(&entry, listing.arn_member, case);

            // The resource's own read resolves the listed identity and reports
            // the same pair back.
            let read = read_back(&emulator, listing, &domain_id, &listed_id).await;
            assert_eq!(
                read.get(listing.id_member).and_then(Value::as_str),
                Some(listed_id.as_str()),
                "case {case:?}: the read must report the listed {}, got {read:?}",
                listing.id_member
            );
            assert_eq!(
                read.get(listing.arn_member).and_then(Value::as_str),
                Some(listed_arn.as_str()),
                "case {case:?}: the read must report the listed {}, got {read:?}",
                listing.arn_member
            );
            assert_eq!(
                read.get("name").and_then(Value::as_str),
                Some(listing.name.as_str()),
                "case {case:?}: the read must report the listed name, got {read:?}"
            );

            // The listed ARN addresses that resource for tagging, so a tag
            // written through it is the tag the resource reports.
            let tags = json!({ "listedBy": listing.operation });
            tag_resource(&emulator, case, &listed_arn, &tags).await;
            let read_tags = listed_tags(&emulator, case, &listed_arn).await;
            assert_eq!(
                read_tags, tags,
                "case {case:?}: the tag written through the listed ARN must be the one the \
                 resource reports, got {read_tags:?}"
            );
        }
    }
}
