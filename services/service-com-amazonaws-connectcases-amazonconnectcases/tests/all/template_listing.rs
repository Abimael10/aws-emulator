// SPDX-License-Identifier: CC0-1.0

//! End-to-end restJson1 tests for `ListTemplates`, driven through the real
//! `MultiServiceRouter` so every assertion goes over the wire the SDK speaks.
//!
//! The rule under test: a listing reports exactly the live resources its named
//! container holds, computed from persisted state, in one stable order, narrowed
//! by the filters the request carries, and cut into pages at `maxResults` with a
//! `nextToken` that resumes immediately after the last entry reported — so a
//! caller following tokens to exhaustion sees every matching resource exactly
//! once, and reading changes nothing a later caller sees.
//!
//! The rows span the class the rule generalises over: how many templates exist,
//! which `TemplateStatus` each carries, every subset of the status catalog as a
//! filter, every page size at and past the model's published bounds, and tokens
//! this service did and did not issue.
//!
//! The last two tests pin the OTHER half of the same rule: an entry a listing
//! reports is SELF-DESCRIBING, so the identity it carries — the resource's own
//! id and ARN, under each name the service reports them by — is on its own
//! enough to address that resource through the operations that take its
//! identity, with no second lookup. They consume each entry the way a client
//! does: the template operations through the id it reported, the tagging
//! operations through the ARN, and `CreateCase` through the id.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_connectcases_amazonconnectcases::ops::types::{
    FIELD_TYPE_VALUES, TAG_PROPAGATION_RESOURCE_TYPE_VALUES, TEMPLATE_STATUS_VALUES,
};
use service_com_amazonaws_connectcases_amazonconnectcases::state::MAX_PAGE_SIZE;

// The crate's shared test harness: one emulator behind the production router,
// and the domain-creating helper every suite in this target uses.
use crate::harness::{domain, is_minted_id, string_at, tags_path, Emulator};

/// Create a template in `domain_id`, optionally with an explicit `status`, and
/// hand back its id.
async fn template(
    emulator: &Emulator,
    domain_id: &str,
    name: &str,
    status: Option<&str>,
) -> String {
    let mut body = json!({ "name": name });
    if let Some(status) = status {
        body["status"] = json!(status);
    }
    let created = emulator
        .ok(name, "POST", &format!("/domains/{domain_id}/templates"), body)
        .await;
    string_at(&created, "templateId", name)
}

/// `ListTemplates` for `domain_id` with `query` appended verbatim.
pub(crate) async fn list(emulator: &Emulator, case: &str, domain_id: &str, query: &str) -> Value {
    emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/templates-list{query}"),
            json!({}),
        )
        .await
}

/// `ListTemplates` that must be refused, as its status and wire error code.
async fn refused(emulator: &Emulator, case: &str, domain_id: &str, query: &str) -> (u16, String) {
    emulator
        .failure(
            case,
            "POST",
            &format!("/domains/{domain_id}/templates-list{query}"),
            Some(json!({})),
        )
        .await
}

/// `GetTemplate`, so a listed entry can be checked against the record a reader
/// of the single resource sees.
async fn get_template(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    template_id: &str,
) -> Value {
    emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/templates/{template_id}"),
            json!({}),
        )
        .await
}

/// The `templates` member of a listing answer, as the ids it reports in order.
pub(crate) fn listed_ids(case: &str, answer: &Value) -> Vec<String> {
    answer
        .get("templates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a listing must carry templates: {answer:?}"))
        .iter()
        .map(|summary| string_at(summary, "templateId", case))
        .collect()
}

/// The `nextToken` a listing answer carries, if any.
fn next_token(answer: &Value) -> Option<String> {
    answer
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `status` query string for `statuses`: `status` is an `httpQuery` list, so
/// each value is its own parameter.
pub(crate) fn status_query(statuses: &[&str]) -> String {
    if statuses.is_empty() {
        String::new()
    } else {
        let joined: Vec<String> = statuses
            .iter()
            .map(|status| format!("status={status}"))
            .collect();
        format!("?{}", joined.join("&"))
    }
}

/// `count` templates in `domain_id`, each carrying the status of its row: the
/// catalog is cycled so every `TemplateStatus` the model declares is
/// represented, and every third template is created with no `status` at all —
/// which `CreateTemplate` stores as `Active`. Returns `(templateId, status)` in
/// creation order.
async fn templates_of_every_status(
    emulator: &Emulator,
    domain_id: &str,
    count: usize,
) -> Vec<(String, String)> {
    let mut created = Vec::new();
    for index in 0..count {
        let (requested, stored) = if index % 3 == 2 {
            // No `status` member: the default a template takes when it is not
            // given one.
            (None, "Active")
        } else {
            let status = TEMPLATE_STATUS_VALUES[index % TEMPLATE_STATUS_VALUES.len()];
            (Some(status), status)
        };
        let name = format!("template-{index:03}");
        let id = template(emulator, domain_id, &name, requested).await;
        created.push((id, stored.to_string()));
    }
    created
}

#[tokio::test]
async fn a_listing_reports_every_live_template_of_its_domain_in_a_stable_order() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "listing-domain").await;

    // An empty container reports an empty page and owes no token.
    let empty = list(&emulator, "no templates yet", &domain_id, "").await;
    assert!(
        listed_ids("no templates yet", &empty).is_empty(),
        "case \"no templates yet\": a domain holding no template must report none, got {empty:?}"
    );
    assert_eq!(
        next_token(&empty),
        None,
        "case \"no templates yet\": nothing remains, so no nextToken is owed, got {empty:?}"
    );

    let created = templates_of_every_status(&emulator, &domain_id, 7).await;
    let expected: Vec<String> = created.iter().map(|(id, _)| id.clone()).collect();

    let answer = list(&emulator, "every template", &domain_id, "").await;
    let listed = listed_ids("every template", &answer);
    assert_eq!(
        listed, expected,
        "case \"every template\": the listing must report every stored template in creation \
         order, got {listed:?}"
    );
    assert_eq!(
        next_token(&answer),
        None,
        "case \"every template\": one page holds them all, so no nextToken is owed, got {answer:?}"
    );

    // A read changes nothing a later caller sees: the same call answers the same
    // listing, in the same order.
    let again = list(&emulator, "every template, again", &domain_id, "").await;
    assert_eq!(
        listed_ids("every template, again", &again),
        listed,
        "case \"every template, again\": a listing is a read, so a second call must answer the \
         same order, got {again:?}"
    );

    // Each summary is computed from the stored record: its members are the ones
    // `GetTemplate` reports for the same template.
    let summaries = answer["templates"]
        .as_array()
        .expect("the listing carries templates")
        .clone();
    for (summary, (id, stored_status)) in summaries.iter().zip(created.iter()) {
        let case = format!("summary of {id}");
        let record = get_template(&emulator, &case, &domain_id, id).await;
        for member in ["templateId", "templateArn", "name", "status"] {
            let listed = string_at(summary, member, &case);
            let read = string_at(&record, member, &case);
            assert_eq!(
                listed, read,
                "case {case:?}: the summary's {member} must be the stored one GetTemplate \
                 reports ({read}), got {listed}"
            );
        }
        assert_eq!(
            string_at(summary, "status", &case),
            *stored_status,
            "case {case:?}: the summary must report the status the template was stored with"
        );
    }
}

#[tokio::test]
async fn a_status_filter_narrows_the_listing_to_the_statuses_it_names() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "filter-domain").await;
    let created = templates_of_every_status(&emulator, &domain_id, 8).await;

    // Every subset of the status catalog the model's `@length(1..2)` filter
    // admits, plus the absent filter. `TEMPLATE_STATUS_VALUES` is the model's own
    // `TemplateStatus` enum (smithy_query(query="shape", shape="TemplateStatus")).
    let mut filters: Vec<Vec<&str>> = vec![vec![]];
    for status in TEMPLATE_STATUS_VALUES {
        filters.push(vec![*status]);
    }
    filters.push(TEMPLATE_STATUS_VALUES.to_vec());
    // The same two values in the other order must select the same templates.
    let mut reversed = TEMPLATE_STATUS_VALUES.to_vec();
    reversed.reverse();
    filters.push(reversed);

    for filter in filters {
        let case = format!("status={filter:?}");
        let expected: Vec<String> = created
            .iter()
            .filter(|(_, status)| filter.is_empty() || filter.contains(&status.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        let answer = list(&emulator, &case, &domain_id, &status_query(&filter)).await;
        let listed = listed_ids(&case, &answer);
        assert_eq!(
            listed, expected,
            "case {case:?}: the listing must report exactly the templates whose stored status \
             the filter names, got {listed:?}"
        );
    }

    // A value that is not a `TemplateStatus` cannot match a template, so it is a
    // bad request rather than an empty page.
    for unknown in ["Draft", "active", "ACTIVE", "Deleted"] {
        let case = format!("status={unknown}");
        let (status, code) =
            refused(&emulator, &case, &domain_id, &format!("?status={unknown}")).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a status that is not a TemplateStatus must be refused with a 400 \
             ValidationException, got {status} {code}"
        );
    }
}

#[tokio::test]
async fn a_page_is_cut_at_max_results_and_its_token_resumes_exactly_once() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "paging-domain").await;
    let created = templates_of_every_status(&emulator, &domain_id, 12).await;
    let expected: Vec<String> = created.iter().map(|(id, _)| id.clone()).collect();

    // Page sizes spanning the model's published `maxResults` range (1..100,
    // smithy_query(query="operation", operation="ListTemplates")): both bounds,
    // sizes inside it that do and do not divide the listing, and one that holds
    // everything.
    for page_size in [1, 2, 5, 7, 12, 13, MAX_PAGE_SIZE] {
        let case = format!("maxResults={page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                Some(value) => format!("?maxResults={page_size}&nextToken={value}"),
                None => format!("?maxResults={page_size}"),
            };
            let answer = list(&emulator, &case, &domain_id, &query).await;
            let page = listed_ids(&case, &answer);
            pages += 1;
            assert!(
                page.len() <= page_size as usize,
                "case {case:?} page {pages}: a page must hold at most maxResults entries, got {}",
                page.len()
            );
            token = next_token(&answer);
            if token.is_some() {
                assert_eq!(
                    page.len(),
                    page_size as usize,
                    "case {case:?} page {pages}: a nextToken may only arrive with a full page, \
                     got {} entries",
                    page.len()
                );
            }
            seen.extend(page);
            if token.is_none() {
                break;
            }
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: following nextToken must terminate, but page {pages} still \
                 carried one after {} entries",
                seen.len()
            );
        }
        assert_eq!(
            seen, expected,
            "case {case:?}: following nextToken to exhaustion must report every template \
             exactly once, in one order, got {seen:?}"
        );
        let distinct: BTreeSet<&String> = seen.iter().collect();
        assert_eq!(
            distinct.len(),
            seen.len(),
            "case {case:?}: no template may be reported twice across pages, got {seen:?}"
        );
    }

    // A page size outside the published range is refused rather than clamped
    // silently: one past each bound.
    for bad in [0, -1, MAX_PAGE_SIZE + 1] {
        let case = format!("maxResults={bad}");
        let (status, code) =
            refused(&emulator, &case, &domain_id, &format!("?maxResults={bad}")).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a maxResults outside 1..{MAX_PAGE_SIZE} must be refused with a 400 \
             ValidationException, got {status} {code}"
        );
    }

    // A token this service never issued is refused, rather than resumed from or
    // silently read as the start of the listing.
    let issued = next_token(&list(&emulator, "first page", &domain_id, "?maxResults=1").await)
        .expect("a page smaller than the listing carries a nextToken");
    for forged in [
        "not-a-token".to_string(),
        // Valid hex, but not a token this service minted.
        "68656c6c6f".to_string(),
        // The issued token with its first hex digit pair dropped: its version
        // marker no longer reads as this service's.
        issued[2..].to_string(),
        // The issued token one character short, so it is not even a byte string.
        issued[..issued.len() - 1].to_string(),
    ] {
        let case = format!("nextToken={forged}");
        let (status, code) =
            refused(&emulator, &case, &domain_id, &format!("?nextToken={forged}")).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a nextToken this service did not issue must be refused with a 400 \
             ValidationException, got {status} {code}"
        );
    }

    // An empty `nextToken` is a value the model's @length(0..9000) admits and
    // means the start of the listing.
    let from_empty = list(&emulator, "empty nextToken", &domain_id, "?nextToken=").await;
    assert_eq!(
        listed_ids("empty nextToken", &from_empty),
        expected,
        "case \"empty nextToken\": an empty token means the start of the listing, got \
         {from_empty:?}"
    );

    // A token belongs to the listing that issued it: presented to a listing
    // narrowed differently, it is refused rather than resumed from, because the
    // entry it names need not even be in that listing.
    let (status, code) = refused(
        &emulator,
        "a token issued for the unfiltered listing, presented with a filter",
        &domain_id,
        &format!("?status=Inactive&maxResults=1&nextToken={issued}"),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case \"a token issued for the unfiltered listing, presented with a filter\": a token \
         from another listing must be refused with a 400 ValidationException, got {status} {code}"
    );

    // And a token belongs to the container it was issued for: a second domain's
    // listing does not accept the first domain's cursor.
    let other_domain = domain(&emulator, "paging-domain-two").await;
    templates_of_every_status(&emulator, &other_domain, 4).await;
    let (status, code) = refused(
        &emulator,
        "a token issued for another domain",
        &other_domain,
        &format!("?maxResults=1&nextToken={issued}"),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case \"a token issued for another domain\": a token from another container must be \
         refused with a 400 ValidationException, got {status} {code}"
    );

    // The status filter and the page cut compose: paging a filtered listing
    // reports exactly the filtered templates.
    let filtered_expected: Vec<String> = created
        .iter()
        .filter(|(_, status)| status == "Inactive")
        .map(|(id, _)| id.clone())
        .collect();
    let mut filtered_seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let case = "filtered paging".to_string();
        let query = match &token {
            Some(value) => format!("?status=Inactive&maxResults=1&nextToken={value}"),
            None => "?status=Inactive&maxResults=1".to_string(),
        };
        let answer = list(&emulator, &case, &domain_id, &query).await;
        filtered_seen.extend(listed_ids(&case, &answer));
        token = next_token(&answer);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        filtered_seen, filtered_expected,
        "case \"filtered paging\": paging a filtered listing must report exactly the templates \
         the filter selects, got {filtered_seen:?}"
    );
}

#[tokio::test]
async fn a_listing_is_scoped_to_the_domain_it_names_and_refuses_one_that_does_not_exist() {
    let emulator = Emulator::new();
    let first = domain(&emulator, "first-domain").await;
    let second = domain(&emulator, "second-domain").await;

    let in_first = templates_of_every_status(&emulator, &first, 3).await;
    let in_second = templates_of_every_status(&emulator, &second, 2).await;

    for (case, domain_id, expected) in [
        ("first domain", &first, &in_first),
        ("second domain", &second, &in_second),
    ] {
        let answer = list(&emulator, case, domain_id, "").await;
        let listed = listed_ids(case, &answer);
        let expected: Vec<String> = expected.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            listed, expected,
            "case {case:?}: a listing reports only the templates of the domain it names, got \
             {listed:?}"
        );
    }

    // A domain the store does not hold is the modelled 404, not an empty page.
    let (status, code) = refused(&emulator, "unknown domain", "no-such-domain", "").await;
    assert_eq!(
        (status, code.as_str()),
        (404, "ResourceNotFoundException"),
        "case \"unknown domain\": listing a domain that does not exist must be refused with a \
         404 ResourceNotFoundException, got {status} {code}"
    );
}

/// The emulator's account id, which every ARN this service mints carries.
const ACCOUNT: &str = "000000000000";

/// The `FieldType` the case field below carries, taken from the model's own
/// catalog (`FIELD_TYPE_VALUES`, `smithy_query(query="shape",shape="FieldType")`):
/// a `Text` field is the one a `stringValue` is written against.
fn text_field_type() -> &'static str {
    FIELD_TYPE_VALUES
        .iter()
        .copied()
        .find(|field_type| *field_type == "Text")
        .expect("the model's FieldType catalog carries Text")
}

/// One row of the list-then-use table: a template to create, and what it is
/// created with. The rows differ along what the rule generalises over — the
/// `TemplateStatus` the template carries (each value of the catalog, and the one
/// a create that names none stores), whether the template requires a field of
/// its domain, and whether it propagates tags — none of which changes what an
/// entry must carry or what it must be enough to address.
struct Consumed {
    case: String,
    /// The `status` the create names, or `None` to leave it out — which stores
    /// the template as `Active`.
    status: Option<&'static str>,
    requires_a_field: bool,
    propagates_tags: bool,
}

impl Consumed {
    /// The status this row's template is STORED with: the one it named, or the
    /// `Active` a create that named none stores.
    fn stored_status(&self) -> &'static str {
        self.status.unwrap_or("Active")
    }
}

/// The table: every `TemplateStatus` a template can carry (plus the absent
/// member) crossed with whether it requires a field and whether it propagates
/// tags.
fn consumed_rows() -> Vec<Consumed> {
    let statuses: Vec<Option<&'static str>> = std::iter::once(None)
        .chain(TEMPLATE_STATUS_VALUES.iter().map(|status| Some(*status)))
        .collect();
    let mut rows = Vec::new();
    for status in statuses {
        for (requires_a_field, propagates_tags) in
            [(false, false), (true, false), (true, true), (false, true)]
        {
            rows.push(Consumed {
                case: format!(
                    "status={}, requiresAField={requires_a_field}, \
                     propagatesTags={propagates_tags}",
                    status.unwrap_or("<none>")
                ),
                status,
                requires_a_field,
                propagates_tags,
            });
        }
    }
    rows
}

/// A `Text` field of `domain_id`, as the field a template may require and a case
/// may carry a value for.
async fn text_field(emulator: &Emulator, domain_id: &str) -> String {
    let case = "a Text field";
    let created = emulator
        .ok(
            case,
            "POST",
            &format!("/domains/{domain_id}/fields"),
            json!({ "name": "listed-template-field", "type": text_field_type() }),
        )
        .await;
    string_at(&created, "fieldId", case)
}

/// Create one row's template, as the `(templateId, templateArn)` the create
/// reported — so an entry's identity is checked against what the create minted
/// rather than against this suite's format string alone.
async fn create_consumed(
    emulator: &Emulator,
    row: &Consumed,
    domain_id: &str,
    name: &str,
    field_id: &str,
) -> (String, String) {
    let mut body = json!({ "name": name });
    if let Some(status) = row.status {
        body["status"] = json!(status);
    }
    if row.requires_a_field {
        body["requiredFields"] = json!([{ "fieldId": field_id }]);
    }
    if row.propagates_tags {
        // `resourceType` is a value of the model's own
        // `TagPropagationResourceType` catalog.
        body["tagPropagationConfigurations"] = json!([{
            "resourceType": TAG_PROPAGATION_RESOURCE_TYPE_VALUES[0],
            "tagMap": { "team": "support" },
        }]);
    }
    let created = emulator
        .ok(
            &row.case,
            "POST",
            &format!("/domains/{domain_id}/templates"),
            body,
        )
        .await;
    (
        string_at(&created, "templateId", &row.case),
        string_at(&created, "templateArn", &row.case),
    )
}

/// Every summary the listing reports, gathered one entry per page so each
/// entry is read off a page of its own: an identity carried only on a first
/// page would fail the assertions below.
async fn every_summary(
    emulator: &Emulator,
    case: &str,
    domain_id: &str,
    at_most: usize,
) -> Vec<Value> {
    let mut gathered: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(
            pages <= at_most + 1,
            "case {case:?}: following nextToken must terminate, but page {pages} still carried \
             one after {} entries",
            gathered.len()
        );
        let query = match &token {
            Some(value) => format!("?maxResults=1&nextToken={value}"),
            None => "?maxResults=1".to_string(),
        };
        let answer = list(emulator, case, domain_id, &query).await;
        gathered.extend(
            answer
                .get("templates")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a listing must carry templates: {answer:?}")
                })
                .clone(),
        );
        token = next_token(&answer);
        if token.is_none() {
            return gathered;
        }
    }
}

/// The entry `name` was listed under, which must be there.
fn summary_named(case: &str, summaries: &[Value], name: &str) -> Value {
    summaries
        .iter()
        .find(|summary| summary.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report template {name:?}: {summaries:?}")
        })
        .clone()
}

/// RULE: every entry a listing reports is self-describing — it carries the
/// listed resource's own identity (its id, and the ARN composed from the
/// request's region, the emulator's account and the container and resource ids)
/// under each name the service reports it by — so the entry alone addresses that
/// resource through the operations that take its identity: the id through the
/// single-resource read, the update and the delete, and the ARN through the
/// tagging operations, which resolve a resource by its own ARN.
///
/// Every call below is made with values read OFF THE ENTRY, never with the ids
/// the creates handed back, so an entry that reported an identity nothing
/// resolves would fail here.
#[tokio::test]
async fn a_listed_template_is_addressable_through_the_identity_its_entry_reports() {
    // Two regions, because the ARN an entry reports is scoped to the region the
    // template was created in and the tagging operations resolve a resource by
    // its own ARN, region segment included.
    for region in ["us-east-1", "eu-west-1"] {
        let emulator = Emulator::in_region(region);
        let domain_id = domain(&emulator, &format!("listed-usable-domain-{region}")).await;
        let field_id = text_field(&emulator, &domain_id).await;

        let rows = consumed_rows();
        let mut created: Vec<(String, String, String)> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let name = format!("usable-template-{index:03}");
            let (template_id, template_arn) =
                create_consumed(&emulator, row, &domain_id, &name, &field_id).await;
            created.push((name, template_id, template_arn));
        }

        let summaries = every_summary(&emulator, "every template", &domain_id, rows.len()).await;
        let mut listed_identities: Vec<(String, String)> = Vec::new();
        for (row, (name, created_id, created_arn)) in rows.iter().zip(created.iter()) {
            let case = format!("{} in {region}", row.case);
            let summary = summary_named(&case, &summaries, name);

            // The identity, under each name the service reports it by.
            let id = string_at(&summary, "id", &case);
            let arn = string_at(&summary, "arn", &case);
            assert_eq!(
                id,
                string_at(&summary, "templateId", &case),
                "case {case:?}: an entry's id and templateId are one identity, got {summary:?}"
            );
            assert_eq!(
                arn,
                string_at(&summary, "templateArn", &case),
                "case {case:?}: an entry's arn and templateArn are one identity, got {summary:?}"
            );
            assert!(
                is_minted_id(&id),
                "case {case:?}: the identity an entry reports is a server-minted id, got {id:?}"
            );
            assert_eq!(
                id, *created_id,
                "case {case:?}: an entry must report the identity the create minted, got {id}"
            );
            assert_eq!(
                arn,
                format!("arn:aws:cases:{region}:{ACCOUNT}:domain/{domain_id}/template/{id}"),
                "case {case:?}: an entry's ARN must address the template its id names, in the \
                 domain it was created in and the region the request named, got {arn}"
            );
            assert_eq!(
                arn, *created_arn,
                "case {case:?}: an entry must report the ARN the create reported, got {arn}"
            );

            // 1. The single-resource read, reached through the entry's id alone.
            let record = get_template(&emulator, &case, &domain_id, &id).await;
            assert_eq!(
                string_at(&record, "name", &case),
                *name,
                "case {case:?}: the entry's id must reach the template it listed, got {record:?}"
            );
            assert_eq!(
                string_at(&record, "status", &case),
                row.stored_status(),
                "case {case:?}: the template the entry's id reaches must carry the stored \
                 status, got {record:?}"
            );

            // 2. The tagging operations, which resolve a resource by its OWN ARN
            //    — so the ARN the entry reported is enough to tag the template
            //    and read the tag back.
            emulator
                .ok(
                    &case,
                    "POST",
                    &tags_path(&arn),
                    json!({ "tags": { "listedBy": "ListTemplates" } }),
                )
                .await;
            let tags = emulator.ok(&case, "GET", &tags_path(&arn), Value::Null).await;
            assert_eq!(
                tags.pointer("/tags/listedBy").and_then(Value::as_str),
                Some("ListTemplates"),
                "case {case:?}: the ARN an entry reports must address the template a tag write \
                 went to, got {tags:?}"
            );
            let tagged = get_template(&emulator, &case, &domain_id, &id).await;
            assert_eq!(
                tagged.pointer("/tags/listedBy").and_then(Value::as_str),
                Some("ListTemplates"),
                "case {case:?}: the tag written through the entry's ARN must be stored on the \
                 template its id names, got {tagged:?}"
            );

            listed_identities.push((id, arn));
        }

        // 3. The update, reached through the entry's id alone: the rename lands
        //    on the listed template, and the identity it is reported by does not
        //    move with it.
        for (index, (id, arn)) in listed_identities.iter().enumerate() {
            let case = format!("rename {id} in {region}");
            emulator
                .ok(
                    &case,
                    "PUT",
                    &format!("/domains/{domain_id}/templates/{id}"),
                    json!({ "name": format!("renamed-template-{index:03}") }),
                )
                .await;
            assert_eq!(
                arn, &format!("arn:aws:cases:{region}:{ACCOUNT}:domain/{domain_id}/template/{id}"),
                "case {case:?}: the ARN an entry reports is the template's own, so a rename \
                 cannot change it"
            );
        }
        let renamed = every_summary(&emulator, "after the renames", &domain_id, rows.len()).await;
        for (index, (id, arn)) in listed_identities.iter().enumerate() {
            let case = format!("renamed template {index:03} in {region}");
            let summary = summary_named(&case, &renamed, &format!("renamed-template-{index:03}"));
            assert_eq!(
                string_at(&summary, "id", &case),
                *id,
                "case {case:?}: the renamed template keeps the identity it was listed under, got \
                 {summary:?}"
            );
            assert_eq!(
                string_at(&summary, "arn", &case),
                *arn,
                "case {case:?}: the renamed template keeps the ARN it was listed under, got \
                 {summary:?}"
            );
        }

        // 4. The delete, reached through the entry's id alone: it lands on that
        //    template — the record reports itself deleted — and the listing stops
        //    reporting it.
        for (id, _) in &listed_identities {
            let case = format!("delete {id} in {region}");
            let (status, body) = emulator
                .send(
                    "DELETE",
                    &format!("/domains/{domain_id}/templates/{id}"),
                    None,
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the identity an entry reports must reach the delete, got \
                 {status} with {body:?}"
            );
            let record = get_template(&emulator, &case, &domain_id, id).await;
            assert_eq!(
                record.get("deleted").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: the delete must land on the template the entry's id names, got \
                 {record:?}"
            );
        }
        let after = list(&emulator, "after the deletes", &domain_id, "").await;
        let remaining = listed_ids("after the deletes", &after);
        assert!(
            remaining.is_empty(),
            "case \"after the deletes in {region}\": every template was deleted through the \
             identity its entry reported, so the listing must report none, got {remaining:?}"
        );
    }
}

/// RULE: the same self-describing entry is what a case is created from — the
/// identity a listing reports is accepted wherever the model takes a template
/// identity, so list-then-use needs no second lookup to resolve the entry.
#[tokio::test]
async fn a_case_is_created_from_the_template_identity_an_entry_reports() {
    let emulator = Emulator::new();
    let domain_id = domain(&emulator, "listed-template-case-domain").await;
    let field_id = text_field(&emulator, &domain_id).await;

    let rows = consumed_rows();
    let mut created: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("case-source-template-{index:03}");
        create_consumed(&emulator, row, &domain_id, &name, &field_id).await;
        created.push(name);
    }

    let summaries = every_summary(&emulator, "every template", &domain_id, rows.len()).await;
    for (row, name) in rows.iter().zip(created.iter()) {
        let case = format!("{}, a case from the listed entry", row.case);
        let summary = summary_named(&case, &summaries, name);
        let id = string_at(&summary, "id", &case);

        // The value satisfies the row's `requiredFields` where it has one, and is
        // a value of the field's declared type in every row.
        let opened = emulator
            .ok(
                &case,
                "POST",
                &format!("/domains/{domain_id}/cases"),
                json!({
                    "templateId": id,
                    "fields": [{ "id": field_id, "value": { "stringValue": "a customer note" } }],
                }),
            )
            .await;
        let case_id = string_at(&opened, "caseId", &case);
        assert!(
            is_minted_id(&case_id),
            "case {case:?}: the create must mint a case id, got {case_id:?}"
        );

        // The case the entry's identity opened reports that identity back.
        let record = emulator
            .ok(
                &case,
                "POST",
                &format!("/domains/{domain_id}/cases/{case_id}"),
                json!({ "fields": [{ "id": field_id }] }),
            )
            .await;
        assert_eq!(
            string_at(&record, "templateId", &case),
            id,
            "case {case:?}: the case must report the template identity it was created from, got \
             {record:?}"
        );
        assert_eq!(
            record
                .pointer("/fields/0/value/stringValue")
                .and_then(Value::as_str),
            Some("a customer note"),
            "case {case:?}: the case created from the listed template must hold the field value \
             it was written with, got {record:?}"
        );
    }
}
