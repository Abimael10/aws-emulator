// SPDX-License-Identifier: CC0-1.0

//! How a read whose `assetModelVersion` names a version the asset model does NOT
//! have is refused.
//!
//! The rule this suite pins: the version a read asks for is part of what that read
//! addresses, so a model holding no such version — one whose create has not
//! settled, and one that settled `FAILED`, neither of which ever had an `ACTIVE`
//! version — is an asset model the read finds nothing for: HTTP 404
//! `ResourceNotFoundException` in the one wording this service states an absent
//! asset model in, naming the label the REQUEST supplied. The selector's
//! catalogue is the modelled one: `AssetModelVersionFilter` carries the pattern
//! `^(LATEST|ACTIVE)$` and `LATEST` is the documented default
//! (`smithy_query(query="shape", shape="AssetModelVersionFilter")`).
//!
//! The rows differ along what the rule generalises over: which read carries the
//! selector (all four of them — `DescribeAssetModel`,
//! `ListAssetModelProperties`, `ListAssetModelCompositeModels`,
//! `DescribeAssetModelCompositeModel`), which state leaves the model without an
//! `ACTIVE` version (`CREATING`, `FAILED`), and which label form the request
//! addresses the model by (the raw id, the `externalId:` alias). The versions the
//! model DOES have sit beside them — `LATEST` answers every one of those reads,
//! and `ACTIVE` answers them once a create settles — so the table cannot pass by
//! refusing everything.
//!
//! The wire harness is `super::asset_lifecycle`; the settle wait is
//! `crate::support::settled_asset_model_status`, which polls for the condition
//! rather than waiting a span.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, error_code, router};
use crate::support::settled_asset_model_status;

/// The selector value naming the newest definition that has become active.
const ACTIVE: &str = "ACTIVE";

/// The selector value naming the newest definition — the documented default.
const LATEST: &str = "LATEST";

/// The `externalId` every subject model carries, so a row can address it by the
/// other label form every `CustomID` member admits.
const MODEL_EXTERNAL: &str = "absent_version_model";

/// The composite model every subject model is seeded with, under an id the
/// request states, so a composite-model read can address it without describing
/// the model first.
const COMPOSITE_ID: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

/// The child model's measurement `externalId`, which the FAILED subject's metric
/// reaches through the hierarchy.
const CHILD_MEASUREMENT_EXTERNAL: &str = "absent_version_child_power";

/// The hierarchy the FAILED subject declares over the child model.
const HIERARCHY: &str = "Turbines";

/// One of the reads that carry `assetModelVersion`
/// (`smithy_query(query="operation", operation="DescribeAssetModel,
/// ListAssetModelProperties,ListAssetModelCompositeModels,
/// DescribeAssetModelCompositeModel")`).
#[derive(Clone, Copy, Debug)]
enum Read {
    /// `DescribeAssetModel`.
    Model,
    /// `ListAssetModelProperties`, under the `ALL` filter so it reaches the
    /// composite model's definitions too.
    Properties,
    /// `ListAssetModelCompositeModels`.
    CompositeModels,
    /// `DescribeAssetModelCompositeModel` on the seeded composite model.
    CompositeModel,
}

/// Every read that carries the selector, so the rule stated once is asserted of
/// all of them.
const READS: [Read; 4] = [
    Read::Model,
    Read::Properties,
    Read::CompositeModels,
    Read::CompositeModel,
];

impl Read {
    /// Where this read is bound, for the model `label` at version `version`.
    fn uri(self, label: &str, version: &str) -> String {
        match self {
            Self::Model => format!("/asset-models/{label}?assetModelVersion={version}"),
            Self::Properties => format!(
                "/asset-models/{label}/properties?filter=ALL&maxResults=250\
                 &assetModelVersion={version}"
            ),
            Self::CompositeModels => format!(
                "/asset-models/{label}/composite-models?maxResults=250\
                 &assetModelVersion={version}"
            ),
            Self::CompositeModel => format!(
                "/asset-models/{label}/composite-models/{COMPOSITE_ID}\
                 ?assetModelVersion={version}"
            ),
        }
    }
}

/// Which state leaves the subject model with no `ACTIVE` version.
#[derive(Clone, Copy, Debug)]
enum Subject {
    /// The create has not settled yet, so the only definition the model holds is
    /// the one that create wrote.
    Creating,
    /// The create settled `FAILED`, so no definition of the model has ever been
    /// active.
    Failed,
}

/// One measurement property definition.
fn measurement(name: &str, external_id: Option<&str>) -> Value {
    let mut property = json!({"name": name, "dataType": "DOUBLE", "type": {"measurement": {}}});
    if let Some(external_id) = external_id {
        property["externalId"] = json!(external_id);
    }
    property
}

/// The subject model's definition: an `externalId`, one measurement, and one
/// composite model under a stated id.
fn subject_body(hierarchies: Value, properties: Value) -> Value {
    json!({
        "assetModelName": "Farm",
        "assetModelExternalId": MODEL_EXTERNAL,
        "assetModelProperties": properties,
        "assetModelHierarchies": hierarchies,
        "assetModelCompositeModels": [{
            "id": COMPOSITE_ID,
            "name": "Alarm",
            "type": "CUSTOM",
            "properties": [measurement("Threshold", None)],
        }],
    })
}

/// `POST /asset-models`, answering the created model's id.
async fn create(router: &MultiServiceRouter, body: Value) -> String {
    let (status, created) = call(router, "POST", "/asset-models", Some(body)).await;
    assert_eq!(status, 202, "CreateAssetModel: got {status} {created}");
    created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string()
}

/// The subject model in the state `subject` names, answering its id.
async fn subject_model(router: &MultiServiceRouter, subject: Subject) -> String {
    match subject {
        // NOT waited for: the transition the create owes has not fallen due, so
        // the model has a latest version and no active one at all.
        Subject::Creating => {
            create(
                router,
                subject_body(json!([]), json!([measurement("Power", None)])),
            )
            .await
        }
        // A metric reaching a NON-metric property of a child model is a fault
        // about the model graph, so the create is accepted and the model settles
        // FAILED — never having had an active version.
        Subject::Failed => {
            let child = create(
                router,
                json!({
                    "assetModelName": "Turbine",
                    "assetModelProperties": [
                        measurement("Power", Some(CHILD_MEASUREMENT_EXTERNAL)),
                    ],
                }),
            )
            .await;
            let model_id = create(
                router,
                subject_body(
                    json!([{"name": HIERARCHY, "childAssetModelId": child}]),
                    json!([{
                        "name": "Rollup",
                        "dataType": "DOUBLE",
                        "type": {"metric": {
                            "expression": "avg(v)",
                            "variables": [{"name": "v", "value": {
                                "propertyId": format!("externalId:{CHILD_MEASUREMENT_EXTERNAL}"),
                                "hierarchyId": HIERARCHY,
                            }}],
                            "window": {"tumbling": {"interval": "1h"}},
                        }},
                    }]),
                ),
            )
            .await;
            let settled = settled_asset_model_status(router, &model_id).await;
            assert_eq!(
                settled["state"].as_str(),
                Some("FAILED"),
                "the subject model must settle FAILED for this row, got {settled}"
            );
            model_id
        }
    }
}

#[tokio::test]
async fn a_read_of_a_version_the_model_does_not_have_reports_the_asset_model_absent() {
    for subject in [Subject::Creating, Subject::Failed] {
        // Each subject stands on its own store, so the labels below resolve to it
        // and to nothing else.
        let router = router();
        let model_id = subject_model(&router, subject).await;

        for (label_case, label) in [
            ("by id", model_id.clone()),
            (
                "by externalId alias",
                format!("externalId:{MODEL_EXTERNAL}"),
            ),
        ] {
            for which in READS {
                let case = format!("{subject:?}, {label_case}, {which:?}");

                // ACTIVE names a version this model does not have.
                let uri = which.uri(&label, ACTIVE);
                let (status, body) = call(&router, "GET", &uri, None).await;
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected 404 from {uri}, got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "ResourceNotFoundException",
                    "case {case:?}: expected ResourceNotFoundException, got {body}"
                );
                // Written out by hand from the rule, as an independent oracle: the
                // absence of the version asked for is the absence of the asset
                // model, stated in this service's one wording for that and naming
                // the label the request supplied.
                let expected = format!("Asset Model with ID {label} not found");
                let message = body["message"].as_str().unwrap_or_default();
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );

                // LATEST names the definition the accepted write wrote, which
                // exists whatever state the model is in.
                let uri = which.uri(&label, LATEST);
                let (status, body) = call(&router, "GET", &uri, None).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: LATEST names a version this model has, got {status} {body} \
                     from {uri}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_read_of_the_active_version_answers_once_the_create_settles() {
    let router = router();
    let model_id = subject_model(&router, Subject::Creating).await;
    let settled = settled_asset_model_status(&router, &model_id).await;
    assert_eq!(
        settled["state"].as_str(),
        Some("ACTIVE"),
        "the subject model must settle ACTIVE for this suite's control, got {settled}"
    );

    for which in READS {
        for (label_case, label) in [
            ("by id", model_id.clone()),
            (
                "by externalId alias",
                format!("externalId:{MODEL_EXTERNAL}"),
            ),
        ] {
            let case = format!("{label_case}, {which:?}");
            let uri = which.uri(&label, ACTIVE);
            let (status, body) = call(&router, "GET", &uri, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a settled model has an ACTIVE version, got {status} {body} from \
                 {uri}"
            );
        }
    }
}
