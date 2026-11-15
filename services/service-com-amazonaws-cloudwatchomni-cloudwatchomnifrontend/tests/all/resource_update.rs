// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the PARTIAL-update rule of CloudWatchOmni's three updatable leaf
//! resources: `UpdateAccessProfile`, `UpdateOmniDashboard` and `UpdateView`.
//!
//! Each of the three is documented as changing "only the provided fields", so this suite
//! asserts that rule over a TABLE rather than one recorded exchange. Each row starts from
//! a resource in a different shape, patches a different subset of the updatable members,
//! and requires
//!
//! * every member the request carried to hold its new value;
//! * every member the request omitted — including the ones the input shape does not even
//!   carry (ids, ARNs, the parent, the ownership category, `createdBy`, tags,
//!   `createdAt`) — to hold exactly what it held before;
//! * the patched record, and not a second copy of it, to be what the resource's own
//!   reader answers afterwards: the write has to reach the store the read comes from;
//! * `updatedAt` to move when a member moves and to stay put when the patch asks for the
//!   values the record already holds.
//!
//! The refusals are asserted the same way: a record the store does not hold is the
//! modelled 404, a rename onto a sibling's name is the modelled 409, a definition that
//! violates its own rule is the modelled 400 — and none of them writes anything.
//!
//! Each assertion names its case and the actual value, so a failure identifies the row of
//! the table that broke.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_access_profile::CreateAccessProfile;
use omni::ops::create_domain::CreateDomain;
use omni::ops::create_omni_dashboard::CreateOmniDashboard;
use omni::ops::create_space::CreateSpace;
use omni::ops::create_view::CreateView;
use omni::ops::get_access_profile::{GetAccessProfile, GetAccessProfileError};
use omni::ops::get_omni_dashboard::{GetOmniDashboard, GetOmniDashboardError};
use omni::ops::get_view::{GetView, GetViewError};
use omni::ops::types::{
    AccessProfile, CreateAccessProfileInput, CreateDomainInput, CreateOmniDashboardInput,
    CreateSpaceInput, CreateViewInput, GetAccessProfileInput, GetAccessProfileOutput,
    GetOmniDashboardInput, GetViewInput, GetViewOutput, OmniDashboard, UpdateAccessProfileInput,
    UpdateAccessProfileOutput, UpdateOmniDashboardInput, UpdateViewInput, UpdateViewOutput,
    GET_ACCESS_PROFILE_INPUT_SCHEMA, UPDATE_ACCESS_PROFILE_INPUT_SCHEMA,
};
use omni::ops::update_access_profile::{UpdateAccessProfile, UpdateAccessProfileError};
use omni::ops::update_omni_dashboard::{UpdateOmniDashboard, UpdateOmniDashboardError};
use omni::ops::update_view::{UpdateView, UpdateViewError};
use omni::state::{ASSUME_STATUS_ALLOWED, VIEW_TYPE_USER};

/// The region the suite signs for.
const REGION: &str = "us-west-2";

/// A role ARN satisfying `Arn`'s `^arn:.+$`, which `CreateSpace` requires.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// A well-formed `SpaceId` (its modelled pattern) that names nothing.
const MISSING_SPACE_ID: &str = "00000000-0000-4000-8000-000000000000";

/// A well-formed `ProfileId` that names nothing.
const MISSING_PROFILE_ID: &str = "no-such-profile";

/// A well-formed `DashboardId` (its modelled pattern) that names nothing.
const MISSING_DASHBOARD_ID: &str = "11111111-2222-4333-8444-555555555555";

/// A well-formed `ViewName` (its modelled pattern) that names nothing.
const MISSING_VIEW_NAME: &str = "view.no-such-view";

/// A dashboard body that satisfies the dashboard-body rule: a closed JSON object whose
/// `panels` member is a list of panel objects, each carrying the `type` and `layout` the
/// rule requires of a panel. This one lays out no panel yet.
const PANELS_BODY: &str = r#"{"panels":[]}"#;

/// A conforming body holding ONE panel, so a row can patch the body to a document that
/// is not empty.
const ONE_PANEL_BODY: &str = r#"{"panels":[{"type":"logs","layout":{"row":0}}]}"#;

/// A timestamp at millisecond resolution — the resolution rpcv2Cbor encodes a `Timestamp`
/// at, so a value compared across a wire round trip is compared there.
fn millis(at: aws_smithy_types::DateTime) -> (i64, u32) {
    (at.secs(), at.subsec_nanos() / 1_000_000)
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name (the router's routing key) and the region the handlers record.
fn signed_request_parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine, the service mounted on a router, and the request
/// metadata a handler reads.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        // The router's service and the operations constructed below all hold the SAME
        // engine, so both front doors read and write one store.
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// A space to hang the suite's profiles and dashboards off, through the handlers that
    /// own those resources.
    async fn space(&self, case: &str, name: &str) -> String {
        let domain = CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: format!("omni-{name}"),
                    identity_providers: vec!["IAM".to_string()],
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain;
        CreateSpace::new(Arc::clone(&self.engine))
            .handle(
                CreateSpaceInput {
                    name: name.to_string(),
                    domain_id: domain.domain_id,
                    data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateSpace failed: {error:?}"))
            .space
            .space_id
    }

    async fn create_profile(
        &self,
        case: &str,
        space_id: &str,
        name: &str,
        description: Option<&str>,
    ) -> AccessProfile {
        CreateAccessProfile::new(Arc::clone(&self.engine))
            .handle(
                CreateAccessProfileInput {
                    space_id: space_id.to_string(),
                    name: name.to_string(),
                    description: description.map(str::to_string),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateAccessProfile failed: {error:?}"))
            .access_profile
    }

    async fn update_profile(
        &self,
        input: &UpdateAccessProfileInput,
    ) -> Result<UpdateAccessProfileOutput, UpdateAccessProfileError> {
        UpdateAccessProfile::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    async fn get_profile(
        &self,
        space_id: &str,
        profile_id: &str,
    ) -> Result<GetAccessProfileOutput, GetAccessProfileError> {
        GetAccessProfile::new(Arc::clone(&self.engine))
            .handle(
                GetAccessProfileInput {
                    space_id: space_id.to_string(),
                    profile_id: profile_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// The stored profile, which every row reads back to check the write landed.
    async fn stored_profile(&self, case: &str, space_id: &str, profile_id: &str) -> AccessProfile {
        self.get_profile(space_id, profile_id)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetAccessProfile failed: {error:?}"))
            .access_profile
    }

    async fn create_dashboard(
        &self,
        case: &str,
        space_id: &str,
        name: &str,
        body: &str,
        description: Option<&str>,
    ) -> OmniDashboard {
        CreateOmniDashboard::new(Arc::clone(&self.engine))
            .handle(
                CreateOmniDashboardInput {
                    space_id: space_id.to_string(),
                    name: name.to_string(),
                    body: body.to_string(),
                    description: description.map(str::to_string),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateOmniDashboard failed: {error:?}"))
            .omni_dashboard
    }

    async fn update_dashboard(
        &self,
        input: &UpdateOmniDashboardInput,
    ) -> Result<OmniDashboard, UpdateOmniDashboardError> {
        UpdateOmniDashboard::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
            .map(|output| output.omni_dashboard)
    }

    async fn get_dashboard(
        &self,
        space_id: &str,
        dashboard_id: &str,
    ) -> Result<OmniDashboard, GetOmniDashboardError> {
        GetOmniDashboard::new(Arc::clone(&self.engine))
            .handle(
                GetOmniDashboardInput {
                    space_id: space_id.to_string(),
                    dashboard_id: dashboard_id.to_string(),
                },
                &self.ctx(),
            )
            .await
            .map(|output| output.omni_dashboard)
    }

    /// The stored dashboard, which every row reads back to check the write landed.
    async fn stored_dashboard(
        &self,
        case: &str,
        space_id: &str,
        dashboard_id: &str,
    ) -> OmniDashboard {
        self.get_dashboard(space_id, dashboard_id)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetOmniDashboard failed: {error:?}"))
    }

    async fn create_view(
        &self,
        case: &str,
        name: &str,
        definition: &str,
        description: Option<&str>,
    ) -> UpdateViewOutput {
        let created = CreateView::new(Arc::clone(&self.engine))
            .handle(
                CreateViewInput {
                    name: name.to_string(),
                    definition: definition.to_string(),
                    description: description.map(str::to_string),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateView failed: {error:?}"));
        // `CreateView`, `UpdateView` and `GetView` answer with the same flat member set,
        // so the suite compares them as one shape.
        UpdateViewOutput {
            name: created.name,
            r#type: created.r#type,
            description: created.description,
            definition: created.definition,
            created_at: created.created_at,
            updated_at: created.updated_at,
            arn: created.arn,
        }
    }

    async fn update_view(
        &self,
        input: &UpdateViewInput,
    ) -> Result<UpdateViewOutput, UpdateViewError> {
        UpdateView::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    async fn get_view(&self, name: &str) -> Result<GetViewOutput, GetViewError> {
        GetView::new(Arc::clone(&self.engine))
            .handle(
                GetViewInput {
                    name: name.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    /// The stored view, as the same flat member set the update answers with.
    async fn stored_view(&self, case: &str, name: &str) -> UpdateViewOutput {
        let fetched = self
            .get_view(name)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetView failed: {error:?}"));
        UpdateViewOutput {
            name: fetched.name,
            r#type: fetched.r#type,
            description: fetched.description,
            definition: fetched.definition,
            created_at: fetched.created_at,
            updated_at: fetched.updated_at,
            arn: fetched.arn,
        }
    }

    /// Dispatch one rpcv2Cbor request over HTTP and return the status and body.
    async fn over_the_wire<I: SerializableStruct>(
        &self,
        operation: &str,
        input_schema: &Schema,
        input: &I,
    ) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(input_schema, input)
            .expect("the input shape serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!(
                "/service/CloudWatchOmniFrontend/operation/{operation}"
            ))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor");
        for (name, value) in &self.parts.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_modelled_error<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
    expected_status: u16,
    expected_code: &str,
) {
    match result {
        Ok(output) => panic!(
            "case {case:?}: expected {expected_code} ({expected_status}), got Ok({output:?})"
        ),
        Err(error) => {
            assert_eq!(
                error.shape_name(),
                expected_code,
                "case {case:?}: expected error code {expected_code}, got {error:?}"
            );
            assert_eq!(
                error.http_status(),
                expected_status,
                "case {case:?}: expected HTTP {expected_status} for {expected_code}, got {}",
                error.http_status()
            );
        }
    }
}

/// The members of a profile that `UpdateAccessProfile`'s input cannot name, rendered for
/// comparison: a patch must bring every one of them through untouched.
fn untouched_profile(profile: &AccessProfile) -> String {
    format!(
        "{:?}",
        (
            &profile.profile_id,
            &profile.space_id,
            &profile.arn,
            &profile.profile_type,
            millis(profile.created_at),
        )
    )
}

/// The members of a dashboard that `UpdateOmniDashboard`'s input cannot name.
fn untouched_dashboard(dashboard: &OmniDashboard) -> String {
    format!(
        "{:?}",
        (
            &dashboard.dashboard_id,
            &dashboard.arn,
            &dashboard.created_by,
            dashboard
                .tags
                .as_ref()
                .map(|tags| {
                    let mut pairs: Vec<(&String, &String)> = tags.iter().collect();
                    pairs.sort();
                    pairs
                })
                .unwrap_or_default(),
            millis(dashboard.created_at),
        )
    )
}

/// The members of a view that `UpdateView`'s input cannot name — its name included, since
/// a view's name is its identity and the input names it only to address it.
fn untouched_view(view: &UpdateViewOutput) -> String {
    format!(
        "{:?}",
        (
            &view.name,
            &view.r#type,
            &view.arn,
            millis(view.created_at),
        )
    )
}

/// One row of the access-profile patch table.
struct ProfilePatch {
    case: &'static str,
    initial_name: String,
    initial_description: Option<String>,
    patch_name: Option<String>,
    patch_description: Option<String>,
    expected_name: String,
    expected_description: Option<String>,
}

/// The rows differ along WHICH members the patch carries (each alone, both, neither, and
/// the values the record already holds) and along the shape of the profile they start
/// from. `name`'s sizes are the published bounds of `AccessProfileName`'s
/// `@length(1..256)` and its character class is the whole of its
/// `^[a-zA-Z0-9 _.:/=+@-]+$`; the longest description is the bound of
/// `UpdateAccessProfileInput.description`'s `@length(max: 1024)`
/// (`smithy_query(query="shape",shape="UpdateAccessProfileInput")`).
fn profile_patches() -> Vec<ProfilePatch> {
    vec![
        ProfilePatch {
            case: "the description alone is replaced",
            initial_name: "profile one".to_string(),
            initial_description: Some("first".to_string()),
            patch_name: None,
            patch_description: Some("second".to_string()),
            expected_name: "profile one".to_string(),
            expected_description: Some("second".to_string()),
        },
        ProfilePatch {
            case: "the description alone is added to a profile created without one",
            initial_name: "profile two".to_string(),
            initial_description: None,
            patch_name: None,
            patch_description: Some("a description".to_string()),
            expected_name: "profile two".to_string(),
            expected_description: Some("a description".to_string()),
        },
        ProfilePatch {
            case: "the name alone keeps the description",
            initial_name: "profile three".to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: Some("profile three renamed".to_string()),
            patch_description: None,
            expected_name: "profile three renamed".to_string(),
            expected_description: Some("kept".to_string()),
        },
        ProfilePatch {
            case: "both members at once, name at @length(min: 1)",
            initial_name: "profile four".to_string(),
            initial_description: None,
            patch_name: Some("a".to_string()),
            patch_description: Some("d".repeat(1024)),
            expected_name: "a".to_string(),
            expected_description: Some("d".repeat(1024)),
        },
        ProfilePatch {
            case: "a name at @length(max: 256)",
            initial_name: "profile five".to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: Some("n".repeat(256)),
            patch_description: None,
            expected_name: "n".repeat(256),
            expected_description: Some("kept".to_string()),
        },
        ProfilePatch {
            case: "a name drawn from the whole modelled character class",
            initial_name: "profile six".to_string(),
            initial_description: None,
            patch_name: Some("ops team_a.b:c/d=e+f@g-h 9".to_string()),
            patch_description: Some(String::new()),
            expected_name: "ops team_a.b:c/d=e+f@g-h 9".to_string(),
            expected_description: Some(String::new()),
        },
        ProfilePatch {
            case: "a request carrying no updatable member leaves the profile whole",
            initial_name: "profile seven".to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: None,
            patch_description: None,
            expected_name: "profile seven".to_string(),
            expected_description: Some("kept".to_string()),
        },
        ProfilePatch {
            case: "a patch asking for the values the profile already holds",
            initial_name: "profile eight".to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: Some("profile eight".to_string()),
            patch_description: Some("kept".to_string()),
            expected_name: "profile eight".to_string(),
            expected_description: Some("kept".to_string()),
        },
    ]
}

#[tokio::test]
async fn update_access_profile_persists_only_the_members_the_request_carries() {
    let omni = Omni::new();
    let space_id = omni.space("profiles", "update-profiles").await;

    for row in profile_patches() {
        let case = row.case;
        let created = omni
            .create_profile(
                case,
                &space_id,
                &row.initial_name,
                row.initial_description.as_deref(),
            )
            .await;

        let updated = omni
            .update_profile(&UpdateAccessProfileInput {
                space_id: space_id.clone(),
                profile_id: created.profile_id.clone(),
                name: row.patch_name.clone(),
                description: row.patch_description.clone(),
            })
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: UpdateAccessProfile failed: {error:?}"))
            .access_profile;

        assert_eq!(
            (updated.name.as_str(), updated.description.as_deref()),
            (
                row.expected_name.as_str(),
                row.expected_description.as_deref()
            ),
            "case {case:?}: expected name {:?} and description {:?}, got {updated:?}",
            row.expected_name,
            row.expected_description
        );
        assert_eq!(
            untouched_profile(&updated),
            untouched_profile(&created),
            "case {case:?}: a member UpdateAccessProfile's input cannot name was changed"
        );
        // The caller that created the profile owns it, so the patched record still reports
        // that caller may assume it.
        assert_eq!(
            updated.assume_status.as_deref(),
            Some(ASSUME_STATUS_ALLOWED),
            "case {case:?}: expected assumeStatus {ASSUME_STATUS_ALLOWED} for the owner, got {:?}",
            updated.assume_status
        );

        // The write has to reach the store the reader answers from, and it must be the
        // SAME record rather than a second copy of it.
        let fetched = omni
            .stored_profile(case, &space_id, &created.profile_id)
            .await;
        assert_eq!(
            format!("{fetched:?}"),
            format!("{updated:?}"),
            "case {case:?}: GetAccessProfile did not return the record the update persisted"
        );

        // `updatedAt` moves when a member moves, and stays where the create left it when
        // the patch asks for nothing new.
        let moved = row.expected_name != row.initial_name
            || row.expected_description != row.initial_description;
        if moved {
            assert!(
                updated.updated_at.secs() >= created.updated_at.secs(),
                "case {case:?}: updatedAt {:?} precedes the value the create left, {:?}",
                updated.updated_at,
                created.updated_at
            );
        } else {
            assert_eq!(
                millis(updated.updated_at),
                millis(created.updated_at),
                "case {case:?}: a patch that changed nothing moved updatedAt to {:?}",
                updated.updated_at
            );
        }
    }
}

/// One row of the dashboard patch table.
struct DashboardPatch {
    case: &'static str,
    initial_name: String,
    initial_body: String,
    initial_description: Option<String>,
    patch_name: Option<String>,
    patch_body: Option<String>,
    patch_description: Option<String>,
    expected_name: String,
    expected_body: String,
    expected_description: Option<String>,
}

/// The rows differ along WHICH members the patch carries and along the FORM of the
/// dashboard body — an empty `panels` list, one panel, many panels, a nested panel
/// document — because the rule is about the body being stored verbatim, not about the
/// text it is written as. The longest description is the bound of
/// `UpdateOmniDashboardInput.description`'s `@length(1..1024)`
/// (`smithy_query(query="shape",shape="UpdateOmniDashboardInput")`).
fn dashboard_patches() -> Vec<DashboardPatch> {
    let many_panels = format!(
        r#"{{"panels":[{}]}}"#,
        (0..12)
            .map(|index| format!(
                r#"{{"type":"timeSeries","layout":{{"row":{index}}},"title":"p{index}"}}"#
            ))
            .collect::<Vec<String>>()
            .join(",")
    );
    let nested_panel =
        "{\n  \"panels\": [\n    {\"type\": \"logs\", \"layout\": {\"rows\":[{\"cells\":[{\"widget\":{}}]}]}}\n  ]\n}";
    vec![
        DashboardPatch {
            case: "the description alone is replaced",
            initial_name: "dashboard-one".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: Some("first".to_string()),
            patch_name: None,
            patch_body: None,
            patch_description: Some("second".to_string()),
            expected_name: "dashboard-one".to_string(),
            expected_body: PANELS_BODY.to_string(),
            expected_description: Some("second".to_string()),
        },
        DashboardPatch {
            case: "the body alone is replaced, by a document with one panel",
            initial_name: "dashboard-two".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: None,
            patch_name: None,
            patch_body: Some(ONE_PANEL_BODY.to_string()),
            patch_description: None,
            expected_name: "dashboard-two".to_string(),
            expected_body: ONE_PANEL_BODY.to_string(),
            expected_description: None,
        },
        DashboardPatch {
            case: "the body alone is replaced, by a document with many panels",
            initial_name: "dashboard-three".to_string(),
            initial_body: ONE_PANEL_BODY.to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: None,
            patch_body: Some(many_panels.clone()),
            patch_description: None,
            expected_name: "dashboard-three".to_string(),
            expected_body: many_panels,
            expected_description: Some("kept".to_string()),
        },
        DashboardPatch {
            case: "the name alone keeps the body and the description",
            initial_name: "dashboard-four".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: Some("dashboard-four.renamed".to_string()),
            patch_body: None,
            patch_description: None,
            expected_name: "dashboard-four.renamed".to_string(),
            expected_body: PANELS_BODY.to_string(),
            expected_description: Some("kept".to_string()),
        },
        DashboardPatch {
            case: "every updatable member at once, description at @length(max: 1024)",
            initial_name: "dashboard-five".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: None,
            patch_name: Some("dashboard_five@2".to_string()),
            patch_body: Some(nested_panel.to_string()),
            patch_description: Some("d".repeat(1024)),
            expected_name: "dashboard_five@2".to_string(),
            expected_body: nested_panel.to_string(),
            expected_description: Some("d".repeat(1024)),
        },
        DashboardPatch {
            case: "a request carrying no updatable member leaves the dashboard whole",
            initial_name: "dashboard-six".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: None,
            patch_body: None,
            patch_description: None,
            expected_name: "dashboard-six".to_string(),
            expected_body: PANELS_BODY.to_string(),
            expected_description: Some("kept".to_string()),
        },
        DashboardPatch {
            case: "a patch asking for the values the dashboard already holds",
            initial_name: "dashboard-seven".to_string(),
            initial_body: PANELS_BODY.to_string(),
            initial_description: Some("kept".to_string()),
            patch_name: Some("dashboard-seven".to_string()),
            patch_body: Some(PANELS_BODY.to_string()),
            patch_description: Some("kept".to_string()),
            expected_name: "dashboard-seven".to_string(),
            expected_body: PANELS_BODY.to_string(),
            expected_description: Some("kept".to_string()),
        },
    ]
}

#[tokio::test]
async fn update_omni_dashboard_persists_only_the_members_the_request_carries() {
    let omni = Omni::new();
    let space_id = omni.space("dashboards", "update-dashboards").await;

    for row in dashboard_patches() {
        let case = row.case;
        let created = omni
            .create_dashboard(
                case,
                &space_id,
                &row.initial_name,
                &row.initial_body,
                row.initial_description.as_deref(),
            )
            .await;

        let updated = omni
            .update_dashboard(&UpdateOmniDashboardInput {
                space_id: space_id.clone(),
                dashboard_id: created.dashboard_id.clone(),
                body: row.patch_body.clone(),
                name: row.patch_name.clone(),
                description: row.patch_description.clone(),
            })
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: UpdateOmniDashboard failed: {error:?}"));

        assert_eq!(
            (
                updated.name.as_str(),
                updated.body.as_str(),
                updated.description.as_deref()
            ),
            (
                row.expected_name.as_str(),
                row.expected_body.as_str(),
                row.expected_description.as_deref()
            ),
            "case {case:?}: the updated dashboard is {updated:?}"
        );
        assert_eq!(
            untouched_dashboard(&updated),
            untouched_dashboard(&created),
            "case {case:?}: a member UpdateOmniDashboard's input cannot name was changed"
        );

        let fetched = omni
            .stored_dashboard(case, &space_id, &created.dashboard_id)
            .await;
        assert_eq!(
            format!("{fetched:?}"),
            format!("{updated:?}"),
            "case {case:?}: GetOmniDashboard did not return the record the update persisted"
        );

        let moved = row.expected_name != row.initial_name
            || row.expected_body != row.initial_body
            || row.expected_description != row.initial_description;
        if moved {
            assert!(
                updated.updated_at.secs() >= created.updated_at.secs(),
                "case {case:?}: updatedAt {:?} precedes the value the create left, {:?}",
                updated.updated_at,
                created.updated_at
            );
        } else {
            assert_eq!(
                millis(updated.updated_at),
                millis(created.updated_at),
                "case {case:?}: a patch that changed nothing moved updatedAt to {:?}",
                updated.updated_at
            );
        }
    }
}

/// One row of the view patch table.
struct ViewPatch {
    case: &'static str,
    suffix: &'static str,
    initial_definition: String,
    initial_description: Option<String>,
    patch_definition: Option<String>,
    patch_description: Option<String>,
    expected_definition: String,
    expected_description: Option<String>,
}

/// The rows differ along WHICH members the patch carries and along the FORM of the SQL the
/// new definition is written as — projection, predicate, aggregate, set operation, common
/// table expression, a leading comment — because the rule is about the definition being
/// ONE query rather than about its text. The longest description is the bound of
/// `ViewDescription`'s `@length(max: 1024)`
/// (`smithy_query(query="shape",shape="ViewDescription")`).
fn view_patches() -> Vec<ViewPatch> {
    vec![
        ViewPatch {
            case: "the description alone is replaced",
            suffix: "one",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: Some("first".to_string()),
            patch_definition: None,
            patch_description: Some("second".to_string()),
            expected_definition: "SELECT * FROM logs".to_string(),
            expected_description: Some("second".to_string()),
        },
        ViewPatch {
            case: "the definition alone is replaced by a predicate query",
            suffix: "two",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: None,
            patch_definition: Some(
                "SELECT service, message FROM logs WHERE level = 'ERROR'".to_string(),
            ),
            patch_description: None,
            expected_definition: "SELECT service, message FROM logs WHERE level = 'ERROR'"
                .to_string(),
            expected_description: None,
        },
        ViewPatch {
            case: "the definition alone is replaced by an aggregate query",
            suffix: "three",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: Some("kept".to_string()),
            patch_definition: Some(
                "SELECT service, count(*) AS errors FROM logs GROUP BY service".to_string(),
            ),
            patch_description: None,
            expected_definition: "SELECT service, count(*) AS errors FROM logs GROUP BY service"
                .to_string(),
            expected_description: Some("kept".to_string()),
        },
        ViewPatch {
            case: "the definition alone is replaced by a set operation",
            suffix: "four",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: None,
            patch_definition: Some("SELECT id FROM logs UNION SELECT id FROM spans".to_string()),
            patch_description: None,
            expected_definition: "SELECT id FROM logs UNION SELECT id FROM spans".to_string(),
            expected_description: None,
        },
        ViewPatch {
            case: "both members at once, a common table expression and the longest description",
            suffix: "five",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: Some("first".to_string()),
            patch_definition: Some(
                "WITH errors AS (SELECT * FROM logs WHERE level = 'ERROR') SELECT count(*) FROM \
                 errors"
                    .to_string(),
            ),
            patch_description: Some("d".repeat(1024)),
            expected_definition:
                "WITH errors AS (SELECT * FROM logs WHERE level = 'ERROR') SELECT count(*) FROM \
                 errors"
                    .to_string(),
            expected_description: Some("d".repeat(1024)),
        },
        ViewPatch {
            case: "a definition behind a leading comment",
            suffix: "six",
            initial_definition: "SELECT 1".to_string(),
            initial_description: None,
            patch_definition: Some(
                "-- the day's errors\nSELECT count(*) FROM logs WHERE level = 'ERROR'".to_string(),
            ),
            patch_description: None,
            expected_definition:
                "-- the day's errors\nSELECT count(*) FROM logs WHERE level = 'ERROR'".to_string(),
            expected_description: None,
        },
        ViewPatch {
            case: "a request carrying no updatable member leaves the view whole",
            suffix: "seven",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: Some("kept".to_string()),
            patch_definition: None,
            patch_description: None,
            expected_definition: "SELECT * FROM logs".to_string(),
            expected_description: Some("kept".to_string()),
        },
        ViewPatch {
            case: "a patch asking for the values the view already holds",
            suffix: "eight",
            initial_definition: "SELECT * FROM logs".to_string(),
            initial_description: Some("kept".to_string()),
            patch_definition: Some("SELECT * FROM logs".to_string()),
            patch_description: Some("kept".to_string()),
            expected_definition: "SELECT * FROM logs".to_string(),
            expected_description: Some("kept".to_string()),
        },
    ]
}

#[tokio::test]
async fn update_view_persists_only_the_members_the_request_carries() {
    let omni = Omni::new();

    for row in view_patches() {
        let case = row.case;
        let name = format!("view.update-{}", row.suffix);
        let created = omni
            .create_view(
                case,
                &name,
                &row.initial_definition,
                row.initial_description.as_deref(),
            )
            .await;

        let updated = omni
            .update_view(&UpdateViewInput {
                name: name.clone(),
                definition: row.patch_definition.clone(),
                description: row.patch_description.clone(),
            })
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: UpdateView failed: {error:?}"));

        assert_eq!(
            (
                updated.definition.as_str(),
                updated.description.as_deref(),
                updated.r#type.as_str()
            ),
            (
                row.expected_definition.as_str(),
                row.expected_description.as_deref(),
                VIEW_TYPE_USER
            ),
            "case {case:?}: the updated view is {updated:?}"
        );
        assert_eq!(
            untouched_view(&updated),
            untouched_view(&created),
            "case {case:?}: a member UpdateView's input cannot name was changed"
        );

        let fetched = omni.stored_view(case, &name).await;
        assert_eq!(
            format!("{fetched:?}"),
            format!("{updated:?}"),
            "case {case:?}: GetView did not return the record the update persisted"
        );

        let moved = row.expected_definition != row.initial_definition
            || row.expected_description != row.initial_description;
        if moved {
            assert!(
                updated.updated_at.secs() >= created.updated_at.secs(),
                "case {case:?}: updatedAt {:?} precedes the value the create left, {:?}",
                updated.updated_at,
                created.updated_at
            );
        } else {
            assert_eq!(
                millis(updated.updated_at),
                millis(created.updated_at),
                "case {case:?}: a patch that changed nothing moved updatedAt to {:?}",
                updated.updated_at
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_a_record_the_store_does_not_hold_is_the_modelled_404() {
    let omni = Omni::new();
    let space_id = omni.space("misses", "update-misses").await;
    let other_space = omni.space("misses", "update-misses-other").await;
    let profile = omni
        .create_profile("misses", &space_id, "profile", None)
        .await;
    let dashboard = omni
        .create_dashboard("misses", &space_id, "dashboard", PANELS_BODY, None)
        .await;
    omni.create_view("misses", "view.update-miss", "SELECT 1", None)
        .await;

    // Rows: an id nothing names, a space nothing names, and a LIVE id addressed under
    // another space — a record is addressed by its space AND its id, so the third is as
    // much a miss as the first two.
    for (case, input) in [
        (
            "an unknown profileId",
            UpdateAccessProfileInput {
                space_id: space_id.clone(),
                profile_id: MISSING_PROFILE_ID.to_string(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        ),
        (
            "a profile under an unknown spaceId",
            UpdateAccessProfileInput {
                space_id: MISSING_SPACE_ID.to_string(),
                profile_id: profile.profile_id.clone(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        ),
        (
            "a live profile addressed under another space",
            UpdateAccessProfileInput {
                space_id: other_space.clone(),
                profile_id: profile.profile_id.clone(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        ),
    ] {
        expect_modelled_error(
            case,
            omni.update_profile(&input).await,
            404,
            "ResourceNotFoundException",
        );
    }

    for (case, input) in [
        (
            "an unknown dashboardId",
            UpdateOmniDashboardInput {
                space_id: space_id.clone(),
                dashboard_id: MISSING_DASHBOARD_ID.to_string(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        ),
        (
            "a live dashboard addressed under another space",
            UpdateOmniDashboardInput {
                space_id: other_space.clone(),
                dashboard_id: dashboard.dashboard_id.clone(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        ),
    ] {
        expect_modelled_error(
            case,
            omni.update_dashboard(&input).await,
            404,
            "ResourceNotFoundException",
        );
    }

    expect_modelled_error(
        "an unknown view name",
        omni.update_view(&UpdateViewInput {
            name: MISSING_VIEW_NAME.to_string(),
            description: Some("second".to_string()),
            ..Default::default()
        })
        .await,
        404,
        "ResourceNotFoundException",
    );

    // None of the refusals touched the records the store does hold.
    let stored_profile = omni
        .stored_profile("misses", &space_id, &profile.profile_id)
        .await;
    assert_eq!(
        stored_profile.description, None,
        "a refused patch wrote a description onto the profile: {stored_profile:?}"
    );
    let stored_dashboard = omni
        .stored_dashboard("misses", &space_id, &dashboard.dashboard_id)
        .await;
    assert_eq!(
        stored_dashboard.description, None,
        "a refused patch wrote a description onto the dashboard: {stored_dashboard:?}"
    );
    let stored_view = omni.stored_view("misses", "view.update-miss").await;
    assert_eq!(
        stored_view.description, None,
        "a refused patch wrote a description onto the view: {stored_view:?}"
    );
}

#[tokio::test]
async fn a_rename_onto_a_name_a_sibling_holds_is_the_modelled_conflict_and_writes_nothing() {
    let omni = Omni::new();
    let space_id = omni.space("conflict", "update-conflict").await;
    let elsewhere = omni.space("conflict", "update-conflict-other").await;

    let first = omni
        .create_profile("conflict", &space_id, "taken profile", None)
        .await;
    let second = omni
        .create_profile("conflict", &space_id, "free profile", Some("kept"))
        .await;
    // The same name in ANOTHER space identifies a different profile, so renaming onto it
    // is not a conflict.
    omni.create_profile("conflict", &elsewhere, "taken profile", None)
        .await;

    expect_modelled_error(
        "a profile renamed onto a sibling's name",
        omni.update_profile(&UpdateAccessProfileInput {
            space_id: space_id.clone(),
            profile_id: second.profile_id.clone(),
            name: Some("taken profile".to_string()),
            // The description rides along, so the refusal is shown to roll back the whole
            // patch rather than only the rename.
            description: Some("second".to_string()),
        })
        .await,
        409,
        "ConflictException",
    );
    let stored = omni
        .stored_profile("conflict", &space_id, &second.profile_id)
        .await;
    assert_eq!(
        (stored.name.as_str(), stored.description.as_deref()),
        ("free profile", Some("kept")),
        "the refused rename changed the profile: {stored:?}"
    );

    // Renaming a profile to the name it already holds is not a conflict with itself.
    let kept = omni
        .update_profile(&UpdateAccessProfileInput {
            space_id: space_id.clone(),
            profile_id: first.profile_id.clone(),
            name: Some("taken profile".to_string()),
            description: None,
        })
        .await
        .expect("renaming a profile to its own name succeeds")
        .access_profile;
    assert_eq!(
        kept.name, "taken profile",
        "the self-rename changed the name to {:?}",
        kept.name
    );
    // The same name is free in another space.
    let moved = omni
        .update_profile(&UpdateAccessProfileInput {
            space_id: elsewhere.clone(),
            profile_id: omni
                .create_profile("conflict", &elsewhere, "free profile", None)
                .await
                .profile_id,
            name: Some("renamed in the other space".to_string()),
            description: None,
        })
        .await
        .expect("a rename in another space succeeds")
        .access_profile;
    assert_eq!(
        moved.name, "renamed in the other space",
        "the rename in the other space answered {moved:?}"
    );

    let taken = omni
        .create_dashboard("conflict", &space_id, "taken-dashboard", PANELS_BODY, None)
        .await;
    let free = omni
        .create_dashboard(
            "conflict",
            &space_id,
            "free-dashboard",
            PANELS_BODY,
            Some("kept"),
        )
        .await;
    expect_modelled_error(
        "a dashboard renamed onto a sibling's name",
        omni.update_dashboard(&UpdateOmniDashboardInput {
            space_id: space_id.clone(),
            dashboard_id: free.dashboard_id.clone(),
            name: Some("taken-dashboard".to_string()),
            description: Some("second".to_string()),
            body: Some(ONE_PANEL_BODY.to_string()),
        })
        .await,
        409,
        "ConflictException",
    );
    let stored = omni
        .stored_dashboard("conflict", &space_id, &free.dashboard_id)
        .await;
    assert_eq!(
        (
            stored.name.as_str(),
            stored.body.as_str(),
            stored.description.as_deref()
        ),
        ("free-dashboard", PANELS_BODY, Some("kept")),
        "the refused rename changed the dashboard: {stored:?}"
    );
    let kept = omni
        .update_dashboard(&UpdateOmniDashboardInput {
            space_id: space_id.clone(),
            dashboard_id: taken.dashboard_id.clone(),
            name: Some("taken-dashboard".to_string()),
            ..Default::default()
        })
        .await
        .expect("renaming a dashboard to its own name succeeds");
    assert_eq!(
        kept.name, "taken-dashboard",
        "the self-rename changed the name to {:?}",
        kept.name
    );
}

#[tokio::test]
async fn a_definition_that_violates_its_own_rule_is_refused_and_the_record_is_untouched() {
    let omni = Omni::new();
    let space_id = omni.space("invalid", "update-invalid").await;
    let dashboard = omni
        .create_dashboard(
            "invalid",
            &space_id,
            "dashboard",
            PANELS_BODY,
            Some("kept"),
        )
        .await;

    // Rows span WHY a body is not a dashboard definition: it is not JSON, it is JSON of
    // the wrong kind, it carries no `panels` member, that member is not a list, an entry
    // of the list is not a panel object, an entry omits a member every panel must carry,
    // or the document carries a top-level member the closed schema does not declare.
    for (case, body) in [
        ("not JSON at all", "panels"),
        ("a JSON array", r#"[{"type":"logs"}]"#),
        ("no panels member", r#"{"widgets":[]}"#),
        ("panels as a string", r#"{"panels":"none"}"#),
        ("a panel that is a number", r#"{"panels":[1]}"#),
        ("a panel carrying neither required member", r#"{"panels":[{}]}"#),
        (
            "a panel carrying only its type",
            r#"{"panels":[{"type":"logs"}]}"#,
        ),
        (
            "a conforming list beside an undeclared member",
            r#"{"panels":[],"widgets":[]}"#,
        ),
    ] {
        expect_modelled_error(
            case,
            omni.update_dashboard(&UpdateOmniDashboardInput {
                space_id: space_id.clone(),
                dashboard_id: dashboard.dashboard_id.clone(),
                body: Some(body.to_string()),
                // The description rides along, so the refusal is shown to roll back the
                // whole patch.
                description: Some("second".to_string()),
                ..Default::default()
            })
            .await,
            400,
            "ValidationException",
        );
        let stored = omni
            .stored_dashboard(case, &space_id, &dashboard.dashboard_id)
            .await;
        assert_eq!(
            (stored.body.as_str(), stored.description.as_deref()),
            (PANELS_BODY, Some("kept")),
            "case {case:?}: the refused patch changed the dashboard: {stored:?}"
        );
    }

    let name = "view.update-invalid";
    omni.create_view("invalid", name, "SELECT * FROM logs", Some("kept"))
        .await;
    // Rows span WHY a definition is not ONE query: it is not SQL, it is incomplete, it is
    // a statement that is not a query, or it is more than one statement.
    for (case, definition) in [
        ("prose", "the daily error count"),
        ("a truncated query", "SELECT * FROM"),
        ("a comment only", "-- nothing but a comment"),
        ("a mutation", "DELETE FROM logs WHERE level = 'ERROR'"),
        ("a DDL statement", "CREATE TABLE logs (id INT)"),
        ("two queries", "SELECT 1; SELECT 2"),
    ] {
        expect_modelled_error(
            case,
            omni.update_view(&UpdateViewInput {
                name: name.to_string(),
                definition: Some(definition.to_string()),
                description: Some("second".to_string()),
            })
            .await,
            400,
            "ValidationException",
        );
        let stored = omni.stored_view(case, name).await;
        assert_eq!(
            (stored.definition.as_str(), stored.description.as_deref()),
            ("SELECT * FROM logs", Some("kept")),
            "case {case:?}: the refused patch changed the view: {stored:?}"
        );
    }
}

#[tokio::test]
async fn an_update_replayed_answers_the_same_record_and_later_patches_compose() {
    // All three operations are modelled `@idempotent`: repeating one patch answers the
    // same record, and a later patch of another member keeps what the earlier one set.
    let omni = Omni::new();
    let space_id = omni.space("compose", "update-compose").await;

    let profile = omni
        .create_profile("compose", &space_id, "profile", None)
        .await;
    let patch = UpdateAccessProfileInput {
        space_id: space_id.clone(),
        profile_id: profile.profile_id.clone(),
        description: Some("second".to_string()),
        ..Default::default()
    };
    let first = omni
        .update_profile(&patch)
        .await
        .expect("the profile patch succeeds")
        .access_profile;
    let replayed = omni
        .update_profile(&patch)
        .await
        .expect("replaying the profile patch succeeds")
        .access_profile;
    assert_eq!(
        format!("{replayed:?}"),
        format!("{first:?}"),
        "an @idempotent patch replayed must answer the same profile, got {replayed:?}"
    );
    let renamed = omni
        .update_profile(&UpdateAccessProfileInput {
            space_id: space_id.clone(),
            profile_id: profile.profile_id.clone(),
            name: Some("profile renamed".to_string()),
            ..Default::default()
        })
        .await
        .expect("the rename succeeds")
        .access_profile;
    assert_eq!(
        (renamed.name.as_str(), renamed.description.as_deref()),
        ("profile renamed", Some("second")),
        "a later patch dropped what the earlier one set: {renamed:?}"
    );

    let dashboard = omni
        .create_dashboard("compose", &space_id, "dashboard", PANELS_BODY, None)
        .await;
    let patch = UpdateOmniDashboardInput {
        space_id: space_id.clone(),
        dashboard_id: dashboard.dashboard_id.clone(),
        description: Some("second".to_string()),
        ..Default::default()
    };
    let first = omni
        .update_dashboard(&patch)
        .await
        .expect("the dashboard patch succeeds");
    let replayed = omni
        .update_dashboard(&patch)
        .await
        .expect("replaying the dashboard patch succeeds");
    assert_eq!(
        format!("{replayed:?}"),
        format!("{first:?}"),
        "an @idempotent patch replayed must answer the same dashboard, got {replayed:?}"
    );
    let rebodied = omni
        .update_dashboard(&UpdateOmniDashboardInput {
            space_id: space_id.clone(),
            dashboard_id: dashboard.dashboard_id.clone(),
            body: Some(ONE_PANEL_BODY.to_string()),
            ..Default::default()
        })
        .await
        .expect("the body patch succeeds");
    assert_eq!(
        (rebodied.body.as_str(), rebodied.description.as_deref()),
        (ONE_PANEL_BODY, Some("second")),
        "a later patch dropped what the earlier one set: {rebodied:?}"
    );

    let name = "view.update-compose";
    omni.create_view("compose", name, "SELECT * FROM logs", None)
        .await;
    let patch = UpdateViewInput {
        name: name.to_string(),
        description: Some("second".to_string()),
        ..Default::default()
    };
    let first = omni
        .update_view(&patch)
        .await
        .expect("the view patch succeeds");
    let replayed = omni
        .update_view(&patch)
        .await
        .expect("replaying the view patch succeeds");
    assert_eq!(
        format!("{replayed:?}"),
        format!("{first:?}"),
        "an @idempotent patch replayed must answer the same view, got {replayed:?}"
    );
    let redefined = omni
        .update_view(&UpdateViewInput {
            name: name.to_string(),
            definition: Some("SELECT count(*) FROM spans".to_string()),
            ..Default::default()
        })
        .await
        .expect("the definition patch succeeds");
    assert_eq!(
        (
            redefined.definition.as_str(),
            redefined.description.as_deref()
        ),
        ("SELECT count(*) FROM spans", Some("second")),
        "a later patch dropped what the earlier one set: {redefined:?}"
    );
}

#[tokio::test]
async fn an_update_and_the_read_that_follows_it_agree_over_the_rpcv2cbor_wire() {
    let omni = Omni::new();
    // The parents are created through the handlers holding the same engine the router's
    // service holds, which is what makes the wire calls below see them.
    let space_id = omni.space("wire", "update-wire").await;
    let created = omni
        .create_profile("wire", &space_id, "wire profile", Some("first"))
        .await;

    let codec = CborCodec::default();
    let (status, body) = omni
        .over_the_wire(
            "UpdateAccessProfile",
            UPDATE_ACCESS_PROFILE_INPUT_SCHEMA,
            &UpdateAccessProfileInput {
                space_id: space_id.clone(),
                profile_id: created.profile_id.clone(),
                description: Some("second".to_string()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        status,
        200,
        "UpdateAccessProfile over the wire answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let updated = {
        let mut deserializer = codec.create_deserializer(&body);
        UpdateAccessProfileOutput::deserialize(&mut deserializer)
            .expect("the output deserializes")
            .access_profile
    };
    assert_eq!(
        updated.description.as_deref(),
        Some("second"),
        "the wire update answered {updated:?}"
    );

    let (status, body) = omni
        .over_the_wire(
            "GetAccessProfile",
            GET_ACCESS_PROFILE_INPUT_SCHEMA,
            &GetAccessProfileInput {
                space_id: space_id.clone(),
                profile_id: created.profile_id.clone(),
            },
        )
        .await;
    assert_eq!(
        status,
        200,
        "GetAccessProfile over the wire answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let fetched = {
        let mut deserializer = codec.create_deserializer(&body);
        GetAccessProfileOutput::deserialize(&mut deserializer)
            .expect("the output deserializes")
            .access_profile
    };
    assert_eq!(
        (fetched.description.as_deref(), fetched.name.as_str()),
        (Some("second"), "wire profile"),
        "the wire read answered the pre-update record: {fetched:?}"
    );
    assert_eq!(
        untouched_profile(&fetched),
        untouched_profile(&created),
        "the wire update changed a member its input cannot name"
    );
}
