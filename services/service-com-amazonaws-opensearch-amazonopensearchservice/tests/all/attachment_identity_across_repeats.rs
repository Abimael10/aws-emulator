// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IDENTITY of a data source attachment across REPEAT attaches —
//! `AttachDataSource`, `DescribeDataSourceAttachment` and `ListDataSourceAttachments` —
//! driven over the real wire path (a `restJson1` HTTP request through the multi-service
//! router into the mounted service), so what is asserted is what a client sees.
//!
//! The rule under test: an application and a data source name AT MOST ONE attachment, so a
//! repeat `AttachDataSource` for a pair the application already holds an attachment of is
//! answered with that one record — the same `attachmentId` and the status the record carries
//! — and mints no second record, whatever status that record is in. The single-attachment
//! read is keyed by the pair alone (`id` and `dataSourceArn`, both `@required`) and reports
//! one `attachmentId`, which is what makes the identifier the pair's rather than the call's.
//!
//! The rows differ along what the rule generalises over — where the data source stands when
//! the attachment is minted, and therefore which status the record is in when the repeats
//! are made: a domain of this account that is already active (ATTACHED at once), one still
//! being provisioned (the attachment waits), this account's AWS Glue Data Catalog (a data
//! source of another vendor's namespace, always active) and an ARN naming a domain this
//! account holds none of (the attachment cannot complete, so the clock leaves it FAILED).
//! Every row is repeated twice over: once while the clock may still owe the record a move,
//! and once after it has settled — so the FAILED record is covered by the same rule as the
//! ATTACHED one. Each repeat also differs in what it CARRIES beyond the pair: nothing, the
//! first call's client token, a token of its own, and a workspace member.
//!
//! The harness comes from [`super::domain_lifecycle`], the attachment calls from
//! [`super::data_source_attachments`] and the active application from
//! [`super::saved_object_migrations`]; this suite adds no second copy of any of them.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    ATTACHMENT_QUOTA, ATTACHMENT_STATUS_ATTACHED, ATTACHMENT_STATUS_FAILED,
    ATTACHMENT_STATUS_PENDING,
};

use super::data_source_attachments::{
    attach, describe_attachment, detach, domain, list_attachments,
};
use super::domain_lifecycle::{poll_until, settled_domain, string_member, Emulator, REGION};
use super::saved_object_migrations::active_application;

/// Every value `DataSourceAttachmentStatus` publishes: "one of: ATTACHED | FAILED | PENDING"
/// (`smithy_query(query="shape",shape="DataSourceAttachmentStatus")`).
const PUBLISHED_STATUSES: [&str; 3] = [
    ATTACHMENT_STATUS_ATTACHED,
    ATTACHMENT_STATUS_FAILED,
    ATTACHMENT_STATUS_PENDING,
];

/// Where the data source a row attaches stands when the row's attachment is minted.
enum Source {
    /// A domain of this account whose provisioning has landed, so the attachment completes
    /// at once.
    ActiveDomain,
    /// A domain of this account that is still being provisioned, so the attachment waits
    /// for it.
    ProvisioningDomain,
    /// This account's AWS Glue Data Catalog — one per account and Region, created and
    /// deleted by no call, so it always names a data source that exists and is active.
    DataCatalog,
    /// An ARN of a domain of this account's own form naming a domain the account holds
    /// none of, so the attachment has nothing to complete against.
    AbsentDomain,
}

/// One row: where the data source stands, and the status the clock leaves the attachment in.
struct Row {
    case: &'static str,
    source: Source,
    settles_in: &'static str,
}

/// The table the identity rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a domain of this account that is already active",
            source: Source::ActiveDomain,
            settles_in: ATTACHMENT_STATUS_ATTACHED,
        },
        Row {
            case: "a domain of this account that is still being provisioned",
            source: Source::ProvisioningDomain,
            settles_in: ATTACHMENT_STATUS_ATTACHED,
        },
        Row {
            case: "this account's AWS Glue Data Catalog",
            source: Source::DataCatalog,
            settles_in: ATTACHMENT_STATUS_ATTACHED,
        },
        Row {
            case: "an ARN naming a domain this account holds none of",
            source: Source::AbsentDomain,
            settles_in: ATTACHMENT_STATUS_FAILED,
        },
    ]
}

/// The ARN of a domain of this account, composed here from the partition, this service's
/// namespace, the Region the request was signed for and the account this emulator serves.
fn domain_arn(name: &str) -> String {
    format!(
        "arn:aws:es:{REGION}:{}:domain/{name}",
        ::engine::emulator_account_id()
    )
}

/// The ARN of this account's AWS Glue Data Catalog in the Region the request was signed for:
/// `arn:<partition>:glue:<region>:<account-id>:catalog`, which carries no resource id
/// because an account holds exactly one.
fn data_catalog_arn() -> String {
    format!(
        "arn:aws:glue:{REGION}:{}:catalog",
        ::engine::emulator_account_id()
    )
}

/// The data source `row` attaches, set up as the row names it.
async fn data_source(emulator: &Emulator, row: &Row, name: &str) -> String {
    match row.source {
        Source::ActiveDomain => {
            let arn = domain(emulator, name).await;
            // The CONDITION the row turns on — the domain's provisioning having landed —
            // is polled for rather than waited out.
            settled_domain(emulator, name).await;
            arn
        }
        Source::ProvisioningDomain => domain(emulator, name).await,
        Source::DataCatalog => data_catalog_arn(),
        Source::AbsentDomain => domain_arn(name),
    }
}

/// The attachment of `data_source_arn` as the single-attachment READ reports it: its
/// identifier and its status.
async fn read_attachment(
    emulator: &Emulator,
    id: &str,
    data_source_arn: &str,
    case: &str,
) -> (String, String) {
    let answer = describe_attachment(emulator, id, data_source_arn).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from DescribeDataSourceAttachment, got {} with {:?}",
        answer.status, answer.body
    );
    (
        string_member(&answer.body, "attachmentId", case),
        string_member(&answer.body, "status", case),
    )
}

/// Every attachment the listing reports for `data_source_arn` — the set the pair names, so a
/// case can assert it holds ONE record rather than one per call.
async fn records_for(
    emulator: &Emulator,
    id: &str,
    data_source_arn: &str,
    case: &str,
) -> Vec<Value> {
    let answer = list_attachments(emulator, id, json!({})).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from ListDataSourceAttachments, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry["dataSourceArn"] == json!(data_source_arn))
        .collect()
}

/// How many attachments the listing reports for `id` in all.
async fn held(emulator: &Emulator, id: &str, case: &str) -> usize {
    let answer = list_attachments(emulator, id, json!({})).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from ListDataSourceAttachments, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["attachments"]
        .as_array()
        .map_or(0, |entries| entries.len())
}

/// The ways a REPEAT of an attach differs from the call that minted the attachment: what it
/// carries beyond the pair, which the rule does not turn on — `clientToken` "ensures
/// idempotency", and `workspaceId` names a workspace rather than a second attachment.
fn repeat_forms(first_token: &str) -> Vec<(String, Value)> {
    vec![
        (
            "a repeat carrying nothing but the pair".to_owned(),
            json!({}),
        ),
        (
            format!("a repeat carrying the first call's client token {first_token:?}"),
            json!({ "clientToken": first_token }),
        ),
        (
            "a repeat carrying a client token of its own".to_owned(),
            json!({ "clientToken": format!("{first_token}-again") }),
        ),
        (
            "a repeat naming an existing workspace".to_owned(),
            json!({ "workspaceId": "ws-0123456789" }),
        ),
    ]
}

/// Repeat the attach of `data_source_arn` in every form, asserting each is answered with the
/// one record the pair holds: `attachment_id`, the data source it was given, a status the
/// model publishes — and, where `expected_status` is given, exactly that status — while the
/// listing still reports ONE record for the pair.
async fn repeats_report_the_one_record(
    emulator: &Emulator,
    id: &str,
    data_source_arn: &str,
    attachment_id: &str,
    token: &str,
    expected_status: Option<&str>,
    phase: &str,
) {
    for (form, extra) in repeat_forms(token) {
        let case = format!("{phase}: {form}");
        let mut body = json!({ "dataSourceArn": data_source_arn });
        if let (Some(target), Some(members)) = (body.as_object_mut(), extra.as_object()) {
            for (member, value) in members {
                target.insert(member.clone(), value.clone());
            }
        }
        let answer = attach(emulator, id, body.clone()).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: a repeat of an attach the application already holds is served, so \
             expected 200 from AttachDataSource({body}), got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            string_member(&answer.body, "attachmentId", &case),
            attachment_id,
            "case {case}: expected the repeat to report the attachment the pair already \
             holds ({attachment_id}), got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body["dataSourceArn"],
            json!(data_source_arn),
            "case {case}: the repeat reports the data source it was given, got {:?}",
            answer.body
        );
        let status = string_member(&answer.body, "status", &case);
        assert!(
            PUBLISHED_STATUSES.contains(&status.as_str()),
            "case {case}: expected one of {PUBLISHED_STATUSES:?}, got {status:?}"
        );
        if let Some(expected) = expected_status {
            assert_eq!(
                status, expected,
                "case {case}: the repeat performs no further action, so expected the status \
                 the record already carries ({expected}), got {status:?}"
            );
        }
        let records = records_for(emulator, id, data_source_arn, &case).await;
        assert_eq!(
            records.len(),
            1,
            "case {case}: expected the pair to hold one attachment record, got {records:?}"
        );
        assert_eq!(
            records[0]["attachmentId"],
            json!(attachment_id),
            "case {case}: the listing reports the record the repeat answered with, got {:?}",
            records[0]
        );
        let (read_id, read_status) = read_attachment(emulator, id, data_source_arn, &case).await;
        assert_eq!(
            read_id, attachment_id,
            "case {case}: the single-attachment read reports the pair's one identifier, got \
             {read_id:?}"
        );
        if let Some(expected) = expected_status {
            assert_eq!(
                read_status, expected,
                "case {case}: the read reports the status the record carries, got \
                 {read_status:?}"
            );
        }
    }
}

/// RULE: an application and a data source name at most one attachment, so a repeat attach of
/// that pair reports the record it already holds — the same `attachmentId` and its current
/// status — and mints no second record, whatever status that record is in.
#[tokio::test]
async fn a_repeat_attach_reports_the_one_attachment_the_pair_already_holds() {
    let emulator = Emulator::new();
    // The application is ACTIVE before the first row, so each row is decided by its own
    // data source rather than by the application still coming up: the condition is polled
    // for rather than waited out.
    let id = active_application(&emulator, "attachidentity").await;

    let mut minted_ids: Vec<String> = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("attach-identity-src{index}");
        let data_source_arn = data_source(&emulator, row, &name).await;
        let token = format!("attach-identity-token-{index}");

        let first = attach(
            &emulator,
            &id,
            json!({ "dataSourceArn": data_source_arn, "clientToken": token }),
        )
        .await;
        assert_eq!(
            first.status, 200,
            "case {case}: an attach is asynchronous, so expected 200, got {} with {:?}",
            first.status, first.body
        );
        let attachment_id = string_member(&first.body, "attachmentId", case);
        let (read_id, _) = read_attachment(&emulator, &id, &data_source_arn, case).await;
        assert_eq!(
            read_id, attachment_id,
            "case {case}: the read reports the identifier the attach minted, got {read_id:?}"
        );

        // While the clock may still owe the record a move, a repeat is answered with the
        // record the pair holds: the identifier is asserted, the status only against the
        // published set, because the clock is free to move it between two calls.
        repeats_report_the_one_record(
            &emulator,
            &id,
            &data_source_arn,
            &attachment_id,
            &token,
            None,
            &format!("{case}, while the record may still be owed a move"),
        )
        .await;

        // Once the record has settled it moves no further, so a repeat now reports that
        // terminal record exactly — the FAILED one included, which is the identity a
        // retrying client depends on.
        let settled_case = format!("{case}, once the record has settled");
        poll_until(&settled_case, || async {
            let (_, status) = read_attachment(&emulator, &id, &data_source_arn, &settled_case).await;
            status != ATTACHMENT_STATUS_PENDING
        })
        .await;
        let (_, settled) = read_attachment(&emulator, &id, &data_source_arn, &settled_case).await;
        assert_eq!(
            settled, row.settles_in,
            "case {settled_case}: expected the attachment to settle in {}, got {settled:?}",
            row.settles_in
        );
        repeats_report_the_one_record(
            &emulator,
            &id,
            &data_source_arn,
            &attachment_id,
            &token,
            Some(&settled),
            &settled_case,
        )
        .await;

        // Every call of this row attached ONE data source, so the application holds one
        // record per row attached so far rather than one per call.
        assert_eq!(
            held(&emulator, &id, case).await,
            index + 1,
            "case {case}: expected the application to hold one attachment per data source \
             attached"
        );
        minted_ids.push(attachment_id);
    }

    // One attachment per pair also means one IDENTIFIER per pair: the rows' identifiers are
    // distinct, so the stability asserted above is not two pairs sharing one id.
    let mut distinct = minted_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted_ids.len(),
        "case one identifier per pair: expected every pair's identifier to differ, got \
         {minted_ids:?}"
    );
}

/// RULE: the attachment bound counts RECORDS, so an attach that stands in place of a record
/// the application already holds for that data source — the one a detach took with it — is
/// served at the bound rather than refused, while an attach that would ADD a record is
/// refused.
#[tokio::test]
async fn an_attach_that_adds_no_record_is_served_at_the_bound() {
    let emulator = Emulator::new();
    let id = active_application(&emulator, "attachbound").await;

    // Every ARN below names a domain of this account's own form that the account holds no
    // domain under, so each attach is accepted and the clock settles it without any domain
    // being created for it.
    let arns: Vec<String> = (0..=ATTACHMENT_QUOTA)
        .map(|index| domain_arn(&format!("attach-bound-src{index}")))
        .collect();
    for arn in arns.iter().take(ATTACHMENT_QUOTA) {
        let answer = attach(&emulator, &id, json!({ "dataSourceArn": arn })).await;
        assert_eq!(
            answer.status, 200,
            "case filling the bound: expected 200 for {arn:?}, got {} with {:?}",
            answer.status, answer.body
        );
    }

    // The attachment a detach is asked of must not be PENDING, so the condition — every
    // record the clock owed a move having landed — is polled for.
    poll_until("every attachment the clock owed a move landing", || async {
        list_attachments(&emulator, &id, json!({}))
            .await
            .body["attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .all(|entry| entry["status"].as_str() != Some(ATTACHMENT_STATUS_PENDING))
    })
    .await;

    // An attach of a data source the application holds no attachment of, at the bound, adds
    // a record — so it is refused.
    let past = arns.last().expect("the table holds one data source past the bound");
    let refused = attach(&emulator, &id, json!({ "dataSourceArn": past })).await;
    assert_eq!(
        refused.status, 400,
        "case an attach past the bound: expected 400, got {} with {:?}",
        refused.status, refused.body
    );

    // A detach takes the attachment with it, so attaching that data source again stands in
    // place of the record the application still holds for it: it adds none, and is served
    // at the bound.
    let detached = &arns[0];
    let answer = detach(&emulator, &id, detached).await;
    assert_eq!(
        answer.status, 200,
        "case a detach at the bound: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let again = attach(&emulator, &id, json!({ "dataSourceArn": detached })).await;
    assert_eq!(
        again.status, 200,
        "case an attach standing in for a detached record at the bound: expected 200, got {} \
         with {:?}",
        again.status, again.body
    );
    let records = records_for(
        &emulator,
        &id,
        detached,
        "an attach standing in for a detached record at the bound",
    )
    .await;
    assert_eq!(
        records.len(),
        1,
        "case an attach standing in for a detached record at the bound: expected the pair to \
         hold one record, got {records:?}"
    );
    assert_eq!(
        records[0]["attachmentId"],
        json!(string_member(
            &again.body,
            "attachmentId",
            "an attach standing in for a detached record at the bound"
        )),
        "case an attach standing in for a detached record at the bound: the listing reports \
         the record the attach answered with, got {:?}",
        records[0]
    );
}
