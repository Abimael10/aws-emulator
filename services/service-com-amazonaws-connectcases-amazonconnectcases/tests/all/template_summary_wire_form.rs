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

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::ops::types::{
    TAG_PROPAGATION_RESOURCE_TYPE_VALUES, TEMPLATE_STATUS_VALUES,
};

use crate::harness::{domain, is_minted_id, string_at, Emulator};

/// The emulator's account id, as every ARN this service mints carries it.
const ACCOUNT: &str = "000000000000";

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

/// The summary of `template_id` in a listing answer.
fn summary_of(case: &str, answer: &Value, template_id: &str) -> Value {
    answer
        .get("templates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a listing must carry templates, got {answer:?}"))
        .iter()
        .find(|summary| summary.get("templateId").and_then(Value::as_str) == Some(template_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report template {template_id}, got {answer:?}")
        })
        .clone()
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
