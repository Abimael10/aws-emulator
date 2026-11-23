// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for the wire form of a `ListTemplates` summary,
//! driven through the real `MultiServiceRouter` so every assertion reads the
//! JSON a client receives.
//!
//! The rule under test: every member a template summary carries is projected
//! from the stored template — its identity under BOTH names a summary reports it
//! by (`templateId` and `id`, `templateArn` and `arn`, which therefore agree),
//! and its tag-propagation list as a list, empty rather than absent for a
//! template that propagates no tags — and the single-template read agrees with
//! the summary on the same stored record.
//!
//! The rows span the class the rule generalises over: how many tag-propagation
//! configurations a template holds (none, and the one the model's
//! `@length(max: 1)` admits), how many entries that configuration's `tagMap`
//! carries (none, one, and the ten `MutableTags` admits), which
//! `TagPropagationResourceType` it names, which `TemplateStatus` the template
//! carries (including the one it takes when the create names none), and the
//! region the caller signs for — which is what the ARN is scoped to.
//!
//! A second table writes each template twice: the projection reports the record
//! as it is stored NOW, so an update that adds, replaces or empties the
//! tag-propagation list — or that moves the other members a summary reports —
//! shows up in the next listing, an emptied list still as `[]`.
//!
//! A third table reads the same rows back the way a client does: it pins the
//! member set a summary carries (none missing, none invented) and then resolves
//! the listed template through the identity the summary alone reported — the
//! `id` and the `arn` — so a summary that reported an identity nothing resolves
//! would fail here.
//!
//! A fourth table crosses the projection with the rest of the STORE: a summary
//! is the projection of its own stored record, so templates created and deleted
//! beside it — and another domain holding a template of the same name — leave
//! every member of its element where it was, and the identity the element
//! reports stays the one the domain that listed it resolves.
//!
//! A fifth table crosses the projection with the PAGE the summary arrived on:
//! a page boundary cuts a listing, so every page size the model admits — not
//! only the one-entry page the tables above read — has to report each element in
//! full.
//!
//! Two further tables take the projection off a single untouched domain. One
//! holds several domains in ONE emulator, where an element's identity has to be
//! the listed template's own: its ARN is scoped to the domain that holds it, the
//! listing reports that domain's templates and no other's, and the identity it
//! reports reaches the template there and nowhere else. The other changes the
//! SET a domain holds between listings — deleting the first, a middle and the
//! last element, and creating one after each — so what an element reports
//! depends on the stored record and not on its position or on the set having
//! been left alone.
//!
//! The suite closes on what the projection is FOR: a listing entry is
//! self-describing, so the last table walks the whole list-then-use chain a
//! client performs with the identity an entry reported and nothing else — the
//! template read, tagged and read back, a case created from it where its status
//! admits one, the template updated and finally deleted.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::ops::types::{
    TAG_PROPAGATION_RESOURCE_TYPE_VALUES, TEMPLATE_STATUS_VALUES,
};
use service_com_amazonaws_connectcases_amazonconnectcases::state::MAX_PAGE_SIZE;

// The crate's shared harness: one emulator behind the production router, and
// the request plumbing every suite in this target drives the lifecycle with —
// the tagging calls and the case create and read this suite consumes a listed
// identity through among them, rather than a copy of its own.
use crate::harness::{
    case_template, create_case, domain, is_minted_id, listed_tags, refused_as_not_found, string_at,
    tag_resource, Emulator,
};
use crate::template_update::text_field;

/// The emulator's account id, as every ARN this service mints carries it.
const ACCOUNT: &str = "000000000000";

/// The `TemplateStatus` of a template a case can be created from: "A template
/// can be either Active or Inactive, as indicated by its status. Inactive
/// templates cannot be used to create cases"
/// (`smithy_query(query="doc", shape="CreateTemplate")`). It is also the status
/// a create that names none stores, which is why the expectations below read it
/// for a row that leaves `status` out.
const TEMPLATE_ACTIVE: &str = "Active";

/// The largest number of entries a `tagMap` holds: `MutableTags` is
/// `@length(max: 10)` (`smithy_query(query="shape",
/// shape="TagPropagationConfiguration")`).
const MAX_TAG_MAP_ENTRIES: usize = 10;

/// One row of the table: a template to create, and the tag-propagation
/// configurations it is created with.
struct Row {
    case: &'static str,
    /// The `status` the create names, or `None` to leave it out — which stores
    /// the template as `Active`.
    status: Option<&'static str>,
    tag_propagation_configurations: Vec<Value>,
}

/// A `tagMap` of `entries` distinct key/value pairs.
fn tag_map(entries: usize) -> Value {
    let mut map = serde_json::Map::new();
    for index in 0..entries {
        map.insert(format!("key-{index:02}"), json!(format!("value-{index:02}")));
    }
    Value::Object(map)
}

/// The table: the tag-propagation shapes a template can hold crossed with the
/// statuses it can carry.
fn rows() -> Vec<Row> {
    let resource_type = TAG_PROPAGATION_RESOURCE_TYPE_VALUES[0];
    let mut rows = vec![
        Row {
            case: "no tag propagation, no status named",
            status: None,
            tag_propagation_configurations: Vec::new(),
        },
        Row {
            case: "one configuration whose tagMap is empty",
            status: Some(TEMPLATE_STATUS_VALUES[0]),
            tag_propagation_configurations: vec![
                json!({ "resourceType": resource_type, "tagMap": tag_map(0) }),
            ],
        },
        Row {
            case: "one configuration with one tag",
            status: Some(TEMPLATE_STATUS_VALUES[0]),
            tag_propagation_configurations: vec![
                json!({ "resourceType": resource_type, "tagMap": tag_map(1) }),
            ],
        },
        Row {
            case: "one configuration with as many tags as a tagMap holds",
            status: Some(TEMPLATE_STATUS_VALUES[0]),
            tag_propagation_configurations: vec![json!({
                "resourceType": resource_type,
                "tagMap": tag_map(MAX_TAG_MAP_ENTRIES),
            })],
        },
    ];
    // Every `TemplateStatus` the model declares, each with no tag propagation:
    // the projection does not depend on the status the template carries.
    for status in TEMPLATE_STATUS_VALUES {
        rows.push(Row {
            case: "no tag propagation, an explicit status",
            status: Some(status),
            tag_propagation_configurations: Vec::new(),
        });
    }
    // And every resource type a configuration can name.
    for resource_type in TAG_PROPAGATION_RESOURCE_TYPE_VALUES {
        rows.push(Row {
            case: "one configuration naming a resource type",
            status: Some(TEMPLATE_STATUS_VALUES[1]),
            tag_propagation_configurations: vec![
                json!({ "resourceType": resource_type, "tagMap": tag_map(2) }),
            ],
        });
    }
    rows
}

/// `CreateTemplate` for one row, as the `(templateId, templateArn)` it minted —
/// so the summary's identity can be checked against what the create itself
/// reported rather than against the test's format string alone.
async fn create(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    name: &str,
    row: &Row,
) -> (String, String) {
    let mut body = json!({ "name": name });
    if let Some(status) = row.status {
        body["status"] = json!(status);
    }
    if !row.tag_propagation_configurations.is_empty() {
        body["tagPropagationConfigurations"] = json!(row.tag_propagation_configurations);
    }
    let created = emulator
        .ok(case, "POST", &format!("/domains/{domain_id}/templates"), body)
        .await;
    (
        string_at(&created, "templateId", case),
        string_at(&created, "templateArn", case),
    )
}

/// `ListTemplates` for `domain_id`, with `query` appended verbatim.
async fn list(emulator: &Emulator, case: &str, domain_id: &str, query: &str) -> Value {
    emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/templates-list{query}"),
            json!({}),
        )
        .await
}

/// `GetTemplate`, so a summary can be checked against the record the
/// single-resource read reports.
async fn get_template(emulator: &Emulator, case: &str, domain_id: &str, template_id: &str) -> Value {
    emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/templates/{template_id}"),
            json!({}),
        )
        .await
}

/// The summary in a listing answer whose `member` carries `value`: the ONE
/// lookup every read below is made through, whichever member identifies the
/// entry it wants.
fn summary_where(case: &str, answer: &Value, member: &str, value: &str) -> Value {
    answer
        .get("templates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a listing must carry templates, got {answer:?}"))
        .iter()
        .find(|summary| summary.get(member).and_then(Value::as_str) == Some(value))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing must report the template whose {member} is {value}, \
                 got {answer:?}"
            )
        })
        .clone()
}

/// The summary of `template_id` in a listing answer.
fn summary_of(case: &str, answer: &Value, template_id: &str) -> Value {
    summary_where(case, answer, "templateId", template_id)
}

/// The `tagPropagationConfigurations` member of a response, which must be a
/// list whether or not the template holds any.
fn tag_propagation_configurations(case: &str, member_of: &str, body: &Value) -> Vec<Value> {
    let member = body.get("tagPropagationConfigurations").unwrap_or_else(|| {
        panic!(
            "case {case:?}: {member_of} must carry tagPropagationConfigurations, got {body:?}"
        )
    });
    member
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: {member_of}'s tagPropagationConfigurations must be a list, got \
                 {member:?}"
            )
        })
        .clone()
}

/// The stored template a summary must report: what the create wrote, and where
/// it was written.
struct Stored<'a> {
    region: &'a str,
    domain_id: &'a str,
    template_id: &'a str,
    name: &'a str,
    status: &'a str,
    tag_propagation_configurations: &'a [Value],
}

impl Stored<'_> {
    /// The ARN of this template, as this service mints one:
    /// `arn:aws:cases:<region>:<account>:domain/<domainId>/template/<templateId>`.
    fn arn(&self) -> String {
        format!(
            "arn:aws:cases:{}:{ACCOUNT}:domain/{}/template/{}",
            self.region, self.domain_id, self.template_id
        )
    }
}

/// Assert the rule on one summary: every member is the stored template's, the
/// identity agrees under both names it is reported by, and the tag-propagation
/// list is present whatever it holds.
fn assert_summary(case: &str, summary: &Value, stored: &Stored<'_>) {
    assert!(
        is_minted_id(stored.template_id),
        "case {case:?}: the templateId must be a server-minted id, got {:?}",
        stored.template_id
    );
    for (member, expected) in [
        ("templateId", stored.template_id.to_string()),
        ("templateArn", stored.arn()),
        ("name", stored.name.to_string()),
        ("status", stored.status.to_string()),
        // The identity members a summary also reports the template by.
        ("id", stored.template_id.to_string()),
        ("arn", stored.arn()),
    ] {
        let actual = string_at(summary, member, case);
        assert_eq!(
            actual, expected,
            "case {case:?}: the summary's {member} must be the stored template's \
             ({expected}), got {actual}"
        );
    }
    let listed = tag_propagation_configurations(case, "a summary", summary);
    assert_eq!(
        listed, stored.tag_propagation_configurations,
        "case {case:?}: the summary must report the tag-propagation configurations the template \
         was created with ({:?}), got {listed:?}",
        stored.tag_propagation_configurations
    );
}

#[tokio::test]
async fn every_summary_member_is_projected_from_the_stored_template() {
    for region in ["us-east-1", "us-west-2"] {
        let emulator = Emulator::in_region(region);
        let domain_id = domain(&emulator, "summary-domain").await;

        let rows = rows();
        let mut created: Vec<(String, String, String, String)> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let name = format!("summary-template-{index:03}");
            let case = format!("{} in {region}", row.case);
            let (template_id, template_arn) =
                create(&emulator, &case, &domain_id, &name, row).await;
            let status = row.status.unwrap_or("Active").to_string();
            created.push((template_id, template_arn, name, status));
        }

        let answer = list(&emulator, "every template", &domain_id, "").await;
        for (row, (template_id, template_arn, name, status)) in rows.iter().zip(created.iter()) {
            let case = format!("{} in {region}", row.case);
            let summary = summary_of(&case, &answer, template_id);
            let stored = Stored {
                region,
                domain_id: &domain_id,
                template_id,
                name,
                status,
                tag_propagation_configurations: &row.tag_propagation_configurations,
            };
            // The ARN the create itself reported is the one the summary owes,
            // so the expected form below is not the test talking to itself.
            assert_eq!(
                *template_arn,
                stored.arn(),
                "case {case:?}: CreateTemplate must report the ARN of the template it stored"
            );
            assert_summary(&case, &summary, &stored);

            // The single-resource read of the same stored record agrees with the
            // summary, tag-propagation list included.
            let record = get_template(&emulator, &case, &domain_id, template_id).await;
            for member in ["templateId", "templateArn", "name", "status"] {
                assert_eq!(
                    string_at(&summary, member, &case),
                    string_at(&record, member, &case),
                    "case {case:?}: the summary's {member} must be what GetTemplate reports for \
                     the same template, got {summary:?} and {record:?}"
                );
            }
            let read = tag_propagation_configurations(&case, "GetTemplate", &record);
            assert_eq!(
                read, row.tag_propagation_configurations,
                "case {case:?}: GetTemplate must report the same tag-propagation configurations \
                 as the summary ({:?}), got {read:?}",
                row.tag_propagation_configurations
            );
        }
    }
}

/// One row of the update table: what the template is created with, the update
/// written over it, and the stored template a summary must report afterwards.
///
/// The rows differ along what the rule generalises over once a template has
/// been written twice: whether the update adds, replaces, clears or leaves
/// alone the tag-propagation list, and whether it touches the members a summary
/// reports beside it.
struct UpdateRow {
    case: &'static str,
    created_with: Vec<Value>,
    /// The `UpdateTemplate` body, minus the rename the flag below asks for.
    update: Value,
    /// Whether the update also renames the template, which a summary reports.
    renames: bool,
    expected_status: &'static str,
    expected_configurations: Vec<Value>,
}

/// One tag-propagation configuration naming `resource_type`, with `entries` tags.
fn configuration(resource_type: &str, entries: usize) -> Value {
    json!({ "resourceType": resource_type, "tagMap": tag_map(entries) })
}

/// The update table: every shape an update gives a stored template's
/// tag-propagation list, plus one update that leaves the list alone while
/// moving the other members a summary reports.
fn update_rows() -> Vec<UpdateRow> {
    let first = TAG_PROPAGATION_RESOURCE_TYPE_VALUES[0];
    let last = TAG_PROPAGATION_RESOURCE_TYPE_VALUES[TAG_PROPAGATION_RESOURCE_TYPE_VALUES.len() - 1];
    vec![
        UpdateRow {
            case: "tag propagation added to a template created without any",
            created_with: Vec::new(),
            update: json!({ "tagPropagationConfigurations": [configuration(first, 1)] }),
            renames: false,
            expected_status: TEMPLATE_STATUS_VALUES[0],
            expected_configurations: vec![configuration(first, 1)],
        },
        UpdateRow {
            case: "tag propagation cleared back to an empty list",
            created_with: vec![configuration(first, 3)],
            update: json!({ "tagPropagationConfigurations": [] }),
            renames: false,
            expected_status: TEMPLATE_STATUS_VALUES[0],
            expected_configurations: Vec::new(),
        },
        UpdateRow {
            case: "the configuration replaced by one with as many tags as a tagMap holds",
            created_with: vec![configuration(first, 1)],
            update: json!({
                "tagPropagationConfigurations": [configuration(last, MAX_TAG_MAP_ENTRIES)],
            }),
            renames: false,
            expected_status: TEMPLATE_STATUS_VALUES[0],
            expected_configurations: vec![configuration(last, MAX_TAG_MAP_ENTRIES)],
        },
        UpdateRow {
            case: "renamed and moved to another status, the tag propagation left alone",
            created_with: vec![configuration(first, 2)],
            update: json!({ "status": TEMPLATE_STATUS_VALUES[1] }),
            renames: true,
            expected_status: TEMPLATE_STATUS_VALUES[1],
            expected_configurations: vec![configuration(first, 2)],
        },
    ]
}

/// A summary is projected from the template as it is STORED NOW, not as it was
/// created: after an update the listing reports the updated members, the
/// identity still agrees under both names it is reported by, and a
/// tag-propagation list the update emptied is reported as `[]` rather than
/// dropped.
#[tokio::test]
async fn a_summary_reports_the_stored_template_after_an_update() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "summary-after-update").await;

    for (index, row) in update_rows().iter().enumerate() {
        let created_name = format!("updated-template-{index:03}");
        let created = Row {
            case: row.case,
            status: Some(TEMPLATE_STATUS_VALUES[0]),
            tag_propagation_configurations: row.created_with.clone(),
        };
        let (template_id, template_arn) =
            create(&emulator, row.case, &domain_id, &created_name, &created).await;

        let expected_name = if row.renames {
            format!("renamed-template-{index:03}")
        } else {
            created_name.clone()
        };
        let mut body = row.update.clone();
        if row.renames {
            body["name"] = json!(expected_name);
        }
        emulator
            .ok(
                row.case,
                "PUT",
                &format!("/domains/{domain_id}/templates/{template_id}"),
                body,
            )
            .await;

        let answer = list(&emulator, row.case, &domain_id, "").await;
        let summary = summary_of(row.case, &answer, &template_id);
        let stored = Stored {
            region: "us-east-1",
            domain_id: &domain_id,
            template_id: &template_id,
            name: &expected_name,
            status: row.expected_status,
            tag_propagation_configurations: &row.expected_configurations,
        };
        assert_summary(row.case, &summary, &stored);
        // An update mints no new identity, so the summary still reports the one
        // the create handed out — under both names.
        assert_eq!(
            string_at(&summary, "arn", row.case),
            template_arn,
            "case {:?}: an update must not change the ARN the create reported ({template_arn})",
            row.case
        );

        // The single-resource read of the same stored record agrees.
        let record = get_template(&emulator, row.case, &domain_id, &template_id).await;
        assert_eq!(
            string_at(&record, "name", row.case),
            expected_name,
            "case {:?}: GetTemplate must report the updated name, got {record:?}",
            row.case
        );
        assert_eq!(
            string_at(&record, "status", row.case),
            row.expected_status,
            "case {:?}: GetTemplate must report the updated status, got {record:?}",
            row.case
        );
        let read = tag_propagation_configurations(row.case, "GetTemplate", &record);
        assert_eq!(
            read, row.expected_configurations,
            "case {:?}: GetTemplate must report the tag-propagation configurations the update \
             left stored ({:?}), got {read:?}",
            row.case, row.expected_configurations
        );
    }
}

/// Every member a template summary carries on the wire: the modelled
/// `templateId`/`templateArn`/`name`/`status`/`tagPropagationConfigurations`
/// (`smithy_query(query="shape", shape="TemplateSummary")`) plus the `id` and
/// `arn` names the service reports the same identity by. A summary carries
/// these and nothing besides them.
const SUMMARY_MEMBERS: [&str; 7] = [
    "templateId",
    "templateArn",
    "name",
    "status",
    "tagPropagationConfigurations",
    "id",
    "arn",
];

/// The `(domainId, templateId)` an ARN addresses — what a client holding only
/// the `arn` a summary reported reads out of it to call the single-template
/// operations.
fn identity_in(case: &str, arn: &str) -> (String, String) {
    let (domain_id, template_id) = arn
        .split_once(":domain/")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("/template/"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the arn must address a domain's template, got {arn:?}")
        });
    (domain_id.to_string(), template_id.to_string())
}

/// A listed template is identifiable and resolvable from its summary alone: the
/// summary carries exactly the member set the service reports, its identity
/// agrees under both names it is reported by, the ARN addresses the domain the
/// template was created in, and the identity read off the summary — the `id` and
/// the `arn`, not the ids the test minted the template with — reaches the
/// template through `GetTemplate`.
///
/// The rows span the class the rule generalises over (tag-propagation shape,
/// status, the region the ARN is scoped to) and each is created in two different
/// domains, so an ARN that did not name the template's own domain would fail
/// here.
#[tokio::test]
async fn a_listed_template_is_resolvable_through_the_identity_its_summary_reports() {
    for region in ["us-east-1", "eu-central-1"] {
        let emulator = Emulator::in_region(region);
        let rows = rows();
        for domain_index in 0..2 {
            let domain_id = domain(&emulator, &format!("identity-domain-{domain_index}")).await;

            let mut created: Vec<(String, String)> = Vec::new();
            for (index, row) in rows.iter().enumerate() {
                let name = format!("identity-template-{domain_index}-{index:03}");
                let case = format!("{} in {region}, domain {domain_index}", row.case);
                let (template_id, _) = create(&emulator, &case, &domain_id, &name, row).await;
                created.push((template_id, name));
            }

            let answer = list(&emulator, "every template", &domain_id, "").await;
            for (row, (template_id, name)) in rows.iter().zip(created.iter()) {
                let case = format!("{} in {region}, domain {domain_index}", row.case);
                let summary = summary_of(&case, &answer, template_id);

                // Exactly the member set the service reports: none missing, none
                // invented.
                let members: Vec<&String> = summary
                    .as_object()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a summary must be an object, got {summary:?}")
                    })
                    .keys()
                    .collect();
                for member in SUMMARY_MEMBERS {
                    assert!(
                        members.iter().any(|carried| *carried == member),
                        "case {case:?}: a summary must carry {member}, got {members:?}"
                    );
                }
                assert_eq!(
                    members.len(),
                    SUMMARY_MEMBERS.len(),
                    "case {case:?}: a summary must carry only {SUMMARY_MEMBERS:?}, got {members:?}"
                );

                // The identity under both names, and the ARN pointing back at
                // the template's own domain and id.
                let id = string_at(&summary, "id", &case);
                let arn = string_at(&summary, "arn", &case);
                assert_eq!(
                    id,
                    string_at(&summary, "templateId", &case),
                    "case {case:?}: the summary's id and templateId must be the same identity, \
                     got {summary:?}"
                );
                assert_eq!(
                    arn,
                    string_at(&summary, "templateArn", &case),
                    "case {case:?}: the summary's arn and templateArn must be the same identity, \
                     got {summary:?}"
                );
                let (addressed_domain, addressed_template) = identity_in(&case, &arn);
                assert_eq!(
                    addressed_domain, domain_id,
                    "case {case:?}: the summary's arn must address the domain the template was \
                     created in ({domain_id}), got {arn}"
                );
                assert_eq!(
                    addressed_template, id,
                    "case {case:?}: the summary's arn must address the template its id names \
                     ({id}), got {arn}"
                );

                // List-then-use: the identity the summary reported is enough to
                // read the template back, with no second lookup to resolve it.
                let record =
                    get_template(&emulator, &case, &addressed_domain, &addressed_template).await;
                assert_eq!(
                    string_at(&record, "name", &case),
                    *name,
                    "case {case:?}: the identity the summary reported must reach the template it \
                     listed, got {record:?}"
                );
                assert_eq!(
                    string_at(&record, "status", &case),
                    row.status.unwrap_or("Active"),
                    "case {case:?}: the template reached through the summary's identity must \
                     carry the stored status, got {record:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_summary_carries_every_member_whatever_shape_the_listing_request_takes() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "summary-request-shapes").await;

    // Two templates of different statuses, one of them propagating tags, so a
    // filtered listing selects one and a page cut splits the pair.
    let mut created: Vec<(String, String, String, Vec<Value>)> = Vec::new();
    for (index, status) in TEMPLATE_STATUS_VALUES.iter().enumerate() {
        let configurations = if index == 0 {
            Vec::new()
        } else {
            vec![json!({
                "resourceType": TAG_PROPAGATION_RESOURCE_TYPE_VALUES[0],
                "tagMap": tag_map(1),
            })]
        };
        let row = Row {
            case: "request-shape row",
            status: Some(status),
            tag_propagation_configurations: configurations.clone(),
        };
        let name = format!("request-shape-template-{index:03}");
        let case = format!("create {name}");
        let (template_id, _) = create(&emulator, &case, &domain_id, &name, &row).await;
        created.push((template_id, name, status.to_string(), configurations));
    }

    // The request shapes a listing takes: unfiltered, one page at a time, and
    // narrowed to each status.
    let mut shapes: Vec<(String, String)> = vec![
        ("unfiltered".to_string(), String::new()),
        ("one entry per page".to_string(), "?maxResults=1".to_string()),
    ];
    for status in TEMPLATE_STATUS_VALUES {
        shapes.push((format!("status={status}"), format!("?status={status}")));
    }

    for (shape, query) in shapes {
        // Follow every page the shape hands out, so the projection is asserted
        // on each entry however the listing was cut. The page count is bounded:
        // a listing that kept handing out a token would fail here rather than
        // run on.
        let mut token: Option<String> = None;
        let mut seen = 0;
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(
                pages <= created.len() + 1,
                "case {shape:?}: following nextToken must terminate, but page {pages} still \
                 carried one after {seen} summary(s)"
            );
            let case = format!("{shape}, page {pages}");
            let paged = match &token {
                Some(value) if query.is_empty() => format!("?nextToken={value}"),
                Some(value) => format!("{query}&nextToken={value}"),
                None => query.clone(),
            };
            let answer = list(&emulator, &case, &domain_id, &paged).await;
            for (template_id, name, status, configurations) in &created {
                let listed = answer["templates"]
                    .as_array()
                    .unwrap_or_else(|| panic!("case {case:?}: no templates in {answer:?}"))
                    .iter()
                    .any(|summary| {
                        summary.get("templateId").and_then(Value::as_str) == Some(template_id)
                    });
                if !listed {
                    continue;
                }
                seen += 1;
                let summary = summary_of(&case, &answer, template_id);
                assert_summary(
                    &case,
                    &summary,
                    &Stored {
                        region: "us-east-1",
                        domain_id: &domain_id,
                        template_id,
                        name,
                        status,
                        tag_propagation_configurations: configurations,
                    },
                );
            }
            token = answer
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            seen > 0,
            "case {shape:?}: the listing must report at least one template to project, got none"
        );
    }
}

/// Every summary of one listing, gathered page by page with the listing cut at
/// `max_results` (`None` for a request that names no page size).
///
/// A page may hold no more entries than the cut admits, and following the tokens
/// has to terminate: a listing that kept handing one out fails here rather than
/// running on. `at_most` is how many entries the listing can report at all,
/// which bounds the number of pages.
async fn summaries_page_by_page(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    max_results: Option<i32>,
    at_most: usize,
) -> Vec<Value> {
    let limit = max_results.unwrap_or(MAX_PAGE_SIZE) as usize;
    let mut gathered: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(
            pages <= at_most + 1,
            "case {case:?}: following nextToken must terminate, but page {pages} still carried \
             one after {} summary(s)",
            gathered.len()
        );
        let query = match (max_results, &token) {
            (Some(size), Some(value)) => format!("?maxResults={size}&nextToken={value}"),
            (Some(size), None) => format!("?maxResults={size}"),
            (None, Some(value)) => format!("?nextToken={value}"),
            (None, None) => String::new(),
        };
        let answer = list(emulator, case, domain_id, &query).await;
        let page = answer
            .get("templates")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case:?}: a listing must carry templates, got {answer:?}")
            })
            .clone();
        assert!(
            page.len() <= limit,
            "case {case:?}: page {pages} may hold at most {limit} summary(s), got {}",
            page.len()
        );
        gathered.extend(page);
        token = answer
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return gathered;
        }
    }
}

/// The summary of `template_id` among the ones a traversal gathered.
fn summary_among(case: &str, summaries: &[Value], template_id: &str) -> Value {
    summaries
        .iter()
        .find(|summary| summary.get("templateId").and_then(Value::as_str) == Some(template_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the pages must report template {template_id}: {summaries:?}")
        })
        .clone()
}

/// A page boundary CUTS a listing; it does not change what an element of it
/// reports. Whatever page size the request names, every summary on every page is
/// the full projection of its stored template — identity under both names it is
/// reported by, name, status and the tag-propagation list, empty rather than
/// absent — each stored template is reported exactly once across the pages, and
/// a traversal reports the same elements as the one before it, because listing
/// changes nothing.
///
/// The rows span the page sizes the request can name: both bounds of the model's
/// `MaxResults` range for this operation (`MAX_PAGE_SIZE` is this service's
/// reading of the `@range(1..100)` it shares with the `@length(max: 100)` of the
/// list a page carries), sizes inside the range that do and do not divide the
/// listing, a size that holds the whole listing, and no page size at all. The
/// templates themselves are the table at the top of this suite, so each page
/// carries elements of differing tag-propagation shape and status.
#[tokio::test]
async fn every_page_of_a_cut_listing_carries_the_full_projection() {
    // The region the caller signs for, which is the one the ARN each summary
    // reports is scoped to: named once, so the emulator and the expected ARNs
    // below cannot drift apart.
    const REGION: &str = "us-east-1";

    let emulator = Emulator::in_region(REGION);
    let domain_id = domain(&emulator, "paged-summary-domain").await;

    let rows = rows();
    let mut created: Vec<(String, String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("paged-template-{index:03}");
        let case = format!("create {name}");
        let (template_id, _) = create(&emulator, &case, &domain_id, &name, row).await;
        created.push((
            template_id,
            name,
            row.status.unwrap_or("Active").to_string(),
        ));
    }

    let entries = rows.len() as i32;
    let cuts: Vec<Option<i32>> = vec![
        None,
        Some(1),
        Some(2),
        Some(entries - 1),
        Some(entries),
        Some(MAX_PAGE_SIZE),
    ];

    let mut first_traversal: Option<(String, Vec<Value>)> = None;
    for cut in cuts {
        let case = match cut {
            Some(size) => format!("maxResults={size}"),
            None => "no maxResults".to_string(),
        };
        let gathered =
            summaries_page_by_page(&emulator, &case, &domain_id, cut, rows.len()).await;
        assert_eq!(
            gathered.len(),
            created.len(),
            "case {case:?}: following the pages to exhaustion must report every template, got \
             {gathered:?}"
        );

        // Every element in full, whichever page it arrived on.
        for (row, (template_id, name, status)) in rows.iter().zip(created.iter()) {
            let element = format!("{case}, {}", row.case);
            let summary = summary_among(&element, &gathered, template_id);
            assert_summary(
                &element,
                &summary,
                &Stored {
                    region: REGION,
                    domain_id: &domain_id,
                    template_id,
                    name,
                    status,
                    tag_propagation_configurations: &row.tag_propagation_configurations,
                },
            );
        }

        // Exactly once each: a template reported on two pages would be counted
        // twice by a client that pages through the listing.
        let identities: Vec<String> = gathered
            .iter()
            .map(|summary| string_at(summary, "id", &case))
            .collect();
        let distinct: BTreeSet<&String> = identities.iter().collect();
        assert_eq!(
            distinct.len(),
            identities.len(),
            "case {case:?}: no template may be reported on two pages, got {identities:?}"
        );

        // And the same elements, in the same order, as the traversal before it.
        match &first_traversal {
            None => first_traversal = Some((case, gathered)),
            Some((first_case, first)) => assert_eq!(
                &gathered, first,
                "case {case:?}: a page size cuts the listing, it does not reorder it or change \
                 what an element reports: expected what {first_case:?} reported ({first:?}), got \
                 {gathered:?}"
            ),
        }
    }
}

/// One row of the population table: how many OTHER templates the domain holds
/// around the one under test, and how that population changes between two
/// listings.
///
/// The rows differ along what the rule generalises over once a summary is a
/// projection of its own stored record: how many other templates the domain
/// holds, how many of them are deleted between the listings, and how many are
/// created between them.
struct PopulationRow {
    case: &'static str,
    /// Templates created in the domain beside the subject before the first
    /// listing.
    neighbours: usize,
    /// How many of those neighbours are deleted between the two listings.
    deleted: usize,
    /// How many further templates are created between the two listings.
    added: usize,
}

/// The population table: a domain holding the subject alone, and domains whose
/// population shrinks, grows, and does both between the listings.
fn population_rows() -> Vec<PopulationRow> {
    vec![
        PopulationRow {
            case: "the only template in its domain",
            neighbours: 0,
            deleted: 0,
            added: 0,
        },
        PopulationRow {
            case: "one neighbour, deleted between the listings",
            neighbours: 1,
            deleted: 1,
            added: 0,
        },
        PopulationRow {
            case: "one neighbour, another template created between the listings",
            neighbours: 1,
            deleted: 0,
            added: 1,
        },
        PopulationRow {
            case: "several neighbours, some deleted and some created",
            neighbours: 5,
            deleted: 2,
            added: 3,
        },
    ]
}

/// Every element of an unfiltered listing is self-describing: it reports ONE
/// identity under both names the service reports a template by, an ARN that
/// addresses the template that identity names, and a tag-propagation list.
///
/// Hands back the identities in the order they were reported, so a caller can
/// check a listing reports each of the domain's templates exactly once.
fn self_describing_identities(case: &str, domain_id: &str, answer: &Value) -> Vec<String> {
    let summaries = answer
        .get("templates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a listing must carry templates, got {answer:?}"));
    let mut identities = Vec::new();
    for summary in summaries {
        let id = string_at(summary, "id", case);
        assert_eq!(
            id,
            string_at(summary, "templateId", case),
            "case {case:?}: an element's id and templateId are one identity, got {summary:?}"
        );
        let arn = string_at(summary, "arn", case);
        assert_eq!(
            arn,
            string_at(summary, "templateArn", case),
            "case {case:?}: an element's arn and templateArn are one identity, got {summary:?}"
        );
        // The ARN read the way a client holding only it reads it: it must
        // address the template the element's id names, in the domain the
        // listing was made against.
        assert_eq!(
            identity_in(case, &arn),
            (domain_id.to_string(), id.clone()),
            "case {case:?}: an element's ARN must address the template its id names, in the \
             domain that listed it, got {arn}"
        );
        // Present whatever the template holds, so the caller never has to tell
        // an empty list from an absent member.
        tag_propagation_configurations(case, "a summary", summary);
        identities.push(id);
    }
    identities
}

/// A summary is the projection of its OWN stored record, so what else the store
/// holds cannot change it: templates created and deleted beside it leave every
/// member of its element — the identity under both names it is reported by, the
/// name, the status and the tag-propagation list, empty rather than absent —
/// exactly as it was, every element of the listing stays self-describing and
/// distinct, the single-template read agrees with the element on the same
/// record, and the identity an element reports is scoped to the domain that
/// holds the template: another domain holding a template of the same name
/// neither shares that identity nor resolves it.
#[tokio::test]
async fn a_summary_reports_its_own_stored_template_whatever_else_the_store_holds() {
    // The region the caller signs for, which the ARN each element reports is
    // scoped to: named once, so the emulator and the expected ARNs cannot drift
    // apart.
    const REGION: &str = "eu-west-1";

    let emulator = Emulator::in_region(REGION);
    let populations = population_rows();
    let subjects = rows();

    // The FULL cross product of the two tables: every row of the table at the
    // top of this suite — each tag-propagation shape, each `TemplateStatus`,
    // each resource type — in every population shape, so which population a
    // shape of template is projected under does not depend on the order either
    // table happens to be written in.
    let pairs: Vec<(usize, &Row, &PopulationRow)> = subjects
        .iter()
        .enumerate()
        .flat_map(|(subject_index, subject)| {
            populations
                .iter()
                .map(move |population| (subject_index, subject, population))
        })
        .collect();

    for (index, (subject_index, subject, population)) in pairs.into_iter().enumerate() {
        // The shape the neighbours and the later arrivals are created with: the
        // next row of the subject table, so the templates around the subject are
        // not of its shape — a population of one shape only could not show that
        // the subject's element is its own record's.
        let neighbour_shape = &subjects[(subject_index + 1) % subjects.len()];
        let case = format!("{} / {}", population.case, subject.case);
        let domain_id = domain(&emulator, &format!("population-domain-{index:03}")).await;
        let other_domain_id =
            domain(&emulator, &format!("population-other-domain-{index:03}")).await;
        let name = format!("population-subject-{index:03}");

        let (template_id, template_arn) =
            create(&emulator, &case, &domain_id, &name, subject).await;
        // The same name and the same shape in ANOTHER domain: a template is
        // identified by its own record, so the two cannot share an identity.
        let (decoy_id, decoy_arn) =
            create(&emulator, &case, &other_domain_id, &name, subject).await;
        assert_ne!(
            decoy_id, template_id,
            "case {case:?}: two templates of the same name in different domains are two \
             templates, so they cannot share an id, got {decoy_id}"
        );
        assert_ne!(
            decoy_arn, template_arn,
            "case {case:?}: an ARN addresses the domain its template lives in, so the two cannot \
             share one, got {decoy_arn}"
        );

        let mut neighbours: Vec<String> = Vec::new();
        for neighbour in 0..population.neighbours {
            let neighbour_case = format!("{case}: neighbour {neighbour:03}");
            let (neighbour_id, _) = create(
                &emulator,
                &neighbour_case,
                &domain_id,
                &format!("population-neighbour-{index:03}-{neighbour:03}"),
                neighbour_shape,
            )
            .await;
            neighbours.push(neighbour_id);
        }

        let stored = Stored {
            region: REGION,
            domain_id: &domain_id,
            template_id: &template_id,
            name: &name,
            status: subject.status.unwrap_or("Active"),
            tag_propagation_configurations: &subject.tag_propagation_configurations,
        };

        let before = list(&emulator, &case, &domain_id, "").await;
        let listed = self_describing_identities(&case, &domain_id, &before);
        assert_eq!(
            listed.len(),
            population.neighbours + 1,
            "case {case:?}: the listing reports the domain's own templates, so it must report \
             {} of them, got {listed:?}",
            population.neighbours + 1
        );
        let distinct: BTreeSet<&String> = listed.iter().collect();
        assert_eq!(
            distinct.len(),
            listed.len(),
            "case {case:?}: each template of the domain is one element, so no identity may be \
             reported twice, got {listed:?}"
        );
        let summary = summary_of(&case, &before, &template_id);
        assert_summary(&case, &summary, &stored);

        // The population changes around the subject, which touches no record of
        // its own.
        for neighbour in neighbours.iter().take(population.deleted) {
            let (status, body) = emulator
                .send(
                    "DELETE",
                    &format!("/domains/{domain_id}/templates/{neighbour}"),
                    Some(json!({})),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: deleting neighbour {neighbour} must succeed, got {status} with \
                 {body:?}"
            );
        }
        for added in 0..population.added {
            let added_case = format!("{case}: added {added:03}");
            create(
                &emulator,
                &added_case,
                &domain_id,
                &format!("population-added-{index:03}-{added:03}"),
                neighbour_shape,
            )
            .await;
        }

        let after = list(&emulator, &case, &domain_id, "").await;
        let relisted = self_describing_identities(&case, &domain_id, &after);
        let expected_count =
            population.neighbours.saturating_sub(population.deleted) + population.added + 1;
        assert_eq!(
            relisted.len(),
            expected_count,
            "case {case:?}: after the population changed the listing must report \
             {expected_count} template(s), got {relisted:?}"
        );
        let again = summary_of(&case, &after, &template_id);
        assert_eq!(
            again, summary,
            "case {case:?}: an element is the projection of its own stored record, so templates \
             created and deleted beside it cannot change it: expected {summary:?}, got {again:?}"
        );
        assert_summary(&case, &again, &stored);

        // The single-template read resolves the identity the element reported
        // — read off the element, not from the create — and agrees with it on
        // the same stored record.
        let reported_id = string_at(&again, "id", &case);
        let record = get_template(&emulator, &case, &domain_id, &reported_id).await;
        assert_eq!(
            string_at(&record, "templateArn", &case),
            string_at(&again, "arn", &case),
            "case {case:?}: the read and the element report one ARN for one template, got \
             {record:?}"
        );
        assert_eq!(
            string_at(&record, "name", &case),
            name,
            "case {case:?}: the element's identity must reach the template it listed, got \
             {record:?}"
        );
        assert_eq!(
            tag_propagation_configurations(&case, "the single-template read", &record),
            tag_propagation_configurations(&case, "a summary", &again),
            "case {case:?}: the read and the element report one tag-propagation list for one \
             template, got {record:?}"
        );

        // And that identity is the subject's alone: the other domain holds a
        // template of the same name and does not resolve it.
        let (status, code) = emulator
            .failure(
                &case,
                "POST",
                &format!("/domains/{other_domain_id}/templates/{reported_id}"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            (status, code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: the identity an element reports names a template of the domain that \
             listed it, so another domain must refuse it with 404 ResourceNotFoundException, got \
             {status} {code}"
        );
    }
}

/// One template a listing has to report, as the create that stored it reported
/// it: the identity it minted, and the members the summary must project from the
/// stored record.
struct Created {
    template_id: String,
    arn: String,
    name: String,
    status: String,
    tag_propagation_configurations: Vec<Value>,
}

/// Create the templates of `rows` in `domain_id`, named off `prefix`.
async fn create_each(
    emulator: &Emulator,
    domain_id: &str,
    prefix: &str,
    rows: &[Row],
) -> Vec<Created> {
    let mut created = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("{prefix}-{index:03}");
        let case = format!("create {name}");
        let (template_id, arn) = create(emulator, &case, domain_id, &name, row).await;
        created.push(Created {
            template_id,
            arn,
            name,
            status: row.status.unwrap_or(TEMPLATE_ACTIVE).to_string(),
            tag_propagation_configurations: row.tag_propagation_configurations.clone(),
        });
    }
    created
}

/// Assert one gathered listing reports EXACTLY `created`, each element the full
/// projection of the template it was created as — and that the identity the
/// element reported is the one the create minted.
fn assert_listing(
    case: &str,
    region: &str,
    domain_id: &str,
    gathered: &[Value],
    created: &[Created],
) {
    assert_eq!(
        gathered.len(),
        created.len(),
        "case {case:?}: the listing must report the {} template(s) the domain holds, got \
         {gathered:?}",
        created.len()
    );
    for template in created {
        let element = format!("{case}, template {}", template.name);
        let summary = summary_among(&element, gathered, &template.template_id);
        assert_summary(
            &element,
            &summary,
            &Stored {
                region,
                domain_id,
                template_id: &template.template_id,
                name: &template.name,
                status: &template.status,
                tag_propagation_configurations: &template.tag_propagation_configurations,
            },
        );
        // The create and the listing report ONE identity for one template: a
        // summary that minted an ARN of its own would differ here.
        for member in ["arn", "templateArn"] {
            let reported = string_at(&summary, member, &element);
            assert_eq!(
                reported, template.arn,
                "case {element:?}: the summary's {member} must be the ARN the create reported \
                 ({}), got {reported}",
                template.arn
            );
        }
    }
    // Nothing else: an element the domain does not hold would be reported to a
    // client as one of its templates.
    let expected: BTreeSet<&str> = created
        .iter()
        .map(|template| template.template_id.as_str())
        .collect();
    let reported: BTreeSet<String> = gathered
        .iter()
        .map(|summary| string_at(summary, "id", case))
        .collect();
    for id in &reported {
        assert!(
            expected.contains(id.as_str()),
            "case {case:?}: the listing may report only the domain's own templates \
             ({expected:?}), got {reported:?}"
        );
    }
}

/// Assert `GetTemplate` for `template_id` in `domain_id` is the modelled 404,
/// which is how a domain that does not hold a template answers for it. The
/// status and `__type` assertion is the shared harness one.
async fn template_not_found(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    template_id: &str,
) {
    let path = format!("/domains/{domain_id}/templates/{template_id}");
    let body = refused_as_not_found(emulator, case, "POST", &path, Some(json!({}))).await;
    assert_eq!(
        body.get("resourceId").and_then(Value::as_str),
        Some(template_id),
        "case {case:?}: the refusal must report the template {template_id} that {domain_id} does \
         not hold, got {body:?}"
    );
}

/// One emulator holds MANY domains, so the identity a page element reports has
/// to be the listed template's own: a summary's ARN is scoped to the domain that
/// holds the template, the listing reports that domain's templates and no
/// other's, and the identity alone reaches the template there — and nowhere
/// else.
///
/// The rows span how many templates a domain holds (one, a few, and the rest of
/// this suite's table, so each domain's elements differ in tag-propagation shape
/// and status) crossed with which domain of the one emulator holds them. A
/// single-domain listing cannot tell a per-template ARN from a per-domain one;
/// these rows can.
#[tokio::test]
async fn a_listing_projects_the_identity_of_the_domains_own_templates() {
    // The region the caller signs for, which the ARN of every template below is
    // scoped to.
    const REGION: &str = "us-west-2";

    let emulator = Emulator::in_region(REGION);
    let table = rows();
    // One domain per slice of the table: 1 template, 2, then the rest.
    let slices: [(usize, usize); 3] = [(0, 1), (1, 3), (3, table.len())];

    let mut domains: Vec<(String, Vec<Created>)> = Vec::new();
    for (index, (from, to)) in slices.iter().enumerate() {
        let domain_id = domain(&emulator, &format!("own-identity-domain-{index}")).await;
        let created = create_each(
            &emulator,
            &domain_id,
            &format!("own-identity-template-{index}"),
            &table[*from..*to],
        )
        .await;
        domains.push((domain_id, created));
    }

    for (index, (domain_id, created)) in domains.iter().enumerate() {
        // Gathered one entry per page, so the projection is read on each page a
        // client would have to follow.
        let case = format!("domain {index} ({} template(s))", created.len());
        let gathered =
            summaries_page_by_page(&emulator, &case, domain_id, Some(1), created.len()).await;
        assert_listing(&case, REGION, domain_id, &gathered, created);

        // List-then-use across the emulator's domains: the identity a summary
        // reported reaches the template in the domain the summary's ARN
        // addresses, and that domain only.
        for template in created {
            let element = format!("{case}, template {}", template.name);
            let summary = summary_among(&element, &gathered, &template.template_id);
            let arn = string_at(&summary, "arn", &element);
            let (addressed_domain, addressed_template) = identity_in(&element, &arn);
            assert_eq!(
                addressed_domain, *domain_id,
                "case {element:?}: a summary's arn must address the domain that holds the \
                 template ({domain_id}), got {arn}"
            );
            let record =
                get_template(&emulator, &element, &addressed_domain, &addressed_template).await;
            assert_eq!(
                string_at(&record, "name", &element),
                template.name,
                "case {element:?}: the identity the summary reported must reach the template it \
                 listed, got {record:?}"
            );
            for (other, _) in domains.iter().filter(|(other, _)| other != domain_id) {
                template_not_found(&emulator, &element, other, &addressed_template).await;
            }
        }
    }
}

/// Which element of the stored order a change takes out of the listing.
#[derive(Debug, Clone, Copy)]
enum Removed {
    /// Nothing is deleted: the listing of the untouched set.
    Nothing,
    First,
    Middle,
    Last,
}

/// A listing reports the templates the domain holds NOW: a template deleted
/// since the last listing is gone from the page, one created since is on it, and
/// every element that remains is still the full projection of its stored
/// template — identity under both names it is reported by, and the
/// tag-propagation list present whatever it holds.
///
/// The rows are the set changes a listing has to follow: nothing deleted, the
/// first element deleted, one in the middle deleted, the last deleted, and after
/// each of them a template created — so no element's projection depends on its
/// position in the stored order or on the set having been left untouched.
#[tokio::test]
async fn a_listing_projects_every_template_that_remains_after_the_set_changes() {
    // The region the caller signs for, which the ARN of every template below is
    // scoped to.
    const REGION: &str = "eu-west-1";

    let emulator = Emulator::in_region(REGION);
    let domain_id = domain(&emulator, "changing-set-domain").await;
    let table = rows();
    let mut remaining = create_each(&emulator, &domain_id, "changing-set-template", &table).await;
    // How many templates were ever created here, which is what the pages a
    // traversal may hand out is bounded by, and what names an appended template
    // apart from every earlier one.
    let mut minted = remaining.len();

    for change in [
        Removed::Nothing,
        Removed::First,
        Removed::Middle,
        Removed::Last,
    ] {
        let case = format!("deleted: {change:?}");
        let index = match change {
            Removed::Nothing => None,
            Removed::First => Some(0),
            Removed::Middle => Some(remaining.len() / 2),
            Removed::Last => Some(remaining.len() - 1),
        };
        if let Some(index) = index {
            let template = remaining.remove(index);
            emulator
                .ok(
                    &case,
                    "DELETE",
                    &format!("/domains/{domain_id}/templates/{}", template.template_id),
                    json!({}),
                )
                .await;
            // The deleted template leaves the listing; the ones beside it stay.
            let gathered = summaries_page_by_page(&emulator, &case, &domain_id, None, minted).await;
            assert!(
                gathered.iter().all(|summary| {
                    summary.get("id").and_then(Value::as_str) != Some(&template.template_id)
                }),
                "case {case:?}: a deleted template must leave the listing, got {gathered:?}"
            );
        }

        // What remains, each element in full, after the change.
        let gathered = summaries_page_by_page(&emulator, &case, &domain_id, None, minted).await;
        assert_listing(&case, REGION, &domain_id, &gathered, &remaining);

        // And a template created after the change is projected the same way, on
        // whatever page it lands.
        let appended = format!("{case}, then a template created");
        let row = &table[minted % table.len()];
        let mut created = create_each(
            &emulator,
            &domain_id,
            &format!("appended-template-{minted:03}"),
            std::slice::from_ref(row),
        )
        .await;
        minted += created.len();
        remaining.append(&mut created);
        let gathered =
            summaries_page_by_page(&emulator, &appended, &domain_id, Some(2), minted).await;
        assert_listing(&appended, REGION, &domain_id, &gathered, &remaining);
    }
}

/// The entry a listing reports for the template named `name`.
///
/// The name is the ONLY thing the walk below knows about a template before it
/// reads its entry: every other value it drives a call with comes out of the
/// entry itself.
fn summary_named(case: &str, answer: &Value, name: &str) -> Value {
    summary_where(case, answer, "name", name)
}

/// Every identity a listing entry reports, as the ids of the templates it
/// listed.
fn listed_identities(case: &str, answer: &Value) -> Vec<String> {
    answer
        .get("templates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a listing must carry templates, got {answer:?}"))
        .iter()
        .map(|summary| string_at(summary, "id", case))
        .collect()
}

/// What the projection is FOR: an entry a listing reports is SELF-DESCRIBING,
/// so the identity it carries — the `id` and the `arn`, under the names the
/// entry reports them by — is on its own enough to consume that resource
/// through every operation that takes its identity, with no second lookup to
/// resolve the entry first.
///
/// One walk per row drives the whole list-then-use chain a client performs with
/// nothing but the entry it read: the template read back through the listed id,
/// tagged through the listed ARN and its tag set read back through it, a case
/// created from the listed id where the template's status admits one
/// ([`TEMPLATE_ACTIVE`]), the template updated through it (and reported under
/// the same identity afterwards) and finally deleted through it — after which it
/// is gone from the listing while the single-resource read still reports the
/// record that identity addressed.
///
/// The rows are this suite's table — tag-propagation shape and `TemplateStatus`
/// — each walked in two regions, so the ARN every call above is driven through
/// is the one scoped to the caller's own region rather than a fixed string.
#[tokio::test]
async fn a_listed_template_is_consumed_through_the_identity_its_summary_reports() {
    // The status a create that names none stores, and the only one a case can be
    // created from, is one of the model's own `TemplateStatus` values.
    assert!(
        TEMPLATE_STATUS_VALUES.contains(&TEMPLATE_ACTIVE),
        "expected {TEMPLATE_ACTIVE:?} among the model's TemplateStatus values, got \
         {TEMPLATE_STATUS_VALUES:?}"
    );
    for region in ["us-east-1", "eu-west-1"] {
        let emulator = Emulator::in_region(region);
        let domain_id = domain(&emulator, "consume-domain").await;
        let field_id = text_field(&emulator, &domain_id, "consume-field").await;

        for (index, row) in rows().iter().enumerate() {
            let name = format!("consume-template-{index:03}");
            let case = format!("{} in {region}", row.case);
            let case = case.as_str();
            let status = row.status.unwrap_or(TEMPLATE_ACTIVE);
            let (minted_id, _) = create(&emulator, case, &domain_id, &name, row).await;

            // The entry, located by the name the create was given — the only
            // thing this walk knows before it reads one.
            let answer = list(&emulator, case, &domain_id, "").await;
            let summary = summary_named(case, &answer, &name);
            let listed_id = string_at(&summary, "id", case);
            let listed_arn = string_at(&summary, "arn", case);
            assert_summary(
                case,
                &summary,
                &Stored {
                    region,
                    domain_id: &domain_id,
                    template_id: &listed_id,
                    name: &name,
                    status,
                    tag_propagation_configurations: &row.tag_propagation_configurations,
                },
            );
            assert_eq!(
                listed_id, minted_id,
                "case {case:?}: the entry must report the identity the create minted \
                 ({minted_id}), got {listed_id}"
            );

            // Read it back through the listed id.
            let record = get_template(&emulator, case, &domain_id, &listed_id).await;
            assert_eq!(
                string_at(&record, "name", case),
                name,
                "case {case:?}: the id the entry reported must reach the template it listed, got \
                 {record:?}"
            );

            // Tag it through the listed ARN, and read that tag set back through
            // the same ARN and through the template itself: an ARN that
            // addressed another record — or none — would fail here.
            let tags = json!({ "owner": format!("row-{index}") });
            tag_resource(&emulator, case, &listed_arn, &tags).await;
            let reported = listed_tags(&emulator, case, &listed_arn).await;
            assert_eq!(
                reported, tags,
                "case {case:?}: the ARN the entry reported must address the tag set the write \
                 stored ({tags:?}), got {reported:?}"
            );
            let tagged = get_template(&emulator, case, &domain_id, &listed_id).await;
            assert_eq!(
                tagged.get("tags"),
                Some(&tags),
                "case {case:?}: the ARN the entry reported must address the template its id \
                 names, got {tagged:?}"
            );

            // Create a case from the listed id: the case is bound to that
            // template. Only an ACTIVE template can be used for this: "A
            // template can be either Active or Inactive, as indicated by its
            // status. Inactive templates cannot be used to create cases"
            // (`smithy_query(query="doc", shape="CreateTemplate")`), so a row
            // whose stored template is Inactive consumes its identity through
            // the other operations only.
            if status == TEMPLATE_ACTIVE {
                let case_id =
                    create_case(&emulator, case, &domain_id, &listed_id, &field_id).await;
                let of_template =
                    case_template(&emulator, case, &domain_id, &case_id, &field_id).await;
                assert_eq!(
                    of_template, listed_id,
                    "case {case:?}: a case created from the listed id must report it, got \
                     {of_template}"
                );
            }

            // Update through the listed id: the next listing reports the stored
            // template under the SAME identity, re-minting neither half of it.
            let renamed = format!("{name}-renamed");
            emulator
                .ok(
                    case,
                    "PUT",
                    &format!("/domains/{domain_id}/templates/{listed_id}"),
                    json!({ "name": renamed }),
                )
                .await;
            let answer = list(&emulator, case, &domain_id, "").await;
            let after = summary_named(case, &answer, &renamed);
            assert_summary(
                case,
                &after,
                &Stored {
                    region,
                    domain_id: &domain_id,
                    template_id: &listed_id,
                    name: &renamed,
                    status,
                    tag_propagation_configurations: &row.tag_propagation_configurations,
                },
            );
            assert_eq!(
                string_at(&after, "arn", case),
                listed_arn,
                "case {case:?}: an update must leave the identity the entry reported alone, got \
                 {after:?}"
            );

            // Delete through the listed id: the template drops out of the
            // listing, while the single-resource read still reports the record
            // that identity addressed.
            emulator
                .ok(
                    case,
                    "DELETE",
                    &format!("/domains/{domain_id}/templates/{listed_id}"),
                    json!({}),
                )
                .await;
            let answer = list(&emulator, case, &domain_id, "").await;
            let remaining = listed_identities(case, &answer);
            assert!(
                !remaining.contains(&listed_id),
                "case {case:?}: a template deleted through the listed id must drop out of the \
                 listing, got {remaining:?}"
            );
            let deleted = get_template(&emulator, case, &domain_id, &listed_id).await;
            assert_eq!(
                deleted.get("deleted"),
                Some(&json!(true)),
                "case {case:?}: the deleted template must still be the record the listed id \
                 addresses, got {deleted:?}"
            );
            assert_eq!(
                string_at(&deleted, "templateId", case),
                listed_id,
                "case {case:?}: a delete must not re-mint the identity the entry reported, got \
                 {deleted:?}"
            );
        }
    }
}
