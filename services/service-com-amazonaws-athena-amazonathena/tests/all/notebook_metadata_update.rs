// SPDX-License-Identifier: CC0-1.0

//! `UpdateNotebookMetadata`, driven end to end over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.UpdateNotebookMetadata`): "Updates the metadata
//! for a notebook".
//!
//! THE RULE UNDER TEST: the update renames the notebook its `NotebookId` names to the name
//! the request carried and moves that notebook's `LastModifiedTime` to the instant of the
//! write, leaving everything else of the notebook — the document it holds, the id it was
//! minted under, the workgroup it lives in, its type and when it was created — as it was, so
//! every reader of that notebook reports the new name afterwards; a name a SECOND notebook
//! of the same workgroup already carries is refused, a `NotebookId` no create or import
//! minted names nothing whose metadata could be updated, and a repeat under one
//! `ClientRequestToken` writes nothing further.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! length, the form and the character set of the name written, the shape of an id that names
//! no notebook, which workgroup already holds the name a rename asks for, and whether an
//! idempotency token was reused for the same name or for another one.
//!
//! Nothing here waits on the wall clock: every assertion is about what a read answers with
//! after a call, and the one ordering assertion compares the instants the service itself
//! reported.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, ERROR_CODE_NOTEBOOK_NOT_FOUND, NOTEBOOK_TYPE_IPYNB,
    PYSPARK_ENGINE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, router};

/// The Spark enabled workgroup the notebooks renamed below live in.
const WORK_GROUP: &str = "spark-notebook-metadata";

/// A SECOND Spark enabled workgroup: a notebook name is unique within ONE workgroup, so the
/// same name in another workgroup names another notebook and collides with nothing.
const OTHER_WORK_GROUP: &str = "spark-notebook-metadata-other";

/// Create the Spark enabled workgroup `name`, which is what makes it a workgroup a notebook
/// can live in.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Create a notebook called `name` in `work_group` and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, work_group: &str, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": work_group, "Name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be created in {work_group}, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create must answer with a NotebookId, got {created:?}"))
        .to_string()
}

/// One `UpdateNotebookMetadata` call, as a client sends it.
async fn update_metadata(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "UpdateNotebookMetadata", request).await
}

/// The rename request that gives the notebook `notebook_id` the name `name`.
fn rename_of(notebook_id: &str, name: &str) -> Value {
    json!({ "NotebookId": notebook_id, "Name": name })
}

/// `GetNotebookMetadata`'s view of one notebook, which must be readable.
async fn metadata(router: &MultiServiceRouter, notebook_id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetNotebookMetadata",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetNotebookMetadata({notebook_id}) must answer 200, got {status} with {body:?}"
    );
    body["NotebookMetadata"].clone()
}

/// `ExportNotebook`'s view of one notebook, which must be readable.
async fn exported(router: &MultiServiceRouter, notebook_id: &str) -> Value {
    let (status, body) = call(
        router,
        "ExportNotebook",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ExportNotebook({notebook_id}) must answer 200, got {status} with {body:?}"
    );
    body
}

/// The names `work_group`'s notebook listing reports, in the order it reports them.
async fn listed_names(router: &MultiServiceRouter, work_group: &str) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListNotebookMetadata",
        &json!({ "WorkGroup": work_group, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListNotebookMetadata({work_group}) must answer 200, got {status} with {listed:?}"
    );
    listed["NotebookMetadataList"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|entry| entry["Name"].as_str().map(str::to_string))
        .collect()
}

/// RULE: the update renames the notebook and moves nothing else of it, whatever the length,
/// the form or the character set of the name written.
#[tokio::test]
async fn an_update_renames_the_notebook_every_reader_reports() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let notebook_id = given_notebook(&router, WORK_GROUP, "analysis").await;

    let created = metadata(&router, &notebook_id).await;
    let creation_time = created["CreationTime"].as_f64();
    assert!(
        creation_time.is_some(),
        "the created notebook carries a CreationTime, got {created:?}"
    );
    assert_eq!(
        created["LastModifiedTime"].as_f64(),
        creation_time,
        "a notebook nothing has updated was last modified when it was created, got {created:?}"
    );
    // The document the notebook holds, which a rename must leave untouched.
    let document = exported(&router, &notebook_id).await["Payload"]
        .as_str()
        .unwrap_or_else(|| panic!("an export must carry the Payload"))
        .to_string();

    // The rows differ along the LENGTH, the FORM and the character set of the name written.
    // The modelled `NotebookName` is 1 to 255 characters matching
    // `^(?!.*[/:\\])[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]+$`, so the
    // shortest it admits is one character, a space and a tab are admitted and the three
    // path characters are not; both published length bounds are written here.
    let names = [
        ("the shortest name the model admits", "a".to_string()),
        ("a name carrying spaces", "quarterly revenue report".to_string()),
        ("a name outside ASCII", "分析ノート".to_string()),
        ("a name carrying a tab", "cost\tmodel".to_string()),
        ("a name of punctuation the model admits", "v2.1_final-(draft)".to_string()),
        ("a name of the longest length the model admits", "n".repeat(255)),
        ("the name the notebook already carries", "n".repeat(255)),
    ];

    let mut previous = created["Name"]
        .as_str()
        .expect("the created notebook carries a Name")
        .to_string();
    for (case, name) in &names {
        let before = metadata(&router, &notebook_id).await;
        assert_eq!(
            before["Name"].as_str(),
            Some(previous.as_str()),
            "case {case:?}: the notebook must still carry the name the last write left"
        );

        let (status, answered) = update_metadata(&router, &rename_of(&notebook_id, name)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateNotebookMetadata to answer 200, got {status} with \
             {answered:?}"
        );

        let after = metadata(&router, &notebook_id).await;
        assert_eq!(
            after["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the read to report the written name ({} characters), \
             got {:?}",
            name.chars().count(),
            after["Name"]
        );
        // Nothing else of the notebook moved with its name.
        assert_eq!(
            after["NotebookId"].as_str(),
            Some(notebook_id.as_str()),
            "case {case:?}: the rename names the same notebook, got {after:?}"
        );
        assert_eq!(
            after["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {case:?}: the notebook stays in its workgroup, got {after:?}"
        );
        assert_eq!(
            after["Type"].as_str(),
            Some(NOTEBOOK_TYPE_IPYNB),
            "case {case:?}: the only notebook type is {NOTEBOOK_TYPE_IPYNB}, got {after:?}"
        );
        assert_eq!(
            after["CreationTime"].as_f64(),
            creation_time,
            "case {case:?}: a rename does not move the creation instant, got {after:?}"
        );
        let last_modified = after["LastModifiedTime"].as_f64();
        assert!(
            last_modified >= creation_time,
            "case {case:?}: expected a LastModifiedTime at or after the CreationTime \
             {creation_time:?}, got {last_modified:?}"
        );
        let body = exported(&router, &notebook_id).await;
        assert_eq!(
            body["Payload"].as_str(),
            Some(document.as_str()),
            "case {case:?}: a rename leaves the document the notebook holds, got {:?}",
            body["Payload"]
        );
        // The three readers of one notebook answer with the same record.
        assert_eq!(
            body["NotebookMetadata"], after,
            "case {case:?}: the export and the metadata read report one record"
        );
        assert_eq!(
            listed_names(&router, WORK_GROUP).await,
            vec![name.clone()],
            "case {case:?}: the workgroup's listing reports the renamed notebook once, under \
             its new name"
        );
        previous = name.clone();
    }
}

/// RULE: a `NotebookId` this account minted no notebook under names nothing whose metadata
/// could be updated, whatever the shape of the id.
#[tokio::test]
async fn an_update_needs_a_notebook_this_account_holds() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let held = given_notebook(&router, WORK_GROUP, "analysis").await;

    // The rows differ along the SHAPE of an id of the modelled form
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`) that names no
    // notebook: a freshly minted one, the all-zero one and the all-f one.
    let misses = [
        ("an id no notebook was minted under", state::new_notebook_id()),
        (
            "the all-zero id of the modelled form",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "the all-f id of the modelled form",
            "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
        ),
    ];

    for (case, missing) in &misses {
        let (status, refused) = update_metadata(&router, &rename_of(missing, "renamed")).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_NOTEBOOK_NOT_FOUND,
            &state::notebook_not_found(missing),
        );
    }

    // No refusal wrote anything: the notebook this account does hold still carries the name
    // its create gave it.
    assert_eq!(
        metadata(&router, &held).await["Name"].as_str(),
        Some("analysis"),
        "a refused rename must write nothing"
    );
}

/// RULE: a notebook's name identifies it within the workgroup it lives in, so a rename onto
/// a name another notebook of that same workgroup already carries is refused — while the
/// same name in ANOTHER workgroup names another notebook and collides with nothing.
#[tokio::test]
async fn a_name_another_notebook_of_the_workgroup_holds_is_refused() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;

    let renamed = given_notebook(&router, WORK_GROUP, "analysis").await;
    // The rows differ along the name ALREADY held by a second notebook of the workgroup: a
    // plain one, one carrying a space, and one outside ASCII.
    let taken = [
        ("a plain name", "forecast"),
        ("a name carrying a space", "monthly forecast"),
        ("a name outside ASCII", "予測"),
    ];
    for (_, name) in &taken {
        given_notebook(&router, WORK_GROUP, name).await;
        // The same name in the OTHER workgroup names another notebook, which this rename
        // must not be measured against.
        given_notebook(&router, OTHER_WORK_GROUP, name).await;
    }

    for (case, name) in &taken {
        let (status, refused) = update_metadata(&router, &rename_of(&renamed, name)).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &state::notebook_name_exists(WORK_GROUP, name),
        );
        assert_eq!(
            metadata(&router, &renamed).await["Name"].as_str(),
            Some("analysis"),
            "case {case:?}: a refused rename must write nothing"
        );
    }

    // A name only ANOTHER workgroup holds is free: the notebook of this workgroup takes it.
    let free = given_notebook(&router, OTHER_WORK_GROUP, "elsewhere").await;
    let (status, answered) = update_metadata(&router, &rename_of(&renamed, "elsewhere")).await;
    assert_eq!(
        status, 200,
        "case \"a name only another workgroup holds\": expected 200, got {status} with \
         {answered:?}"
    );
    assert_eq!(
        metadata(&router, &renamed).await["Name"].as_str(),
        Some("elsewhere"),
        "case \"a name only another workgroup holds\": expected the written name"
    );
    assert_eq!(
        metadata(&router, &free).await["Name"].as_str(),
        Some("elsewhere"),
        "the other workgroup's notebook of that name is untouched"
    );

    // Renaming a notebook to the name it already carries collides with nothing: the one
    // notebook that may already hold the name is the notebook being renamed.
    let (status, answered) = update_metadata(&router, &rename_of(&renamed, "elsewhere")).await;
    assert_eq!(
        status, 200,
        "case \"the name the notebook already carries\": expected 200, got {status} with \
         {answered:?}"
    );
}

/// RULE: `ClientRequestToken` makes the update execute only once: a repeat under the token
/// the stored name already stands under writes nothing further, and the same token carrying
/// another name describes another update, which is refused.
#[tokio::test]
async fn one_client_request_token_updates_the_metadata_once() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    // The modelled `ClientRequestToken` is a hyphenated hex token
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`); the rows differ
    // along the name written under each.
    let rows = [
        (
            "a plain name",
            "11111111-1111-4111-8111-111111111111",
            "renamed once",
        ),
        (
            "a name outside ASCII",
            "22222222-2222-4222-8222-222222222222",
            "冪等な名前",
        ),
    ];

    for (case, token, name) in rows {
        let notebook_id = given_notebook(&router, WORK_GROUP, &format!("idempotent {case}")).await;
        let mut request = rename_of(&notebook_id, name);
        request["ClientRequestToken"] = json!(token);

        let (status, first) = update_metadata(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first rename to answer 200, got {status} with \
             {first:?}"
        );
        let after_first = metadata(&router, &notebook_id).await;
        assert_eq!(
            after_first["Name"].as_str(),
            Some(name),
            "case {case:?}: expected the written name, got {after_first:?}"
        );

        let (status, again) = update_metadata(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a rename repeated under one token executes once and answers the \
             same way, got {status} with {again:?}"
        );
        assert_eq!(
            metadata(&router, &notebook_id).await,
            after_first,
            "case {case:?}: the repeat must write nothing, so the notebook keeps the instant \
             of the one write it was modified by"
        );

        // A name no other notebook of the workgroup holds, so what the rows below measure
        // is the token and nothing else.
        let other_name = format!("renamed again {case}");

        // The same token carrying ANOTHER name describes another update.
        let mut changed = rename_of(&notebook_id, &other_name);
        changed["ClientRequestToken"] = json!(token);
        let (status, mismatched) = update_metadata(&router, &changed).await;
        assert_refusal(
            case,
            status,
            &mismatched,
            ERROR_CODE_INVALID_INPUT,
            state::IDEMPOTENT_PARAMETERS_DO_NOT_MATCH,
        );
        assert_eq!(
            metadata(&router, &notebook_id).await["Name"].as_str(),
            Some(name),
            "case {case:?}: the refused repeat must write nothing"
        );

        // ANOTHER token is another update, however alike the two requests are.
        let mut second = rename_of(&notebook_id, &other_name);
        second["ClientRequestToken"] = json!("33333333-3333-4333-8333-333333333333");
        let (status, accepted) = update_metadata(&router, &second).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a rename under another token to answer 200, got {status} \
             with {accepted:?}"
        );
        assert_eq!(
            metadata(&router, &notebook_id).await["Name"].as_str(),
            Some(other_name.as_str()),
            "case {case:?}: expected the name the second token wrote"
        );
    }
}
