// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares a LAKE FORMATION PERMISSION really grants it in Lake
//! Formation, and removing that resource revokes exactly what it granted.
//!
//! The rule under test: **a resource of either Lake Formation permission type IS one grant —
//! `AWS::LakeFormation::Permissions` and `AWS::LakeFormation::PrincipalPermissions` describe
//! the same grant under two spellings — so a template declaring one is accepted, the grant is
//! made through Lake Formation's own `GrantPermissions`, each type's `Ref` is the pair that
//! identifies the grant (the newer type's two read-only properties being its halves), and a
//! deployment that no longer declares the resource revokes the permissions it granted and
//! leaves every permission it did not grant in place. A declared permission outside the set
//! the type's schema states is refused BEFORE any of that: the stack rolls back and Lake
//! Formation never hears of the grant.** Four crates have to agree for either half to hold
//! (CloudFormation makes the calls; Lake Formation, AWS Glue and IAM serve them), so the
//! suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: which of the two types declares the
//! grant, which Data Catalog resource it is on (a database, a table), how many permissions it
//! carries, whether any of them carries the grant option, whether the declaration states the
//! Data Catalog id or leaves it to its documented default, and whether the resource is named
//! by a literal or by a `Ref` to the database of the same stack. The refusal rows differ along
//! which permission list carries the off-enum value and where in it.
//!
//! Every expected value is computed here from what the row declares — the role ARN from the
//! role name the template states, the Data Catalog ARN from the Region, the account and the
//! database the grant names — never read back out of the answer being checked.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{block_of, elements, form_encode, members_of, opt_element, outputs_of};

/// How long a stack operation is given to reach a terminal status. Generous: what the suite
/// waits for is the CONDITION (a status that is not `…_IN_PROGRESS`), polled, so a slower host
/// takes more polls rather than failing.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often the stack is read while it settles.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The account id every ARN this emulator mints carries, which is also the Data Catalog id
/// every catalog call defaults to.
const ACCOUNT: &str = "000000000000";

/// The Region every request below is signed for.
const REGION: &str = "us-east-2";

/// The group a database is created holding `ALL` for in an account under IAM access control,
/// which is the permission a grant of this stack's own did NOT make and so must survive it.
const IAM_ALLOWED_PRINCIPALS: &str = "IAM_ALLOWED_PRINCIPALS";
const ALL: &str = "ALL";

/// The values `AWS::LakeFormation::PrincipalPermissions`' schema states for the items of its
/// two permission lists, in the order it states them: Lake Formation's own `Permission`
/// (`smithy_query(query="shape", shape="Permission")` of the `lakeformation` model) in that
/// enum's declaration order, as the type's schema carries them — the three the model has
/// added since (`CREATE_LF_TAG_EXPRESSION`, `CREATE_CATALOG`, `SUPER_USER`) are no part of
/// this type's schema. This is the set a refusal of a value outside it lists back.
const PERMISSION_VALUES: &[&str] = &[
    "ALL",
    "SELECT",
    "ALTER",
    "DROP",
    "DELETE",
    "INSERT",
    "DESCRIBE",
    "CREATE_DATABASE",
    "CREATE_TABLE",
    "DATA_LOCATION_ACCESS",
    "CREATE_LF_TAG",
    "ASSOCIATE",
    "GRANT_WITH_LF_TAG_EXPRESSION",
];

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lakeformation_awslakeformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from. The signature is never checked:
/// this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Lake Formation operation over its `restJson1` wire, on the URI that operation's
/// own `@http` binding declares (`POST /<operation>`).
async fn lakeformation(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("lakeformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lakeformation"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack as it reads once its operation has reached a TERMINAL status, polled rather than
/// slept for: how many spans a deployment takes is the deployment's to decide and not this
/// suite's to assume.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let began = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || began.elapsed() >= PATIENCE {
            return described;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `(Permissions, PermissionsWithGrantOption)` Lake Formation reports for `principal` on
/// `resource`, one pair per record it holds there, in the order the listing reports them.
///
/// Filtered by the RESOURCE alone: a listing that names a principal and a resource together
/// "returns effective permissions rather than the explicitly granted permissions", which
/// would add what the IAM-allowed group holds, and what this suite asserts about is the grant
/// the stack itself made. The entries are then narrowed to the principal in hand.
async fn held_by(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    resource: &Value,
) -> Vec<(Vec<String>, Vec<String>)> {
    let (status, body) = lakeformation(
        router,
        "ListPermissions",
        &json!({ "Resource": resource }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListPermissions to answer 200, got {status} {body}"
    );
    reported_entries(&body)
        .into_iter()
        .filter(|(held_by, _, _)| held_by == principal)
        .map(|(_, permissions, with_grant_option)| (permissions, with_grant_option))
        .collect()
}

/// Every record one `ListPermissions` answer reports, as `(principal, permissions, with grant
/// option)`.
fn reported_entries(body: &Value) -> Vec<(String, Vec<String>, Vec<String>)> {
    body.get("PrincipalResourcePermissions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("ListPermissions must carry a PrincipalResourcePermissions list, got {body}")
        })
        .iter()
        .map(|entry| {
            (
                entry
                    .get("Principal")
                    .and_then(|principal| principal.get("DataLakePrincipalIdentifier"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                permission_list(entry, "Permissions"),
                permission_list(entry, "PermissionsWithGrantOption"),
            )
        })
        .collect()
}

/// One permission list of a reported record, in the order it is reported.
fn permission_list(entry: &Value, member: &str) -> Vec<String> {
    entry
        .get(member)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The ARN of the IAM role a row's template creates, which is the grant's principal: the total
/// function of the role name IAM builds one from.
fn role_arn(role: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{role}")
}

/// The ARN of a Data Catalog database, which is the resource half of the legacy type's
/// identifier.
fn database_arn(database: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT}:database/{database}")
}

/// The ARN of a Data Catalog table.
fn table_arn(database: &str, table: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT}:table/{database}/{table}")
}

/// Which Data Catalog resource a row's grant is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum On {
    Database,
    Table,
}

/// One row of the round-trip table.
struct Row {
    case: &'static str,
    /// The stack, the role and the database the row deploys, each its own so the rows share
    /// nothing.
    stack: &'static str,
    role: &'static str,
    database: &'static str,
    /// Whether the newer type declares the grant.
    newer: bool,
    /// The Data Catalog resource it is on.
    on: On,
    /// Whether the declaration states the Data Catalog id or leaves it to its default.
    states_catalog: bool,
    /// Whether the resource names the database by a `Ref` to the stack's own database rather
    /// than by its name.
    by_ref: bool,
    permissions: &'static [&'static str],
    with_grant_option: &'static [&'static str],
    /// What a SECOND deployment declares for the same grant, before the resource is removed
    /// — `None` for a row that goes straight to removing it.
    changed: Option<Changed>,
}

/// The in-place change one row's second deployment makes to its grant: the lists it then
/// declares, and what Lake Formation holds once the change has been applied.
struct Changed {
    permissions: &'static [&'static str],
    with_grant_option: &'static [&'static str],
    /// What the record then holds, in the order it holds it: what it KEPT, in the order it was
    /// first granted, and then what the change added — a changed declaration reaches Lake
    /// Formation as the revoke of what it no longer carries and the grant of what it adds.
    held: &'static [&'static str],
    held_with_grant_option: &'static [&'static str],
}

/// The table in the database a `Table` row grants on.
const TABLE: &str = "orders";

const ROWS: &[Row] = &[
    Row {
        case: "the legacy type on a database, one permission",
        stack: "lfperm-legacy-database",
        role: "lfperm-legacy-database-role",
        database: "lfperm_legacy_database",
        newer: false,
        on: On::Database,
        states_catalog: false,
        by_ref: true,
        permissions: &["DESCRIBE"],
        with_grant_option: &[],
        // Widened in place: the grant keeps `DESCRIBE` and gains `ALTER`.
        changed: Some(Changed {
            permissions: &["DESCRIBE", "ALTER"],
            with_grant_option: &[],
            held: &["DESCRIBE", "ALTER"],
            held_with_grant_option: &[],
        }),
    },
    Row {
        case: "the newer type on a database named with its catalog id",
        stack: "lfperm-newer-database",
        role: "lfperm-newer-database-role",
        database: "lfperm_newer_database",
        newer: true,
        on: On::Database,
        states_catalog: true,
        by_ref: false,
        permissions: &["DESCRIBE"],
        with_grant_option: &[],
        changed: None,
    },
    Row {
        case: "the newer type granting two permissions, one of them onward",
        stack: "lfperm-newer-grant-option",
        role: "lfperm-newer-grant-option-role",
        database: "lfperm_newer_grant_option",
        newer: true,
        on: On::Database,
        states_catalog: false,
        by_ref: true,
        permissions: &["CREATE_TABLE", "DESCRIBE"],
        with_grant_option: &["DESCRIBE"],
        // Narrowed AND widened at once, and the grant option withdrawn: `CREATE_TABLE` goes,
        // `SELECT` arrives, `DESCRIBE` stays and stops being passable onward.
        changed: Some(Changed {
            permissions: &["DESCRIBE", "SELECT"],
            with_grant_option: &[],
            held: &["DESCRIBE", "SELECT"],
            held_with_grant_option: &[],
        }),
    },
    Row {
        case: "the legacy type granting two permissions",
        stack: "lfperm-legacy-two",
        role: "lfperm-legacy-two-role",
        database: "lfperm_legacy_two",
        newer: false,
        on: On::Database,
        states_catalog: true,
        by_ref: true,
        permissions: &["ALTER", "DROP"],
        with_grant_option: &[],
        // Narrowed in place: `DROP` goes and `ALTER` stays.
        changed: Some(Changed {
            permissions: &["ALTER"],
            with_grant_option: &[],
            held: &["ALTER"],
            held_with_grant_option: &[],
        }),
    },
    Row {
        case: "the newer type on a table of the database",
        stack: "lfperm-newer-table",
        role: "lfperm-newer-table-role",
        database: "lfperm_newer_table",
        newer: true,
        on: On::Table,
        states_catalog: false,
        by_ref: true,
        permissions: &["ALTER", "DESCRIBE"],
        with_grant_option: &[],
        changed: None,
    },
];

/// The trust policy every row's role carries: Lake Formation grants permissions to an IAM
/// principal, and the one this stack creates is a role AWS Glue acts as.
fn glue_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "glue.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The database the grant is made on, named as the row declares it.
fn declared_database(row: &Row) -> Value {
    if row.by_ref {
        json!({ "Ref": "Lake" })
    } else {
        json!(row.database)
    }
}

/// The `Resource` the row's grant is on, in LAKE FORMATION's own spelling — which is what the
/// newer type declares, and what a listing filters on.
fn lake_formation_resource(row: &Row, resolved: bool) -> Value {
    let database = if resolved {
        json!(row.database)
    } else {
        declared_database(row)
    };
    let catalog = row.states_catalog.then(|| json!(ACCOUNT));
    match row.on {
        On::Database => {
            let mut member = json!({ "Name": database });
            if let Some(catalog) = catalog {
                member["CatalogId"] = catalog;
            }
            json!({ "Database": member })
        }
        On::Table => {
            let mut member = json!({ "DatabaseName": database, "Name": TABLE });
            if let Some(catalog) = catalog {
                member["CatalogId"] = catalog;
            }
            json!({ "Table": member })
        }
    }
}

/// The same resource in the LEGACY type's spelling, which names each member with a
/// `…Resource` suffix.
fn legacy_resource(row: &Row) -> Value {
    let resource = lake_formation_resource(row, false);
    let (named, member) = match row.on {
        On::Database => ("DatabaseResource", resource["Database"].clone()),
        On::Table => ("TableResource", resource["Table"].clone()),
    };
    json!({ named: member })
}

/// The grant resource a row declares, in the spelling of the type it declares it with,
/// carrying the permission lists given — the row's own on the first deployment, and the ones
/// its change declares on the second.
fn grant_resource(
    row: &Row,
    permissions: &[&str],
    with_grant_option: &[&str],
) -> Value {
    let principal = json!({ "DataLakePrincipalIdentifier": { "Fn::GetAtt": ["Analyst", "Arn"] } });
    if row.newer {
        let mut properties = json!({
            "Principal": principal,
            "Resource": lake_formation_resource(row, false),
            "Permissions": permissions,
            "PermissionsWithGrantOption": with_grant_option,
        });
        if row.states_catalog {
            properties["Catalog"] = json!(ACCOUNT);
        }
        json!({
            "Type": "AWS::LakeFormation::PrincipalPermissions",
            "Properties": properties,
        })
    } else {
        json!({
            "Type": "AWS::LakeFormation::Permissions",
            "Properties": {
                "DataLakePrincipal": principal,
                "Resource": legacy_resource(row),
                "Permissions": permissions,
                "PermissionsWithGrantOption": with_grant_option,
            },
        })
    }
}

/// The resources a row's template declares beside its grant: the role the grant is made to,
/// the Data Catalog database it is on, and — for a `Table` row — the table in it.
fn supporting_resources(row: &Row) -> Value {
    let mut resources = json!({
        "Analyst": {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": row.role,
                "AssumeRolePolicyDocument": glue_trust_policy(),
            },
        },
        "Lake": {
            "Type": "AWS::Glue::Database",
            "Properties": {
                "CatalogId": ACCOUNT,
                "DatabaseInput": { "Name": row.database },
            },
        },
    });
    if row.on == On::Table {
        resources["Orders"] = json!({
            "Type": "AWS::Glue::Table",
            "Properties": {
                "CatalogId": ACCOUNT,
                "DatabaseName": { "Ref": "Lake" },
                "TableInput": { "Name": TABLE },
            },
        });
    }
    resources
}

/// The template a row deploys: its supporting resources, the grant with the permission lists
/// `grant` names, and the outputs that read the grant back — `Ref` for every row, and the two
/// read-only properties the newer type publishes for a row that declares one. `None` is the
/// deployment that no longer declares the grant at all.
fn template(row: &Row, grant: Option<(&[&str], &[&str])>) -> String {
    let mut resources = supporting_resources(row);
    let mut outputs = json!({});
    if let Some((permissions, with_grant_option)) = grant {
        let mut grant = grant_resource(row, permissions, with_grant_option);
        // The grant is made ON a Data Catalog entity, so it cannot be made before that entity
        // exists; a row that names the database by `Ref` already says so, and one that names
        // it by its own name says it here.
        if !row.by_ref {
            grant["DependsOn"] = json!(["Lake"]);
        } else if row.on == On::Table {
            grant["DependsOn"] = json!(["Orders"]);
        }
        resources["Grant"] = grant;
        outputs["GrantRef"] = json!({ "Value": { "Ref": "Grant" } });
        if row.newer {
            outputs["PrincipalIdentifier"] =
                json!({ "Value": { "Fn::GetAtt": ["Grant", "PrincipalIdentifier"] } });
            outputs["ResourceIdentifier"] =
                json!({ "Value": { "Fn::GetAtt": ["Grant", "ResourceIdentifier"] } });
        }
    } else {
        // A deployment that declares the grant no longer has to carry an output of its own:
        // what it says about the resource is that the stack no longer holds it.
        outputs["Database"] = json!({ "Value": { "Ref": "Lake" } });
    }
    json!({ "Resources": resources, "Outputs": outputs }).to_string()
}

/// The two halves of the newer type's composite identifier, as the documents they are: its
/// `Ref` is the two joined, and its two read-only properties are each of them.
fn identity_halves(row: &Row, principal: &str) -> (Value, Value) {
    let resource = lake_formation_resource(row, true);
    let populated = match row.on {
        On::Database => "Database",
        On::Table => "Table",
    };
    let mut projected = json!({});
    // The members the type's `Resource` publishes: the one the declaration states carries its
    // value and the rest are present and null.
    for member in [
        "Catalog",
        "Database",
        "Table",
        "TableWithColumns",
        "DataLocation",
        "DataCellsFilter",
        "LFTag",
        "LFTagPolicy",
    ] {
        projected[member] = if member == populated {
            resource[populated].clone()
        } else {
            Value::Null
        };
    }
    (
        json!({ "DataLakePrincipalIdentifier": principal }),
        projected,
    )
}

/// One output value read as the JSON document it carries.
fn document(outputs: &std::collections::BTreeMap<String, String>, key: &str, case: &str) -> Value {
    let text = outputs
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"));
    serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("case {case:?}: {key} must carry a JSON document, got {text:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stack_declaring_a_lake_formation_permission_grants_it_and_gives_it_back_on_removal() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let principal = role_arn(row.role);
        let body = template(row, Some((row.permissions, row.with_grant_option)));
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the template declares only \
             types CloudFormation knows — got {status} with body {created}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {described}"
        );

        // The grant is really in Lake Formation: exactly the permissions the row declared,
        // held by the role the stack created, at the resource the declaration named.
        let filter = lake_formation_resource(row, true);
        let held = held_by(&router, case, &principal, &filter).await;
        let expected: Vec<(Vec<String>, Vec<String>)> = vec![(
            row.permissions.iter().map(|p| p.to_string()).collect(),
            row.with_grant_option
                .iter()
                .map(|p| p.to_string())
                .collect(),
        )];
        assert_eq!(
            held, expected,
            "case {case:?}: expected the stack's grant {expected:?} in Lake Formation, got \
             {held:?}"
        );

        // ...and what the stack PUBLISHES for it is the pair that identifies the grant.
        let outputs = outputs_of(&described);
        let reference = outputs
            .get("GrantRef")
            .unwrap_or_else(|| panic!("case {case:?}: no GrantRef output in {outputs:?}"));
        if row.newer {
            let (principal_half, resource_half) = identity_halves(row, &principal);
            let (reference_principal, reference_resource) = reference
                .split_once('|')
                .unwrap_or_else(|| panic!("case {case:?}: expected a composite Ref, got {reference:?}"));
            let halves: (Value, Value) = (
                serde_json::from_str(reference_principal).unwrap_or(Value::Null),
                serde_json::from_str(reference_resource).unwrap_or(Value::Null),
            );
            assert_eq!(
                halves,
                (principal_half.clone(), resource_half.clone()),
                "case {case:?}: expected the Ref to be the two halves of the grant's \
                 identifier, got {reference:?}"
            );
            assert_eq!(
                (
                    document(&outputs, "PrincipalIdentifier", case),
                    document(&outputs, "ResourceIdentifier", case),
                ),
                (principal_half, resource_half),
                "case {case:?}: expected the two read-only properties to be the halves of the \
                 identifier, got {outputs:?}"
            );
        } else {
            let resource = match row.on {
                On::Database => database_arn(row.database),
                On::Table => table_arn(row.database, TABLE),
            };
            assert_eq!(
                reference.as_str(),
                format!("LakeFormation-{principal}::{resource}"),
                "case {case:?}: expected the Ref to name the principal and the resource, got \
                 {outputs:?}"
            );
        }

        // A deployment that declares the SAME grant with different permission lists is
        // applied to the grant that exists: the revoke of what it no longer carries and the
        // grant of what it adds.
        if let Some(changed) = &row.changed {
            let body = template(row, Some((changed.permissions, changed.with_grant_option)));
            let (status, updated) = cloudformation(
                &router,
                "UpdateStack",
                &[
                    ("StackName", row.stack),
                    ("TemplateBody", &body),
                    ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from the changing UpdateStack, got {status} \
                 with body {updated}"
            );
            let described = settled_stack(&router, case, row.stack).await;
            assert_eq!(
                opt_element(&described, "StackStatus").as_deref(),
                Some("UPDATE_COMPLETE"),
                "case {case:?}: expected the changing update to complete, got {described}"
            );
            let held = held_by(&router, case, &principal, &filter).await;
            let expected: Vec<(Vec<String>, Vec<String>)> = vec![(
                changed.held.iter().map(|p| p.to_string()).collect(),
                changed
                    .held_with_grant_option
                    .iter()
                    .map(|p| p.to_string())
                    .collect(),
            )];
            assert_eq!(
                held, expected,
                "case {case:?}: expected the changed declaration to leave {expected:?} in Lake \
                 Formation, got {held:?}"
            );
        }

        // A deployment that no longer declares the grant revokes it...
        let body = template(row, None);
        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );
        let held = held_by(&router, case, &principal, &filter).await;
        assert_eq!(
            held,
            Vec::new(),
            "case {case:?}: expected the removed resource's grant to be revoked, got {held:?}"
        );

        // ...and leaves the permission it never granted exactly as it was: the `ALL` the
        // database was created holding for the IAM-allowed group.
        let database = json!({ "Database": { "Name": row.database } });
        let (status, body) =
            lakeformation(&router, "ListPermissions", &json!({ "Resource": database })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListPermissions to answer 200, got {status} {body}"
        );
        let group: Vec<(Vec<String>, Vec<String>)> = reported_entries(&body)
            .into_iter()
            .filter(|(held_by, _, _)| held_by == IAM_ALLOWED_PRINCIPALS)
            .map(|(_, permissions, with_grant_option)| (permissions, with_grant_option))
            .collect();
        assert_eq!(
            group,
            vec![(vec![ALL.to_string()], Vec::new())],
            "case {case:?}: expected the database's own default grant to survive the revoke, \
             got {group:?}"
        );
    }
}

/// One refusal row: which permission list carries a value the type's schema does not state,
/// which value it is, and where in that list it sits.
struct Refused {
    case: &'static str,
    stack: &'static str,
    property: &'static str,
    rejected: &'static str,
    /// The index of the rejected value in the list the row declares, which the reported path
    /// names.
    at: usize,
    permissions: &'static [&'static str],
    with_grant_option: &'static [&'static str],
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "the only permission granted is not one the schema states",
        stack: "lfperm-bad-only",
        property: "Permissions",
        rejected: "BOGUS_PERM",
        at: 0,
        permissions: &["BOGUS_PERM"],
        with_grant_option: &[],
    },
    Refused {
        case: "the second of two permissions is not one the schema states",
        stack: "lfperm-bad-second",
        property: "Permissions",
        rejected: "describe",
        at: 1,
        permissions: &["SELECT", "describe"],
        with_grant_option: &[],
    },
    Refused {
        case: "the grant-option list carries one the schema does not state",
        stack: "lfperm-bad-grant-option",
        property: "PermissionsWithGrantOption",
        rejected: "GRANT",
        at: 1,
        permissions: &["SELECT"],
        with_grant_option: &["SELECT", "GRANT"],
    },
    Refused {
        // A value Lake Formation's own enum carries that this type's schema does not: what a
        // template is held to is the schema's set.
        case: "a permission the API has added since the type's schema was published",
        stack: "lfperm-bad-newer-value",
        property: "Permissions",
        rejected: "SUPER_USER",
        at: 1,
        permissions: &["SELECT", "SUPER_USER"],
        with_grant_option: &[],
    },
];

/// The template a refusal row deploys: the same role and database every row carries, with a
/// newer-type grant whose permission lists are the row's own.
fn refused_template(row: &Refused) -> String {
    let template_row = Row {
        case: row.case,
        stack: row.stack,
        role: "lfperm-refused-role",
        database: "lfperm_refused",
        newer: true,
        on: On::Database,
        states_catalog: false,
        by_ref: true,
        permissions: row.permissions,
        with_grant_option: row.with_grant_option,
        changed: None,
    };
    let mut resources = supporting_resources(&template_row);
    resources["Grant"] = grant_resource(&template_row, row.permissions, row.with_grant_option);
    json!({ "Resources": resources }).to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_off_enum_permission_rolls_the_stack_back_before_anything_is_granted() {
    let router = build_router();

    for row in REFUSED {
        let case = row.case;
        let body = refused_template(row);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the request is valid and the \
             stack exists — got {status} with body {created}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the stack to roll back, got {described}"
        );

        // The detail of the refusal: the validation that failed, on the resource that failed
        // it, quoting the rejected value and the set its property admits.
        let (status, events) =
            cloudformation(&router, "DescribeEvents", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeEvents, got {status} with body \
             {events}"
        );
        let validations: Vec<Vec<String>> = members_of(&events)
            .into_iter()
            .filter(|event| block_of(event, "ValidationName").is_some())
            .map(|event| {
                [
                    "LogicalResourceId",
                    "ValidationName",
                    "ValidationStatus",
                    "ValidationFailureMode",
                    "ValidationStatusReason",
                    "ValidationPath",
                ]
                .iter()
                .map(|tag| elements(event, tag).into_iter().next().unwrap_or_default())
                .collect()
            })
            .collect();
        let expected: Vec<Vec<String>> = vec![vec![
            "Grant".to_string(),
            "PROPERTY_VALIDATION".to_string(),
            "FAILED".to_string(),
            "FAIL".to_string(),
            format!(
                "{} is not a valid enum value. Supported values: [{}]",
                row.rejected,
                PERMISSION_VALUES.join(", ")
            ),
            format!("/Resources/Grant/Properties/{}/{}", row.property, row.at),
        ]];
        assert_eq!(
            validations, expected,
            "case {case:?}: expected the validation result {expected:?}, got {validations:?}"
        );

        // ...and nothing of the kind reached Lake Formation: the grant the template described
        // was never asked for.
        let held = held_by(
            &router,
            case,
            &role_arn("lfperm-refused-role"),
            &json!({ "Database": { "Name": "lfperm_refused" } }),
        )
        .await;
        assert_eq!(
            held,
            Vec::new(),
            "case {case:?}: expected no grant for a resource the refused stack never created, \
             got {held:?}"
        );
    }
}
