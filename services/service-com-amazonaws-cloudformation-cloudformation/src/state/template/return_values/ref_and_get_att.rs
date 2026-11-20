// SPDX-License-Identifier: CC0-1.0

//! The per-type table of what `Ref` returns for a resource and what `Fn::GetAtt` reads off
//! it — one concern of [`super`], split out so each file stays readable.

use super::*;

/// What `Ref` returns for a resource of this type, and what `Fn::GetAtt` reads off it.
///
/// Source: the "Return values" section the AWS CloudFormation resource and property type
/// reference publishes for each type — `Ref` returns the topic ARN for
/// `AWS::SNS::Topic`, the queue URL for `AWS::SQS::Queue`, the bucket name for
/// `AWS::S3::Bucket`, and so on, with `Fn::GetAtt` reading the listed attributes. A type
/// this catalogue does not carry gets its physical NAME as its physical resource id,
/// which is what the majority of types return, and no attributes: `Fn::GetAtt` against it
/// yields no value rather than a guessed one.
///
/// An attribute whose documented return value is a LIST (an
/// `As2ServiceManagedEgressIpAddresses`, say) is not carried either: the attribute map is
/// string-valued, and reporting one element of a list as the whole of it would be worse
/// than reporting no value.
///
/// The whole declared resource is in scope, not just its type and name, because a few
/// published return values are functions of the resource's own properties — a DB
/// endpoint's port is the one the template declared, or the declared engine's default —
/// and those properties are evaluated through the template's intrinsics, which is what the
/// `scope` and the `budget` are for.
pub(crate) fn return_values(
    declared: &DeclaredResource,
    name: &str,
    scope: &Scope<'_>,
    budget: &mut Budget,
) -> (String, BTreeMap<String, String>) {
    let region = scope.stack.region;
    let resource_type = declared.resource_type.as_str();
    let properties = &declared.properties;
    let mut attributes = BTreeMap::new();
    match resource_type {
        // "Ref returns the report group name"; Fn::GetAtt: Arn. This is the type
        // `super::provisioning` creates in CodeBuild itself, which then answers with the
        // report group's own ARN — the same ARN this computes, since both are built from
        // the one account id this emulator mints and the group's name, which is how
        // CodeBuild keys a report group (`report_group_arn` in its own state module).
        CODEBUILD_REPORT_GROUP => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    CODEBUILD_ARN_NAMESPACE,
                    region,
                    &format!("report-group/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the project name"; Fn::GetAtt: Arn. The same position the report
        // group above is in: `super::provisioning` creates the project in CodeBuild, which
        // answers with the project's own ARN — the same ARN this computes, since both are
        // built from the one account id this emulator mints and the project's name, which
        // is how CodeBuild keys a project (`project_arn` in its own state module).
        CODEBUILD_PROJECT => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    CODEBUILD_ARN_NAMESPACE,
                    region,
                    &format!("project/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the pipeline name"; Fn::GetAtt: Arn, Version. This is the type
        // `super::provisioning` creates in CodePipeline itself, which keys a pipeline by
        // that name — so the ARN computed here is the one that service mints
        // (`pipeline_arn` in its own state module): the bare pipeline name as the ARN's
        // resource path, under the one account id this emulator mints. The VERSION is
        // CodePipeline's to allocate, so what this gives the resource is the version a
        // created pipeline carries ([`CODEPIPELINE_FIRST_PIPELINE_VERSION`]) and the create
        // reports the version that service really answered.
        CODEPIPELINE_PIPELINE => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(CODEPIPELINE_ARN_NAMESPACE, region, name),
            );
            attributes.insert(
                "Version".to_string(),
                CODEPIPELINE_FIRST_PIPELINE_VERSION.to_string(),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the webhook name"; Fn::GetAtt: Url. The URL is the one CodePipeline
        // generates for the webhook, carrying a token minted in that service's own store, so
        // it stands in until `PutWebhook` has reported it and
        // `super::super::provisioning` re-points it at what that service answered.
        CODEPIPELINE_WEBHOOK => {
            attributes.insert("Url".to_string(), service_assigned_document(name, "Url"));
            (name.to_string(), attributes)
        }
        // "Ref returns the topic ARN"; Fn::GetAtt: TopicArn, TopicName. This is the type
        // `super::provisioning` creates in Amazon SNS itself, which then answers with the
        // topic's own ARN — the same ARN this computes, since both are built from the one
        // account id this emulator mints and the topic's name, which is how SNS keys a
        // topic (`topic_arn` in its own state module).
        SNS_TOPIC => {
            let arn = ::emulator_server::emulator_arn("sns", region, name);
            attributes.insert("TopicArn".to_string(), arn.clone());
            attributes.insert("TopicName".to_string(), name.to_string());
            (arn, attributes)
        }
        // "Ref returns the queue URL"; Fn::GetAtt: Arn, QueueName, QueueUrl. This is the
        // type `super::provisioning` creates in Amazon SQS itself, which then reports the
        // queue's own url back over the `QueueUrl` attribute — the same url this computes,
        // since both are built from the one account id and the queue's name.
        SQS_QUEUE => {
            let url = format!(
                "https://sqs.{region}.{URL_SUFFIX}/{}/{name}",
                ::emulator_server::emulator_account_id()
            );
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("sqs", region, name),
            );
            attributes.insert("QueueName".to_string(), name.to_string());
            attributes.insert("QueueUrl".to_string(), url.clone());
            (url, attributes)
        }
        // "Ref returns the ARN of the activity"; Fn::GetAtt: Arn, Name. This is the type
        // `super::provisioning` creates in AWS Step Functions itself, which then answers
        // with the activity's own ARN — the same ARN this computes, since both are built
        // from the one account id this emulator mints and the activity's name, which is how
        // Step Functions keys an activity (`activity_arn` in its own state module). The ARN
        // namespace is that model's `arnNamespace`, `states`, not its CloudFormation name.
        STEP_FUNCTIONS_ACTIVITY => {
            let arn = ::emulator_server::emulator_arn(
                STEP_FUNCTIONS_ARN_NAMESPACE,
                region,
                &format!("activity:{name}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Name".to_string(), name.to_string());
            (arn, attributes)
        }
        // "Ref returns the ARN of the state machine"; Fn::GetAtt: Arn, Name. This is the
        // type `super::provisioning` creates in AWS Step Functions itself, which then answers
        // with the machine's own ARN — the same ARN this computes, since both are built from
        // the one account id this emulator mints and the machine's name, which is how Step
        // Functions keys a state machine (`state_machine_arn` in its own state module). The
        // name is the declared `StateMachineName` where the template states one, as it is for
        // every type whose name is a property, and the generated one otherwise.
        STEP_FUNCTIONS_STATE_MACHINE => {
            let declared = properties
                .get("StateMachineName")
                .and_then(|value| text(value, scope, 0, budget))
                .filter(|declared| !declared.is_empty());
            let machine = declared.unwrap_or_else(|| name.to_string());
            let arn = ::emulator_server::emulator_arn(
                STEP_FUNCTIONS_ARN_NAMESPACE,
                region,
                &format!("stateMachine:{machine}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Name".to_string(), machine);
            (arn, attributes)
        }
        // The Amazon SageMaker AI types `super::provisioning` creates in SageMaker
        // itself ([`crate::state::provisioner::sagemaker_resources`]). Each publishes ONE
        // `Fn::GetAtt` attribute, which is the resource's own NAME — `ModelName` ("the name
        // of the model"), `NotebookInstanceName` ("the name of the notebook instance") and
        // `NotebookInstanceLifecycleConfigName` ("the name of the lifecycle configuration")
        // — so the attribute is the name CloudFormation decided and the create made the
        // resource under.
        //
        // That name is this resource's physical id too, which is the position
        // [`SAGEMAKER_MODEL_PACKAGE_GROUP`] is in and for the same reason: the name is what
        // every SageMaker operation addresses one of these resources by (`DescribeModel` and
        // `DeleteModel` take a `ModelName`, `DescribeNotebookInstance` and
        // `CreatePresignedNotebookInstanceUrl` a `NotebookInstanceName`, and a notebook
        // instance's `LifecycleConfigName` names a configuration by name), so the identifier
        // a client can resolve is preferred over an ARN minted from a pattern. The ARN each
        // create answers is not computed here, no type of the three publishing a return
        // value for it.
        crate::state::provisioner::sagemaker_resources::SAGEMAKER_MODEL => {
            attributes.insert("ModelName".to_string(), name.to_string());
            (name.to_string(), attributes)
        }
        // The endpoint pair publishes its own name the same way: `EndpointConfigName` ("the
        // name of the endpoint configuration") and `EndpointName` ("the name of the
        // endpoint"), each the name every SageMaker call addresses that resource by —
        // `CreateEndpoint` takes an `EndpointConfigName`, and `DescribeEndpoint`,
        // `InvokeEndpoint` and `CreateInferenceComponent` an `EndpointName`.
        crate::state::provisioner::sagemaker_resources::SAGEMAKER_ENDPOINT_CONFIG => {
            attributes.insert("EndpointConfigName".to_string(), name.to_string());
            (name.to_string(), attributes)
        }
        crate::state::provisioner::sagemaker_resources::SAGEMAKER_ENDPOINT => {
            attributes.insert("EndpointName".to_string(), name.to_string());
            (name.to_string(), attributes)
        }
        crate::state::provisioner::sagemaker_resources::SAGEMAKER_NOTEBOOK_INSTANCE => {
            attributes.insert("NotebookInstanceName".to_string(), name.to_string());
            (name.to_string(), attributes)
        }
        crate::state::provisioner::sagemaker_resources::SAGEMAKER_NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG => {
            attributes.insert(
                "NotebookInstanceLifecycleConfigName".to_string(),
                name.to_string(),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the user pool ID"; Fn::GetAtt: Arn
        // ("arn:aws:cognito-idp:us-east-1:123412341234:userpool/us-east-1_123412341"),
        // ProviderName ("The provider name of the Amazon Cognito user pool") and
        // ProviderURL ("The URL of the provider of the Amazon Cognito user pool"). The
        // provider name is the pool's endpoint host and its identity
        // (`cognito-idp.<region>.amazonaws.com/<user pool id>`) — the name an identity
        // pool's `CognitoIdentityProviders`, a principal-tag mapping's
        // `IdentityProviderName` and a `Logins` entry all name the pool by — and the
        // provider URL is that same host and identity as an `https` URL, which is the
        // issuer an OpenID relying party reads. Both are built from the resource's OWN
        // identity, so whatever a `Ref` to the pool answers, the provider the pool
        // publishes names that same pool; making that identity the id the owning service
        // mints is the business of the open issue on service-assigned physical ids
        // (`20261023T114649Z-cloudformation-outputs-carry-no-outputvalue-failed-resources-never`).
        // "Ref returns the Amazon Resource Name (ARN) of the domain"; Fn::GetAtt: Arn,
        // Name, Owner. This is the type `super::provisioning` creates in CodeArtifact
        // itself, which then answers with the domain's own ARN and owning account — the same
        // values this computes, since both are built from the one account id this emulator
        // mints and the domain's name, which is how CodeArtifact keys a domain (`domain_arn`
        // in that service's own state module).
        CODEARTIFACT_DOMAIN => {
            let arn = ::emulator_server::emulator_arn(
                CODEARTIFACT_ARN_NAMESPACE,
                region,
                &format!("domain/{name}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Name".to_string(), name.to_string());
            attributes.insert(
                "Owner".to_string(),
                ::emulator_server::emulator_account_id().to_string(),
            );
            (arn, attributes)
        }
        // "Ref returns the Amazon Resource Name (ARN) of the repository"; Fn::GetAtt: Arn,
        // DomainOwner, Name. A repository ARN NESTS inside the domain that holds it
        // (`repository/<domain>/<name>`), so it is built on the domain the declaration's
        // `DomainName` names — a declaration that names none leaves the repository with
        // CloudFormation's own generated identity rather than an ARN built on a domain
        // nothing named, exactly as an index of no vector bucket does.
        CODEARTIFACT_REPOSITORY => {
            let domain = properties
                .get("DomainName")
                .and_then(|value| text(value, scope, 0, budget));
            attributes.insert(
                "DomainOwner".to_string(),
                ::emulator_server::emulator_account_id().to_string(),
            );
            attributes.insert("Name".to_string(), name.to_string());
            match domain {
                Some(domain) => {
                    let arn = ::emulator_server::emulator_arn(
                        CODEARTIFACT_ARN_NAMESPACE,
                        region,
                        &format!("repository/{domain}/{name}"),
                    );
                    attributes.insert("Arn".to_string(), arn.clone());
                    (arn, attributes)
                }
                None => (name.to_string(), attributes),
            }
        }
        // "Ref returns the user pool ID"; Fn::GetAtt: Arn, ProviderName, ProviderURL. Each of
        // the three is read off the IDENTITY in force ([`user_pool_attributes`]): the name
        // computed here until the pool's own create has answered, and the id Amazon Cognito
        // minted from then on ([`crate::state::provisioning::cognito_user_pool`] re-points
        // them, exactly as the physical id itself is re-pointed).
        COGNITO_USER_POOL => {
            attributes.extend(user_pool_attributes(region, name));
            (name.to_string(), attributes)
        }
        // "Ref returns the client ID"; Fn::GetAtt: ClientSecret — the secret Amazon Cognito
        // generated for the client, which only `CreateUserPoolClient`'s own answer carries,
        // so what is computed here is the stand-in a declaration is resolved against until
        // that create has answered ([`service_assigned_document`]).
        COGNITO_USER_POOL_CLIENT => {
            attributes.insert(
                "ClientSecret".to_string(),
                service_assigned_document(name, "ClientSecret"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the bucket name"; Fn::GetAtt: Arn, DomainName,
        // DualStackDomainName, RegionalDomainName, WebsiteURL — each one a DNS name S3
        // serves the bucket under, in the form that type's "Return values" section
        // publishes an example of: `DomainName`
        // "mystack-mybucket-kdwwxmddtr2g.s3.amazonaws.com" (the IPv4 DNS name),
        // `DualStackDomainName`
        // "mystack-mybucket-kdwwxmddtr2g.s3.dualstack.us-east-2.amazonaws.com" (the IPv6
        // one), `RegionalDomainName`
        // "mystack-mybucket-kdwwxmddtr2g.s3.us-east-2.amazonaws.com" and `WebsiteURL`
        // "http://mystack-mybucket-kdwwxmddtr2g.s3-website-us-east-2.amazonaws.com" (the
        // bucket's website endpoint). An Amazon S3 bucket ARN carries neither a region nor
        // an account id (`arn:aws:s3:::<bucket>`).
        S3_BUCKET => {
            attributes.insert("Arn".to_string(), format!("arn:{PARTITION}:s3:::{name}"));
            attributes.insert("DomainName".to_string(), format!("{name}.s3.{URL_SUFFIX}"));
            attributes.insert(
                "DualStackDomainName".to_string(),
                format!("{name}.s3.dualstack.{region}.{URL_SUFFIX}"),
            );
            attributes.insert(
                "RegionalDomainName".to_string(),
                format!("{name}.s3.{region}.{URL_SUFFIX}"),
            );
            attributes.insert(
                "WebsiteURL".to_string(),
                format!("http://{name}.s3-website-{region}.{URL_SUFFIX}"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the table bucket ARN": this type's primary identifier is its
        // `TableBucketARN`, which is also its one published `Fn::GetAtt` attribute — the
        // handle every S3 Tables operation on a table bucket takes
        // (`GetTableBucket$tableBucketARN`, an `@httpLabel` whose pattern is
        // `…:bucket/[a-z0-9_-]{3,63}`), so the identifier this type publishes is an ARN
        // rather than the declared name the sibling `AWS::S3::Bucket` publishes. It is the
        // type `super::provisioning` creates in Amazon S3 Tables itself, which then answers
        // with the bucket's own ARN — the same ARN this computes, since both are built from
        // the one account id this emulator mints and the bucket's name, which is how S3
        // Tables keys a table bucket (`table_bucket_arn` in that service's own state
        // module).
        S3TABLES_TABLE_BUCKET => {
            let arn = ::emulator_server::emulator_arn(
                S3TABLES_ARN_NAMESPACE,
                region,
                &format!("{S3TABLES_BUCKET_SEGMENT}{name}"),
            );
            attributes.insert(S3TABLES_TABLE_BUCKET_ARN_ATTRIBUTE.to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the vector bucket ARN": this type's primary identifier is its
        // `VectorBucketArn`, which is also its one published `Fn::GetAtt` attribute. It is
        // the type `super::provisioning` creates in Amazon S3 Vectors itself, which then
        // answers with the bucket's own ARN — the same ARN this computes, since both are
        // built from the one account id this emulator mints and the bucket's name, which is
        // how S3 Vectors keys a vector bucket (`bucket_arn` in that service's own state
        // module).
        S3VECTORS_VECTOR_BUCKET => {
            let arn = ::emulator_server::emulator_arn(
                S3VECTORS_ARN_NAMESPACE,
                region,
                &format!("{S3VECTORS_BUCKET_SEGMENT}{name}"),
            );
            attributes.insert("VectorBucketArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the vector index ARN", and an index ARN NESTS inside the ARN of the
        // vector bucket that holds it (`bucket/<bucket>/index/<name>`), so it is resolved
        // once that bucket's identity is known ([`resolve_dependent_identities`]). The
        // declaration names the bucket the two ways S3 Vectors' own API addresses one — by
        // name or by ARN ([`declared_vector_bucket`]) — and one that names neither leaves
        // the index with CloudFormation's own generated identity rather than an ARN built
        // on a bucket nothing named.
        S3VECTORS_INDEX => {
            let bucket = declared_vector_bucket(properties, scope, budget);
            match bucket {
                Some(bucket) => {
                    let arn = ::emulator_server::emulator_arn(
                        S3VECTORS_ARN_NAMESPACE,
                        region,
                        &format!("{S3VECTORS_BUCKET_SEGMENT}{bucket}/index/{name}"),
                    );
                    attributes.insert("IndexArn".to_string(), arn.clone());
                    (arn, attributes)
                }
                None => (name.to_string(), attributes),
            }
        }
        // "Ref returns the distribution ID"; Fn::GetAtt: DomainName, Id. CloudFront
        // ASSIGNS both — the type has no name property for a template to set — so a
        // declaration reports identifiers of CLOUDFRONT's forms rather than
        // CloudFormation's `<stack>-<logical id>-<unique id>` name (the case
        // AWS::Transfer::Server and AWS::RUM::AppMonitor are in): an id is
        // [`cloudfront_assigned_id`] and the domain name
        // [`cloudfront_distribution_domain_name`].
        CLOUDFRONT_DISTRIBUTION => {
            let id = cloudfront_assigned_id(CLOUDFRONT_DISTRIBUTION_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "DomainName".to_string(),
                cloudfront_distribution_domain_name(name),
            );
            (id, attributes)
        }
        // "Ref returns the origin access control's unique identifier"; Fn::GetAtt: Id.
        // Service-assigned in the same form a distribution id takes.
        CLOUDFRONT_ORIGIN_ACCESS_CONTROL => {
            let id = cloudfront_assigned_id(CLOUDFRONT_ORIGIN_ACCESS_CONTROL_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the CloudFront origin access identity"; Fn::GetAtt: Id,
        // S3CanonicalUserId. The id is service-assigned in the form
        // `CloudFrontOriginAccessIdentity$Id` documents ("for example, E74FTE3AJFJ256A"),
        // and the canonical user id is the one S3 knows the identity by, in the
        // [`S3_CANONICAL_USER_ID_HEX_CHARS`] lowercase hex characters S3 writes one in.
        CLOUDFRONT_ORIGIN_ACCESS_IDENTITY => {
            let id = cloudfront_assigned_id(CLOUDFRONT_ORIGIN_ACCESS_IDENTITY_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "S3CanonicalUserId".to_string(),
                derived_digits(
                    CLOUDFRONT_S3_CANONICAL_USER_ID_SALT,
                    name,
                    S3_CANONICAL_USER_ID_HEX_CHARS,
                    Alphabet::Lower,
                ),
            );
            (id, attributes)
        }
        // "Ref returns the cache policy's unique identifier"; Fn::GetAtt: Id,
        // LastModifiedTime. CloudFront identifies each of its three policy kinds by a
        // UUID it mints, so that is what `Ref` and `Id` report. The policy this emulator
        // CREATES in CloudFront also publishes the instant that service last changed it,
        // which only that service can state: the stand-in says so, and the deployment replaces
        // it with what the create answered.
        CLOUDFRONT_CACHE_POLICY => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "LastModifiedTime".to_string(),
                service_assigned_document(name, "LastModifiedTime"),
            );
            (id, attributes)
        }
        // The other two policy kinds are identified the same way. An ORIGIN REQUEST policy is
        // one this emulator creates in CloudFront as well, so it publishes that service's own
        // `LastModifiedTime` beside its id; a RESPONSE HEADERS policy is created by no service
        // here, so its own is NOT carried — nothing would ever answer it, and an invented
        // timestamp would be worse than no value.
        CLOUDFRONT_ORIGIN_REQUEST_POLICY => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "LastModifiedTime".to_string(),
                service_assigned_document(name, "LastModifiedTime"),
            );
            (id, attributes)
        }
        CLOUDFRONT_RESPONSE_HEADERS_POLICY => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the function ARN"; Fn::GetAtt: FunctionARN,
        // FunctionMetadata.FunctionARN, Stage. The ARN is the one "a distribution's cache
        // behavior" names a published function by, built around the NAME the declaration gives
        // the function, and the type publishes it under both spellings. The STAGE the function
        // is in is CloudFront's to state.
        CLOUDFRONT_FUNCTION => {
            let arn = cloudfront_arn(&format!("function/{name}"));
            attributes.insert("FunctionARN".to_string(), arn.clone());
            attributes.insert("FunctionMetadata.FunctionARN".to_string(), arn.clone());
            attributes.insert(
                "Stage".to_string(),
                service_assigned_document(name, "Stage"),
            );
            (arn, attributes)
        }
        // "Ref returns the key group ID"; Fn::GetAtt: Id, LastModifiedTime. A key group is
        // identified by a UUID CloudFront mints, exactly as its policies are, and the instant
        // it was last changed is that service's to state.
        CLOUDFRONT_KEY_GROUP => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "LastModifiedTime".to_string(),
                service_assigned_document(name, "LastModifiedTime"),
            );
            (id, attributes)
        }
        // "Ref returns the public key ID"; Fn::GetAtt: Id, CreatedTime. CloudFront ASSIGNS the
        // id in the letter-and-13-characters form of that namespace
        // ([`cloudfront_public_key_id`]) — the type declares the key's name and its encoded
        // body, never its identifier — and the instant the key was uploaded is that service's
        // to state.
        CLOUDFRONT_PUBLIC_KEY => {
            let id = cloudfront_public_key_id(name);
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "CreatedTime".to_string(),
                service_assigned_document(name, "CreatedTime"),
            );
            (id, attributes)
        }
        // "Ref returns the key value store ARN"; Fn::GetAtt: Arn, Id. The identity of a key
        // value store is that ARN — what the store's own data plane takes as the `KvsARN` of
        // every one of its routes, and what a function's `KeyValueStoreAssociation` names —
        // built around the UUID CloudFront mints for the store. `Status` is the state that
        // service reports the store in, which only it can state.
        CLOUDFRONT_KEY_VALUE_STORE => {
            let id = generated_uuid(resource_type, name);
            let arn = cloudfront_arn(&format!("key-value-store/{id}"));
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Id".to_string(), id);
            attributes.insert(
                "Status".to_string(),
                service_assigned_document(name, "Status"),
            );
            (arn, attributes)
        }
        // "Ref returns the connection function ARN"; Fn::GetAtt: ConnectionFunctionArn, Id.
        // CloudFront mints the id and builds the ARN around it; the ARN is what a
        // distribution's `ConnectionFunctionAssociation` names, and the id is what the
        // function's own publish, update and delete take. Its status, its stage and the two
        // instants are that service's own report of the function, which only it can state.
        CLOUDFRONT_CONNECTION_FUNCTION => {
            let id = cloudfront_namespaced_id(
                CLOUDFRONT_CONNECTION_FUNCTION_ID_SALT,
                CLOUDFRONT_CONNECTION_FUNCTION_ID_LABEL,
                name,
            );
            let arn = cloudfront_arn(&format!("connection-function/{id}"));
            attributes.insert("ConnectionFunctionArn".to_string(), arn.clone());
            attributes.insert("Id".to_string(), id);
            for attribute in ["CreatedTime", "LastModifiedTime", "Stage", "Status"] {
                attributes.insert(
                    attribute.to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (arn, attributes)
        }
        // "Ref returns the ID of the connection group"; Fn::GetAtt: Arn, Id, RoutingEndpoint.
        // The id is the one CloudFront mints, the ARN is built around it, and the routing
        // endpoint is the DNS name that service assigns the group
        // ([`cloudfront_routing_endpoint`]). Its status, whether it is the account's default
        // group and the two instants are that service's own report of the group, which only it
        // can state.
        CLOUDFRONT_CONNECTION_GROUP => {
            let id = cloudfront_namespaced_id(
                CLOUDFRONT_CONNECTION_GROUP_ID_SALT,
                CLOUDFRONT_CONNECTION_GROUP_ID_LABEL,
                name,
            );
            attributes.insert(
                "Arn".to_string(),
                cloudfront_arn(&format!("connection-group/{id}")),
            );
            attributes.insert("Id".to_string(), id.clone());
            attributes.insert(
                "RoutingEndpoint".to_string(),
                cloudfront_routing_endpoint(name),
            );
            for attribute in ["CreatedTime", "IsDefault", "LastModifiedTime", "Status"] {
                attributes.insert(
                    attribute.to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (id, attributes)
        }
        // "Ref returns the RestApi ID"; Fn::GetAtt: RestApiId, RootResourceId. API Gateway
        // ASSIGNS both ids — a template names the API but not its identifier — in the form
        // [`api_gateway_assigned_id`] describes, and the root resource is a resource of the
        // API, so it carries an id of its own rather than the API's.
        APIGATEWAY_REST_API => {
            let id = api_gateway_assigned_id(APIGATEWAY_REST_API_ID_SALT, name);
            attributes.insert("RestApiId".to_string(), id.clone());
            attributes.insert(
                "RootResourceId".to_string(),
                api_gateway_assigned_id(APIGATEWAY_ROOT_RESOURCE_ID_SALT, name),
            );
            (id, attributes)
        }
        // "Ref returns the resource ID"; Fn::GetAtt: ResourceId.
        APIGATEWAY_RESOURCE => {
            let id = api_gateway_assigned_id(APIGATEWAY_RESOURCE_ID_SALT, name);
            attributes.insert("ResourceId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the deployment ID"; Fn::GetAtt: DeploymentId.
        APIGATEWAY_DEPLOYMENT => {
            let id = api_gateway_assigned_id(APIGATEWAY_DEPLOYMENT_ID_SALT, name);
            attributes.insert("DeploymentId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the API key ID"; Fn::GetAtt: APIKeyId. API Gateway ASSIGNS the id —
        // a template names the key but not its identifier — so the stand-in is of the form
        // [`api_gateway_assigned_id`] describes and is REPLACED by the id `CreateApiKey`
        // really minted, which is what [`identity_is_service_assigned`] names this type
        // for.
        APIGATEWAY_API_KEY => {
            let id = api_gateway_assigned_id(APIGATEWAY_API_KEY_ID_SALT, name);
            attributes.insert("APIKeyId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the authorizer ID"; Fn::GetAtt: AuthorizerId. The same
        // service-assigned shape, replaced by the id `CreateAuthorizer` mints.
        APIGATEWAY_AUTHORIZER => {
            let id = api_gateway_assigned_id(APIGATEWAY_AUTHORIZER_ID_SALT, name);
            attributes.insert("AuthorizerId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the ID for the account"; Fn::GetAtt: Id. The account settings are a
        // SINGLETON of the account and Region — `UpdateAccount` takes no identifier and
        // answers none — so the id a stack publishes for the resource is CloudFormation's
        // own, of the shape API Gateway writes an identifier in, and
        // [`identity_is_service_assigned`] does NOT name this type.
        APIGATEWAY_ACCOUNT => {
            let id = api_gateway_assigned_id(APIGATEWAY_ACCOUNT_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the request validator ID"; Fn::GetAtt: RequestValidatorId. The same
        // service-assigned shape, replaced by the id `CreateRequestValidator` mints.
        APIGATEWAY_REQUEST_VALIDATOR => {
            let id = api_gateway_assigned_id(APIGATEWAY_REQUEST_VALIDATOR_ID_SALT, name);
            attributes.insert("RequestValidatorId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the usage plan ID"; Fn::GetAtt: Id. The same service-assigned shape,
        // replaced by the id `CreateUsagePlan` mints.
        APIGATEWAY_USAGE_PLAN => {
            let id = api_gateway_assigned_id(APIGATEWAY_USAGE_PLAN_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the usage plan key ID"; Fn::GetAtt: Id. An association's id is the id
        // of the API KEY it put in the plan, which only API Gateway produces either, so the
        // stand-in takes the same shape and is replaced by what `CreateUsagePlanKey`
        // answered.
        APIGATEWAY_USAGE_PLAN_KEY => {
            let id = api_gateway_assigned_id(APIGATEWAY_USAGE_PLAN_KEY_ID_SALT, name);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the VPC link ID"; Fn::GetAtt: VpcLinkId. API Gateway ASSIGNS the id —
        // a template names the link but not its identifier — so the stand-in takes the same
        // shape and is REPLACED by the id `CreateVpcLink` really minted, which is what
        // [`identity_is_service_assigned`] names this type for.
        APIGATEWAY_VPC_LINK => {
            let id = api_gateway_assigned_id(APIGATEWAY_VPC_LINK_ID_SALT, name);
            attributes.insert("VpcLinkId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the API ID"; Fn::GetAtt: ApiEndpoint ("The default endpoint for an
        // API"), ApiId and ExecuteApiArn — the api's own identity, the endpoint it is
        // invoked on and the ARN of that invoke surface, every one of them a value only API
        // Gateway v2 produces. API Gateway v2 ASSIGNS the id — a template names the api but
        // not its identifier — so the id is of the form [`api_gateway_assigned_id`]
        // describes, the endpoint is the `{api-id}.execute-api.{region}.amazonaws.com` host
        // `Api$ApiEndpoint` documents under the scheme the declared `ProtocolType` speaks,
        // and the ARN is that id's own `execute-api` resource. All three are REPLACED by the
        // ones built from the id that service really minted once the resource is created
        // there ([`super::provisioner`]), which is what [`identity_is_service_assigned`]
        // names this type for.
        APIGATEWAYV2_API => {
            let id = api_gateway_assigned_id(APIGATEWAYV2_API_ID_SALT, name);
            attributes.insert(
                "ApiEndpoint".to_string(),
                apigatewayv2_api_endpoint(&id, region, properties, scope, budget),
            );
            attributes.insert("ApiId".to_string(), id.clone());
            attributes.insert(
                "ExecuteApiArn".to_string(),
                apigatewayv2_execute_api_arn(&id, region),
            );
            (id, attributes)
        }
        // "Ref returns the API mapping ID"; Fn::GetAtt: ApiMappingId. The same
        // service-assigned shape, replaced by the id `CreateApiMapping` mints.
        APIGATEWAYV2_API_MAPPING => {
            let id = api_gateway_assigned_id(APIGATEWAYV2_API_MAPPING_ID_SALT, name);
            attributes.insert("ApiMappingId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the domain name"; Fn::GetAtt: DomainNameArn, RegionalDomainName,
        // RegionalHostedZoneId. The physical id is the domain name the template states
        // (the type's own identifier, read through [`declared_name_property`]); the ARN is
        // the one API Gateway names a custom domain by (its own REST path,
        // `/domainnames/{name}`, under the `apigateway` ARN namespace that service's model
        // declares), and the regional domain name is the
        // `d-{id}.execute-api.{region}.amazonaws.com` host a caller aliases their DNS
        // record to. `RegionalHostedZoneId` is NOT carried: it names a Route 53 zone this
        // emulator neither owns nor can look up, and an invented zone id is one a caller
        // could put in a real alias record.
        APIGATEWAYV2_DOMAIN_NAME => {
            attributes.insert(
                "DomainNameArn".to_string(),
                ::emulator_server::emulator_arn(
                    APIGATEWAYV2_ARN_NAMESPACE,
                    region,
                    &format!("/domainnames/{name}"),
                ),
            );
            attributes.insert(
                "RegionalDomainName".to_string(),
                apigatewayv2_regional_domain_name(name, region),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the table name"; Fn::GetAtt: Arn, StreamArn. The stream's own ARN
        // nests inside the table's (`table/<name>/stream/<label>`) and its LABEL is "a
        // timestamp, in ISO 8601 format" the owning service writes when it creates the stream
        // — which no naming rule could compute — so what the rules give the resource is the
        // stand-in form of it, and the table's own create re-points the attribute at the ARN
        // Amazon DynamoDB answered ([`crate::state::provisioning`] re-points every attribute
        // the owning service reports differently). The attribute is published ONLY for a
        // table whose declaration enables a stream ("You must specify the StreamSpecification
        // property to use this attribute"): a table with no stream has no stream ARN, and a
        // declaration reading one reads an attribute that resource does not have.
        "AWS::DynamoDB::Table" => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("dynamodb", region, &format!("table/{name}")),
            );
            if stream_is_enabled(properties.get(DYNAMODB_STREAM_SPECIFICATION)) {
                let label = service_assigned_label(
                    DYNAMODB_STREAM_LABEL_SALT,
                    name,
                    DYNAMODB_STREAM_LABEL_DIGITS,
                );
                attributes.insert(
                    "StreamArn".to_string(),
                    ::emulator_server::emulator_arn(
                        "dynamodb",
                        region,
                        &format!("table/{name}/stream/{label}"),
                    ),
                );
            }
            (name.to_string(), attributes)
        }
        // "Ref returns the table name"; Fn::GetAtt: Arn, StreamArn, TableId. The same type
        // declared for a GROUP of Regions as the one above is for one, so the table ARN and
        // the stand-in stream ARN are computed the same way — the stack's own Region holds
        // the table the resource IS, and each replica's ARN is that Region's. `TableId` is
        // "unique identifier of the table" Amazon DynamoDB mints per regional table, which
        // no naming rule can compute: what the rules give the resource is a stand-in of that
        // member's own form, which the create re-points at the id that service answered.
        DYNAMODB_GLOBAL_TABLE => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("dynamodb", region, &format!("table/{name}")),
            );
            // A global table always HAS a stream: the recorded deployment of this type
            // reports one for every regional table of the group, whatever the declaration
            // states, so the attribute is published for every such resource — unlike the
            // single-table type above, whose stream exists only where a declaration enables
            // one.
            let label = service_assigned_label(
                DYNAMODB_STREAM_LABEL_SALT,
                name,
                DYNAMODB_STREAM_LABEL_DIGITS,
            );
            attributes.insert(
                "StreamArn".to_string(),
                ::emulator_server::emulator_arn(
                    "dynamodb",
                    region,
                    &format!("table/{name}/stream/{label}"),
                ),
            );
            attributes.insert("TableId".to_string(), generated_uuid(resource_type, name));
            (name.to_string(), attributes)
        }
        // "Ref returns the function name"; Fn::GetAtt: Arn.
        "AWS::Lambda::Function" => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("lambda", region, &format!("function:{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the ARN of the function version"; Fn::GetAtt: FunctionArn, Version.
        // Both carry the version NUMBER, which is AWS Lambda's to allocate — "each time you
        // publish a version, Lambda increments the version number" — so neither can be
        // computed from the template: what this gives the resource is the stand-in the
        // publish then replaces, under the identifier CloudFormation computed for it
        // ([`identity_is_service_assigned`] names the type, and
        // [`crate::state::provisioning`] re-points the id and the attribute together).
        LAMBDA_VERSION => {
            match lambda_function_arn(properties, scope, budget, region) {
                Some(function_arn) => {
                    let arn = format!("{function_arn}:{name}");
                    attributes.insert("FunctionArn".to_string(), arn.clone());
                    attributes.insert("Version".to_string(), name.to_string());
                    (arn, attributes)
                }
                // A declaration naming no function leaves the version with
                // CloudFormation's own generated identity rather than an ARN built on a
                // value that is not one.
                None => (name.to_string(), attributes),
            }
        }
        // "Ref returns the alias ARN"; Fn::GetAtt: AliasArn. An alias's ARN is the FUNCTION's
        // own qualified by the alias name ("arn:aws:lambda:us-west-2:123456789012:function:
        // my-function:BLUE"), so it is resolved once the function the declaration names has
        // an identity ([`resolve_dependent_identities`]) — and it is the ARN `CreateAlias`
        // answers, since both are built from the one account id this emulator mints, the
        // function's name and the alias's.
        LAMBDA_ALIAS => {
            match lambda_function_arn(properties, scope, budget, region) {
                Some(function_arn) => {
                    let arn = format!("{function_arn}:{name}");
                    attributes.insert("AliasArn".to_string(), arn.clone());
                    (arn, attributes)
                }
                None => (name.to_string(), attributes),
            }
        }
        // "Ref returns the log group name"; Fn::GetAtt: Arn. A CloudWatch Logs log-group
        // ARN ends in `:*` ("such as
        // arn:aws:logs:us-east-1:123456789012:log-group:/mystack-testgroup-12ABC1AB12A1:*").
        "AWS::Logs::LogGroup" => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    "logs",
                    region,
                    &format!("log-group:{name}:*"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the Amazon Resource Name (ARN) of the server"; Fn::GetAtt: Arn,
        // ServerId ("the service-assigned ID of the server that is created") and the
        // list-valued As2ServiceManagedEgressIpAddresses, which is not carried. The
        // template names no server — AWS::Transfer::Server has no name property — so the
        // id is the service-assigned `s-` + 17 hexadecimal form.
        TRANSFER_SERVER => {
            let server_id = generated_service_id("s", name, TRANSFER_ID_HEX_DIGITS);
            let arn =
                ::emulator_server::emulator_arn("transfer", region, &format!("server/{server_id}"));
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("ServerId".to_string(), server_id);
            (arn, attributes)
        }
        // The same shape one type along: "Ref returns the Amazon Resource Name (ARN)";
        // Fn::GetAtt: Arn, WorkflowId. Transfer's `WorkflowId` is `w-` + 17 characters of
        // `[a-z0-9]` (model pattern `^w-([a-z0-9]{17})$`), which hexadecimal satisfies.
        "AWS::Transfer::Workflow" => {
            let workflow_id = generated_service_id("w", name, TRANSFER_ID_HEX_DIGITS);
            let arn = ::emulator_server::emulator_arn(
                "transfer",
                region,
                &format!("workflow/{workflow_id}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("WorkflowId".to_string(), workflow_id);
            (arn, attributes)
        }
        // "Ref returns the AWS::IAM::AccessKey resource's access key ID"; Fn::GetAtt:
        // SecretAccessKey — "the secret access key for the specified AWS::IAM::AccessKey
        // resource". IAM MINTS both: `CreateAccessKey` takes no identifier and answers the
        // key id and the secret it generated, and the secret "is returned only in response
        // to CreateAccessKey", so neither is a value these naming rules can compute. The
        // identifier a declaration reports until that create answers is the name
        // CloudFormation generated for the resource ([`identity_is_service_assigned`] names
        // the type, so [`super::provisioning::reconcile`] adopts the minted id over it), and
        // the secret is a stand-in that same walk re-points at what IAM answered — never an
        // invented credential.
        IAM_ACCESS_KEY => {
            attributes.insert(
                "SecretAccessKey".to_string(),
                service_assigned_document(name, "SecretAccessKey"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the role name"; Fn::GetAtt: Arn. IAM is a global service, so its
        // ARNs carry no region.
        "AWS::IAM::Role" => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("iam", "", &format!("role/{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the rule name"; Fn::GetAtt: Arn. An EventBridge rule ARN names the
        // bus the rule is bound to unless that is the account's `default` bus
        // (`arn:aws:events:<region>:<account>:rule/[<bus>/]<name>`), which is the form the
        // `events` service mints for the rule this stack provisions.
        EVENTS_RULE => {
            let bus = properties
                .get("EventBusName")
                .and_then(|value| text(value, scope, 0, budget))
                .map(|bus| event_bus_name(&bus))
                .filter(|bus| !bus.is_empty() && bus != DEFAULT_EVENT_BUS)
                .map(|bus| format!("{bus}/"))
                .unwrap_or_default();
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("events", region, &format!("rule/{bus}{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the event bus name"; Fn::GetAtt: Arn, Name
        // ([`EVENTS_EVENT_BUS_ATTRIBUTES`]).
        EVENTS_EVENT_BUS => (name.to_string(), event_bus_attributes(region, name)),
        // "Ref returns the name of the parameter"; Fn::GetAtt: Type, Value — the two
        // required properties of the declaration, which is what the parameter Systems
        // Manager holds carries.
        SSM_PARAMETER => {
            for attribute in ["Type", "Value"] {
                if let Some(declared) = properties
                    .get(attribute)
                    .and_then(|value| text(value, scope, 0, budget))
                {
                    attributes.insert(attribute.to_string(), declared);
                }
            }
            (name.to_string(), attributes)
        }
        // "Ref returns the group name"; Fn::GetAtt: Arn. This is the type
        // `super::provisioning` creates in Resource Groups itself, which keys a group by its
        // name and mints the same ARN from the Region and that name (`group_arn` in its own
        // state module). The ARN namespace is that model's `arnNamespace`,
        // [`RESOURCE_GROUPS_ARN_NAMESPACE`], not the type's CloudFormation segment.
        RESOURCE_GROUPS_GROUP => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    RESOURCE_GROUPS_ARN_NAMESPACE,
                    region,
                    &format!("group/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the ApplicationARN"; Fn::GetAtt: ApplicationARN — the one value the type
        // publishes, and the handle a client names the application by. This is the type
        // `crate::state::provisioner` creates in Application Insights itself, which keys an
        // application by the RESOURCE GROUP it monitors, so the ARN is built from that group
        // rather than from a name of the resource's own: an application has no name property at
        // all. That service's own record of an application (`ApplicationInfo`) carries no ARN
        // member for a create to report back, and the ARN is a total function of the partition,
        // its namespace, the Region, the account and the group — so it is computed here, exactly
        // as a resource group's and a schedule group's are above. The namespace is that model's
        // `arnNamespace`, [`APPLICATION_INSIGHTS_ARN_NAMESPACE`], not the type's CloudFormation
        // segment.
        //
        // A declaration that states no group yet — one whose `ResourceGroupName` is a reference
        // this evaluation cannot resolve — publishes no ARN at all, and the resource keeps the
        // generated name as its stand-in identity until the group's own is known
        // ([`super::super::identity_depends_on_another_resource`]).
        APPLICATION_INSIGHTS_APPLICATION => {
            let Some(group) = properties
                .get(APPLICATION_INSIGHTS_RESOURCE_GROUP_PROPERTY)
                .and_then(|value| text(value, scope, 0, budget))
                .filter(|group| !group.is_empty())
            else {
                return (name.to_string(), attributes);
            };
            let arn = ::emulator_server::emulator_arn(
                APPLICATION_INSIGHTS_ARN_NAMESPACE,
                region,
                &format!("{APPLICATION_INSIGHTS_APPLICATION_PATH}/{group}"),
            );
            attributes.insert(
                APPLICATION_INSIGHTS_APPLICATION_ARN_ATTRIBUTE.to_string(),
                arn.clone(),
            );
            (arn, attributes)
        }
        // "Ref returns the schedule group name"; Fn::GetAtt: Arn (and the group's State,
        // CreationDate and LastModificationDate, which are EventBridge Scheduler's own to
        // decide and are not computed here). This is the type `super::provisioning`
        // creates in that service, which keys a group by its name and mints the same ARN
        // from the Region and that name (`schedule_group_arn` in its own state module).
        SCHEDULER_SCHEDULE_GROUP => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    SCHEDULER_ARN_NAMESPACE,
                    region,
                    &format!("schedule-group/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the schedule name"; Fn::GetAtt: Arn. A schedule ARN nests the
        // schedule inside the GROUP that holds it (`schedule/<group>/<name>`), and a
        // declaration naming no group is in the account's `default` group — the same ARN
        // EventBridge Scheduler mints for the schedule this stack provisions
        // (`schedule_arn` in its own state module).
        SCHEDULER_SCHEDULE => {
            let group = properties
                .get("GroupName")
                .and_then(|value| text(value, scope, 0, budget))
                .filter(|group| !group.is_empty())
                .unwrap_or_else(|| DEFAULT_SCHEDULE_GROUP.to_string());
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    SCHEDULER_ARN_NAMESPACE,
                    region,
                    &format!("schedule/{group}/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the repository name"; Fn::GetAtt: Arn. Amazon ECR Public is a
        // partition-wide service, so the ARN carries no region — the same form the
        // `ecr-public` service mints for the repository this stack provisions, which is
        // what lets the two be compared.
        ECR_PUBLIC_REPOSITORY => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("ecr-public", "", &format!("repository/{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the delivery stream name"; Fn::GetAtt: Arn. This is the type
        // `super::provisioning` creates in Amazon Data Firehose itself, which then
        // reports the stream's own ARN back over this attribute.
        FIREHOSE_DELIVERY_STREAM => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    "firehose",
                    region,
                    &format!("deliverystream/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the stream name"; Fn::GetAtt: Arn. This is the type
        // `super::provisioning` creates in Amazon Kinesis Data Streams itself, which then
        // reports the stream's own ARN back over this attribute — the same ARN this
        // computes, since both are built from the one account id this emulator mints and
        // the stream's name, which is how that service keys a stream (`stream_arn` in its
        // own state module).
        KINESIS_STREAM => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("kinesis", region, &format!("stream/{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the repository name"; Fn::GetAtt: Arn, RepositoryUri. The URI is
        // the registry host of the account and Region plus the repository's name
        // ("The format is `aws_account_id.dkr.ecr.region.amazonaws.com/repository-name`"),
        // which is the form a client's own reference is written in. A repository this stack
        // PROVISIONS reports the URI Amazon ECR itself answered instead — the address that
        // registry announces for a push and a pull, read off the create
        // (`super::super::provisioner`) — so this stands in only until that call has run.
        ECR_REPOSITORY => {
            // "A repository name must start with a letter and can only contain lowercase
            // letters, numbers, hyphens, underscores, and forward slashes"
            // (`RepositoryName`'s pattern is lower case only), which is the character set
            // [`naming_rules`] already holds a GENERATED name to; a name the template
            // declared is reported exactly as it wrote it.
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("ecr", region, &format!("repository/{name}")),
            );
            attributes.insert(
                "RepositoryUri".to_string(),
                format!(
                    "{}.dkr.ecr.{region}.{URL_SUFFIX}/{name}",
                    ::emulator_server::emulator_account_id()
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the cluster name"; Fn::GetAtt: Arn. This is the type
        // `super::provisioning` creates in Amazon ECS itself, so the cluster a stack's
        // output names is one `ecs ListTasks` finds.
        ECS_CLUSTER => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("ecs", region, &format!("cluster/{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the name of the cluster"; Fn::GetAtt: Arn, CertificateAuthorityData,
        // ClusterSecurityGroupId, EncryptionConfigKeyArn, Endpoint, OpenIdConnectIssuerUrl.
        // This is the type `super::provisioning` creates in Amazon EKS itself, so the
        // cluster a stack's output names is one `eks describe-cluster` finds — and every
        // attribute but the ARN is a value only that service can produce (the API server's
        // endpoint, the cluster CA, the OIDC issuer, the security group it made for the
        // control plane), so each stands in until the create reports what EKS really
        // answered.
        EKS_CLUSTER => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(EKS_ARN_NAMESPACE, region, &format!("cluster/{name}")),
            );
            for attribute in [
                "CertificateAuthorityData",
                "ClusterSecurityGroupId",
                "EncryptionConfigKeyArn",
                "Endpoint",
                "OpenIdConnectIssuerUrl",
            ] {
                attributes.insert(
                    attribute.to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (name.to_string(), attributes)
        }
        // "Ref returns the add-on name" / the node group's name; Fn::GetAtt: Arn. Both are
        // types `super::provisioning` creates in Amazon EKS itself, on the cluster the
        // declaration names, and the ARN each publishes carries an identifier only that
        // service draws (`addon/<cluster>/<add-on>/<id>`,
        // `nodegroup/<cluster>/<group>/<id>`), so it stands in until the create reports what
        // EKS really answered.
        EKS_ADDON | EKS_NODEGROUP => {
            attributes.insert(
                "Arn".to_string(),
                service_assigned_document(name, "Arn"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the association ARN"; Fn::GetAtt: AssociationArn, AssociationId.
        // Amazon EKS ASSIGNS both — `CreatePodIdentityAssociation` takes the cluster, the
        // namespace, the service account and the role and answers the association it minted
        // — so a declaration reports a stand-in until `super::provisioning` has created the
        // association and reported the ARN and the id that service really assigned.
        EKS_POD_IDENTITY_ASSOCIATION => {
            let arn = ::emulator_server::emulator_arn(
                EKS_ARN_NAMESPACE,
                region,
                &format!("podidentityassociation/{name}"),
            );
            attributes.insert("AssociationArn".to_string(), arn.clone());
            attributes.insert(
                "AssociationId".to_string(),
                service_assigned_document(name, "AssociationId"),
            );
            (arn, attributes)
        }
        // "Ref returns the compute environment ARN"; Fn::GetAtt: ComputeEnvironmentArn.
        // This is the type `super::provisioning` creates in AWS Batch itself, so the
        // environment a stack's output names is one `batch describe-compute-environments`
        // finds — under the same ARN that service mints from the Region and the name
        // (`compute_environment_arn` in its own state module).
        BATCH_COMPUTE_ENVIRONMENT => {
            let arn = ::emulator_server::emulator_arn(
                BATCH_ARN_NAMESPACE,
                region,
                &format!("compute-environment/{name}"),
            );
            attributes.insert("ComputeEnvironmentArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the job queue ARN"; Fn::GetAtt: JobQueueArn. The same shape one
        // type along, on the ARN AWS Batch mints for a queue (`job_queue_arn`).
        BATCH_JOB_QUEUE => {
            let arn = ::emulator_server::emulator_arn(
                BATCH_ARN_NAMESPACE,
                region,
                &format!("job-queue/{name}"),
            );
            attributes.insert("JobQueueArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the job definition ARN"; Fn::GetAtt: JobDefinitionArn.
        //
        // A job definition is identified by its name AND a revision, not by the name alone:
        // the ARN AWS Batch mints for one ends `:<revision>` (`job_definition_arn` in that
        // service's own state module), and that revision is the one the REGISTRATION was
        // given — "the revision of the job definition", one past the highest the account
        // already holds under the name. Nothing in a template decides it, so the revision
        // here is the FIRST one, which is what a name no revision is registered under yet
        // gets, and the identity in force is the one that service answered
        // ([`identity_is_service_assigned`] names the type, and
        // [`crate::state::provisioner::batch_job_definitions`] registers the revision).
        BATCH_JOB_DEFINITION => {
            let arn = ::emulator_server::emulator_arn(
                BATCH_ARN_NAMESPACE,
                region,
                &format!("job-definition/{name}:{FIRST_JOB_DEFINITION_REVISION}"),
            );
            attributes.insert(
                "JobDefinitionArn".to_string(),
                arn.clone(),
            );
            (arn, attributes)
        }
        // And one more: "Ref returns the service environment ARN"; Fn::GetAtt:
        // ServiceEnvironmentArn (`service_environment_arn`).
        BATCH_SERVICE_ENVIRONMENT => {
            let arn = ::emulator_server::emulator_arn(
                BATCH_ARN_NAMESPACE,
                region,
                &format!("service-environment/{name}"),
            );
            attributes.insert("ServiceEnvironmentArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the Amazon Resource Name (ARN) of the task definition";
        // Fn::GetAtt: TaskDefinitionArn.
        //
        // A task definition is identified by its FAMILY and a revision, not by a name
        // property ("the family and revision (family:revision)"), so the family is the
        // declared `Family` — evaluated through the template's intrinsics, as every
        // declared name is — and the revision is the FIRST one, which is the revision the
        // registration a stack declaring a family makes carries. A later registration of
        // the same family is numbered by Amazon ECS, and nothing here re-reads that
        // number.
        ECS_TASK_DEFINITION => {
            let family = properties
                .get("Family")
                .and_then(|value| text(value, scope, 0, budget))
                .unwrap_or_else(|| name.to_string());
            let arn = ::emulator_server::emulator_arn(
                "ecs",
                region,
                &format!("task-definition/{family}:{FIRST_TASK_DEFINITION_REVISION}"),
            );
            attributes.insert("TaskDefinitionArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the ARN of the load balancer"; Fn::GetAtt: CanonicalHostedZoneID,
        // DNSName, LoadBalancerFullName, LoadBalancerName, SecurityGroups.
        //
        // This is the type `super::provisioning` creates in Elastic Load Balancing itself, so
        // the ARN a stack's output names is one that service really holds a load balancer
        // for, and the values only IT can produce stand in until its create has answered:
        // the hosted zone it publishes for the Region and the family, and the groups in front
        // of the load balancer — which for an Application Load Balancer a declaration names
        // none for are the default security group of its VPC. The ARN and the DNS name carry
        // an id that service ASSIGNS (`.../loadbalancer/app/<name>/<id>`,
        // `<name>-<id>.<region>.elb.amazonaws.com`, prefixed `internal-` for an internal
        // scheme), which is why those too are the create's to report and the values computed
        // here are only what a declaration is resolved against until then.
        ELB_LOAD_BALANCER => {
            let id = service_assigned_label(ELB_LOAD_BALANCER_ID_SALT, name, ELB_ID_HEX_DIGITS);
            let type_code = load_balancer_type_code(properties, scope, budget);
            let path = format!("{type_code}/{name}/{id}");
            let internal = properties
                .get("Scheme")
                .and_then(|value| text(value, scope, 0, budget))
                .as_deref()
                == Some(ELB_SCHEME_INTERNAL);
            let prefix = if internal { "internal-" } else { "" };
            attributes.insert(
                "CanonicalHostedZoneID".to_string(),
                service_assigned_document(name, "CanonicalHostedZoneID"),
            );
            attributes.insert(
                "DNSName".to_string(),
                format!("{prefix}{name}-{id}.{region}.elb.{URL_SUFFIX}"),
            );
            attributes.insert("LoadBalancerFullName".to_string(), path.clone());
            attributes.insert("LoadBalancerName".to_string(), name.to_string());
            attributes.insert(
                "SecurityGroups".to_string(),
                service_assigned_document(name, "SecurityGroups"),
            );
            (
                ::emulator_server::emulator_arn(
                    "elasticloadbalancing",
                    region,
                    &format!("loadbalancer/{path}"),
                ),
                attributes,
            )
        }
        // "Ref returns the ARN of the target group"; Fn::GetAtt: LoadBalancerArns,
        // TargetGroupArn, TargetGroupFullName, TargetGroupName. The full name is the ARN's own
        // resource path, `targetgroup/<name>/<id>`, with the same service-assigned id form,
        // and `LoadBalancerArns` — "the ARN of the load balancer that routes traffic to this
        // target group" — is a value only Elastic Load Balancing can produce, so it stands in
        // until that service has answered for the group `super::provisioning` creates there.
        ELB_TARGET_GROUP => {
            let id = service_assigned_label(ELB_TARGET_GROUP_ID_SALT, name, ELB_ID_HEX_DIGITS);
            let path = format!("targetgroup/{name}/{id}");
            let arn =
                ::emulator_server::emulator_arn("elasticloadbalancing", region, &path);
            attributes.insert(
                "LoadBalancerArns".to_string(),
                service_assigned_document(name, "LoadBalancerArns"),
            );
            attributes.insert("TargetGroupArn".to_string(), arn.clone());
            attributes.insert("TargetGroupFullName".to_string(), path);
            attributes.insert("TargetGroupName".to_string(), name.to_string());
            (arn, attributes)
        }
        // "Ref returns the DBClusterIdentifier"; Fn::GetAtt: DBClusterArn,
        // DBClusterResourceId, Endpoint.Address, Endpoint.Port, MasterUserSecret.SecretArn,
        // ReadEndpoint.Address. The endpoints are the published Aurora forms,
        // `<identifier>.cluster-<label>` and `<identifier>.cluster-ro-<label>` under the
        // regional RDS suffix. The managed secret's ARN is a value only Amazon RDS can
        // produce — it mints the secret when the declaration turns ManageMasterUserPassword
        // on — so it stands in until that service has answered for the cluster.
        "AWS::RDS::DBCluster" => {
            let label = service_assigned_label(
                RDS_ENDPOINT_LABEL_SALT,
                name,
                RDS_ENDPOINT_LABEL_HEX_DIGITS,
            );
            attributes.insert(
                "DBClusterArn".to_string(),
                ::emulator_server::emulator_arn("rds", region, &format!("cluster:{name}")),
            );
            attributes.insert(
                "DBClusterResourceId".to_string(),
                generated_upper_resource_id("cluster", name, RDS_RESOURCE_ID_DIGITS),
            );
            attributes.insert(
                "Endpoint.Address".to_string(),
                format!("{name}.cluster-{label}.{region}.rds.{URL_SUFFIX}"),
            );
            if let Some(port) = database_port(properties, scope, budget) {
                attributes.insert("Endpoint.Port".to_string(), port);
            }
            attributes.insert(
                MASTER_USER_SECRET_ARN_ATTRIBUTE.to_string(),
                service_assigned_document(name, MASTER_USER_SECRET_ARN_ATTRIBUTE),
            );
            attributes.insert(
                "ReadEndpoint.Address".to_string(),
                format!("{name}.cluster-ro-{label}.{region}.rds.{URL_SUFFIX}"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the DBInstanceIdentifier"; Fn::GetAtt: DBInstanceArn, DbiResourceId,
        // Endpoint.Address, Endpoint.Port, MasterUserSecret.SecretArn (Endpoint.HostedZoneId
        // is a value AWS allocated per Region — nothing computes it — so it is left out
        // rather than invented). The managed secret's ARN stands in for the same reason the
        // DB cluster's does.
        "AWS::RDS::DBInstance" => {
            let label = service_assigned_label(
                RDS_ENDPOINT_LABEL_SALT,
                name,
                RDS_ENDPOINT_LABEL_HEX_DIGITS,
            );
            attributes.insert(
                "DBInstanceArn".to_string(),
                ::emulator_server::emulator_arn("rds", region, &format!("db:{name}")),
            );
            attributes.insert(
                "DbiResourceId".to_string(),
                generated_upper_resource_id("db", name, RDS_RESOURCE_ID_DIGITS),
            );
            attributes.insert(
                "Endpoint.Address".to_string(),
                format!("{name}.{label}.{region}.rds.{URL_SUFFIX}"),
            );
            if let Some(port) = database_port(properties, scope, budget) {
                attributes.insert("Endpoint.Port".to_string(), port);
            }
            attributes.insert(
                MASTER_USER_SECRET_ARN_ATTRIBUTE.to_string(),
                service_assigned_document(name, MASTER_USER_SECRET_ARN_ATTRIBUTE),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the ARN of the secret"; Fn::GetAtt: Id, which is that same ARN. A
        // Secrets Manager secret ARN ends in a service-assigned six-character suffix
        // (`arn:aws:secretsmanager:<region>:<account>:secret:<name>-<suffix>`), so the
        // resource id is of THAT form rather than the name.
        SECRETSMANAGER_SECRET => {
            let suffix =
                service_assigned_label(SECRET_ARN_SUFFIX_SALT, name, SECRET_ARN_SUFFIX_CHARACTERS);
            let arn = ::emulator_server::emulator_arn(
                "secretsmanager",
                region,
                &format!("secret:{name}-{suffix}"),
            );
            attributes.insert("Id".to_string(), arn.clone());
            (arn, attributes)
        }
        // The Amazon EC2 VPC-network types ([`EC2_VPC`] and its siblings), each of which
        // publishes "Ref returns the ID of …" and one `Fn::GetAtt` attribute that IS that
        // id: a VPC's `VpcId`, a subnet's `SubnetId`, a security group's `GroupId`, an
        // internet gateway's `InternetGatewayId`, and the plain `Id` the types that publish
        // no narrower name carry. The id is the `<prefix>-` + 17 hexadecimal form Amazon
        // EC2 assigns, never the name CloudFormation computed, because EC2 takes no name on
        // any of these creates ([`ec2_network_identity`]).
        EC2_VPC => {
            let id = ec2_network_identity("vpc", "VpcId", name, &mut attributes);
            (id, attributes)
        }
        EC2_SUBNET => {
            let id = ec2_network_identity("subnet", "SubnetId", name, &mut attributes);
            (id, attributes)
        }
        EC2_SECURITY_GROUP => {
            let id = ec2_network_identity("sg", "GroupId", name, &mut attributes);
            (id, attributes)
        }
        EC2_INTERNET_GATEWAY => {
            let id = ec2_network_identity("igw", "InternetGatewayId", name, &mut attributes);
            (id, attributes)
        }
        EC2_ROUTE_TABLE => {
            let id = ec2_network_identity("rtb", "RouteTableId", name, &mut attributes);
            (id, attributes)
        }
        // An Elastic IP address, whose published return values are the two halves of what
        // `AllocateAddress` answers: "Ref returns the Elastic IP address" and "AllocationId
        // — the ID that represents the allocation of the Elastic IP address". Only the
        // allocation id has a FORM a rule here can compute (Amazon EC2's own `eipalloc-` plus
        // 17 hexadecimal characters), which is the stand-in a NAT gateway of the same
        // template is resolved against before the address is allocated and which the
        // deployment then re-points at the allocation that service answered. The address
        // itself is this resource's physical id, so it is the identity's own stand-in rather
        // than an attribute computed beside it, and the create reports the address EC2
        // allocated ([`super::provisioning`]).
        EC2_EIP => {
            attributes.insert(
                "AllocationId".to_string(),
                generated_service_id("eipalloc", name, EC2_ID_HEX_DIGITS),
            );
            (name.to_string(), attributes)
        }
        EC2_NAT_GATEWAY => {
            let id = ec2_network_identity("nat", "NatGatewayId", name, &mut attributes);
            (id, attributes)
        }
        EC2_DHCP_OPTIONS => {
            let id = ec2_network_identity("dopt", "DhcpOptionsId", name, &mut attributes);
            (id, attributes)
        }
        EC2_VPN_GATEWAY => {
            let id = ec2_network_identity("vgw", "VPNGatewayId", name, &mut attributes);
            (id, attributes)
        }
        EC2_NETWORK_ACL => {
            let id = ec2_network_identity("acl", "Id", name, &mut attributes);
            (id, attributes)
        }
        EC2_NETWORK_INTERFACE => {
            let id = ec2_network_identity("eni", "Id", name, &mut attributes);
            (id, attributes)
        }
        EC2_VPC_ENDPOINT => {
            let id = ec2_network_identity("vpce", "Id", name, &mut attributes);
            (id, attributes)
        }
        // An Amazon EBS volume, whose identity Amazon EC2 assigns the same way: "Ref returns
        // the volume ID", and the type's one `Fn::GetAtt` attribute IS that id.
        EC2_VOLUME => {
            let id = ec2_network_identity("vol", "VolumeId", name, &mut attributes);
            (id, attributes)
        }
        // A CLIENT VPN ENDPOINT, whose identity Amazon EC2 assigns the same way again: "Ref
        // returns the Client VPN endpoint ID" — the `cvpn-endpoint-` + 17 hexadecimal form —
        // while the type publishes no `Fn::GetAtt` attribute at all, so no attribute is
        // recorded beside it. The computed id is only the stand-in a declaration is resolved
        // against before the endpoint exists ([`identity_is_service_assigned`] names the
        // type); the deployment re-points it at the id that service minted
        // ([`crate::state::provisioner::client_vpn`]).
        crate::state::provisioner::client_vpn::EC2_CLIENT_VPN_ENDPOINT => (
            generated_service_id("cvpn-endpoint", name, EC2_ID_HEX_DIGITS),
            attributes,
        ),
        // A LAUNCH TEMPLATE, whose identity Amazon EC2 assigns the same way: "Ref returns the
        // launch template ID" (the `lt-` + 17 hexadecimal form), and whose published
        // `Fn::GetAtt` attributes are that same id and the two VERSION numbers of the
        // template — the latest and the default, both 1 for the one version
        // `CreateLaunchTemplate` creates ([`FIRST_LAUNCH_TEMPLATE_VERSION`]). The create
        // re-points all three at what EC2 answered.
        EC2_LAUNCH_TEMPLATE => {
            let id = ec2_network_identity("lt", "LaunchTemplateId", name, &mut attributes);
            for attribute in ["DefaultVersionNumber", "LatestVersionNumber"] {
                attributes.insert(
                    attribute.to_string(),
                    FIRST_LAUNCH_TEMPLATE_VERSION.to_string(),
                );
            }
            (id, attributes)
        }
        // An INSTANCE, whose identity Amazon EC2 assigns the same way again — "Ref returns the
        // ID of the instance", the `i-` + 17 hexadecimal form — and whose published
        // `Fn::GetAtt` attributes are NONE of them that id: they are the zone the instance was
        // placed in and its four addresses and DNS names, every one of which only Amazon EC2
        // can produce. Each therefore starts EMPTY, which is both what a reference to a
        // resource that does not exist yet can resolve to and what AWS answers for an address
        // an instance was never assigned; the launch's own answer and the read that follows the
        // instance coming up are what fill them (`crate::state::provisioning`'s instance
        // provider).
        EC2_INSTANCE => {
            for attribute in published_attribute_names(EC2_INSTANCE) {
                attributes.insert((*attribute).to_string(), String::new());
            }
            (
                generated_service_id("i", name, EC2_ID_HEX_DIGITS),
                attributes,
            )
        }
        // "Ref returns the Amazon Resource Name (ARN)"; Fn::GetAtt: Name. A service ARN
        // carries the CLUSTER that hosts it (`service/<cluster>/<name>`), which the
        // declaration names by short name or by cluster ARN and which defaults to the
        // `default` cluster — so the cluster is resolved once the resource the declaration
        // names has an identity ([`resolve_dependent_identities`]). The `Arn`/`ServiceArn`
        // the convention adds is that same ARN, since it is the service's physical id.
        ECS_SERVICE => {
            let cluster = properties
                .get("Cluster")
                .and_then(|value| text(value, scope, 0, budget))
                .map(|reference| ecs_cluster_name(&reference))
                .unwrap_or_else(|| ECS_DEFAULT_CLUSTER.to_string());
            attributes.insert("Name".to_string(), name.to_string());
            (
                ::emulator_server::emulator_arn(
                    "ecs",
                    region,
                    &format!("service/{cluster}/{name}"),
                ),
                attributes,
            )
        }
        // "Ref returns the ID of the task set" — which for this type is the TRIPLE Amazon ECS
        // addresses one by, the cluster and the service it lives in and the `id` that service
        // minted (`crate::state::provisioner::ecs_task_sets`); Fn::GetAtt: Id, that id itself.
        // The id is Amazon ECS's own to mint — `CreateTaskSet` takes none and answers the one
        // it assigned — so what the naming rules give the resource is the STAND-IN both are
        // resolved against until that create has answered, exactly as the AppConfig rollout
        // types' numbers are ([`identity_is_service_assigned`] names this type, and
        // `super::provisioning` re-points the identity and the attribute at what Amazon ECS
        // really answered).
        crate::state::provisioner::ecs_task_sets::ECS_TASK_SET => {
            attributes.insert(
                crate::state::provisioner::ecs_task_sets::ID_ATTRIBUTE.to_string(),
                service_assigned_document(
                    name,
                    crate::state::provisioner::ecs_task_sets::ID_ATTRIBUTE,
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the ARN of the listener"; Fn::GetAtt: ListenerArn. A listener's ARN
        // nests inside the ARN of the load balancer its declaration names
        // (`listener/<type>/<lb>/<lb id>/<listener id>`), so it is resolved once that load
        // balancer's identity is known ([`resolve_dependent_identities`]); a declaration
        // naming none leaves the listener with CloudFormation's own generated identity
        // rather than an ARN built on a value that is not one.
        ELB_LISTENER => {
            let listener_arn = properties
                .get("LoadBalancerArn")
                .and_then(|value| text(value, scope, 0, budget))
                .and_then(|parent| {
                    nested_elb_arn(
                        &parent,
                        ELB_LOAD_BALANCER_SEGMENT,
                        ELB_LISTENER_SEGMENT,
                        &service_assigned_label(ELB_LISTENER_ID_SALT, name, ELB_ID_HEX_DIGITS),
                    )
                });
            match listener_arn {
                Some(arn) => {
                    attributes.insert("ListenerArn".to_string(), arn.clone());
                    (arn, attributes)
                }
                None => (name.to_string(), attributes),
            }
        }
        // "Ref returns the rule ARN"; Fn::GetAtt: IsDefault, RuleArn. A rule's ARN nests
        // inside its listener's the same way. `IsDefault` is false for a rule a template
        // declares: a listener's default rule is created WITH the listener (it is that
        // listener's `DefaultActions`), never as a resource of its own.
        ELB_LISTENER_RULE => {
            let rule_arn = properties
                .get("ListenerArn")
                .and_then(|value| text(value, scope, 0, budget))
                .and_then(|parent| {
                    nested_elb_arn(
                        &parent,
                        ELB_LISTENER_SEGMENT,
                        ELB_LISTENER_RULE_SEGMENT,
                        &service_assigned_label(
                            ELB_LISTENER_RULE_ID_SALT,
                            name,
                            ELB_ID_HEX_DIGITS,
                        ),
                    )
                });
            attributes.insert("IsDefault".to_string(), false.to_string());
            match rule_arn {
                Some(arn) => {
                    attributes.insert("RuleArn".to_string(), arn.clone());
                    (arn, attributes)
                }
                None => (name.to_string(), attributes),
            }
        }
        // The Amazon Bedrock AgentCore types. Source for each one's identifiers: the
        // AgentCore control-plane model, whose `arnReference` traits name these very
        // resource types — `MemoryArn` references `AWS::BedrockAgentCore::Memory`,
        // `GatewayArn` `AWS::BedrockAgentCore::Gateway`, `AgentRuntimeArn`
        // `AWS::BedrockAgentCore::Runtime`, `WorkloadIdentityArnType`
        // `AWS::BedrockAgentCore::WorkloadIdentity`, and `CodeInterpreterArn` /
        // `BrowserArn` the two sandbox types — and whose create responses publish
        // the members a CloudFormation resource of that type returns. `Ref` returns the
        // resource's own identifier (the type's primary identifier) and `Fn::GetAtt` reads
        // the identifier and the ARN, each in the form the model's pattern fixes; the
        // members those responses carry that describe a resource's RUNNING state (a
        // status, a timestamp, a failure reason, a gateway's URL) are not carried, because
        // nothing here provisioned a resource to be in a state.
        //
        // "Ref returns the memory id"; Fn::GetAtt: MemoryId, MemoryArn.
        "AWS::BedrockAgentCore::Memory" => {
            let id = generated_service_id(
                &agentcore_label(name, memory_label_char, false, 'm'),
                name,
                AGENTCORE_ID_SUFFIX_CHARS,
            );
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("memory/{id}"),
            );
            attributes.insert("MemoryId".to_string(), id.clone());
            attributes.insert("MemoryArn".to_string(), arn);
            (id, attributes)
        }
        // "Ref returns the agent runtime id"; Fn::GetAtt: AgentRuntimeId,
        // AgentRuntimeArn, AgentRuntimeVersion. A create answers version `1`
        // (`AgentRuntimeVersion` is `^([1-9][0-9]{0,4})$`, and the first version of a
        // runtime is its first), which is a value of the declared resource rather than of
        // its running state.
        "AWS::BedrockAgentCore::Runtime" => {
            let id = generated_service_id(
                &agentcore_label(name, runtime_label_char, false, 'r'),
                name,
                AGENTCORE_ID_SUFFIX_CHARS,
            );
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("runtime/{id}"),
            );
            attributes.insert("AgentRuntimeId".to_string(), id.clone());
            attributes.insert("AgentRuntimeArn".to_string(), arn);
            attributes.insert(
                "AgentRuntimeVersion".to_string(),
                FIRST_AGENTCORE_RUNTIME_VERSION.to_string(),
            );
            (id, attributes)
        }
        // The two SANDBOX types, the same shape one resource along. `CodeInterpreterId`
        // and `BrowserId` are `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$` — the agent
        // runtime's label alphabet exactly, hence `runtime_label_char` — and their ARN
        // patterns admit `code-interpreter(-custom)?` / `browser(-custom)?`: the bare
        // segment is the service's built-in `aws.codeinterpreter.v1` /
        // `aws.browser.v1` sandbox, so a sandbox a template declares is a
        // customer-created one and carries the `-custom` segment.
        //
        // "Ref returns the code interpreter id"; Fn::GetAtt: CodeInterpreterId,
        // CodeInterpreterArn.
        "AWS::BedrockAgentCore::CodeInterpreter" => {
            let id = generated_service_id(
                &agentcore_label(name, runtime_label_char, false, 'c'),
                name,
                AGENTCORE_ID_SUFFIX_CHARS,
            );
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("code-interpreter-custom/{id}"),
            );
            attributes.insert("CodeInterpreterId".to_string(), id.clone());
            attributes.insert("CodeInterpreterArn".to_string(), arn);
            (id, attributes)
        }
        // "Ref returns the browser id"; Fn::GetAtt: BrowserId, BrowserArn.
        "AWS::BedrockAgentCore::Browser" => {
            let id = generated_service_id(
                &agentcore_label(name, runtime_label_char, false, 'b'),
                name,
                AGENTCORE_ID_SUFFIX_CHARS,
            );
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("browser-custom/{id}"),
            );
            attributes.insert("BrowserId".to_string(), id.clone());
            attributes.insert("BrowserArn".to_string(), arn);
            (id, attributes)
        }
        // "Ref returns the gateway id"; Fn::GetAtt: GatewayId, GatewayArn. A gateway
        // identifier is lower case only (`GatewayId`: `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`).
        "AWS::BedrockAgentCore::Gateway" => {
            let id = generated_service_id(
                &agentcore_label(name, gateway_label_char, true, 'g'),
                name,
                AGENTCORE_ID_SUFFIX_CHARS,
            );
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("gateway/{id}"),
            );
            attributes.insert("GatewayId".to_string(), id.clone());
            attributes.insert("GatewayArn".to_string(), arn);
            (id, attributes)
        }
        // Amazon Bedrock's own control plane: the guardrail an `AWS::Bedrock::Guardrail`
        // declares. "Ref returns the guardrail ARN" (the type's primary identifier);
        // Fn::GetAtt: GuardrailId, GuardrailArn, Version. Amazon Bedrock MINTS the id —
        // `CreateGuardrail` answers a `guardrailId` of `^[a-z0-9]+$`, which is what every
        // guardrail API takes as its `guardrailIdentifier` and which the guardrail's NAME
        // (`^[0-9a-zA-Z-_]+$`) is not — so the values here are STAND-INS of the forms that
        // service's own model fixes, and `super::provisioning` re-points them at what the
        // create really answered ([`super::provisioner::Provisioner::BedrockGuardrail`]).
        // The create opens the guardrail's `DRAFT` revision (`version` is `^DRAFT$`), which
        // is the version a freshly created guardrail carries.
        BEDROCK_GUARDRAIL => {
            let id = service_assigned_label(
                BEDROCK_GUARDRAIL_ID_SALT,
                name,
                BEDROCK_GUARDRAIL_ID_DIGITS,
            );
            let arn = ::emulator_server::emulator_arn(
                BEDROCK_ARN_NAMESPACE,
                region,
                &format!("guardrail/{id}"),
            );
            attributes.insert("GuardrailId".to_string(), id);
            attributes.insert("GuardrailArn".to_string(), arn.clone());
            attributes.insert(
                "Version".to_string(),
                BEDROCK_GUARDRAIL_DRAFT_VERSION.to_string(),
            );
            (arn, attributes)
        }
        // Amazon Bedrock Agent's build-time control plane: the FLOW an `AWS::Bedrock::Flow`
        // declares. "Ref returns the flow ARN" (the type's primary identifier); `Fn::GetAtt`
        // reads `Arn`, `Id`, `Status`, `Version`, `CreatedAt` and `UpdatedAt`. That service
        // MINTS the id — `CreateFlow` answers an `id` of `^[0-9a-zA-Z]{10}$`, which no name
        // CloudFormation decides satisfies — and the ARN around it, so the values here are
        // STAND-INS of the forms that service's own model fixes and `super::provisioning`
        // re-points them at what the create really answered
        // ([`super::provisioner::Provisioner::BedrockFlow`]). The version a created flow
        // carries is its working copy (`version` is `^DRAFT$`); its status and the two
        // instants are that service's to report and reach the resource off its create's own
        // answer rather than being computed here.
        BEDROCK_FLOW => {
            let id = service_assigned_label(BEDROCK_FLOW_ID_SALT, name, BEDROCK_FLOW_ID_DIGITS);
            let arn = ::emulator_server::emulator_arn(
                BEDROCK_ARN_NAMESPACE,
                region,
                &format!("{BEDROCK_FLOW_PATH}/{id}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Id".to_string(), id);
            attributes.insert(
                "Version".to_string(),
                BEDROCK_FLOW_DRAFT_VERSION.to_string(),
            );
            (arn, attributes)
        }
        // Amazon Bedrock Agent's build-time control plane: the knowledge base an
        // `AWS::Bedrock::KnowledgeBase` declares. "Ref returns the knowledge base ID" (the
        // type's primary identifier); Fn::GetAtt reads `KnowledgeBaseId`,
        // `KnowledgeBaseArn`, `Status`, `CreatedAt` and `UpdatedAt`. That service MINTS the
        // id — `CreateKnowledgeBase` answers a `knowledgeBaseId` of `^[0-9a-zA-Z]{10}$`,
        // which is what every knowledge-base API takes and which the knowledge base's NAME
        // (`^([0-9a-zA-Z][_-]?){1,100}$`) is not — so the two values here are STAND-INS of
        // the forms that service's own model fixes, and `super::provisioning` re-points them
        // at what the create really answered
        // ([`super::provisioner::Provisioner::BedrockKnowledgeBase`]). The status and the two
        // instants are that service's to report and are not computed here: they reach the
        // resource off its create's own answer.
        BEDROCK_KNOWLEDGE_BASE => {
            let id = service_assigned_label(
                BEDROCK_KNOWLEDGE_BASE_ID_SALT,
                name,
                BEDROCK_KNOWLEDGE_BASE_ID_DIGITS,
            );
            attributes.insert(
                "KnowledgeBaseArn".to_string(),
                ::emulator_server::emulator_arn(
                    BEDROCK_ARN_NAMESPACE,
                    region,
                    &format!("knowledge-base/{id}"),
                ),
            );
            attributes.insert("KnowledgeBaseId".to_string(), id.clone());
            (id, attributes)
        }
        // Amazon Bedrock Agent's build-time control plane: the data source an
        // `AWS::Bedrock::DataSource` attaches to one of those knowledge bases. The type's
        // primary identifier is the PAIR `<KnowledgeBaseId>|<DataSourceId>`, which is what
        // `Ref` returns; `Fn::GetAtt` reads `DataSourceId` beside the data source's status and
        // the two instants. That service MINTS the id — `CreateDataSource` answers a
        // `dataSourceId` of `^[0-9a-zA-Z]{10}$`, which is what `GetDataSource`,
        // `UpdateDataSource`, `DeleteDataSource` and `StartIngestionJob` each take and which
        // the data source's NAME (`^([0-9a-zA-Z][_-]?){1,100}$`) is not — so the id here is a
        // STAND-IN of the form that service's own model fixes, and `super::provisioning`
        // re-points it and the identity built around it at what the create really answered
        // ([`super::provisioner::Provisioner::BedrockDataSource`]). The knowledge base half is
        // the declaration's own, resolved through the template's intrinsics, so a `Ref` to a
        // knowledge base of the same stack carries the id that service minted for it by the
        // time the create is made. The status and the two instants are that service's to
        // report and are not computed here: they reach the resource off its create's own
        // answer, exactly as a knowledge base's do.
        BEDROCK_DATA_SOURCE => {
            let data_source_id = service_assigned_label(
                BEDROCK_DATA_SOURCE_ID_SALT,
                name,
                BEDROCK_DATA_SOURCE_ID_DIGITS,
            );
            let knowledge_base_id = properties
                .get(BEDROCK_DATA_SOURCE_KNOWLEDGE_BASE_ID_PROPERTY)
                .and_then(|value| text(value, scope, 0, budget))
                .unwrap_or_default();
            attributes.insert(
                BEDROCK_DATA_SOURCE_ID_ATTRIBUTE.to_string(),
                data_source_id.clone(),
            );
            (
                bedrock_data_source_identity(&knowledge_base_id, &data_source_id),
                attributes,
            )
        }
        // Amazon Bedrock Agent's RUNTIME plane: the session an `AWS::Bedrock::Session`
        // declares. `Ref` returns the session ARN (the type's primary identifier); `Fn::GetAtt`
        // reads that ARN and the `SessionId` that service minted. `CreateSession` takes no
        // identifier at all — the declaration states the encryption key, the metadata and the
        // tags — and answers the `sessionId` it drew (a UUID) with the `session/<uuid>` ARN
        // built around it, so the two values here are STAND-INS of the forms that service's own
        // model fixes and `super::provisioning` re-points them at what the create really
        // answered ([`super::provisioner::Provisioner::BedrockSession`]). The session's status
        // and the two instants it keeps are that service's to report and reach the resource off
        // its create's own answer rather than being computed here.
        BEDROCK_SESSION => {
            let session_id = generated_uuid(resource_type, name);
            let arn = ::emulator_server::emulator_arn(
                BEDROCK_ARN_NAMESPACE,
                region,
                &format!("{BEDROCK_SESSION_PATH}/{session_id}"),
            );
            attributes.insert(
                BEDROCK_SESSION_ARN_ATTRIBUTE.to_string(),
                arn.clone(),
            );
            attributes.insert(BEDROCK_SESSION_ID_ATTRIBUTE.to_string(), session_id);
            (arn, attributes)
        }
        // The Billing and Cost Management Dashboards control plane: the dashboard an
        // `AWS::BCM::Dashboard` declares and the scheduled report an
        // `AWS::BCM::ScheduledReport` declares. "Ref returns the Arn" for either, and
        // `Fn::GetAtt` reads the instants that service recorded beside it (and, for a
        // dashboard, the kind it records it as). That service MINTS the ARN — neither create
        // takes an identifier, and the resource segment carries a 32-hexadecimal-character
        // id of its own, with no Region segment at all — so the value here is a STAND-IN of
        // that form, which `super::provisioning` re-points at the ARN the create really
        // answered ([`super::provisioner::Provisioner::BcmDashboard`]). The instants and the
        // kind are that service's own, so each stands in until it has reported them.
        BCM_DASHBOARD | BCM_SCHEDULED_REPORT => {
            let (path, published) = if resource_type == BCM_DASHBOARD {
                (BCM_DASHBOARD_PATH, BCM_DASHBOARD_ATTRIBUTES)
            } else {
                (BCM_SCHEDULED_REPORT_PATH, BCM_SCHEDULED_REPORT_ATTRIBUTES)
            };
            let arn = ::emulator_server::emulator_arn(
                BCM_DASHBOARDS_ARN_NAMESPACE,
                "",
                &format!(
                    "{path}/{}",
                    crate::state::stable_identifier_digits(
                        &format!("{resource_type}/{name}"),
                        BCM_RESOURCE_ID_DIGITS
                    )
                ),
            );
            attributes.insert(BCM_ARN_ATTRIBUTE.to_string(), arn.clone());
            for attribute in published
                .iter()
                .filter(|attribute| **attribute != BCM_ARN_ATTRIBUTE)
            {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (arn, attributes)
        }
        // Amazon Braket's own control plane: the spending limit an
        // `AWS::Braket::SpendingLimit` declares. "Ref returns the spending limit ARN" (the
        // type's primary identifier); Fn::GetAtt reads `SpendingLimitArn` and `CreatedAt`.
        // Amazon Braket MINTS that ARN — `CreateSpendingLimit` takes no identifier for the
        // limit and answers the `spendingLimitArn` it assigned, whose resource path carries a
        // uuid of that service's own (`SpendingLimitArn`:
        // `^arn:aws[a-z\-]*:braket:[a-z0-9\-]+:[0-9]{12}:spending-limit/.*$`) — so the value
        // here is a STAND-IN of that form, which `super::provisioning` re-points at the ARN
        // the create really answered
        // ([`super::provisioner::Provisioner::BraketSpendingLimit`]). `CreatedAt` is that
        // service's clock and is not computed here: it reaches the resource off that
        // service's own report of the limit it created.
        BRAKET_SPENDING_LIMIT => {
            let arn = ::emulator_server::emulator_arn(
                BRAKET_ARN_NAMESPACE,
                region,
                &format!(
                    "{BRAKET_SPENDING_LIMIT_PATH}/{}",
                    generated_uuid(resource_type, name)
                ),
            );
            attributes.insert("SpendingLimitArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // CloudWatch Application Signals' own control plane: the service level objective an
        // `AWS::ApplicationSignals::ServiceLevelObjective` declares. "Ref returns the ARN",
        // which that service builds from its own namespace, the Region and the objective's
        // NAME — the same ARN this computes, since both are built from the one account id this
        // emulator mints (`slo_arn` in that service's own state module) — so the identity is
        // computable here and `identity_is_service_assigned` does not name the type. The four
        // read-only values beside it are Application Signals' own (the two instants of its
        // clock, which of the two indicators the objective was created with, and the metric
        // source that indicator names), so each stands in until `super::provisioning` has
        // reported what that service really answered
        // ([`super::provisioner::Provisioner::ApplicationSignalsServiceLevelObjective`]).
        APPLICATION_SIGNALS_SERVICE_LEVEL_OBJECTIVE => {
            let arn = ::emulator_server::emulator_arn(
                APPLICATION_SIGNALS_ARN_NAMESPACE,
                region,
                &format!("{APPLICATION_SIGNALS_SLO_PATH}/{name}"),
            );
            attributes.insert(
                APPLICATION_SIGNALS_SLO_ARN_ATTRIBUTE.to_string(),
                arn.clone(),
            );
            for attribute in APPLICATION_SIGNALS_SLO_ATTRIBUTES
                .iter()
                .filter(|attribute| **attribute != APPLICATION_SIGNALS_SLO_ARN_ATTRIBUTE)
            {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (arn, attributes)
        }
        // The grouping configuration an `AWS::ApplicationSignals::GroupingConfiguration`
        // declares. `PutGroupingConfiguration` keeps ONE configuration per account and Region
        // ("Creates or updates the grouping configuration for this account") and takes no
        // identifier for it, and the read that reports one narrows by `AwsAccountId` — so the
        // resource's identity is the account whose configuration it is, not a name of its own,
        // and a second declaration of the type in the same account names the same resource.
        // Its `UpdatedAt` is that service's clock and stands in until `super::provisioning`
        // has reported what the put answered
        // ([`super::provisioner::Provisioner::ApplicationSignalsGroupingConfiguration`]).
        APPLICATION_SIGNALS_GROUPING_CONFIGURATION => {
            let account_id = ::emulator_server::emulator_account_id().to_string();
            attributes.insert(
                APPLICATION_SIGNALS_ACCOUNT_ID_ATTRIBUTE.to_string(),
                account_id.clone(),
            );
            for attribute in APPLICATION_SIGNALS_GROUPING_CONFIGURATION_ATTRIBUTES
                .iter()
                .filter(|attribute| **attribute != APPLICATION_SIGNALS_ACCOUNT_ID_ATTRIBUTE)
            {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (account_id, attributes)
        }
        // The instrumentation point an `AWS::ApplicationSignals::InstrumentationConfig`
        // declares. `CreateInstrumentationConfiguration` takes no identifier for it and
        // answers the ARN it minted, which carries the service, the environment, the signal
        // type and the `LocationHash` that service computed from the LOCATION — a hash no
        // naming rule computes, which is what [`identity_is_service_assigned`] names this type
        // for. The value here is therefore a STAND-IN of that form: the three parts the
        // declaration states, and a stable digest of the declared location at the width that
        // member carries, re-pointed at the ARN the create really answered
        // ([`super::provisioner::Provisioner::ApplicationSignalsInstrumentationConfig`]). The
        // instant and the hash itself are that service's too, so both stand in beside it.
        APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG => {
            let mut declared_part = |property: &str| {
                properties
                    .get(property)
                    .and_then(|value| text(value, scope, 0, budget))
                    .unwrap_or_default()
            };
            let service = declared_part(APPLICATION_SIGNALS_SERVICE_PROPERTY);
            let environment = declared_part(APPLICATION_SIGNALS_ENVIRONMENT_PROPERTY);
            let signal_type = declared_part(APPLICATION_SIGNALS_SIGNAL_TYPE_PROPERTY);
            // The location is a STRUCTURE rather than a scalar, so what the stand-in hashes is
            // the declaration's own spelling of it, under the NAME CloudFormation computed for
            // this resource: stable for one declaration, and different for two — so two
            // declarations that state the same location, or that state none at all, never stand
            // in under one identity while the create that settles it is still to come.
            let location = properties
                .get(APPLICATION_SIGNALS_LOCATION_PROPERTY)
                .map(ToString::to_string)
                .unwrap_or_default();
            let arn = ::emulator_server::emulator_arn(
                APPLICATION_SIGNALS_ARN_NAMESPACE,
                region,
                &format!(
                    "{APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG_PATH}/{service}/{environment}/\
                     {signal_type}/{}",
                    crate::state::stable_identifier_digits(
                        &format!("{name}/{location}"),
                        APPLICATION_SIGNALS_LOCATION_HASH_CHARS
                    )
                ),
            );
            attributes.insert(
                APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG_ARN_ATTRIBUTE.to_string(),
                arn.clone(),
            );
            for attribute in APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG_ATTRIBUTES
                .iter()
                .filter(|attribute| {
                    **attribute != APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG_ARN_ATTRIBUTE
                })
            {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (arn, attributes)
        }
        // Clean Rooms ML's own control plane: the training dataset an
        // `AWS::CleanRoomsML::TrainingDataset` declares. "Ref returns the training dataset ARN"
        // (the type's primary identifier); Fn::GetAtt reads `TrainingDatasetArn`. That service
        // MINTS the ARN — `CreateTrainingDataset` takes no identifier for the dataset and
        // answers the `trainingDatasetArn` it assigned, whose resource path carries an
        // identifier of its own (`TrainingDatasetArn`:
        // `^arn:aws[-a-z]*:cleanrooms-ml:[-a-z0-9]+:[0-9]{12}:training-dataset/[-a-zA-Z0-9_/.]+$`)
        // — so the value here is a STAND-IN of that form, which `super::provisioning` re-points
        // at the ARN the create really answered
        // ([`super::provisioner::Provisioner::CleanRoomsMlTrainingDataset`]).
        CLEANROOMSML_TRAINING_DATASET => {
            let arn = ::emulator_server::emulator_arn(
                CLEANROOMSML_ARN_NAMESPACE,
                region,
                &format!(
                    "{CLEANROOMSML_TRAINING_DATASET_PATH}/{}",
                    generated_uuid(resource_type, name)
                ),
            );
            attributes.insert(
                CLEANROOMSML_TRAINING_DATASET_ARN_ATTRIBUTE.to_string(),
                arn.clone(),
            );
            (arn, attributes)
        }
        // Backup Gateway's own control plane: the hypervisor an
        // `AWS::BackupGateway::Hypervisor` declares. "Ref returns the hypervisor ARN" (the
        // type's primary identifier); Fn::GetAtt reads `HypervisorArn`. That service MINTS
        // that ARN — `ImportHypervisorConfiguration` takes no identifier for the hypervisor
        // and answers the `HypervisorArn` it assigned, whose resource path carries an id of
        // its own form (`ServerArn`:
        // `^arn:(aws|aws-cn|aws-us-gov):backup-gateway(:[a-zA-Z-0-9]+){3}\/[a-zA-Z-0-9]+$`) —
        // so the value here is a STAND-IN of that form, which `super::provisioning` re-points
        // at the ARN the import really answered
        // ([`super::provisioner::Provisioner::BackupGatewayHypervisor`]).
        BACKUP_GATEWAY_HYPERVISOR => {
            let arn = ::emulator_server::emulator_arn(
                BACKUP_GATEWAY_ARN_NAMESPACE,
                region,
                &format!(
                    "{BACKUP_GATEWAY_HYPERVISOR_PATH}/{BACKUP_GATEWAY_HYPERVISOR_ID_PREFIX}{}",
                    service_assigned_label(
                        BACKUP_GATEWAY_HYPERVISOR_ID_SALT,
                        name,
                        BACKUP_GATEWAY_HYPERVISOR_ID_DIGITS,
                    )
                ),
            );
            attributes.insert("HypervisorArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // AWS Network Security Manager's own control plane: the rule, the template, the
        // policy and the scope an `AWS::NetworkSecurityManager::*` declares. "Ref returns the
        // ARN" of the entity — each type's primary identifier, and the `…Identifier` every
        // call of that service on one takes (its ARN member carries the model's own
        // `resourceIdentifier`) — and `Fn::GetAtt` reads that ARN and the id inside it
        // ([`NETWORK_SECURITY_MANAGER_RULE_ATTRIBUTES`] and its siblings). That service MINTS
        // the id: each create takes no identifier for the entity and answers the `…Id` it
        // assigned (`^[a-z0-9]{1,64}$`) with the ARN built around it, so the two values here
        // are STAND-INS of the forms its own model fixes, which `super::provisioning`
        // re-points at what the create really answered
        // ([`super::provisioner::Provisioner::NetworkSecurityManagerEntity`]).
        NETWORK_SECURITY_MANAGER_RULE
        | NETWORK_SECURITY_MANAGER_TEMPLATE
        | NETWORK_SECURITY_MANAGER_POLICY
        | NETWORK_SECURITY_MANAGER_SCOPE => {
            let (path, arn_attribute, id_attribute) = match resource_type {
                NETWORK_SECURITY_MANAGER_TEMPLATE => (
                    NETWORK_SECURITY_MANAGER_TEMPLATE_PATH,
                    "TemplateArn",
                    "TemplateId",
                ),
                NETWORK_SECURITY_MANAGER_POLICY => (
                    NETWORK_SECURITY_MANAGER_POLICY_PATH,
                    "PolicyArn",
                    "PolicyId",
                ),
                NETWORK_SECURITY_MANAGER_SCOPE => {
                    (NETWORK_SECURITY_MANAGER_SCOPE_PATH, "ScopeArn", "ScopeId")
                }
                // The RULE, which is the fourth of the four types this arm serves.
                _ => (NETWORK_SECURITY_MANAGER_RULE_PATH, "RuleArn", "RuleId"),
            };
            let id = service_assigned_label(
                resource_type,
                name,
                NETWORK_SECURITY_MANAGER_ID_DIGITS,
            );
            let arn = ::emulator_server::emulator_arn(
                NETWORK_SECURITY_MANAGER_ARN_NAMESPACE,
                region,
                &format!("{path}/{id}"),
            );
            attributes.insert(arn_attribute.to_string(), arn.clone());
            attributes.insert(id_attribute.to_string(), id);
            (arn, attributes)
        }
        // Recycle Bin's own control plane: the retention rule an `AWS::Rbin::Rule` declares.
        // "Ref returns the rule ARN" (the type's primary identifier); Fn::GetAtt reads `Arn`,
        // `Identifier` and `LockState`. That service MINTS the identifier —
        // `CreateRule` takes none and answers the `Identifier` it assigned
        // (`^[0-9a-zA-Z]{11}$`) with the `RuleArn` built around it — so the two values here
        // are STAND-INS of the forms its own model fixes, which `super::provisioning`
        // re-points at what the create really answered
        // ([`super::provisioner::Provisioner::RbinRule`]). The lock state follows from the
        // declaration: a rule declared with a lock configuration is created locked, and one
        // declared without it has never been locked, which this type publishes as the
        // `unlocked` state that denotes.
        RBIN_RULE => {
            let identifier =
                service_assigned_label(RBIN_RULE_ID_SALT, name, RBIN_RULE_ID_DIGITS);
            let arn = ::emulator_server::emulator_arn(
                RBIN_ARN_NAMESPACE,
                region,
                &format!("{RBIN_RULE_PATH}/{identifier}"),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert("Identifier".to_string(), identifier);
            attributes.insert(
                "LockState".to_string(),
                crate::state::provisioner::rbin_rules::declared_lock_state(properties)
                    .to_string(),
            );
            (arn, attributes)
        }
        // Neptune Analytics' own control plane: the graph an `AWS::NeptuneGraph::Graph`
        // declares and the snapshot an `AWS::NeptuneGraph::GraphSnapshot` declares. "Ref
        // returns the graph id" / the snapshot id — each type's primary identifier — and a
        // graph's `Fn::GetAtt` reads [`NEPTUNE_GRAPH_GRAPH_ATTRIBUTES`].
        //
        // That service MINTS both ids — `CreateGraph` and `CreateGraphSnapshot` each take the
        // resource's NAME and answer the id they assigned — so the ids here are STAND-INS of
        // the forms its own model fixes, which `super::provisioning` re-points at what the
        // create really answered ([`super::provisioner::Provisioner::NeptuneGraphGraph`] and
        // its snapshot sibling). A graph's ARN is built around that id the way its own service
        // builds it, from the one account id this emulator mints; its ENDPOINT is a value only
        // that service produces, so it stands in ([`service_assigned_document`]) until the
        // graph reports it.
        NEPTUNE_GRAPH_GRAPH => {
            let id = generated_service_id(
                NEPTUNE_GRAPH_ID_PREFIX,
                name,
                NEPTUNE_GRAPH_ID_DIGITS,
            );
            attributes.insert(
                "GraphArn".to_string(),
                ::emulator_server::emulator_arn(
                    NEPTUNE_GRAPH_ARN_NAMESPACE,
                    region,
                    &format!("{NEPTUNE_GRAPH_GRAPH_PATH}/{id}"),
                ),
            );
            attributes.insert(
                "Endpoint".to_string(),
                service_assigned_document(name, "Endpoint"),
            );
            attributes.insert("GraphId".to_string(), id.clone());
            (id, attributes)
        }
        NEPTUNE_GRAPH_GRAPH_SNAPSHOT => {
            let id = generated_service_id(
                NEPTUNE_SNAPSHOT_ID_PREFIX,
                name,
                NEPTUNE_GRAPH_ID_DIGITS,
            );
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    NEPTUNE_GRAPH_ARN_NAMESPACE,
                    region,
                    &format!("{NEPTUNE_GRAPH_SNAPSHOT_PATH}/{id}"),
                ),
            );
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // AWS Amplify's own control plane: the app an `AWS::Amplify::App` declares. "Ref
        // returns the ARN of the app"; `Fn::GetAtt` reads [`AMPLIFY_APP_ATTRIBUTES`]. Every
        // one of those is a value only that service can produce — the `appId` it mints, the
        // ARN built around it, the default domain it serves the app's branches under, and the
        // name as it records it — so each is a STAND-IN here, which `super::provisioning`
        // re-points at what the create really answered
        // ([`super::provisioner::Provisioner::AmplifyApp`]).
        AMPLIFY_APP => {
            for attribute in AMPLIFY_APP_ATTRIBUTES {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (
                service_assigned_document(name, APP_ARN_STAND_IN_ATTRIBUTE),
                attributes,
            )
        }
        // AWS Amplify's own control plane: the BRANCH an `AWS::Amplify::Branch` declares. "Ref
        // returns the ARN of the branch"; `Fn::GetAtt` reads [`AMPLIFY_BRANCH_ATTRIBUTES`]. That
        // ARN is built around the `appId` AWS Amplify minted for the app the branch belongs to,
        // so it is a STAND-IN here, which `super::provisioning` re-points at what the create
        // really answered ([`super::provisioner::Provisioner::AmplifyBranch`]). The branch's
        // NAME is the declaration's own, which is this resource's physical name.
        AMPLIFY_BRANCH => {
            let arn = service_assigned_document(name, BRANCH_ARN_STAND_IN_ATTRIBUTE);
            attributes.insert(BRANCH_ARN_STAND_IN_ATTRIBUTE.to_string(), arn.clone());
            attributes.insert(BRANCH_NAME_STAND_IN_ATTRIBUTE.to_string(), name.to_string());
            (arn, attributes)
        }
        // Amplify UI Builder's own control plane: the component, the form or the theme an
        // `AWS::AmplifyUIBuilder::{Component,Form,Theme}` declares. The type's primary
        // identifier is the TRIPLE its own resource shape is identified by — the app, the
        // environment and the id — so "Ref returns" that triple joined with
        // [`COMPOSITE_IDENTITY_SEPARATOR`] ([`amplifyuibuilder_identity`]), which is also the
        // resource's physical id; `Fn::GetAtt` reads the three parts
        // ([`AMPLIFYUIBUILDER_ATTRIBUTES`]) and, for a component and a theme, the two instants
        // their structures carry read-only.
        //
        // That service MINTS the id — each create takes the app and the environment as URI
        // labels and the entity's name in its payload, takes no identifier, and answers the
        // `Uuid` it assigned, which is what every call on the entity is addressed by — so the
        // id here, and the identity built around it, are STAND-INS, which
        // `super::provisioning` re-points at what the create really answered
        // ([`super::provisioner::Provisioner::AmplifyUiBuilderComponent`] and its two
        // siblings). The app and the environment are the declaration's own values, reported as
        // the entity itself reports them.
        AMPLIFYUIBUILDER_COMPONENT | AMPLIFYUIBUILDER_FORM | AMPLIFYUIBUILDER_THEME => {
            let id = generated_uuid(resource_type, name);
            for property in [AMPLIFYUIBUILDER_APP_ID, AMPLIFYUIBUILDER_ENVIRONMENT_NAME] {
                if let Some(value) = properties
                    .get(property)
                    .and_then(|value| text(value, scope, 0, budget))
                {
                    attributes.insert(property.to_string(), value);
                }
            }
            attributes.insert(AMPLIFYUIBUILDER_ID_ATTRIBUTE.to_string(), id.clone());
            if resource_type != AMPLIFYUIBUILDER_FORM {
                for attribute in AMPLIFYUIBUILDER_INSTANT_ATTRIBUTES {
                    attributes.insert(
                        (*attribute).to_string(),
                        service_assigned_document(name, attribute),
                    );
                }
            }
            let identity = amplifyuibuilder_identity(
                attributes
                    .get(AMPLIFYUIBUILDER_APP_ID)
                    .map(String::as_str)
                    .unwrap_or_default(),
                attributes
                    .get(AMPLIFYUIBUILDER_ENVIRONMENT_NAME)
                    .map(String::as_str)
                    .unwrap_or_default(),
                &id,
            );
            (identity, attributes)
        }
        // AWS AppConfig's rollout: the configuration content an
        // `AWS::AppConfig::HostedConfigurationVersion` hosts for a profile, and the
        // `AWS::AppConfig::Deployment` that rolls one version of it into an environment. Each
        // type's `Ref` returns the triple its own API addresses one resource by
        // ([`appconfig_rollout_identity`]), whose last part is the number AppConfig MINTS —
        // `CreateHostedConfigurationVersion` answers the `VersionNumber` it assigned and
        // `StartDeployment` the `DeploymentNumber` it allocated — so the identity and the
        // attributes read off it are STAND-INS here, which `super::provisioning` re-points at
        // what each create really answered
        // ([`crate::state::provisioning::appconfig_rollout`], and
        // [`identity_is_service_assigned`] names both types). `Fn::GetAtt` reads that number,
        // and for a deployment the `State` AppConfig reports the rollout in, which only that
        // service produces.
        APPCONFIG_HOSTED_CONFIGURATION_VERSION => {
            attributes.insert(
                APPCONFIG_VERSION_NUMBER_ATTRIBUTE.to_string(),
                service_assigned_document(name, APPCONFIG_VERSION_NUMBER_ATTRIBUTE),
            );
            (name.to_string(), attributes)
        }
        APPCONFIG_DEPLOYMENT => {
            for attribute in APPCONFIG_DEPLOYMENT_ATTRIBUTES {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (name.to_string(), attributes)
        }
        // Amazon Managed Workflows for Apache Airflow's own control plane: the environment an
        // `AWS::MWAA::Environment` declares. "Ref returns the name of the Amazon MWAA
        // environment", which is the handle every Amazon MWAA operation on one takes, so the
        // physical id is that name; Fn::GetAtt reads the ten values
        // [`MWAA_ENVIRONMENT_ATTRIBUTES`] names. The ARN is the one of them these rules can
        // compute — both this and that service build it from the one account id this emulator
        // mints and the environment's name (`environment_arn` in its own state module) — and
        // every other is a value only Amazon MWAA produces, so each stands in until
        // `super::provisioning` has reported what that service really answered
        // ([`super::provisioner::Provisioner::MwaaEnvironment`]).
        MWAA_ENVIRONMENT => {
            attributes.insert(
                MWAA_ENVIRONMENT_ARN_ATTRIBUTE.to_string(),
                ::emulator_server::emulator_arn(
                    MWAA_ARN_NAMESPACE,
                    region,
                    &format!("{MWAA_ENVIRONMENT_PATH}/{name}"),
                ),
            );
            for attribute in MWAA_ENVIRONMENT_ATTRIBUTES
                .iter()
                .filter(|attribute| **attribute != MWAA_ENVIRONMENT_ARN_ATTRIBUTE)
            {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (name.to_string(), attributes)
        }
        // Amazon Connect Customer Profiles' own control plane: the domain an
        // `AWS::CustomerProfiles::Domain` declares. "Ref returns the DomainName", which is the
        // handle every operation on a domain takes, so the physical id is that name; Fn::GetAtt
        // reads the seven values [`CUSTOMER_PROFILES_DOMAIN_ATTRIBUTES`] names, every one of
        // which is a read-only value only that service produces — the instants it recorded, the
        // status its rule-based matching configuration is in and the counts it keeps over the
        // domain's data — so each stands in until `super::provisioning` has reported what that
        // service really answered
        // ([`super::provisioner::Provisioner::CustomerProfilesDomain`]).
        CUSTOMER_PROFILES_DOMAIN => {
            for attribute in CUSTOMER_PROFILES_DOMAIN_ATTRIBUTES {
                attributes.insert(
                    (*attribute).to_string(),
                    service_assigned_document(name, attribute),
                );
            }
            (name.to_string(), attributes)
        }
        // "Ref returns the name"; Fn::GetAtt: Name, WorkloadIdentityArn. A workload
        // identity is named, not id'd: `CreateWorkloadIdentity` takes a `name`
        // (`^[A-Za-z0-9_.-]+$`) and answers that name plus the ARN, whose shape the model
        // leaves unpatterned, so it is built in this service's own ARN form.
        "AWS::BedrockAgentCore::WorkloadIdentity" => {
            let arn = ::emulator_server::emulator_arn(
                AGENTCORE_ARN_SERVICE,
                region,
                &format!("workload-identity-directory/default/workload-identity/{name}"),
            );
            attributes.insert("Name".to_string(), name.to_string());
            attributes.insert("WorkloadIdentityArn".to_string(), arn);
            (name.to_string(), attributes)
        }
        // "Ref returns the app monitor name"; Fn::GetAtt: Id ("The unique ID of the new
        // app monitor"). CloudWatch RUM ASSIGNS that id — the template cannot name it —
        // and its form is fixed by RUM's own Smithy model: `AppMonitorId` is
        // `^[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}$`,
        // `length 36..36`, so the resource reports an identifier of THAT form rather than
        // CloudFormation's `<stack>-<logical id>-<unique id>` name (the same case
        // AWS::Transfer::Server is in above).
        "AWS::RUM::AppMonitor" => {
            attributes.insert("Id".to_string(), generated_uuid("rum/appmonitor", name));
            (name.to_string(), attributes)
        }
        // "Ref returns the association ID"; Fn::GetAtt: AssociationId. Systems Manager ASSIGNS
        // that id — `CreateAssociation` takes no identifier for the association, only the
        // document it applies and the targets it applies it to — so a declaration reports an
        // identifier of that service's own shape (the 8-4-4-4-12 hexadecimal its
        // `AssociationId` fixes) rather than the document `Name` or CloudFormation's
        // `<stack>-<logical id>-<unique id>`, neither of which any Systems Manager call on an
        // association would accept. The stand-in is a function of the STACK and the LOGICAL
        // ID rather than of the resource's name, because the name a declaration of this type
        // carries is the document's and two associations of one stack may apply the same
        // document; `super::provisioning` re-points it at the id the create really answered.
        SSM_ASSOCIATION => {
            let id = generated_uuid(
                resource_type,
                &format!("{}/{}", scope.stack.stack_id, declared.logical_id),
            );
            attributes.insert(SSM_ASSOCIATION_ID_ATTRIBUTE.to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the patch baseline ID"; Fn::GetAtt: Id. Systems Manager ASSIGNS that id
        // too — `CreatePatchBaseline` takes the name the baseline is CALLED by and no
        // identifier — so a declaration reports an identifier of that service's own shape
        // (`pb-` and 17 hexadecimal characters, which its `BaselineId` pattern admits) rather
        // than the `Name` the declaration states, which no Systems Manager call on a baseline
        // would accept. The stand-in is a function of the STACK and the LOGICAL ID for the
        // same reason the association's is — a declaration need state no name at all, and two
        // baselines of one stack may be called the same — and `super::provisioning` re-points
        // it at the id the create really answered.
        crate::state::provisioning::ssm_patch_baseline::SSM_PATCH_BASELINE => {
            let id = generated_service_id(
                SSM_PATCH_BASELINE_ID_PREFIX,
                &format!("{}/{}", scope.stack.stack_id, declared.logical_id),
                SSM_PATCH_BASELINE_ID_DIGITS,
            );
            attributes.insert(SSM_PATCH_BASELINE_ID_ATTRIBUTE.to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the alarm name"; Fn::GetAtt: Arn. A CloudWatch alarm's ARN spells
        // its resource with a COLON
        // (`arn:aws:cloudwatch:<region>:<account>:alarm:<alarm name>`), which is why the
        // type is catalogued rather than left to the uniform `<type>/<name>` convention.
        // A COMPOSITE alarm publishes the same two values, and its ARN spells its resource
        // the same way: "The ARN format of an alarm is
        // arn:aws:cloudwatch:Region:account-id:alarm:alarm-name", whichever kind of alarm it
        // is, so both types are catalogued in the one arm.
        "AWS::CloudWatch::Alarm"
        | crate::state::provisioner::cloudwatch_composite_alarms::CLOUDWATCH_COMPOSITE_ALARM => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn("cloudwatch", region, &format!("alarm:{name}")),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the name of the metric stream"; Fn::GetAtt: Arn, CreationDate,
        // LastUpdateDate, State. Only the ARN is a function of the resource's identity (a
        // metric stream's ARN spells its resource `metric-stream/<name>`, which is what
        // CloudWatch's own `PutMetricStream` answers); the other three are values the
        // OWNING service decides when it creates the stream — two instants of its clock and
        // the state its lifecycle is in — so they are reported only where that service
        // reports them (`super::provisioner` records what the create answered) rather than
        // minted here.
        CLOUDWATCH_METRIC_STREAM => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    "cloudwatch",
                    region,
                    &format!("metric-stream/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the experiment template ID"; Fn::GetAtt: Id. FIS ASSIGNS that id —
        // the type has no name property for a template to set — so a declaration reports an
        // identifier of FIS's own shape (its `EXT` prefix and its 12-character suffix, each
        // character one FIS's own minting admits) rather than CloudFormation's
        // `<stack>-<logical id>-<unique id>` name (the case AWS::Transfer::Server and
        // AWS::RUM::AppMonitor are in). The characters here are uppercase hexadecimal, a
        // subset of the base-36 alphabet FIS mints from: a stand-in, since this is also the
        // type `super::provisioning` creates in FIS itself, which reports the id IT minted
        // back over the `Id` attribute — and THAT id, not this one, is what the stack
        // reports for a resource FIS created.
        FIS_EXPERIMENT_TEMPLATE => {
            let id = format!(
                "{FIS_TEMPLATE_ID_PREFIX}{}",
                derived_digits(
                    FIS_TEMPLATE_ID_PREFIX,
                    name,
                    FIS_TEMPLATE_ID_DIGITS,
                    Alphabet::Upper,
                )
            );
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the detector's id"; Fn::GetAtt: Id. GuardDuty ASSIGNS that id —
        // `CreateDetector` takes no identifier of any kind — so a declaration reports an
        // identifier of GuardDuty's own shape (32 lowercase hexadecimal characters) rather
        // than CloudFormation's `<stack>-<logical id>-<unique id>` name. This is also the
        // type `super::provisioning` creates in GuardDuty itself, which reports the id IT
        // minted back over the `Id` attribute — and THAT id, not this one, is what the stack
        // reports for a detector GuardDuty created.
        GUARDDUTY_DETECTOR => {
            let id = derived_digits(
                DETECTOR_ID_SALT,
                name,
                DETECTOR_ID_HEX_DIGITS,
                Alphabet::Lower,
            );
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the IPSet's id"; Fn::GetAtt: Id. GuardDuty ASSIGNS that id —
        // `CreateIPSet` takes the list's name, format and location and answers with the
        // `ipSetId` it minted — so a declaration reports an identifier of GuardDuty's own
        // shape (32 lowercase hexadecimal characters, as a detector id is) rather than
        // CloudFormation's `<stack>-<logical id>-<unique id>` name, which no GuardDuty call
        // would accept as an `IpSetId`. This is also the type `super::provisioning` creates
        // in GuardDuty itself, which reports the id IT minted back over the `Id` attribute
        // — and THAT id, not this one, is what the stack reports for a list GuardDuty
        // created.
        GUARDDUTY_IP_SET => {
            let id = derived_digits(IP_SET_ID_SALT, name, DETECTOR_ID_HEX_DIGITS, Alphabet::Lower);
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the hosted zone ID"; Fn::GetAtt: Id, NameServers. Route 53 ASSIGNS
        // that id — `CreateHostedZone` takes no identifier, and the zone's `Name` is only
        // the DNS name it answers for — so a declaration reports an identifier of Route 53's
        // own shape rather than the name, which no operation of that service would accept as
        // a zone id. `NameServers` is a LIST, so no attribute carries it. This is also the
        // type `super::provisioning` creates in Route 53 itself, which reports the id IT
        // minted back over the `Id` attribute — and THAT id, not this one, is what the stack
        // reports for a zone Route 53 created.
        ROUTE53_HOSTED_ZONE => {
            let id = format!(
                "{HOSTED_ZONE_ID_PREFIX}{}",
                derived_digits(
                    HOSTED_ZONE_ID_SALT,
                    name,
                    HOSTED_ZONE_ID_DIGITS,
                    Alphabet::Upper,
                )
            );
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the health check ID"; Fn::GetAtt: HealthCheckId. Route 53 ASSIGNS
        // that id — `CreateHealthCheck` takes no identifier, and the type declares no name
        // at all — so a declaration reports an identifier of Route 53's own shape (the
        // 8-4-4-4-12 lowercase hex a health check id takes) rather than CloudFormation's
        // `<stack>-<logical id>-<unique id>` name, which no operation of that service would
        // accept as a health check id. This is also the type `super::provisioning` creates
        // in Route 53 itself, which reports the id IT minted back over the `HealthCheckId`
        // attribute — and THAT id, not this one, is what the stack reports for a health
        // check Route 53 created.
        ROUTE53_HEALTH_CHECK => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("HealthCheckId".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the collection ID"; Fn::GetAtt: Arn, Id. Route 53 ASSIGNS that id —
        // `CreateCidrCollection` takes the collection's `Name` and answers the UUID it
        // minted, which is what every call on the collection takes — so a declaration
        // reports an identifier of that service's own shape (the 8-4-4-4-12 lowercase hex
        // the model's `UUID` fixes) rather than the name, and the collection's ARN is built
        // from it the way Route 53 builds one ([`route53_entity_arn`]: no Region and no
        // account, since Route 53 is a global service). This is also the type
        // `super::provisioning` creates in Route 53 itself, which reports the id IT minted
        // and the ARN it built back over these attributes.
        ROUTE53_CIDR_COLLECTION => {
            let id = generated_uuid(resource_type, name);
            attributes.insert(
                "Arn".to_string(),
                route53_entity_arn(ROUTE53_CIDR_COLLECTION_ENTITY, &id),
            );
            attributes.insert("Id".to_string(), id.clone());
            (id, attributes)
        }
        // "Ref returns the delegation set ARN"; Fn::GetAtt: Id (and `NameServers`, a LIST,
        // which Route 53 assigns and `super::provisioning` reports off its create).
        // `CreateReusableDelegationSet` takes no identifier — the type declares only the
        // caller reference and the zone whose name servers the set is made reusable from —
        // so a declaration reports an identifier of Route 53's own shape rather than
        // CloudFormation's `<stack>-<logical id>-<unique id>` name, which no operation of
        // that service would accept as a `ResourceId`, with the set's ARN built from it the
        // way Route 53 builds one ([`delegation_set_arn`]).
        ROUTE53_DELEGATION_SET => {
            let id = format!(
                "{DELEGATION_SET_ID_PREFIX}{}",
                derived_digits(
                    DELEGATION_SET_ID_SALT,
                    name,
                    HOSTED_ZONE_ID_DIGITS,
                    Alphabet::Upper,
                )
            );
            attributes.insert("Id".to_string(), id.clone());
            (delegation_set_arn(&id), attributes)
        }
        // "Ref returns the traffic policy ARN"; Fn::GetAtt: Id, Type, Version. Route 53
        // ASSIGNS that id — `CreateTrafficPolicy` takes the policy's name and document and
        // answers the id it minted, and the policy's `Name` is only what it is called — so a
        // declaration reports an identifier of that service's own shape (the 8-4-4-4-12
        // lowercase hex a traffic policy id takes, its `TrafficPolicyId` being
        // `@length(min: 1, max: 36)`) rather than the name, which no operation of that
        // service would accept as a policy id. The other two published values are not
        // assigned at all: "for a new traffic policy, the value of `Version` is always 1"
        // (`TrafficPolicy$Version`), and the `Type` is "the DNS type of the resource record
        // sets that Amazon Route 53 creates when you use [the] traffic policy", which the
        // DOCUMENT the declaration carries states ([`declared_record_type`]).
        ROUTE53_TRAFFIC_POLICY => {
            let id = generated_uuid(resource_type, name);
            attributes.insert("Id".to_string(), id.clone());
            if let Some(record_type) = declared_record_type(properties) {
                attributes.insert("Type".to_string(), record_type);
            }
            attributes.insert(
                "Version".to_string(),
                FIRST_TRAFFIC_POLICY_VERSION.to_string(),
            );
            (traffic_policy_arn(&id), attributes)
        }
        // "Ref returns the key ID"; Fn::GetAtt: Arn, KeyId. KMS ASSIGNS that id —
        // `CreateKey` takes no identifier — so a declaration reports an identifier of KMS's
        // own shape (a version-4 UUID) rather than CloudFormation's
        // `<stack>-<logical id>-<unique id>` name, with the key ARN built from it the way
        // KMS builds one (`key/<key id>`). This is also the type `super::provisioning`
        // creates in KMS itself, which reports the id IT minted and the ARN it built back
        // over these attributes — and THOSE, not the ones computed here, are what the stack
        // reports for a key KMS created.
        KMS_KEY => {
            let id = generated_uuid(resource_type, name);
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(KMS_ARN_NAMESPACE, region, &format!("key/{id}")),
            );
            attributes.insert("KeyId".to_string(), id.clone());
            (id, attributes)
        }
        // The nine AWS AppSync types a GraphQL API's estate, an Event API's and the link of a
        // source API into a Merged API are declared with, whose return
        // values — the ARNs AppSync builds on the ids it mints, and the endpoints only that
        // service can report — are [`appsync`]'s.
        APPSYNC_GRAPHQL_API
        | APPSYNC_GRAPHQL_SCHEMA
        | APPSYNC_DATA_SOURCE
        | APPSYNC_RESOLVER
        | APPSYNC_FUNCTION_CONFIGURATION
        | APPSYNC_API_KEY
        | APPSYNC_API
        | APPSYNC_CHANNEL_NAMESPACE
        | APPSYNC_SOURCE_API_ASSOCIATION => {
            appsync_return_values(resource_type, name, properties, scope, budget)
        }
        // The five AWS Cloud Map types a namespace's estate is declared with, whose return
        // values — the `ns-…`/`srv-…` ids Cloud Map mints, the ARNs it builds on them and the
        // hosted zone only that service can report — are [`service_discovery`]'s.
        SERVICEDISCOVERY_HTTP_NAMESPACE
        | SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE
        | SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE
        | SERVICEDISCOVERY_SERVICE
        | SERVICEDISCOVERY_INSTANCE => {
            service_discovery_return_values(resource_type, name, properties, scope, budget)
        }
        // "Ref returns the ARN of the cluster" for both kinds of Amazon MSK cluster, and
        // that ARN is the type's one published `Fn::GetAtt` attribute too. Amazon MSK
        // MINTS it: its resource path is `cluster/<name>/<id>`, where the id is the
        // service's own, so what is computed here is an ARN of that published shape with a
        // stand-in id derived from the resource's name. These are also the types
        // `super::provisioning` creates in Amazon MSK itself, which reports the ARN IT
        // minted back as the resource's identity — and THAT ARN, not this one, is what the
        // stack reports for a cluster MSK created.
        MSK_CLUSTER | MSK_SERVERLESS_CLUSTER => {
            let arn = ::emulator_server::emulator_arn(
                MSK_ARN_NAMESPACE,
                region,
                &format!(
                    "cluster/{name}/{}-{MSK_RESOURCE_ARN_ORDINAL}",
                    generated_uuid(resource_type, name)
                ),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            (arn, attributes)
        }
        // "Ref returns the ARN of the configuration", and `Fn::GetAtt` reads that ARN
        // beside the configuration's LATEST REVISION — its number and the instant it was
        // created. Amazon MSK MINTS all three: the ARN's resource path is
        // `configuration/<name>/<id>-<ordinal>`, where the id and the ordinal are that
        // service's own, and a revision is numbered and timestamped by it — so what is
        // computed here is an ARN of that published shape with a stand-in id derived from
        // the resource's name, the number a created configuration's FIRST revision carries,
        // and a stand-in for the instant only that service can report. This is also the
        // type `super::provisioning` creates in Amazon MSK itself, which reports the ARN and
        // the revision IT minted back as the resource's identity — and THOSE, not the
        // values computed here, are what the stack reports for a configuration MSK created.
        MSK_CONFIGURATION => {
            let arn = ::emulator_server::emulator_arn(
                MSK_ARN_NAMESPACE,
                region,
                &format!(
                    "configuration/{name}/{}-{MSK_RESOURCE_ARN_ORDINAL}",
                    generated_uuid(resource_type, name)
                ),
            );
            attributes.insert("Arn".to_string(), arn.clone());
            attributes.insert(
                MSK_CONFIGURATION_REVISION_CREATION_TIME.to_string(),
                service_assigned_document(name, MSK_CONFIGURATION_REVISION_CREATION_TIME),
            );
            attributes.insert(
                MSK_CONFIGURATION_REVISION_NUMBER.to_string(),
                MSK_CONFIGURATION_FIRST_REVISION.to_string(),
            );
            (arn, attributes)
        }
        // "Ref returns the Amazon MQ broker ID"; Fn::GetAtt: Arn ("the broker's ARN").
        // Amazon MQ MINTS that id — `CreateBroker` takes a broker NAME and answers with the
        // `brokerId` it generated — so a declaration reports an identifier of Amazon MQ's
        // own shape (`b-` followed by 128 bits in the 8-4-4-4-12 grouping its broker ids
        // carry) rather than CloudFormation's `<stack>-<logical id>-<unique id>` name, with
        // the broker ARN built from it the way Amazon MQ builds one
        // (`broker:<name>:<broker id>`). This is also the type `super::provisioning` creates
        // in Amazon MQ itself, which reports the id IT minted and the ARN it built back as
        // the resource's identity — and THOSE, not the ones computed here, are what the
        // stack reports for a broker Amazon MQ created, which is what
        // [`identity_is_service_assigned`] names this type for.
        MQ_BROKER => {
            let id = format!("{MQ_BROKER_ID_PREFIX}{}", generated_uuid(resource_type, name));
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    MQ_ARN_NAMESPACE,
                    region,
                    &format!("broker:{name}:{id}"),
                ),
            );
            (id, attributes)
        }
        // "Ref returns the backup vault name"; Fn::GetAtt: BackupVaultArn, BackupVaultName.
        // This is the type `super::provisioning` creates in AWS Backup itself, which then
        // answers with the vault's own ARN — the same ARN this computes, since both are
        // built from the one account id this emulator mints and the vault's name, which is
        // how AWS Backup keys a vault (`backup_vault_arn` in that service's own state
        // module). (A plan and a selection of the same namespace carry no arm here: each
        // one's identifier and ARN are MINTED by AWS Backup, so there is nothing for these
        // naming rules to compute — see [`identity_is_service_assigned`].)
        BACKUP_VAULT => {
            attributes.insert(
                "BackupVaultArn".to_string(),
                ::emulator_server::emulator_arn(
                    BACKUP_ARN_NAMESPACE,
                    region,
                    &format!("backup-vault:{name}"),
                ),
            );
            attributes.insert("BackupVaultName".to_string(), name.to_string());
            (name.to_string(), attributes)
        }
        // "Ref returns the Amazon Resource Name (ARN) of the private CA"; Fn::GetAtt: Arn,
        // CertificateSigningRequest. Both are Amazon Web Services Private CA's OWN: it
        // mints the ARN (`identity_is_service_assigned` names the type) and it builds the
        // certification request from the key it generated, so neither is a value these
        // naming rules can compute — each is a stand-in
        // ([`super::provisioning::repoint_references`] re-points it at what that service
        // answered once the CA has been created).
        ACMPCA_CERTIFICATE_AUTHORITY => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    ACMPCA_ARN_NAMESPACE,
                    region,
                    &format!("certificate-authority/{name}"),
                ),
            );
            attributes.insert(
                "CertificateSigningRequest".to_string(),
                service_assigned_document(name, "CertificateSigningRequest"),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the ARN of the issued certificate"; Fn::GetAtt: Arn, Certificate.
        // The ARN carries the serial that service allocated and the body is the
        // certificate it signed, so both are stand-ins here too.
        ACMPCA_CERTIFICATE => {
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    ACMPCA_ARN_NAMESPACE,
                    region,
                    &format!("certificate-authority/{name}/certificate/{name}"),
                ),
            );
            attributes.insert(
                "Certificate".to_string(),
                service_assigned_document(name, "Certificate"),
            );
            (name.to_string(), attributes)
        }
        // Fn::GetAtt: CompleteCertificateChain — the certificate the CA was activated with
        // beside the chain above it, which is that service's to assemble. The resource IS
        // the activation of ONE private CA, and `CertificateAuthorityArn` is the property
        // naming it, so that ARN is what identifies the resource.
        ACMPCA_CERTIFICATE_AUTHORITY_ACTIVATION => {
            attributes.insert(
                "CompleteCertificateChain".to_string(),
                service_assigned_document(name, "CompleteCertificateChain"),
            );
            let identity = properties
                .get("CertificateAuthorityArn")
                .and_then(|declared| text(declared, scope, 0, budget))
                .unwrap_or_else(|| name.to_string());
            (identity, attributes)
        }
        // The two Hook types a template declares a CloudFormation Hook with. "Ref returns the
        // Hook ARN" and `Fn::GetAtt: HookArn` returns that same ARN — the registry ARN form an
        // activated Hook of this account and Region is addressed by (`type/hook/<Type-Name>`,
        // built by [`crate::state::registry::type_arn`], so the output a stack publishes is
        // the identifier the extension APIs take for a Hook rather than a name of
        // CloudFormation's own).
        //
        // The Hook's own type name is the `Alias` its declaration states ("the type name
        // alias for the Hook … must be in the form `Private::Hook::MyCustomHook`"), which is
        // what makes the ARN name the Hook a client configures rather than the logical id of
        // the resource that declared it; a declaration stating none leaves the ARN on
        // CloudFormation's own generated name for the resource, exactly as every other type
        // whose name is a property does.
        CLOUDFORMATION_GUARD_HOOK | CLOUDFORMATION_LAMBDA_HOOK => {
            let alias = properties
                .get("Alias")
                .and_then(|declared| text(declared, scope, 0, budget))
                .filter(|alias| !alias.is_empty())
                .unwrap_or_else(|| name.to_string());
            let arn = crate::state::registry::type_arn(
                region,
                crate::state::registry::registry_type::HOOK,
                &alias,
            );
            attributes.insert("HookArn".to_string(), arn.clone());
            (arn, attributes)
        }
        // The two Amazon MemoryDB groups a cluster is configured out of. "Ref returns the
        // name" for each, which is the `ParameterGroupName`/`SubnetGroupName` the declaration
        // states and the identifier every MemoryDB call on one takes, and each publishes its
        // own ARN under the upper-case member name that service's model spells it with
        // ([`UPPER_CASE_ARN_ATTRIBUTE`], `ParameterGroup$ARN` / `SubnetGroup$ARN`).
        //
        // The arm exists for the ARN's RESOURCE PATH: Amazon MemoryDB writes these two
        // segments as one word (`parametergroup/<name>`, `subnetgroup/<name>` — the segments
        // its own `ListTags`/`TagResource` resolve and its describes report in the `ARN`
        // member), where the uniform convention
        // ([`super::super::conventional_attributes`]) would hyphenate the type's segment into
        // `parameter-group`/`subnet-group`. The ARN computed here is therefore the one that
        // service reports for the group of this name, and the conventional `Arn`/`<Type>Arn`
        // names are read off it rather than minted beside it.
        MEMORYDB_PARAMETER_GROUP | MEMORYDB_SUBNET_GROUP => {
            let segment = if resource_type == MEMORYDB_PARAMETER_GROUP {
                MEMORYDB_PARAMETER_GROUP_SEGMENT
            } else {
                MEMORYDB_SUBNET_GROUP_SEGMENT
            };
            attributes.insert(
                UPPER_CASE_ARN_ATTRIBUTE.to_string(),
                ::emulator_server::emulator_arn(
                    MEMORYDB_ARN_NAMESPACE,
                    region,
                    &format!("{segment}/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the name" for each of the three AWS Elemental MediaConvert types, and
        // each publishes `Arn` — "the Amazon Resource Name (ARN) of the queue", of the job
        // template, of the preset. The arm exists for the ARN's RESOURCE PATH: MediaConvert
        // writes it with the PLURAL noun (`queues/<name>`, `jobTemplates/<name>`,
        // `presets/<name>` — the segments its own answers carry in each resource's `Arn`
        // member and its tagging calls resolve), where the uniform convention
        // ([`super::super::conventional_attributes`]) would give them the singular, hyphenated
        // type segment. The ARN computed here is therefore the one that service reports for the
        // resource of this name, which is also the one
        // [`crate::state::provisioning::mediaconvert_resources`] reads off the create's own
        // answer — both built from the one account id this emulator mints and the resource's
        // name.
        MEDIACONVERT_QUEUE | MEDIACONVERT_JOB_TEMPLATE | MEDIACONVERT_PRESET => {
            let segment = match resource_type {
                MEDIACONVERT_QUEUE => MEDIACONVERT_QUEUE_SEGMENT,
                MEDIACONVERT_JOB_TEMPLATE => MEDIACONVERT_JOB_TEMPLATE_SEGMENT,
                _ => MEDIACONVERT_PRESET_SEGMENT,
            };
            attributes.insert(
                "Arn".to_string(),
                ::emulator_server::emulator_arn(
                    MEDIACONVERT_ARN_NAMESPACE,
                    region,
                    &format!("{segment}/{name}"),
                ),
            );
            (name.to_string(), attributes)
        }
        // "Ref returns the record name"; the type publishes no `Fn::GetAtt` attribute. The
        // record name CloudFormation reports is the DNS name WITHOUT its terminal dot: a
        // zone's own names are fully qualified in Route 53 ("Route 53 treats
        // `www.example.com` and `www.example.com.` as identical"), while the value this type
        // hands back is the name in the form a client uses it in — joined into a URL, or
        // compared with the name the template declared minus the dot. The name the record is
        // CREATED under is the declaration's own ([`crate::state::provisioner`]), so the
        // trailing dot is dropped only from the identifier the stack reports.
        ROUTE53_RECORD_SET => (
            without_terminal_dot(name).to_string(),
            attributes,
        ),
        // A ROUTE in a route table. Amazon EC2 mints no identifier for one — every call on a
        // route addresses it by the table it is in and the destination it matches — so the
        // physical resource id of one is that PAIR ([`route_identity`]), which is what the
        // type's `Ref` returns and what its one published attribute carries: "CidrBlock — the
        // CIDR block used for the destination match", the destination the declaration states.
        //
        // Both halves come out of the declaration, so the id carries the stand-in a `Ref` to
        // the route table resolved to until that table is created; the deployment re-points it
        // at the `rtb-…` id Amazon EC2 minted, as it re-points every other value built on one
        // ([`crate::state::provisioning::repoint_identity`]), and a later deployment resolves
        // the declaration against the id in force.
        EC2_ROUTE => {
            let resolved = resolved_members(properties, EC2_ROUTE_IDENTITY_MEMBERS, scope, budget);
            if let Some(destination) = ec2_route_destination(&resolved) {
                attributes.insert("CidrBlock".to_string(), destination);
            }
            let identity = ec2_route_identity(&resolved).unwrap_or_else(|| name.to_string());
            (identity, attributes)
        }
        // The ATTACHMENT that puts one gateway on one VPC. Amazon EC2 mints no identifier for
        // one either — both detaches address it by the gateway and the VPC — so "Ref returns
        // the name of the VPC gateway attachment", which is the KIND of gateway attached and
        // the VPC it is attached to ([`ec2_gateway_attachment_identity`]). The type publishes
        // no `Fn::GetAtt` attribute of its own.
        EC2_VPC_GATEWAY_ATTACHMENT => {
            let resolved =
                resolved_members(properties, EC2_ATTACHMENT_IDENTITY_MEMBERS, scope, budget);
            let identity =
                ec2_gateway_attachment_identity(&resolved).unwrap_or_else(|| name.to_string());
            (identity, attributes)
        }
        // The two LAKE FORMATION PERMISSION types, each of which declares ONE grant and no
        // name: what identifies a grant is the pair it binds, so that pair is the physical id
        // the type's `Ref` returns ([`crate::state::provisioner::lake_formation_permissions`]
        // holds both projections, beside the arms that make the grant).
        //
        // The newer type's primary identifier is COMPOSITE, and its two halves are the two
        // read-only properties it publishes: `Fn::GetAtt PrincipalIdentifier` is the
        // `DataLakePrincipal` the grant was made to and `Fn::GetAtt ResourceIdentifier` the
        // `Resource` it is on, each as the structure that shape is — so the id is those two
        // joined with the separator a composite identifier is joined with.
        LAKEFORMATION_PRINCIPAL_PERMISSIONS => {
            let declared = resolve(properties, scope, budget).unwrap_or(Value::Null);
            let principal = lake_formation_permissions::principal_identifier(&declared);
            let resource = lake_formation_permissions::resource_identifier(&declared);
            let identity = format!("{principal}{COMPOSITE_IDENTITY_SEPARATOR}{resource}");
            attributes.insert(
                lake_formation_permissions::PRINCIPAL_IDENTIFIER_PROPERTY.to_string(),
                principal,
            );
            attributes.insert(
                lake_formation_permissions::RESOURCE_IDENTIFIER_PROPERTY.to_string(),
                resource,
            );
            (identity, attributes)
        }
        // The legacy type publishes no `Fn::GetAtt` attribute at all, and its id is the
        // principal together with the ARN of the Data Catalog resource the grant is on. A
        // declaration that names neither describes no grant, and such a resource keeps the
        // identifier the naming rules computed.
        LAKEFORMATION_PERMISSIONS => {
            let declared = resolve(properties, scope, budget).unwrap_or(Value::Null);
            let identity = lake_formation_permissions::legacy_identity(&declared, region)
                .unwrap_or_else(|| name.to_string());
            (identity, attributes)
        }
        _ => (name.to_string(), attributes),
    }
}
