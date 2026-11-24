// SPDX-License-Identifier: CC0-1.0

//! The AWS Amplify BRANCH a stack declares, end to end: the branch exists in AWS Amplify
//! itself, under the app the declaration names, the ARN the stack publishes resolves there, a
//! changed declaration reaches the branch that exists, and removing the branch from the
//! template — or deleting the stack — takes it away.
//!
//! RULE: a declared resource reaches `CREATE_COMPLETE` only once the service that OWNS the type
//! really holds it, so every handle the stack publishes for it (`Ref`, each `Fn::GetAtt`) is one
//! that service answers for — and a create that service REFUSES is a failed resource rather
//! than a completed stack.
//!
//! The rows differ along what the rule generalises over: the FORM of the branch name (a plain
//! name and one carrying the `/` and `.` symbols Amplify's own name rule admits), which optional
//! properties the declaration carries (none beyond the required pair, and the whole published
//! set — the stage, the framework, the flags, the environment variables, the basic-authorization
//! pair and the Gen 2 backend), whether the declaration carries the type's own `Tags` beside the
//! stack's, how many branches one stack declares, and the template spelling (JSON with long-form
//! intrinsics, YAML with the short-form tags). The app id is read off the app resource the same
//! body declares in the table, and off a stack parameter naming an app a client created through
//! Amplify's own API in the update and removal cases.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds
//! — the CloudFormation calls over its awsQuery wire, the Amplify calls over that service's own
//! `restJson1` wire — so what is asserted is what a client that deploys the template and then
//! calls Amplify observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, member, router, settled_stack_status, stack_event_reasons, xml_text, REGION,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value};

/// The resource types under test.
const BRANCH: &str = "AWS::Amplify::Branch";
const APP: &str = "AWS::Amplify::App";

/// The status AWS Amplify answers for a branch it does not hold (the modelled 404 of
/// `GetBranch`), and the `HandlerErrorCode` CloudFormation reports a resource whose owning
/// service answered that refusal under.
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND_HANDLER_CODE: &str = "HandlerErrorCode: NotFound";

/// The CloudFormation stack a Gen 2 branch names as its backend. Any value of the form
/// `Backend$stackArn`'s own pattern fixes
/// (`^arn:aws:cloudformation:[a-z0-9-]+:\d{12}:stack/.+/.+$`) serves: what is under test is that
/// the declared document reaches the branch, not what that stack is.
const BACKEND_STACK_ARN: &str =
    "arn:aws:cloudformation:us-east-1:000000000000:stack/amplify-backend/1111";

/// The three tags CloudFormation records on every resource it provisions.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const STACK_ID_TAG: &str = "aws:cloudformation:stack-id";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

/// One AWS Amplify call over the `restJson1` wire its model declares, on the method and URI
/// that operation's own `@http` binding names, signed for `amplify` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
pub(crate) async fn amplify(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://amplify.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("amplify.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/amplify/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed AWS Amplify request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `CreateApp` for one name: the id AWS Amplify minted for the app, as a client's own call gets
/// it.
pub(crate) async fn create_app(router: &MultiServiceRouter, name: &str) -> String {
    let (status, document) = amplify(router, "POST", "/apps", Some(&json!({"name": name}))).await;
    assert_eq!(
        status, 200,
        "CreateApp {name:?}: expected HTTP 200, got {status} with {document}"
    );
    document
        .pointer("/app/appId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateApp {name:?}: no appId in {document}"))
        .to_string()
}

/// That service's own read of ONE branch of ONE app: the status and, for a 200, the branch
/// record itself.
pub(crate) async fn get_branch(
    router: &MultiServiceRouter,
    app_id: &str,
    branch_name: &str,
) -> (u16, Value) {
    let (status, document) = amplify(
        router,
        "GET",
        // The branch name is escaped as ONE path segment ([`form_encode`] leaves exactly the
        // unreserved set literal), so a name carrying `/` addresses one branch rather than
        // another path — which is what a client's own SDK sends for such a name.
        &format!(
            "/apps/{}/branches/{}",
            form_encode(app_id),
            form_encode(branch_name)
        ),
        None,
    )
    .await;
    if status == 200 {
        return (status, document.get("branch").cloned().unwrap_or(Value::Null));
    }
    (status, document)
}

/// Every branch name AWS Amplify lists for one app.
pub(crate) async fn list_branch_names(router: &MultiServiceRouter, app_id: &str) -> Vec<String> {
    let (_, document) = amplify(
        router,
        "GET",
        &format!("/apps/{}/branches", form_encode(app_id)),
        None,
    )
    .await;
    document
        .get("branches")
        .and_then(Value::as_array)
        .map(|branches| {
            branches
                .iter()
                .filter_map(|branch| branch.get("branchName").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The tags one reported branch carries, as the map that service answers them in.
fn reported_tags(reported: &Value) -> BTreeMap<String, String> {
    reported
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// One submit of a template: the action that submits it, the stack it is submitted for, the
/// parameters and stack-level tags it carries, and the `StackStatus` it must reach.
struct Submit<'a> {
    action: &'a str,
    stack_name: &'a str,
    template: &'a str,
    parameters: &'a [(&'a str, &'a str)],
    stack_tags: &'a [(&'a str, &'a str)],
    wanted: &'a str,
}

/// Submit one template and wait for the stack to settle; answers the `DescribeStacks` body of
/// the settled stack, quoting the stack's own failure reasons when it did not reach the status
/// the submit wants.
async fn deployed(router: &MultiServiceRouter, case: &str, submit: Submit<'_>) -> String {
    let Submit {
        action,
        stack_name,
        template,
        parameters,
        stack_tags,
        wanted,
    } = submit;
    let mut members = vec![
        member("StackName", stack_name),
        member("TemplateBody", template),
    ];
    for (position, (key, value)) in parameters.iter().enumerate() {
        let position = position + 1;
        members.push(member(
            &format!("Parameters.member.{position}.ParameterKey"),
            key,
        ));
        members.push(member(
            &format!("Parameters.member.{position}.ParameterValue"),
            value,
        ));
    }
    for (position, (key, value)) in stack_tags.iter().enumerate() {
        let position = position + 1;
        members.push(member(&format!("Tags.member.{position}.Key"), key));
        members.push(member(&format!("Tags.member.{position}.Value"), value));
    }
    let (status, body) = call(router, action, &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action} — an {BRANCH} is a type a template may \
         declare — got {status} with body {body}"
    );
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, wanted,
        "case {case:?}: expected the stack to reach {wanted}, got {reached} with failure \
         reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// What one row expects of one declared branch.
struct Declared {
    /// The branch's name, as the declaration states it: the type marks `BranchName` required, so
    /// a row always states one.
    branch_name: &'static str,
    /// The output key whose value is `Ref` for the branch — the ARN AWS Amplify built for it.
    ref_output: &'static str,
    /// The output key whose value is the `Arn` attribute the type publishes, which is that same
    /// value read through `Fn::GetAtt`.
    arn_output: &'static str,
    /// The output key whose value is the `BranchName` attribute the type publishes.
    name_output: &'static str,
    /// The members the branch must report, as the declaration stated them.
    members: Value,
    /// The tags the branch must carry, declared and stack-level together.
    tags: &'static [(&'static str, &'static str)],
}

/// One row: a template, the stack-level tags it is deployed with, and the branches it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    stack_tags: &'static [(&'static str, &'static str)],
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // One branch in the app the body declares, carrying the whole published property set,
        // with every return value read back, in JSON with long-form intrinsics and the type's
        // own `Tags` beside the stack's.
        Row {
            case: "one branch carrying every published property, every return value read, JSON",
            stack_name: "amplify-branch-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "App": {
                        "Type": APP,
                        "Properties": {
                            "Name": "branch-round-trip",
                            "Description": "the app the branch belongs to"
                        }
                    },
                    "Branch": {
                        "Type": BRANCH,
                        "Properties": {
                            "AppId": {"Fn::GetAtt": ["App", "AppId"]},
                            "BranchName": "main",
                            "Description": "the production branch",
                            "Stage": "PRODUCTION",
                            "Framework": "React",
                            "EnableAutoBuild": false,
                            "EnablePerformanceMode": true,
                            "EnablePullRequestPreview": false,
                            "EnableSkewProtection": true,
                            "PullRequestEnvironmentName": "preview",
                            "BuildSpec": "version: 1",
                            "ComputeRoleArn": COMPUTE_ROLE_ARN,
                            "EnvironmentVariables": [
                                {"Name": "STAGE", "Value": "prod"},
                                {"Name": "REGION", "Value": REGION}
                            ],
                            "BasicAuthConfig": {
                                "EnableBasicAuth": true,
                                "Username": "studio",
                                "Password": "secret"
                            },
                            "Backend": {"StackArn": BACKEND_STACK_ARN},
                            "Tags": [{"Key": "owner", "Value": "studio"}]
                        }
                    }
                },
                "Outputs": {
                    "AppIdAtt": {"Value": {"Fn::GetAtt": ["App", "AppId"]}},
                    "BranchRef": {"Value": {"Ref": "Branch"}},
                    "BranchArn": {"Value": {"Fn::GetAtt": ["Branch", "Arn"]}},
                    "BranchNameAtt": {"Value": {"Fn::GetAtt": ["Branch", "BranchName"]}}
                }
            })
            .to_string(),
            stack_tags: &[("sim2real", "amplify-branch-round-trip")],
            declared: vec![Declared {
                branch_name: "main",
                ref_output: "BranchRef",
                arn_output: "BranchArn",
                name_output: "BranchNameAtt",
                members: json!({
                    "branchName": "main",
                    "description": "the production branch",
                    "stage": "PRODUCTION",
                    "framework": "React",
                    "enableAutoBuild": false,
                    "enablePerformanceMode": true,
                    "enablePullRequestPreview": false,
                    "enableSkewProtection": true,
                    "pullRequestEnvironmentName": "preview",
                    "buildSpec": "version: 1",
                    "computeRoleArn": COMPUTE_ROLE_ARN,
                    "enableBasicAuth": true,
                    "environmentVariables": {"STAGE": "prod", "REGION": REGION},
                    "backend": {"stackArn": BACKEND_STACK_ARN}
                }),
                tags: &[
                    ("owner", "studio"),
                    ("sim2real", "amplify-branch-round-trip"),
                ],
            }],
        },
        // TWO branches in one stack — the required pair alone on one, a stage and a framework on
        // the other, whose name carries the `/` and `.` symbols Amplify's own branch-name rule
        // admits — in YAML with the short-form tags.
        Row {
            case: "two branches, one named with the symbols the name rule admits, YAML",
            stack_name: "amplify-branch-yaml",
            template: format!(
                "\
Resources:
  App:
    Type: {APP}
    Properties:
      Name: branch-yaml
  Dev:
    Type: {BRANCH}
    Properties:
      AppId: !GetAtt App.AppId
      BranchName: dev
  Release:
    Type: {BRANCH}
    Properties:
      AppId: !GetAtt App.AppId
      BranchName: release/1.0
      Stage: EXPERIMENTAL
      Framework: Next.js - SSR
Outputs:
  AppIdAtt:
    Value: !GetAtt App.AppId
  DevRef:
    Value: !Ref Dev
  DevArn:
    Value: !GetAtt Dev.Arn
  DevName:
    Value: !GetAtt Dev.BranchName
  ReleaseRef:
    Value: !Ref Release
  ReleaseArn:
    Value: !GetAtt Release.Arn
  ReleaseName:
    Value: !GetAtt Release.BranchName
"
            ),
            stack_tags: &[],
            declared: vec![
                Declared {
                    branch_name: "dev",
                    ref_output: "DevRef",
                    arn_output: "DevArn",
                    name_output: "DevName",
                    members: json!({"branchName": "dev"}),
                    tags: &[],
                },
                Declared {
                    branch_name: "release/1.0",
                    ref_output: "ReleaseRef",
                    arn_output: "ReleaseArn",
                    name_output: "ReleaseName",
                    members: json!({
                        "branchName": "release/1.0",
                        "stage": "EXPERIMENTAL",
                        "framework": "Next.js - SSR"
                    }),
                    tags: &[],
                },
            ],
        },
    ]
}

/// The IAM role one row assigns a branch of an SSR app, of the form `ComputeRoleArn` admits (a
/// plain string up to 1000 characters).
const COMPUTE_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/amplify-ssr-compute";

#[tokio::test]
async fn every_declared_branch_is_created_in_amplify() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(
            &router,
            case,
            Submit {
                action: "CreateStack",
                stack_name: row.stack_name,
                template: &row.template,
                parameters: &[],
                stack_tags: row.stack_tags,
                wanted: "CREATE_COMPLETE",
            },
        )
        .await;

        let app_id = output_value(&described, "AppIdAtt")
            .unwrap_or_else(|| panic!("case {case:?}: no AppIdAtt output in {described}"));
        let stack_id = xml_text(&described, "StackId").unwrap_or_default();
        let listed = list_branch_names(&router, &app_id).await;

        for declared in &row.declared {
            let case = format!("{case} [{}]", declared.branch_name);
            let published = |key: &str| {
                output_value(&described, key)
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
            };

            // The branch the stack reports created is one AWS Amplify itself holds, resolved by
            // the app id the stack published and the name the template declared.
            let (status, reported) = get_branch(&router, &app_id, declared.branch_name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected AWS Amplify to hold the branch the stack created, got \
                 {status} with {reported}"
            );

            // "Ref returns the ARN of the branch", which is the ARN that service built around
            // the app id it minted — the same value the `Arn` attribute publishes.
            let branch_arn = reported
                .get("branchArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            for key in [declared.ref_output, declared.arn_output] {
                let value = published(key);
                assert_eq!(
                    value, branch_arn,
                    "case {case:?}: expected the published {key} to be the branch's own ARN \
                     {branch_arn:?}, got {value:?}"
                );
            }
            assert!(
                branch_arn.contains(&format!("apps/{app_id}/branches/")),
                "case {case:?}: expected the ARN to carry the app the branch belongs to, got \
                 {branch_arn:?}"
            );
            let name = published(declared.name_output);
            assert_eq!(
                name, declared.branch_name,
                "case {case:?}: expected the published BranchName to be the declared one, got \
                 {name:?}"
            );

            for (member, wanted) in declared
                .members
                .as_object()
                .expect("the row's members are an object")
            {
                let got = reported.get(member);
                assert_eq!(
                    got,
                    Some(wanted),
                    "case {case:?}: expected the declared {member} {wanted}, got {got:?} in \
                     {reported}"
                );
            }

            let tags = reported_tags(&reported);
            for (key, value) in declared.tags {
                assert_eq!(
                    tags.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the tag {key}={value} on the branch, got {tags:?}"
                );
            }
            // CloudFormation records the stack it created the resource for, and the resource's
            // place in it, on the resource itself.
            assert_eq!(
                tags.get(STACK_NAME_TAG).map(String::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own provenance tag, got {tags:?}"
            );
            assert_eq!(
                tags.get(STACK_ID_TAG).map(String::as_str),
                Some(stack_id.as_str()),
                "case {case:?}: expected the stack id provenance tag, got {tags:?}"
            );
            assert!(
                tags.contains_key(LOGICAL_ID_TAG),
                "case {case:?}: expected the logical-id provenance tag, got {tags:?}"
            );

            assert!(
                listed.contains(&declared.branch_name.to_string()),
                "case {case:?}: expected AWS Amplify to list the branch for the app, got \
                 {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_applies_the_changed_declaration_to_the_branch_it_created() {
    let router = router();
    let case = "a re-declared description and stage";
    let stack_name = "amplify-branch-updated";
    // The app is created through AWS Amplify's own API, exactly as a client creates one, and the
    // stack states its id as a PARAMETER: what the update is about is the branch, not the app it
    // lives in.
    let app_id = create_app(&router, "branch-updated").await;
    let declaration = |description: &str, stage: &str| {
        json!({
            "Parameters": {"AppId": {"Type": "String"}},
            "Resources": {
                "Branch": {
                    "Type": BRANCH,
                    "Properties": {
                        "AppId": {"Ref": "AppId"},
                        "BranchName": "main",
                        "Description": description,
                        "Stage": stage
                    }
                }
            },
            "Outputs": {"BranchRef": {"Value": {"Ref": "Branch"}}}
        })
        .to_string()
    };
    let parameters: Vec<(&str, &str)> = vec![("AppId", app_id.as_str())];

    let described = deployed(
        &router,
        case,
        Submit {
            action: "CreateStack",
            stack_name,
            template: &declaration("the first description", "BETA"),
            parameters: &parameters,
            stack_tags: &[],
            wanted: "CREATE_COMPLETE",
        },
    )
    .await;
    let reference = output_value(&described, "BranchRef")
        .unwrap_or_else(|| panic!("case {case:?}: no BranchRef output in {described}"));

    let described = deployed(
        &router,
        case,
        Submit {
            action: "UpdateStack",
            stack_name,
            template: &declaration("the second description", "PRODUCTION"),
            parameters: &parameters,
            stack_tags: &[],
            wanted: "UPDATE_COMPLETE",
        },
    )
    .await;
    assert_eq!(
        output_value(&described, "BranchRef").as_deref(),
        Some(reference.as_str()),
        "case {case:?}: expected the branch to keep the ARN that service built, got {described}"
    );

    // The change reached the branch AWS Amplify holds, not just the stack's record: a branch is
    // identified by its name inside its app, and `UpdateBranch` takes a new configuration for
    // the branch that exists.
    let (status, reported) = get_branch(&router, &app_id, "main").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the branch to still be there, got {status} with {reported}"
    );
    for (member, wanted) in [
        ("description", "the second description"),
        ("stage", "PRODUCTION"),
    ] {
        let got = reported.get(member).and_then(Value::as_str);
        assert_eq!(
            got,
            Some(wanted),
            "case {case:?}: expected the re-declared {member} {wanted:?}, got {got:?} in \
             {reported}"
        );
    }
    let listed = list_branch_names(&router, &app_id).await;
    assert_eq!(
        listed,
        vec!["main".to_string()],
        "case {case:?}: expected the one branch to be changed rather than a second one created, \
         got {listed:?}"
    );
}

#[tokio::test]
async fn removing_the_branch_from_the_template_removes_it_from_amplify() {
    let router = router();
    let case = "a branch removed from the template by an update";
    let stack_name = "amplify-branch-removed";
    let app_id = create_app(&router, "branch-removed").await;
    let with_branch = json!({
        "Parameters": {"AppId": {"Type": "String"}},
        "Resources": {
            "Branch": {
                "Type": BRANCH,
                "Properties": {"AppId": {"Ref": "AppId"}, "BranchName": "main"}
            }
        },
        "Outputs": {"BranchRef": {"Value": {"Ref": "Branch"}}}
    })
    .to_string();
    // The same stack with the branch gone, and one resource of another type in its place so the
    // template still declares something.
    let without_branch = json!({
        "Parameters": {"AppId": {"Type": "String"}},
        "Resources": {
            "Keep": {"Type": APP, "Properties": {"Name": "branch-removed-keep"}}
        },
        "Outputs": {"KeptApp": {"Value": {"Fn::GetAtt": ["Keep", "AppId"]}}}
    })
    .to_string();
    let parameters: Vec<(&str, &str)> = vec![("AppId", app_id.as_str())];

    deployed(
        &router,
        case,
        Submit {
            action: "CreateStack",
            stack_name,
            template: &with_branch,
            parameters: &parameters,
            stack_tags: &[],
            wanted: "CREATE_COMPLETE",
        },
    )
    .await;
    let (status, reported) = get_branch(&router, &app_id, "main").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the branch to exist before the update, got {status} with \
         {reported}"
    );

    deployed(
        &router,
        case,
        Submit {
            action: "UpdateStack",
            stack_name,
            template: &without_branch,
            parameters: &parameters,
            stack_tags: &[],
            wanted: "UPDATE_COMPLETE",
        },
    )
    .await;

    // ONLY NOW does AWS Amplify refuse the read, with its own modelled 404 and the sentence it
    // words a branch it does not hold with.
    let (status, answer) = get_branch(&router, &app_id, "main").await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected the removed branch to be gone, got {status} with {answer}"
    );
    assert_eq!(
        answer.get("message").and_then(Value::as_str),
        Some("Branch main not found."),
        "case {case:?}: expected the service's own not-found sentence, got {answer}"
    );
    let listed = list_branch_names(&router, &app_id).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected the app to list no branch, got {listed:?}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_branch_it_created() {
    let router = router();
    let case = "a branch deleted with its stack";
    let stack_name = "amplify-branch-deleted";
    let template = json!({
        "Resources": {
            "App": {"Type": APP, "Properties": {"Name": "branch-deleted"}},
            "Branch": {
                "Type": BRANCH,
                "Properties": {
                    "AppId": {"Fn::GetAtt": ["App", "AppId"]},
                    "BranchName": "main"
                }
            }
        },
        "Outputs": {"AppIdAtt": {"Value": {"Fn::GetAtt": ["App", "AppId"]}}}
    })
    .to_string();

    let described = deployed(
        &router,
        case,
        Submit {
            action: "CreateStack",
            stack_name,
            template: &template,
            parameters: &[],
            stack_tags: &[],
            wanted: "CREATE_COMPLETE",
        },
    )
    .await;
    let app_id = output_value(&described, "AppIdAtt")
        .unwrap_or_else(|| panic!("case {case:?}: no AppIdAtt output in {described}"));
    let listed = list_branch_names(&router, &app_id).await;
    assert_eq!(
        listed,
        vec!["main".to_string()],
        "case {case:?}: expected the branch to be there before the delete, got {listed:?}"
    );

    let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    // The deletion settles when this service no longer reports the stack at all.
    crate::harness::settled_stack(case, &router, REGION, stack_name).await;

    // The stack's deletion took the branch and the app it belonged to with it.
    let (status, answer) = get_branch(&router, &app_id, "main").await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected the branch to be gone from AWS Amplify, got {status} with \
         {answer}"
    );
    let (status, answer) = amplify(
        &router,
        "GET",
        &format!("/apps/{}", form_encode(&app_id)),
        None,
    )
    .await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected the app to be gone from AWS Amplify, got {status} with {answer}"
    );
}

#[tokio::test]
async fn a_branch_whose_app_amplify_does_not_hold_rolls_the_stack_back() {
    let router = router();
    let case = "a branch declared under an app id AWS Amplify holds no app for";
    let stack_name = "amplify-branch-no-app";
    // An app id of the form that service's own `AppId` member fixes (`^d[a-z0-9]+$`) that no
    // app was ever created under.
    let missing_app = "d0nosuchappatall";
    let template = json!({
        "Resources": {
            "Branch": {
                "Type": BRANCH,
                "Properties": {"AppId": missing_app, "BranchName": "main"}
            }
        },
        "Outputs": {"BranchRef": {"Value": {"Ref": "Branch"}}}
    })
    .to_string();

    let described = deployed(
        &router,
        case,
        Submit {
            action: "CreateStack",
            stack_name,
            template: &template,
            parameters: &[],
            stack_tags: &[],
            wanted: "ROLLBACK_COMPLETE",
        },
    )
    .await;
    assert_eq!(
        output_value(&described, "BranchRef"),
        None,
        "case {case:?}: expected no output for a stack that rolled back, got {described}"
    );

    // The failure is reported on the resource that failed, in the sentence this type's PROVIDER
    // words a not-found refusal of the owning service with — which carries the type it manages
    // and the identifier it holds for the branch, and none of that service's own wording. The
    // wording itself is pinned, over a set of app ids, by
    // `amplify_branch_unknown_app_wording.rs`.
    let reasons = stack_event_reasons(&router, REGION, stack_name).await;
    let sentence = format!("Resource of type '{BRANCH}' with identifier 'null' was not found.");
    assert!(
        reasons.iter().any(|reason| reason.contains(&sentence)),
        "case {case:?}: expected a failure reason carrying {sentence:?}, got {reasons:?}"
    );
    assert!(
        !reasons
            .iter()
            .any(|reason| reason.contains(&format!("App {missing_app} not found."))),
        "case {case:?}: expected no reason to carry the owning service's own sentence, got \
         {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains(NOT_FOUND_HANDLER_CODE)),
        "case {case:?}: expected the failure to be reported as {NOT_FOUND_HANDLER_CODE:?}, got \
         {reasons:?}"
    );
    // Nothing was created for the branch: the app id it named holds no branch either.
    let listed = list_branch_names(&router, missing_app).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected no branch under an app that does not exist, got {listed:?}"
    );
}
