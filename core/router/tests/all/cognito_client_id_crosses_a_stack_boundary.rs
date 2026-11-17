// SPDX-License-Identifier: CC0-1.0

//! An app client's identity carried from the stack that creates it to ANOTHER stack that
//! federates it, over `Export` and `Fn::ImportValue`.
//!
//! RULE under test: **the physical resource id of a resource whose identity its OWNING
//! service assigns is the identifier that service's own create answered with, and that is the
//! value every path a stack publishes it through carries** — `Ref`, `Fn::Sub`,
//! `DescribeStackResource`, a stack `Output` and the cross-stack export that output declares —
//! so a resource in any stack built from it is created with the minted identifier and the
//! constraint its own model puts on that member is satisfied by construction. For an app
//! client the identifier is the `ClientId` `CreateUserPoolClient` minted (`ClientIdType`,
//! `@length 1..128`, `@pattern ^[\w+]+$`,
//! `smithy_query(query="shape",shape="ClientIdType")`), never a name CloudFormation computed
//! from the stack name and the logical id: an identity pool's
//! `CognitoIdentityProviders[].ClientId` targets `CognitoIdentityProviderClientId`,
//! `@length 1..128`, `@pattern ^[\w_]+$`
//! (`smithy_query(query="shape",shape="CognitoIdentityProviderClientId")`), which a
//! hyphenated name cannot satisfy, so an importing stack wired on the published value reaches
//! `CREATE_COMPLETE` and the identity pool there really federates that client.
//!
//! The sibling suite `cloudformation_cognito_federation_estate.rs` pins the rule inside ONE
//! stack, where the value never leaves the deployment that minted it. The rows here differ
//! along what this suite adds to that: the value crosses a stack boundary, written on the
//! producing side with `Ref` and with `Fn::Sub`, read on the consuming side straight into a
//! resource property, nested inside `Fn::Join` to key a role mapping and echoed by an output
//! of the importing stack; the language of each template (JSON, YAML short forms); the number
//! of clients whose ids cross (one, two); whether the client's declaration names it or leaves
//! CloudFormation to name it; and the intrinsic the role mapping's key is written with
//! (`Fn::Join`'s long form over two imports, its YAML short form). Every expected value is
//! computed from the ids the owning services minted in the same run — nothing here is a
//! literal from a recording.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread, through the harness the sibling suite states once.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::json;

use super::cloudformation_cognito_federation_estate::{
    is_minted_client_id, is_minted_identity_pool_id, Wire, BOOT_STACK_SIZE, COGNITO_IDENTITY,
    COGNITO_IDP,
};
use super::cloudformation_provisions_cognito_user_pools::{is_minted_pool_id, output};
use super::query_wire::{blocks_of, element, elements};

/// One app client whose identity crosses the boundary: the producer output (and export) that
/// publishes it, the logical id of the client it belongs to, and the `ClientName` the
/// declaration states — `None` for a client CloudFormation names itself.
struct PublishedClient {
    output_key: &'static str,
    logical_id: &'static str,
    declared_name: Option<&'static str>,
}

/// One row: the producing stack, the consuming stack that imports from it, and what the rule
/// says must then hold of the value that travelled.
struct Row {
    case: &'static str,
    /// The Region both stacks deploy in, which is where every resource lives.
    region: &'static str,
    producer_stack: &'static str,
    producer_template: &'static str,
    /// The app clients the producer publishes, in no particular order.
    clients: &'static [PublishedClient],
    consumer_stack: &'static str,
    consumer_template: &'static str,
    /// The producer output key whose client id keys the consumer's one role mapping
    /// (`<provider name>:<client id>`), or `None` for a consumer that declares no mapping.
    /// Both rows key one by a client id that crossed the boundary; where a row's pool
    /// federates two clients, the mapping names the one stated here.
    mapping_keyed_by: Option<&'static str>,
    /// `(the consumer output key, the producer output key)` for a consumer that echoes an
    /// import of its own, or `None`.
    echoed_import: Option<(&'static str, &'static str)>,
    /// The export names, published by the producing stack, that carry an app client's id and
    /// that the consuming stack imports — which is what `ListImports` must then report that
    /// stack for.
    client_exports: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        // The reported wiring, split in two: the client's `Ref` leaves the producer as an
        // export, and the consumer reads it both straight into the identity pool's provider
        // and inside an `Fn::Join` that keys the role mapping by provider and client.
        case: "one client exported by Ref, imported into a provider and a role mapping",
        region: "eu-west-1",
        producer_stack: "cognito-client-exporter-ref",
        producer_template: r#"{
  "Resources": {
    "UserPool": {
      "Type": "AWS::Cognito::UserPool",
      "Properties": {"UserPoolName": "exported-client-directory"}
    },
    "UserPoolClient": {
      "Type": "AWS::Cognito::UserPoolClient",
      "Properties": {
        "UserPoolId": {"Ref": "UserPool"},
        "ClientName": "exported-client-app",
        "GenerateSecret": false,
        "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
      }
    }
  },
  "Outputs": {
    "UserPoolId": {
      "Value": {"Ref": "UserPool"},
      "Export": {"Name": "ref-exported-pool-id"}
    },
    "UserPoolClientId": {
      "Value": {"Ref": "UserPoolClient"},
      "Export": {"Name": "ref-exported-client-id"}
    },
    "ProviderName": {
      "Value": {"Fn::GetAtt": ["UserPool", "ProviderName"]},
      "Export": {"Name": "ref-exported-provider-name"}
    }
  }
}"#,
        clients: &[PublishedClient {
            output_key: "UserPoolClientId",
            logical_id: "UserPoolClient",
            declared_name: Some("exported-client-app"),
        }],
        consumer_stack: "cognito-client-importer-ref",
        consumer_template: r#"{
  "Resources": {
    "IdentityPool": {
      "Type": "AWS::Cognito::IdentityPool",
      "Properties": {
        "IdentityPoolName": "imported_client_identities",
        "AllowUnauthenticatedIdentities": false,
        "CognitoIdentityProviders": [{
          "ClientId": {"Fn::ImportValue": "ref-exported-client-id"},
          "ProviderName": {"Fn::ImportValue": "ref-exported-provider-name"},
          "ServerSideTokenCheck": false
        }]
      }
    },
    "AuthenticatedRole": {
      "Type": "AWS::IAM::Role",
      "Properties": {
        "AssumeRolePolicyDocument": {
          "Version": "2012-10-17",
          "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": "cognito-identity.amazonaws.com"},
            "Action": "sts:AssumeRoleWithWebIdentity",
            "Condition": {
              "StringEquals": {
                "cognito-identity.amazonaws.com:aud": {"Ref": "IdentityPool"}
              }
            }
          }]
        }
      }
    },
    "IdentityPoolRoleAttachment": {
      "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
      "Properties": {
        "IdentityPoolId": {"Ref": "IdentityPool"},
        "Roles": {"authenticated": {"Fn::GetAtt": ["AuthenticatedRole", "Arn"]}},
        "RoleMappings": {
          "cognitoProvider": {
            "Type": "Token",
            "AmbiguousRoleResolution": "AuthenticatedRole",
            "IdentityProvider": {"Fn::Join": [":", [
              {"Fn::ImportValue": "ref-exported-provider-name"},
              {"Fn::ImportValue": "ref-exported-client-id"}
            ]]}
          }
        }
      }
    }
  },
  "Outputs": {
    "IdentityPoolId": {"Value": {"Ref": "IdentityPool"}},
    "RoleName": {"Value": {"Ref": "AuthenticatedRole"}},
    "FederatedClientId": {"Value": {"Fn::ImportValue": "ref-exported-client-id"}}
  }
}"#,
        mapping_keyed_by: Some("UserPoolClientId"),
        echoed_import: Some(("FederatedClientId", "UserPoolClientId")),
        client_exports: &["ref-exported-client-id"],
    },
    Row {
        // TWO clients of one pool, one of them a client CloudFormation names itself, exported
        // through `Fn::Sub` and imported by YAML short forms into one identity pool: the rule
        // holds per client, so the pool federates both minted ids and neither is a name.
        case: "two clients exported by Fn::Sub, both imported by YAML short forms",
        region: "ap-southeast-2",
        producer_stack: "cognito-client-exporter-sub",
        producer_template: r#"AWSTemplateFormatVersion: "2010-09-09"
Resources:
  UserPool:
    Type: AWS::Cognito::UserPool
    Properties:
      UserPoolName: two-client-directory
  WebClient:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref UserPool
      ClientName: two-client-web
      GenerateSecret: false
  MobileClient:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref UserPool
      GenerateSecret: false
Outputs:
  UserPoolId:
    Value: !Sub '${UserPool}'
    Export:
      Name: sub-exported-pool-id
  WebClientId:
    Value: !Sub '${WebClient}'
    Export:
      Name: sub-exported-web-client-id
  MobileClientId:
    Value: !Sub '${MobileClient}'
    Export:
      Name: sub-exported-mobile-client-id
  ProviderName:
    Value: !GetAtt UserPool.ProviderName
    Export:
      Name: sub-exported-provider-name
"#,
        clients: &[
            PublishedClient {
                output_key: "WebClientId",
                logical_id: "WebClient",
                declared_name: Some("two-client-web"),
            },
            PublishedClient {
                output_key: "MobileClientId",
                logical_id: "MobileClient",
                declared_name: None,
            },
        ],
        consumer_stack: "cognito-client-importer-sub",
        consumer_template: r#"AWSTemplateFormatVersion: "2010-09-09"
Resources:
  IdentityPool:
    Type: AWS::Cognito::IdentityPool
    Properties:
      IdentityPoolName: two_client_identities
      AllowUnauthenticatedIdentities: false
      CognitoIdentityProviders:
        - ClientId: !ImportValue sub-exported-web-client-id
          ProviderName: !ImportValue sub-exported-provider-name
        - ClientId: !ImportValue sub-exported-mobile-client-id
          ProviderName: !ImportValue sub-exported-provider-name
  AuthenticatedRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal: { Federated: cognito-identity.amazonaws.com }
            Action: sts:AssumeRoleWithWebIdentity
            Condition:
              StringEquals: { "cognito-identity.amazonaws.com:aud": !Ref IdentityPool }
  IdentityPoolRoleAttachment:
    Type: AWS::Cognito::IdentityPoolRoleAttachment
    Properties:
      IdentityPoolId: !Ref IdentityPool
      Roles: { authenticated: !GetAtt AuthenticatedRole.Arn }
      RoleMappings:
        webClient:
          Type: Token
          AmbiguousRoleResolution: AuthenticatedRole
          IdentityProvider: !Join
            - ":"
            - - !ImportValue sub-exported-provider-name
              - !ImportValue sub-exported-web-client-id
Outputs:
  IdentityPoolId:
    Value: !Ref IdentityPool
  RoleName:
    Value: !Ref AuthenticatedRole
"#,
        mapping_keyed_by: Some("WebClientId"),
        echoed_import: None,
        client_exports: &[
            "sub-exported-web-client-id",
            "sub-exported-mobile-client-id",
        ],
    },
];

/// Every row: the client id the producer publishes is the one Amazon Cognito minted, the
/// stack's own resource read reports the same value, the owning service answers for it, and
/// the importing stack built on it completes and federates exactly those clients.
#[test]
fn an_app_clients_minted_id_is_what_an_importing_stack_federates() {
    std::thread::Builder::new()
        .name("cognito-client-id-across-stacks".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let wire = Wire {
                router: &router,
                runtime: &runtime,
            };
            for row in ROWS {
                drive(&wire, row);
            }
        })
        .expect("spawn the cross-stack client-id thread")
        .join()
        .expect("an app client's minted id must be what an importing stack federates");
}

/// One row, from the producing deploy to what the consuming stack's identity pool holds.
fn drive(wire: &Wire<'_>, row: &Row) {
    let case = row.case;
    let region = row.region;

    let published = wire.deploy(region, row.producer_stack, row.producer_template, case);
    let pool = output(&published, "UserPoolId", case);
    assert!(
        is_minted_pool_id(&pool, region),
        "case {case:?}: expected the published user-pool handle to be the id Amazon Cognito \
         minted, got {pool:?}"
    );
    // The provider name an identity pool federates a user pool under, computed from the
    // published handle rather than read back out of an answer being checked.
    let provider = format!("cognito-idp.{region}.amazonaws.com/{pool}");
    assert_eq!(
        output(&published, "ProviderName", case),
        provider,
        "case {case:?}: expected the published provider name to be the one built on the \
         minted pool id"
    );

    let mut minted_ids: BTreeSet<String> = BTreeSet::new();
    for client in row.clients {
        let key = client.output_key;
        let id = output(&published, key, case);
        assert!(
            is_minted_client_id(&id),
            "case {case:?}, client {key:?}: expected the published id to be one ClientIdType \
             admits (word characters and '+', at most 128 of them), got {id:?}"
        );
        if let Some(declared) = client.declared_name {
            assert_ne!(
                id, declared,
                "case {case:?}, client {key:?}: expected the published id to be the minted \
                 ClientId, not the declared ClientName"
            );
        }
        assert!(
            minted_ids.insert(id.clone()),
            "case {case:?}, client {key:?}: expected an id of its own per app client, {id:?} \
             was published for another client too"
        );

        // A `Ref` IS the physical resource id, so the stack's own single-resource read cannot
        // report anything else for the client.
        let described = wire.cloudformation(
            region,
            "DescribeStackResource",
            &[
                ("StackName", row.producer_stack),
                ("LogicalResourceId", client.logical_id),
            ],
        );
        assert_eq!(
            described.status,
            200,
            "case {case:?}, client {key:?}: expected DescribeStackResource to answer, got {} \
             with body {:?}",
            described.status,
            described.text()
        );
        let body = described.text();
        let reported = element(&body, "PhysicalResourceId");
        assert_eq!(
            reported, id,
            "case {case:?}, client {key:?}: expected the reported physical id to be the \
             published {id:?}, got {reported:?}"
        );

        // The round trip: the owning service answers for that id, with the client the
        // declaration asked for, in the pool the stack published.
        let answered = wire.rpc_ok(
            COGNITO_IDP,
            true,
            region,
            "DescribeUserPoolClient",
            &json!({"UserPoolId": &pool, "ClientId": &id}),
            case,
        );
        let held = &answered["UserPoolClient"];
        assert_eq!(
            held["ClientId"].as_str(),
            Some(id.as_str()),
            "case {case:?}, client {key:?}: expected the app client the published id names to \
             carry it as its ClientId, got {answered}"
        );
        assert_eq!(
            held["UserPoolId"].as_str(),
            Some(pool.as_str()),
            "case {case:?}, client {key:?}: expected the app client to live in the pool the \
             stack published, got {answered}"
        );
        if let Some(declared) = client.declared_name {
            assert_eq!(
                held["ClientName"].as_str(),
                Some(declared),
                "case {case:?}, client {key:?}: expected the declared ClientName to be the \
                 name the owning service holds, got {answered}"
            );
        }
    }

    // The consuming stack: every provider of its identity pool is wired on a value that
    // crossed the boundary, so a published name instead of a minted id would be refused by
    // `CognitoIdentityProviders[].ClientId` and roll this stack back.
    let imported = wire.deploy(region, row.consumer_stack, row.consumer_template, case);
    let identity_pool = output(&imported, "IdentityPoolId", case);
    assert!(
        is_minted_identity_pool_id(&identity_pool, region),
        "case {case:?}: expected the importing stack's identity-pool handle to be the \
         REGION:GUID Amazon Cognito Identity minted, got {identity_pool:?}"
    );
    if let Some((consumer_key, producer_key)) = row.echoed_import {
        let echoed = output(&imported, consumer_key, case);
        assert_eq!(
            echoed,
            output(&published, producer_key, case),
            "case {case:?}: expected the importing stack's own output to echo the client id \
             the producing stack exported"
        );
    }

    // The import is RECORDED against the export it resolved, which is what says the consuming
    // stack is wired on the producing stack's value rather than on one of its own.
    for export in row.client_exports {
        let listed = wire.cloudformation(region, "ListImports", &[("ExportName", export)]);
        assert_eq!(
            listed.status,
            200,
            "case {case:?}, export {export:?}: expected ListImports to answer, got {} with \
             body {:?}",
            listed.status,
            listed.text()
        );
        let body = listed.text();
        let importers = match blocks_of(&body, "Imports").into_iter().next() {
            Some(block) => elements(block, "member"),
            None => Vec::new(),
        };
        assert!(
            importers.iter().any(|stack| stack == row.consumer_stack),
            "case {case:?}, export {export:?}: expected the importing stack {:?} to be \
             recorded as an importer, got {importers:?}",
            row.consumer_stack
        );
    }

    let described = wire.rpc_ok(
        COGNITO_IDENTITY,
        true,
        region,
        "DescribeIdentityPool",
        &json!({"IdentityPoolId": &identity_pool}),
        case,
    );
    let federated: BTreeSet<String> = described["CognitoIdentityProviders"]
        .as_array()
        .map(|providers| {
            providers
                .iter()
                .filter_map(|entry| entry["ClientId"].as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        federated, minted_ids,
        "case {case:?}: expected the imported identity pool to federate exactly the client \
         ids the producing stack published ({minted_ids:?}), got {described}"
    );
    for entry in described["CognitoIdentityProviders"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        assert_eq!(
            entry["ProviderName"].as_str(),
            Some(provider.as_str()),
            "case {case:?}: expected every federated provider to name the user pool the \
             producing stack created, got {entry}"
        );
    }

    // What the attachment applied: the authenticated role is the role the importing stack
    // created, and a mapping it declares is keyed by the provider identifier its intrinsics
    // resolved — which carries the minted client id where the row keys it by one.
    let role_name = output(&imported, "RoleName", case);
    let role_arn = format!(
        "arn:aws:iam::{}:role/{role_name}",
        engine::emulator_account_id()
    );
    let configured = wire.rpc_ok(
        COGNITO_IDENTITY,
        true,
        region,
        "GetIdentityPoolRoles",
        &json!({"IdentityPoolId": &identity_pool}),
        case,
    );
    assert_eq!(
        configured["Roles"]["authenticated"].as_str(),
        Some(role_arn.as_str()),
        "case {case:?}: expected the attached authenticated role to be the role the importing \
         stack created ({role_arn}), got {configured}"
    );
    let expected_keys: Vec<String> = row
        .mapping_keyed_by
        .map(|key| vec![format!("{provider}:{}", output(&published, key, case))])
        .unwrap_or_default();
    let mapped: Vec<String> = configured["RoleMappings"]
        .as_object()
        .map(|mappings| mappings.keys().cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        mapped, expected_keys,
        "case {case:?}: expected the role mappings to be keyed by the resolved provider \
         identifier ({expected_keys:?}), got {configured}"
    );
}
