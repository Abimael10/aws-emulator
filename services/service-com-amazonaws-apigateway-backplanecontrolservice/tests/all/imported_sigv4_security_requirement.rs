// SPDX-License-Identifier: CC0-1.0

//! What an imported method's AUTHORIZATION is, over `ImportRestApi` and `PutRestApi`.
//!
//! The rule under test: **an imported operation is created under the authorization its
//! security requirement names, where that authorization needs nothing created for it: the
//! caller's own SigV4 credential is a security SCHEME — the scheme whose
//! `x-amazon-apigateway-authtype` names SigV4 — so a method requiring it is imported with
//! `authorizationType` `AWS_IAM` and no authorizer, whether the requirement is the
//! operation's own or the document's. A requirement an operation does not state is the
//! DOCUMENT's, and an operation that states the EMPTY requirement needs no authorization at
//! all. A requirement naming a scheme an authorizer would have to serve is what the import
//! reports it cannot carry over.**
//!
//! Why it matters: the definition is what an api is created FROM — a CloudFormation or AWS SAM
//! deployment imports one — so an import that drops a method's SigV4 requirement deploys that
//! route open to anonymous callers, while the document the client wrote says every caller must
//! sign.
//!
//! The rows differ along what the rule generalises over: where the requirement is declared
//! (on the operation, across the document, and the operation's empty override), how the scheme
//! set is spelled (Swagger 2.0's `securityDefinitions` and OpenAPI 3's
//! `components.securitySchemes`), the NAME the SigV4 scheme is declared under (which is the
//! document's to choose — the authentication type is what says what it stands for), the verb
//! and path the method is declared on, and a requirement naming a scheme that is NOT SigV4.
//!
//! `rest_api_import_security_warning.rs` owns the diagnostic a requirement the import cannot
//! carry over earns; this suite is about the authorization it CAN.

use serde_json::{json, Value};

use crate::rest_api_lifecycle::{create_api, page_items, string_member, Gateway};

/// The authentication type API Gateway names the caller's own SigV4 credential by — the
/// `x-amazon-apigateway-authtype` of a security scheme that stands for that authorization,
/// which is also the value this service's own EXPORT of an `AWS_IAM` method renders.
const AUTHTYPE_SIGV4: &str = "awsSigv4";

/// The `authorizationType` values this suite is stated about: the one whose caller signs the
/// request, and the open one.
const AWS_IAM: &str = "AWS_IAM";
const NONE: &str = "NONE";

/// Where a row declares its security requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declared {
    /// On the operation itself.
    OnTheOperation,
    /// Across the document, which every operation stating none of its own is imported under.
    AcrossTheDocument,
    /// Across the document, with the operation stating the EMPTY requirement over it.
    OverriddenByTheOperation,
}

/// One row: the document a definition import is given, and the authorization the method it
/// creates must carry.
struct Row {
    case: &'static str,
    /// Swagger 2.0 (`securityDefinitions`) when true, OpenAPI 3
    /// (`components.securitySchemes`) when false.
    swagger: bool,
    /// The name the scheme is declared under, which the document chooses.
    scheme_name: &'static str,
    /// The `x-amazon-apigateway-authtype` the scheme carries, if any.
    authtype: Option<&'static str>,
    declared: Declared,
    path: &'static str,
    operation_key: &'static str,
    verb: &'static str,
    /// The `authorizationType` the imported method must carry.
    authorization_type: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a SigV4 requirement on the operation, in a swagger document",
        swagger: true,
        scheme_name: "sigv4",
        authtype: Some(AUTHTYPE_SIGV4),
        declared: Declared::OnTheOperation,
        path: "/orders",
        operation_key: "post",
        verb: "POST",
        authorization_type: AWS_IAM,
    },
    Row {
        case: "a SigV4 requirement across an openapi document",
        swagger: false,
        scheme_name: "sigv4",
        authtype: Some(AUTHTYPE_SIGV4),
        declared: Declared::AcrossTheDocument,
        path: "/items/{id}",
        operation_key: "get",
        verb: "GET",
        authorization_type: AWS_IAM,
    },
    Row {
        case: "the same scheme declared under another name, on the catch-all verb",
        swagger: true,
        scheme_name: "AWS_IAM",
        authtype: Some(AUTHTYPE_SIGV4),
        declared: Declared::OnTheOperation,
        path: "/any",
        operation_key: "x-amazon-apigateway-any-method",
        verb: "ANY",
        authorization_type: AWS_IAM,
    },
    Row {
        case: "an operation overriding the document's SigV4 requirement with the empty one",
        swagger: false,
        scheme_name: "sigv4",
        authtype: Some(AUTHTYPE_SIGV4),
        declared: Declared::OverriddenByTheOperation,
        path: "/public",
        operation_key: "get",
        verb: "GET",
        authorization_type: NONE,
    },
    Row {
        case: "a requirement naming a scheme an authorizer would have to serve",
        swagger: true,
        scheme_name: "custom",
        authtype: Some("custom"),
        declared: Declared::OnTheOperation,
        path: "/gated",
        operation_key: "delete",
        verb: "DELETE",
        authorization_type: NONE,
    },
    Row {
        case: "a requirement naming a scheme that says what it stands for nowhere",
        swagger: true,
        scheme_name: "keyAuth",
        authtype: None,
        declared: Declared::OnTheOperation,
        path: "/keyed",
        operation_key: "put",
        verb: "PUT",
        authorization_type: NONE,
    },
];

/// The security scheme a row declares: the `apiKey` scheme every authorization API Gateway
/// carries over is declared as, with the authentication type the row states.
fn scheme(row: &Row) -> Value {
    let mut declared = json!({ "type": "apiKey", "name": "Authorization", "in": "header" });
    if let (Some(authtype), Some(members)) = (row.authtype, declared.as_object_mut()) {
        members.insert(
            "x-amazon-apigateway-authtype".to_string(),
            json!(authtype),
        );
    }
    declared
}

/// The definition document a row is imported from.
fn definition(row: &Row) -> String {
    let requirement = json!([{ row.scheme_name: [] }]);
    let mut operation = json!({ "responses": { "200": { "description": "ok" } } });
    match row.declared {
        Declared::OnTheOperation => operation["security"] = requirement.clone(),
        Declared::OverriddenByTheOperation => operation["security"] = json!([]),
        Declared::AcrossTheDocument => {}
    }
    let mut document = if row.swagger {
        json!({
            "swagger": "2.0",
            "info": { "title": row.case, "version": "2026-01-01" },
            "securityDefinitions": { row.scheme_name: scheme(row) },
        })
    } else {
        json!({
            "openapi": "3.0.1",
            "info": { "title": row.case, "version": "2026-01-01" },
            "components": { "securitySchemes": { row.scheme_name: scheme(row) } },
        })
    };
    if row.declared != Declared::OnTheOperation {
        document["security"] = requirement;
    }
    document["paths"] = json!({ row.path: { row.operation_key: operation } });
    document.to_string()
}

/// The resource of `api` whose full path is `path`.
async fn resource_id(gateway: &Gateway, api: &str, path: &str, case: &str) -> String {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/resources"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetResources expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let item = page_items(&answer.body, case)
        .into_iter()
        .find(|item| item["path"].as_str() == Some(path))
        .unwrap_or_else(|| panic!("case {case:?}: expected an imported resource at {path:?}"));
    string_member(&item, "id", case)
}

/// The method `verb` of the resource at `path` of `api`, read back through that service's own
/// read.
async fn imported_method(gateway: &Gateway, api: &str, row: &Row) -> Value {
    let resource = resource_id(gateway, api, row.path, row.case).await;
    let read = gateway
        .call(
            "GET",
            &format!("/restapis/{api}/resources/{resource}/methods/{}", row.verb),
            None,
        )
        .await;
    assert_eq!(
        read.status, 200,
        "case {:?}: GetMethod({}) expected 200, got {} with {:?}",
        row.case, row.verb, read.status, read.body
    );
    read.body
}

/// The warnings an answered import reports. An absent member is no warnings: the service
/// omits an empty list.
fn warnings(body: &Value) -> Vec<String> {
    body["warnings"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_imported_method_carries_the_authorization_its_requirement_names() {
    let gateway = Gateway::new();
    for row in ROWS {
        let body = definition(row);
        let answer = gateway
            .call_raw("POST", "/restapis?mode=import", &body)
            .await;
        assert_eq!(
            answer.status, 201,
            "case {:?}: ImportRestApi expected 201, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        let api = string_member(&answer.body, "id", row.case);

        // ROUND TRIP: the method the import created reports the authorization through that
        // service's own read, which is what a client and the api's data plane go on.
        let method = imported_method(&gateway, &api, row).await;
        assert_eq!(
            method["authorizationType"].as_str(),
            Some(row.authorization_type),
            "case {:?}: expected {} on the imported method, got {:?}",
            row.case,
            row.authorization_type,
            method["authorizationType"]
        );
        // The SigV4 requirement needs no authorizer: nothing was created for it, and
        // nothing is referenced.
        assert!(
            method["authorizerId"].is_null(),
            "case {:?}: expected no authorizer on the imported method, got {:?}",
            row.case,
            method["authorizerId"]
        );

        // A method whose authorization WAS carried over is not reported as unsecured; one
        // whose requirement needs an authorizer still is.
        let unsecured = warnings(&answer.body)
            .into_iter()
            .any(|warning| warning.contains("will be not be secured"));
        let needs_an_authorizer = row.authorization_type == NONE
            && row.declared != Declared::OverriddenByTheOperation;
        assert_eq!(
            unsecured, needs_an_authorizer,
            "case {:?}: expected the unsecured-method diagnostic {needs_an_authorizer}, got \
             {:?}",
            row.case,
            warnings(&answer.body)
        );
    }
}

#[tokio::test]
async fn the_same_document_put_on_an_api_imports_the_same_authorization() {
    // `PutRestApi` reads the definition through the same reader as `ImportRestApi`, so a
    // method's authorization cannot differ between the two.
    let gateway = Gateway::new();
    for row in ROWS {
        let body = definition(row);
        let (api, _root) = create_api(&gateway, row.case).await;
        let answer = gateway
            .call_raw("PUT", &format!("/restapis/{api}?mode=overwrite"), &body)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: PutRestApi expected 200, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        let method = imported_method(&gateway, &api, row).await;
        assert_eq!(
            method["authorizationType"].as_str(),
            Some(row.authorization_type),
            "case {:?}: expected {} on the method the put created, got {:?}",
            row.case,
            row.authorization_type,
            method["authorizationType"]
        );
    }
}

#[tokio::test]
async fn a_requirement_naming_both_authorizations_takes_the_one_that_needs_nothing_created() {
    // A requirement may name several schemes. The caller's own credential is the one this
    // import can carry over, so a requirement that names it is carried over whatever else it
    // names.
    let case = "a requirement naming the SigV4 scheme and a custom one";
    let body = json!({
        "swagger": "2.0",
        "info": { "title": case, "version": "2026-01-01" },
        "securityDefinitions": {
            "sigv4": {
                "type": "apiKey",
                "name": "Authorization",
                "in": "header",
                "x-amazon-apigateway-authtype": AUTHTYPE_SIGV4,
            },
            "custom": {
                "type": "apiKey",
                "name": "Authorization",
                "in": "header",
                "x-amazon-apigateway-authtype": "custom",
            },
        },
        "paths": {
            "/both": {
                "get": {
                    "responses": {},
                    "security": [{ "custom": [] }, { "sigv4": [] }],
                },
            },
        },
    })
    .to_string();
    let gateway = Gateway::new();
    let answer = gateway
        .call_raw("POST", "/restapis?mode=import", &body)
        .await;
    assert_eq!(
        answer.status, 201,
        "case {case:?}: ImportRestApi expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    let api = string_member(&answer.body, "id", case);
    let row = Row {
        case,
        swagger: true,
        scheme_name: "sigv4",
        authtype: Some(AUTHTYPE_SIGV4),
        declared: Declared::OnTheOperation,
        path: "/both",
        operation_key: "get",
        verb: "GET",
        authorization_type: AWS_IAM,
    };
    let method = imported_method(&gateway, &api, &row).await;
    assert_eq!(
        method["authorizationType"].as_str(),
        Some(AWS_IAM),
        "case {case:?}: expected the SigV4 requirement to be carried over, got {:?}",
        method["authorizationType"]
    );
}
