// SPDX-License-Identifier: CC0-1.0

//! One entry per resource type this emulator really creates: which service owns the type,
//! and the create, update and delete calls that make its resources exist, change and stop
//! existing.
//!
//! This is the half of provisioning that is PER RESOURCE TYPE. The half that is not —
//! reconciling what a deployment declares against what the stack already has, deleting
//! what it replaced, and the in-process transport every one of these calls rides on —
//! is [`super::provisioning`], which owns the algorithm and reaches this module only
//! through [`provisioner_for`]. Adding a type is therefore a change to THIS file alone:
//! one arm in [`provisioner_for`] and the create/delete pair it names.
//!
//! A type with no entry here is left declared-only, which
//! [`super::provisioning`](crate::state::provisioning) documents: nothing is created in
//! any service, and the resource still carries the physical id and attributes
//! CloudFormation's documented naming rules give it.
//!
//! A deployment that re-declares a resource with a DIFFERENT configuration is applied
//! through the owning service's own update API ([`Provisioner::update`]), which is per
//! type for the same reason the create is: `AWS::SQS::Queue`'s configuration is its
//! attributes and its tags, so the change is a `SetQueueAttributes` plus a
//! `TagQueue`/`UntagQueue` ([`update_queue`]). A type with no arm there has nothing asked
//! of its service and the deployment reports the change as not applied, exactly as a type
//! with no create arm is left declared-only — never a stack that completed while the
//! caller's change went nowhere.
//!
//! How a create call is BUILT depends on what the type's properties are:
//!
//! * `AWS::KinesisFirehose::DeliveryStream`'s properties ARE `CreateDeliveryStream`'s own
//!   members, so the evaluated property document is the request, passed through verbatim
//!   with the one member CloudFormation decides (the name) set on the way out. That create
//!   is asynchronous and leaves the stream in a status in which it takes no data, so the
//!   resource's creation is held open until Amazon Data Firehose reports the stream `ACTIVE`
//!   ([`delivery_stream_readiness`]);
//! * `AWS::Glue::Job`'s are `CreateJob`'s own members the same way, with its tags the map
//!   that call takes; the `JobUpdate` that call's sibling `UpdateJob` "completely
//!   overwrites" a job definition with is that same document less the job's name, so one
//!   declaration serves this type's create and its in-place update alike;
//! * `AWS::CloudWatch::MetricStream`'s are `PutMetricStream`'s own members the same way,
//!   and that one call is documented "Creates or updates a metric stream" — so it is also
//!   this type's UPDATE, the one arm here that can apply a changed declaration to a
//!   resource the stack already has;
//! * `AWS::CloudWatch::Dashboard`'s three properties are `PutDashboard`'s own members,
//!   with the dashboard's name the one CloudFormation decides, its `DashboardBody` taken as
//!   the JSON TEXT the request carries, and its `Tags` the declaration's own tag list beside
//!   the ones CloudFormation propagates. That one call is documented "Creates a dashboard if
//!   it does not already exist, or updates an existing dashboard" — so it is this type's
//!   update as well as its create. Its `Tags` member applies on the CREATE alone, that
//!   member being documented as ignored when the call updates a dashboard that exists, so a
//!   later deployment's tag change reaches CloudWatch through that service's own
//!   `TagResource`/`UntagResource` ([`Provisioner::retag`]);
//! * `AWS::CodePipeline::Pipeline`'s are the same document with one transformation: they
//!   are `PipelineDeclaration`'s members spelled with an upper-case initial, so
//!   [`camel_cased`] lower-cases each key's first letter and the document is the
//!   declaration;
//! * `AWS::ECR::PublicRepository`'s are NOT (`RepositoryName` and
//!   `RepositoryCatalogData.RepositoryDescription` against the API's `repositoryName` and
//!   `catalogData.description`), so its arm maps the members AWS's resource reference
//!   publishes one by one, and sends nothing for a property the template leaves out;
//! * the `AWS::OpenSearchServerless::*` types are the same case (`Name`/`Type`/`Policy`
//!   against `name`/`type`/`policy`), and a collection's identifier is one the OWNING
//!   service mints rather than a name the template asked for, so its arm answers that
//!   identifier and [`super::template::identity_is_service_assigned`] names the type it
//!   belongs to;
//! * `AWS::S3::Bucket`'s and `AWS::IAM::Role`'s reach services whose wire is not JSON at
//!   all, so those arms go through the official `aws-sdk-s3` / `aws-sdk-iam` clients over
//!   the same in-process callback ([`s3_client`], [`iam_client`]) and name the members
//!   they send one by one;
//! * `AWS::ECS::Cluster`'s are the same members under a different CASE — the Amazon ECS
//!   model spells them in lower camel case, and renames one (`ClusterSettings` sets
//!   `settings`) — so its arm names the properties `CreateCluster` takes and re-cases each
//!   value;
//! * `AWS::SQS::Queue`'s properties are the queue's ATTRIBUTES, named identically on both
//!   sides, so its arm passes them through as `CreateQueue`'s `Attributes` map, lifting
//!   out the two that are not attributes (the name CloudFormation decides, and the tag
//!   list the create takes as its own member);
//! * `AWS::KMS::Key`'s properties are `CreateKey`'s own members but for five: the key
//!   policy, which that call takes as the document's TEXT, and the four no member of it
//!   carries at all — the rotation of the key's material and whether the key is enabled,
//!   each applied through the KMS call that owns it, the rotation period that first one
//!   takes, and the waiting period the key's deletion is scheduled with;
//! * `AWS::KMS::Alias`'s two properties ARE `CreateAlias`'s own members, named identically
//!   on both sides: the alias name, which is this resource's physical id and so
//!   CloudFormation's to decide, and the `TargetKeyId` the alias is bound to — which the
//!   declaration states as a reference to the key, so what reaches KMS is the identifier
//!   KMS minted for it;
//! * `AWS::SageMaker::ModelPackageGroup`'s are its create's members EXCEPT ONE — a model
//!   group's resource policy is attached through SageMaker's own
//!   `PutModelPackageGroupPolicy` — so its arm passes the properties through and then makes
//!   that second call, the same pair a client makes;
//! * `AWS::SNS::Topic`'s are the topic's attributes in the same way, with the three that
//!   are not attributes lifted out (the name, the tag list and the data-protection policy,
//!   each a `CreateTopic` member of its own), the inline `Subscription` list made by
//!   `Subscribe` calls on the created topic, and `DeliveryStatusLogging` written as the
//!   per-protocol feedback attributes SNS names. A changed declaration reaches the topic
//!   that exists through the one call that writes an attribute of a live one
//!   ([`update_topic`]), its tags through that service's own tagging calls
//!   ([`retag_topic`]);
//! * `AWS::FIS::ExperimentTemplate`'s are the same members in lower camel case, but its
//!   `Targets` and `Actions` are MAPS keyed by names the template chose (as are a target's
//!   `ResourceTags` and `Parameters` and an action's `Parameters` and `Targets`), so its
//!   arm maps each modelled structure member by member and passes every caller-keyed map
//!   through as written. Its physical id is one the owning service ASSIGNS: FIS mints the
//!   experiment template's id, so that id — not a computed name — is what the stack reports
//!   for the resource;
//! * `AWS::WAFRegional::IPSet`'s two properties reach AWS WAF Classic through TWO calls,
//!   because that is how its API takes them: `CreateIPSet` takes the `Name`, each declared
//!   `IPSetDescriptors` entry is INSERTed by an `UpdateIPSet`, and both carry a change
//!   token of their own. Its identity is its owning service's too ("Ref returns the IPSet
//!   ID"), so its arm answers the `IPSetId` AWS WAF returned as the resource's physical id.
//! * the four `AWS::WAFv2::*` types — a web ACL, a rule group, an IP set and a regex
//!   pattern set — have properties that ARE their create's own members, named identically on
//!   both sides, so the evaluated document is the request with the name CloudFormation
//!   decides set on it. The few members the two sides spell DIFFERENTLY are rewritten both
//!   ways ([`wafv2_spelled`]): a byte-match statement's search value is a BLOB, which the
//!   wire carries base64-encoded while a template declares the bytes, and the ARN a
//!   reference statement names another entity by is `ARN` on the wire and `Arn` in a
//!   template. Each
//!   type's identity is WAF's own — its create assigns the entity `Id` and builds the ARN
//!   that carries the scope, the name and that id — so its arm answers both, and what the
//!   stack publishes for the resource is what `AssociateWebACL` and the `Get…` calls take.
//! * `AWS::WAFv2::WebACLAssociation` is none of those: it is no entity of WAF's own but the
//!   BINDING between a protected resource and one of those web ACLs, so its two properties
//!   are `AssociateWebACL`'s own two members and that one call is its whole deployment
//!   ([`associate_web_acl`]) — with the pair it bound as the resource's physical id, which
//!   is what makes `Ref` name which resource is protected by which web ACL.
//! * `AWS::Grafana::Workspace`'s properties are `CreateWorkspace`'s own members under
//!   DIFFERENT names — the type spells the workspace's own fields without the `workspace`
//!   prefix that API carries (`Name`/`Description`/`RoleArn`/`DataSources` against
//!   `workspaceName`/`workspaceDescription`/`workspaceRoleArn`/`workspaceDataSources`) —
//!   so its arm names the members the resource reference publishes one by one and sends
//!   nothing for a property the template leaves out. Its identity is that service's as
//!   well: Amazon Managed Grafana mints the workspace id its `Ref` returns.
//! * `AWS::EMR::Cluster`'s are mostly `RunJobFlow`'s own members (`ReleaseLabel`,
//!   `Applications`, `BootstrapActions`, `Tags`, the roles and the scalars), so they pass
//!   through verbatim; the two that are shaped differently are mapped — the per-role
//!   capacity properties of `Instances` onto the one `InstanceGroups` or `InstanceFleets`
//!   list the API takes, and a `Configurations` entry's `ConfigurationProperties` onto the
//!   API's `Properties`. Its identity is its owning service's as well ("Ref returns the
//!   cluster ID"), so its arm answers the `j-` job-flow id Amazon EMR minted as the
//!   resource's physical id;
//! * `AWS::GuardDuty::Detector`'s are the `CreateDetector` request's own members spelled
//!   with an initial capital (`Enable` / `enable`), all the way down, so its arm carries the
//!   whole evaluated document through the same re-casing [`lower_camel_members`] the ECS
//!   arm needs and lifts out the tag list, which the create takes as a map. A detector's id
//!   is GuardDuty's OWN to mint — `CreateDetector` takes no identifier and answers with
//!   `detectorId` — so, as for the FIS, AWS WAF Classic and Amazon EMR types above, what
//!   the stack reports for the resource is the id that create answered with.
//! * `AWS::GuardDuty::IPSet`'s are `CreateIPSet`'s own members spelled the same way, with
//!   the detector the list hangs off addressing the call rather than travelling in its
//!   body, and the list's NAME taken from the naming rules so a declaration that names none
//!   still creates a list. An IPSet's id is GuardDuty's OWN to mint — `CreateIPSet` takes no
//!   identifier and answers with `ipSetId` — so what the stack reports for the resource is
//!   the id that create answered with, which is what `GetIPSet` resolves.
//! * `AWS::EC2::VPC`'s and `AWS::EC2::Subnet`'s are `CreateVpc`'s and `CreateSubnet`'s own
//!   members under the same names ([`EC2_VPC_CREATE_MEMBERS`],
//!   [`EC2_SUBNET_CREATE_MEMBERS`]), walked against those catalogues by the same
//!   [`query_members`] the Amazon RDS arms use — every one of them a scalar — so a property
//!   neither catalogue holds is REFUSED the way theirs refuse one. Each
//!   type's identity is Amazon EC2's OWN — neither create takes an identifier, and each
//!   answers the `vpc-…`/`subnet-…` id it assigned, which is what that type's `Ref`
//!   returns — so a stack's outputs carry ids EC2 really holds those resources for. The
//!   properties that are not members of either create are each the call that owns them: a
//!   VPC's two DNS switches and a subnet's launch-time switches are `ModifyVpcAttribute`
//!   and `ModifySubnetAttribute`, one attribute per call ("You can only modify one
//!   attribute at a time"), and the tag set is `CreateTags`, the one EC2 call that admits
//!   the reserved `aws:cloudformation:*` keys from a service provisioning on a caller's
//!   behalf. `CreateVpc` answers a VPC `pending` and `DescribeVpcs` reports it `available`
//!   once that service's own clock makes the move due, which is EC2's transition and not a
//!   wait on the creation: the resource the create answered exists and takes the calls made
//!   on it (a subnet is created in it straight away), exactly as the DB instance an
//!   `AWS::RDS::DBInstance` creates is reported created while Amazon RDS still has it
//!   `creating`.
//! * `AWS::EC2::SecurityGroup`'s are `CreateSecurityGroup`'s own members
//!   ([`EC2_SECURITY_GROUP_CREATE_MEMBERS`]) beside the NAME CloudFormation decides, which
//!   that call requires and the type leaves optional, and the two INLINE RULE lists, which
//!   are not members of the create at all: a group is created with no inbound rule and each
//!   declared rule is one permission of the authorize call for its direction
//!   ([`security_group_rule_parameters`]). Its identity is Amazon EC2's own as well — the
//!   create takes no id and answers the `sg-…` one it assigned, which is what "Ref returns
//!   the ID of the security group" names — and its tag set reaches EC2 through the same
//!   `CreateTags` a VPC's does.
//! * `AWS::RDS::DBInstance`'s and `AWS::RDS::DBSubnetGroup`'s are the members
//!   `CreateDBInstance` / `CreateDBSubnetGroup` take, named identically on both sides
//!   except the two Amazon RDS spells differently (`VPCSecurityGroups` is
//!   `VpcSecurityGroupIds`, `CustomIAMInstanceProfile` is `CustomIamInstanceProfile`), so
//!   each arm walks the declaration against its create's member catalogue
//!   ([`CREATE_DB_INSTANCE_MEMBERS`], [`CREATE_DB_SUBNET_GROUP_MEMBERS`], taken from that
//!   service's own model) and writes each member as the awsQuery form carries it — a list
//!   under the element name its list member's `@xmlName` fixes
//!   (`SubnetIds.SubnetIdentifier.1`). A property the catalogue does not carry is REFUSED,
//!   except the few that are members of a DIFFERENT call and are documented as such
//!   ([`DB_INSTANCE_PROPERTIES_NOT_SENT`]). Both identities are CloudFormation's — "Ref
//!   returns the DBInstanceIdentifier" / "the DBSubnetGroupName" — so each arm sets that
//!   member from the resource's physical id, and the DB instance's published endpoint,
//!   ARN and resource id are replaced by the ones Amazon RDS answered with, since those
//!   are the values a client reaches the database through.
//! * `AWS::RDS::DBParameterGroup`'s and `AWS::RDS::DBClusterParameterGroup`'s are their
//!   creates' own members read the same way ([`ParameterGroupCalls`]), with the one property
//!   Amazon RDS spells differently renamed (`Family` is `DBParameterGroupFamily`) and the
//!   name CloudFormation decides set on the way out — each type's `Ref` returns its group's
//!   name. The two declare the same document under different member and action names, so one
//!   arm serves both. Their `Parameters` MAP is no member of either create: a group's
//!   parameter values are written by the modify a client makes next, so the arm makes that
//!   second call ([`modify_parameters`]) in the batches that request's own bound admits —
//!   which is also this type's UPDATE, the modify being the one call of either group's API
//!   that writes a parameter value ([`update_parameter_group`]).
//! * `AWS::Events::Rule`'s are `PutRule`'s own members, spelled identically, with two
//!   exceptions: `EventPattern` is a template DOCUMENT where the request takes the JSON
//!   TEXT of one, and `Targets` is no `PutRule` member at all — a rule's targets are
//!   installed by the `PutTargets` a client makes next, so the arm makes that second call
//!   ([`install_targets`]), maps each declared target against `Target`'s own members and
//!   fails the resource on a target EventBridge reports as a failed entry. `PutRule` is
//!   documented "Creates or updates the specified rule", so it is also this type's UPDATE,
//!   with the declared target list reconciled against the installed one.
//! * `AWS::SSM::Parameter`'s are `PutParameter`'s own members the same way, with the name
//!   CloudFormation decides set on the way out and the declared tag MAP sent as the
//!   `[{Key, Value}]` list that request takes. That one call is "Create or update a
//!   parameter in Parameter Store", so `Overwrite` makes it this type's update too.
//! * `AWS::Cognito::UserPoolGroup`'s are `CreateGroup`'s own members, spelled identically on
//!   both sides (`UserPoolId`, `GroupName`, `Description`, `Precedence`, `RoleArn`), so the
//!   evaluated properties are the request with the one member CloudFormation decides (the
//!   group's name, which is this resource's physical id) set on the way out. A group's
//!   identity is a PAIR — the pool that holds it and its name — so every call of its arm
//!   carries the declared `UserPoolId` beside that name, and a declaration that moves the
//!   group to another pool is no in-place change: `UpdateGroup` changes only the three
//!   configuration members, so such a deployment reports the change as not applied.
//! * the two `AWS::S3Vectors::*` types' properties are their creates' own members with a
//!   lower-case initial, and each type's IDENTITY is its own ARN rather than a name: `Ref`
//!   returns the vector bucket's `bucket/<name>` ARN and the index's
//!   `bucket/<bucket>/index/<name>` one, so the names each create carries are read back off
//!   that ARN ([`vector_bucket_name`], [`vector_index_identity`]) and the delete and the
//!   drift read address the resource by the ARN itself.
//! * the two `AWS::Scheduler::*` types' properties ARE their creates' own members, spelled
//!   identically all the way down, with the name CloudFormation decides carried in the URI
//!   those `restJson1` operations bind it to — so each arm passes the evaluated property
//!   document through, a group's declared tags beside the ones CloudFormation gives it. A
//!   schedule is keyed by its name AND the group holding it
//!   ([`scheduled_group_of`]), so its delete and its drift read carry both. The same
//!   document is what a CHANGED declaration reaches that service as: `UpdateSchedule`
//!   overrides a schedule with every value the request states, and a group's tags — its one
//!   property no call changes on a group that exists — reach it through its own tagging
//!   calls ([`retag_schedule_group`]).
//! * the two `AWS::MSK::*` cluster types' properties are their creates' own members in
//!   lower camel case, so each arm walks the evaluated document ([`msk_members`]) with the
//!   one member whose API spelling is not a case change renamed
//!   ([`MSK_RENAMED_MEMBERS`]); a serverless cluster's two configuration properties go
//!   under the `serverless` body `CreateClusterV2` takes, whose `VpcConfigs` entries are
//!   mapped by name. Each type's identity is the `ClusterArn` Amazon MSK mints ("Ref
//!   returns the ARN of the cluster"), so that ARN — not a computed name — is what the
//!   stack reports for the resource, and the delete and the drift read address the cluster
//!   by it.
//!
//! * `AWS::AmazonMQ::Broker`'s properties ARE `CreateBroker`'s own members with the initial
//!   letter lower-cased, all the way down (a `Users` entry's `Username`/`Password`, a
//!   `Logs`' `General`, a `MaintenanceWindowStartTime`'s `DayOfWeek`), so its arm carries
//!   the evaluated document through with the broker's name — the one value CloudFormation
//!   decides — set on the way out and the declared `[{Key, Value}]` tag list sent as the tag
//!   MAP that create takes. Its identity is Amazon MQ's own ("Ref returns the Amazon MQ
//!   broker ID"): `CreateBroker` takes no identifier and answers the `brokerId` it minted
//!   beside the `brokerArn` it built around that id, so the stack reports that id for the
//!   resource and that ARN as its `Arn`, and the delete and the drift read address the
//!   broker by the id.
//!
//! * the three `AWS::Route53Resolver::*` types' properties ARE their creates' own members,
//!   spelled identically on both sides all the way down, so each arm carries the evaluated
//!   document through ([`resolver_request`]) with the one member CloudFormation decides
//!   added: the `CreatorRequestId` every one of those creates requires and no such type
//!   publishes ([`resolver_creator_request_id`]). Each type's identity is Route 53
//!   Resolver's OWN — `Ref` returns the `rslvr-out-…` endpoint id, the `rslvr-rr-…` rule id
//!   and the `rslvr-rrassoc-…` association id, and none of the creates takes an identifier —
//!   so what the stack reports for the resource is the id that create answered with, and an
//!   association is removed by the rule/VPC PAIR its declaration carries rather than by its
//!   own id.
//!
//! * the three `AWS::Route53Resolver::Firewall*` types are the same case with one difference
//!   each: a DNS Firewall domain list's `Domains` are not members of its create at all —
//!   "creates an EMPTY firewall domain list", which `UpdateFirewallDomains` then populates
//!   ([`create_firewall_domain_list`]) — a rule group's `FirewallRules` are each a
//!   `CreateFirewallRule` against the group the first call made
//!   ([`create_firewall_rule_group`]), and an association's properties ARE
//!   `AssociateFirewallRuleGroup`'s own members ([`associate_firewall_rule_group`]). All
//!   three identities are Route 53 Resolver's own (`rslvr-fdl-…`, `rslvr-frg-…`,
//!   `rslvr-frgassoc-…`), and an association is removed by the id that service minted for
//!   it, which is what `DisassociateFirewallRuleGroup` takes.
//!
//! * the three `AWS::CodeDeploy::*` types' properties are their creates' own members with the
//!   initial letter lower-cased — EXCEPT the ones whose nested structures CodeDeploy ITSELF
//!   spells in upper camel, its `Tag`, `EC2TagFilter` and `TagFilter` shapes each spelling
//!   `Key`, `Value` and `Type` — so every property of those types carries the rule saying
//!   which of the two it is ([`CodeDeployMember`]), and a property no entry names is REFUSED
//!   rather than dropped. A deployment group's `Deployment` property is no member of its
//!   create at all: it is an application revision deployed as soon as the group exists, so it
//!   is a `CreateDeployment` of its own on the group just created — the same second call a
//!   client makes. An application and a deployment configuration are keyed by their names,
//!   which is what each type's `Ref` returns; a group is keyed by the PAIR of the application
//!   and the group name ([`codedeploy_application_of`]), so its delete and its drift read
//!   carry both.
//!
//! * `AWS::EKS::AccessEntry`'s properties are `CreateAccessEntry`'s own members with a
//!   lower-case initial (`KubernetesGroups`, `PrincipalArn`, `Type`, `Username`), with the
//!   cluster carried in the URI that create binds it to and `Tags` sent as that call's tag
//!   MAP. `AccessPolicies` is no member of that create at all: each entry of it is an
//!   `AssociateAccessPolicy` of its own on the entry just created — the same second call a
//!   client makes — so a policy the declaration lists is associated to the entry rather
//!   than dropped. The entry is keyed by the PAIR of the cluster and the principal ARN
//!   ([`access_entry_identity`]), so its delete and its drift read carry both; an
//!   association Amazon EKS refuses leaves no half-configured entry behind, because the
//!   entry this arm just created is removed before the refusal is reported.
//!
//! * the `AWS::ApiGatewayV2::*` types' properties are their creates' own members with the
//!   initial letter lower-cased, all the way down ([`api_gateway_request`],
//!   [`apigatewayv2_child_request`]), with the declared tags sent as those calls' tag MAP
//!   where the type publishes any. The identity of an api, a mapping, an integration, a
//!   route, a deployment and an authorizer is API Gateway v2's OWN — `Ref` returns the id
//!   each create mints, and none of them takes one — so the stack reports what the create
//!   answered, and an api's `ApiEndpoint` attribute is the endpoint that service really
//!   serves it on. A custom domain name is keyed by the name the template states and a
//!   stage by the stage name it states; a mapping is keyed by the PAIR of its domain and the
//!   minted id ([`apigatewayv2_api_mapping_uri`]) and every resource that lives inside an
//!   api by the pair of that api and its own identifier ([`apigatewayv2_child_uri`]), so
//!   their deletes and drift reads carry both. An api declared with an inline OpenAPI
//!   definition is IMPORTED from it ([`import_apigatewayv2_api`]), which is a different call
//!   on a different binding; one declared with a definition held in Amazon S3 is refused
//!   rather than created without it.
//!
//! * `AWS::ApiGateway::DomainName`'s properties are the REST control plane's own
//!   `CreateDomainName` members under the same rule ([`api_gateway_request`]), with its
//!   `Policy` sent as the TEXT of the document a template declares, since that member is "a
//!   stringified JSON policy document". Its identity is the domain name the template states
//!   ("Ref returns the domain name"), so the create is sent that name and the delete and the
//!   drift read address the domain by it; the endpoint names and the ARN the type publishes
//!   are read off the create's answer ([`APIGATEWAY_DOMAIN_NAME_ATTRIBUTES`]), because only
//!   API Gateway assigns them.
//!
//! * `AWS::SES::ConfigurationSet`'s one property that reaches a call of this service is
//!   `TrackingOptions`: a configuration set's own create takes nothing but the name
//!   CloudFormation decides, and the custom open-and-click domain declared beside it is the
//!   `CreateConfigurationSetTrackingOptions` a client makes next on the set just created
//!   ([`create_ses_configuration_set`]). Every other published property of the type reaches
//!   no Amazon SES operation this emulator implements YET, so a declaration carrying one
//!   is REFUSED rather than reported as a set created without it, and a tracking domain
//!   Amazon SES refuses leaves no half-configured set behind. Its identity is
//!   CloudFormation's own — "Ref returns the configuration set name" — so the delete and
//!   the drift read address the set by the resource's physical id.
//!
//! * `AWS::SES::ReceiptRuleSet`'s one property IS the name CloudFormation decides, since
//!   `CreateReceiptRuleSet` takes nothing else ([`create_ses_receipt_rule_set`]), and
//!   `AWS::SES::ReceiptRule`'s three ARE `CreateReceiptRule`'s own members, spelled
//!   identically on both sides (`RuleSetName`, `After`, `Rule`), so the evaluated document
//!   is the request with the one member CloudFormation decides set on the way out: a rule's
//!   `Rule.Name` is REQUIRED by that call, so a declaration that states none has the
//!   resource's own computed name pushed for it ([`create_ses_receipt_rule`]). That name is
//!   what the rule's `Ref` returns, and it is nested inside a property rather than being a
//!   name property of its own, so the arm answers it as the resource's physical id.
//!
//! * `AWS::CodeCommit::Repository`'s properties are `CreateRepository`'s own members with a
//!   lower-case initial (`RepositoryDescription`, `KmsKeyId`), with the repository's name
//!   the one CloudFormation decides and `Tags` sent as that create's tag MAP. `Triggers` is
//!   no member of that create at all: the list it declares IS the repository's trigger
//!   configuration, so it is a `PutRepositoryTriggers` of its own on the repository just
//!   created — the same second call a client makes — and a trigger CodeCommit refuses fails
//!   the resource rather than leaving a repository with no triggers behind. The `Code`
//!   property, which commits a bundle from Amazon S3 into the new repository, is mapped onto
//!   no call here and is REFUSED rather than dropped.
//!
//! * `AWS::Route53::HostedZone`'s properties reach `CreateHostedZone` member by member —
//!   the zone's `Name`, its `HostedZoneConfig`'s comment and the first of the `VPCs` a
//!   private zone answers for, each further one an `AssociateVPCWithHostedZone` of its own —
//!   beside the `CallerReference` every such create requires and the type publishes no
//!   property for ([`route53_caller_reference`]). Its `HostedZoneTags` are no member of
//!   that create at all: a zone is tagged by `ChangeTagsForResource`, in the batches that
//!   call's own bound admits. The zone's identity is Route 53's OWN — "Ref returns the hosted
//!   zone ID", which that create MINTS and no naming rule could compute — so the id it
//!   answered with is what the stack reports for the resource, and that is the value
//!   `GetHostedZone`, `TestDNSAnswer` and a record set's `HostedZoneId` all take.
//!
//! * `AWS::Route53::RecordSet`'s properties ARE the members of the `ResourceRecordSet` a
//!   change batch carries ([`RECORD_SET_MEMBERS`]), spelled identically on both sides, with
//!   one shaped differently — `ResourceRecords` is a list of record VALUES in a template and
//!   a list of `ResourceRecord` structures on the wire — and three that are no members of it:
//!   `HostedZoneId`/`HostedZoneName` say which zone holds the record (the first IS the id,
//!   the second is resolved through `ListHostedZonesByName` as a client resolves it), and
//!   `Comment` is the change batch's own. A record set is created by the `CREATE` action and
//!   removed by the `DELETE` one, which Route 53 matches against the record's own values, so
//!   the delete carries the declaration the create was made from. Its identity is
//!   CloudFormation's ("Ref returns the record name"), so the create answers none of its own.
//!
//! * `AWS::Route53::HealthCheck`'s `HealthCheckConfig` property states the members of the
//!   configuration `CreateHealthCheck` takes ([`HEALTH_CHECK_CONFIG_MEMBERS`]), spelled
//!   identically on both sides, beside the `CallerReference` that create requires and the
//!   type publishes no property for ([`route53_caller_reference`]); its `HealthCheckTags`
//!   reach Route 53 through `ChangeTagsForResource`, as a zone's do. A changed declaration
//!   is applied through `UpdateHealthCheck`, whose members are the editable settings of a
//!   live check, with `ResetElements` for an element the declaration stopped stating. The
//!   check's identity is Route 53's OWN — "Ref returns the health check ID", which that
//!   create MINTS — so the id it answered with is what the stack reports, and that is the
//!   value `GetHealthCheck` and a record set's `HealthCheckId` take.
//!
//! * the three `AWS::Backup::*` types' properties are their creates' own members, with two
//!   differences and no others: a plan's rule list is published as `BackupPlanRule` where
//!   `CreateBackupPlan` takes `Rules`, and a rule's destination vault as
//!   `TargetBackupVault` where it takes `TargetBackupVaultName`
//!   ([`BACKUP_PLAN_MEMBERS`], [`BACKUP_RULE_MEMBERS`]) — so a declared plan body and a
//!   declared selection body reach AWS Backup as the bodies those calls take, and a member
//!   the declaration leaves out is absent from the request, leaving the plan with the
//!   service's own default for it. Each taggable type publishes its tags as a MAP of its
//!   own (`BackupVaultTags`, `BackupPlanTags`), which is the member its create takes them
//!   as ([`backup_declared_tags`]); a selection publishes none. A vault's
//!   `LockConfiguration` is no member of its create at all: it is the
//!   `PutBackupVaultLockConfiguration` a client makes next on the vault just created, and
//!   a lock AWS Backup refuses leaves no unlocked vault behind. A vault is keyed by the
//!   name the template states ("Ref returns the backup vault name"), while a plan's and a
//!   selection's identifiers are AWS Backup's OWN — neither create takes one, and each
//!   answers the id it minted — so what the stack reports for one is that id, which is
//!   what `GetBackupPlan`, `ListBackupSelections` and `GetBackupSelection` resolve; a
//!   selection is addressed by the PAIR of its plan and that id
//!   ([`backup_selection_uri`]), so its delete and its drift read carry both.
//!
//! ## The tags every created resource carries
//!
//! Beside what its declaration asks for, a resource CloudFormation creates carries the tags
//! CloudFormation gives it: the stack-level `Tags` plus `aws:cloudformation:stack-name`,
//! `aws:cloudformation:stack-id` and `aws:cloudformation:logical-id`
//! ([`super::provisioning::StackProvenance`]). That SET is the same for every type, so it is
//! computed once there; APPLYING it is per type, because each owning API takes a tag set its
//! own way — a `Tags` member of the create whose `Tag` shape spells `Key`/`Value` (Amazon
//! SNS, Amazon Data Firehose, Amazon ECR Public, Amazon CloudWatch, Amazon EMR, Amazon
//! SageMaker, IAM), the
//! same list spelled `key`/`value`
//! (Amazon ECS, CodePipeline), a map of key to value (Amazon SQS, AWS FIS), or a tagging
//! call of its own where the create takes no tags at all (Amazon S3's `PutBucketTagging`).
//! [`tags_of`] is the one place the declared and the propagated halves are merged, and a key
//! the declaration itself states keeps the declaration's value. A type that is not taggable
//! — `AWS::WAFRegional::IPSet`, which publishes no `Tags` property — is tagged with nothing,
//! because CloudFormation propagates no tags to one, and a type whose owning API refuses the
//! reserved `aws:` namespace on the path this provisioning call is made over carries the
//! stack's own tags without the three ([`Provisioner::applicable_tags`]).
//!
//! No arm invents a value: a property the template does not declare is absent from the
//! request, and what the owning service answers is what the stack records. A property an
//! arm does not map onto the create's request at all is REFUSED rather than dropped, so a
//! stack never reports a resource configured differently from its declaration — the FIS
//! arm's `ExperimentReportConfiguration` and the ACM certificate arm's
//! `DomainValidationOptions.HostedZoneId` are the two such properties today. Inside a
//! nested structure the member tables are the type's published members, so a member not in
//! one is not a member of the type.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

// The modules and free items of `crate::state` that the per-type arms of this module reach
// by their `super::` path. The arms live in this module's own files
// ([`data_and_storage`] and the ones beside it), so `super::` resolves HERE rather than in
// `crate::state`: re-exporting the names under this module is what keeps one path spelling
// for them across every file of the provisioner, rather than each file importing its own.
// Some are reached only by those files' doc links, which is why the unused-import lint is
// allowed here: dropping them would leave those links pointing at nothing.
#[allow(unused_imports)]
pub(super) use super::{
    drift, dynamic_references, provisioning, scalar_text, service_role, template, DEFAULT_REGION,
};

use super::provisioning::{
    call_json_rpc, call_query, call_rest_json, call_rest_json_named, call_rest_xml,
    percent_encoded, published_instant, xml_element, xml_elements,
    xml_entry_map, xml_entry_structures, xml_leaf, xml_member_structures, xml_scalar_children,
    Created, Readiness,
    Refusal, Removal,
    AWS_JSON_1_0, AWS_JSON_1_1,
};
use super::template::{
    ACMPCA_CERTIFICATE, ACMPCA_CERTIFICATE_AUTHORITY, ACMPCA_CERTIFICATE_AUTHORITY_ACTIVATION,
    ACMPCA_PERMISSION,
    ACM_ACME_ENDPOINT, ACM_CERTIFICATE, AMPLIFYUIBUILDER_COMPONENT, AMPLIFYUIBUILDER_FORM,
    AMPLIFYUIBUILDER_THEME, AMPLIFY_APP, AMPLIFY_BRANCH, AOSS_COLLECTION,
    AOSS_SECURITY_POLICY, AOSS_VPC_ENDPOINT, APIGATEWAY_BASE_PATH_MAPPING, APIGATEWAY_DOMAIN_NAME, APIGATEWAYV2_API,
    APIGATEWAYV2_API_MAPPING, APIGATEWAYV2_AUTHORIZER, APIGATEWAYV2_DEPLOYMENT,
    APIGATEWAYV2_DOMAIN_NAME, APIGATEWAYV2_INTEGRATION, APIGATEWAYV2_ROUTE,
    APIGATEWAYV2_ROUTING_RULE,
    APIGATEWAYV2_STAGE, APPLICATION_INSIGHTS_APPLICATION, APPLICATION_INSIGHTS_APPLICATION_PATH,
    APPLICATION_SIGNALS_GROUPING_CONFIGURATION,
    APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG, APPLICATION_SIGNALS_SERVICE_LEVEL_OBJECTIVE,
    BACKUP_GATEWAY_HYPERVISOR, BACKUP_PLAN, BACKUP_SELECTION, BACKUP_VAULT,
    BATCH_COMPUTE_ENVIRONMENT,
    BATCH_JOB_DEFINITION, BATCH_JOB_QUEUE, BATCH_SERVICE_ENVIRONMENT, BCM_DASHBOARD,
    BCM_SCHEDULED_REPORT, BEDROCK_DATA_SOURCE,
    BEDROCK_FLOW,
    BEDROCK_GUARDRAIL,
    BEDROCK_KNOWLEDGE_BASE,
    BEDROCK_SESSION,
    BRAKET_SPENDING_LIMIT, CLEANROOMSML_TRAINING_DATASET,
    CLOUDWATCH_DASHBOARD,
    CLOUDWATCH_METRIC_STREAM, CODEARTIFACT_DOMAIN, CODEARTIFACT_REPOSITORY,
    CODEBUILD_PROJECT,
    CODEBUILD_REPORT_GROUP, CODECOMMIT_REPOSITORY, CODEDEPLOY_APPLICATION,
    CODEDEPLOY_DEPLOYMENT_CONFIG, CODEDEPLOY_DEPLOYMENT_GROUP,
    CODEPIPELINE_ACTION_TYPE_IDENTITY, CODEPIPELINE_CUSTOM_ACTION_TYPE, CODEPIPELINE_PIPELINE,
    CODEPIPELINE_WEBHOOK,
    DIRECT_CONNECT_GATEWAY, DIRECT_CONNECT_RESILIENCY_GROUP,
    AUTOSCALING_GROUP, CUSTOMER_PROFILES_DOMAIN,
    COGNITO_USER_POOL_GROUP, DEFAULT_SCHEDULE_GROUP, DLM_LIFECYCLE_POLICY, EC2_LAUNCH_TEMPLATE, EC2_SECURITY_GROUP,
    EC2_SUBNET,
    EC2_VOLUME, EC2_VPC,
    ECR_PUBLIC_REPOSITORY, ECR_REPOSITORY, ECS_CLUSTER, ECS_SERVICE, EKS_ACCESS_ENTRY,
    EKS_ADDON,
    EKS_CLUSTER,
    EKS_NODEGROUP,
    EKS_POD_IDENTITY_ASSOCIATION,
    EMR_CLUSTER, EVENTS_EVENT_BUS, EVENTS_EVENT_BUS_POLICY, EVENTS_RULE,
    FIREHOSE_DELIVERY_STREAM, FIS_EXPERIMENT_TEMPLATE,
    ELASTIC_BEANSTALK_APPLICATION, ELASTIC_BEANSTALK_APPLICATION_VERSION,
    ELASTIC_BEANSTALK_ENVIRONMENT, GLUE_CRAWLER, GLUE_DATABASE, GLUE_TABLE,
    GLUE_TABLE_OPTIMIZER, GRAFANA_WORKSPACE,
    GUARDDUTY_DETECTOR, GUARDDUTY_IP_SET,
    IAM_ACCESS_KEY,
    IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
    KINESIS_STREAM,
    KMS_ALIAS, KMS_KEY,
    MASTER_USER_SECRET_ARN_ATTRIBUTE,
    MEDIACONVERT_CERTIFICATE_ASSOCIATION,
    MQ_BROKER, MSK_CLUSTER, MSK_CONFIGURATION,
    MSK_CONFIGURATION_REVISION_CREATION_TIME, MSK_CONFIGURATION_REVISION_DESCRIPTION,
    MSK_CONFIGURATION_REVISION_NUMBER,
    MSK_SERVERLESS_CLUSTER, MWAA_ENVIRONMENT, NEPTUNE_GRAPH_GRAPH, NEPTUNE_GRAPH_GRAPH_SNAPSHOT,
    DIRECTORY_SERVICE_MICROSOFT_AD, DIRECTORY_SERVICE_SIMPLE_AD,
    NETWORK_SECURITY_MANAGER_POLICY, NETWORK_SECURITY_MANAGER_RULE,
    NETWORK_SECURITY_MANAGER_SCOPE, NETWORK_SECURITY_MANAGER_TEMPLATE,
    OPENSEARCH_DOMAIN, PIPES_PIPE, RBIN_RULE,
    REDSHIFT_CLUSTER,
    REDSHIFT_CLUSTER_SUBNET_GROUP, RESOURCE_GROUPS_GROUP, ROUTE53RESOLVER_ENDPOINT,
    ROUTE53RESOLVER_FIREWALL_DOMAIN_LIST, ROUTE53RESOLVER_FIREWALL_RULE_GROUP,
    ROUTE53RESOLVER_FIREWALL_RULE_GROUP_ASSOCIATION,
    ROUTE53RESOLVER_RULE,
    ROUTE53RESOLVER_RULE_ASSOCIATION, ROUTE53_HEALTH_CHECK, ROUTE53_HOSTED_ZONE,
    ROUTE53_CIDR_COLLECTION, ROUTE53_DELEGATION_SET, ROUTE53_TRAFFIC_POLICY,
    ROUTE53_RECORD_SET, ROUTE53_RECORD_SET_GROUP,
    S3TABLES_BUCKET_SEGMENT, S3TABLES_NAMESPACE, S3TABLES_NAMESPACE_IDENTITY,
    S3TABLES_NAMESPACE_PROPERTY, S3TABLES_TABLE_BUCKET, S3TABLES_TABLE_BUCKET_ARN_ATTRIBUTE,
    S3VECTORS_BUCKET_SEGMENT, S3VECTORS_INDEX,
    S3VECTORS_VECTOR_BUCKET, SAGEMAKER_MODEL_PACKAGE_GROUP, SCHEDULER_SCHEDULE,
    SCHEDULER_SCHEDULE_GROUP, SES_CONFIGURATION_SET, SES_EMAIL_IDENTITY, SES_RECEIPT_RULE,
    SES_RECEIPT_RULE_SET,
    SNS_SUBSCRIPTION,
    SNS_TOPIC, SNS_TOPIC_INLINE_POLICY, SNS_TOPIC_POLICY, SQS_QUEUE, SQS_QUEUE_INLINE_POLICY,
    SQS_QUEUE_POLICY,
    SSM_ASSOCIATION, SSM_PARAMETER, STEP_FUNCTIONS_ACTIVITY,
    TRANSFER_SERVER, WAFV2_IP_SET, WAFV2_REGEX_PATTERN_SET, WAFV2_RULE_GROUP, WAFV2_WEB_ACL,
    WAFV2_WEB_ACL_ASSOCIATION, WAF_REGIONAL_IP_SET, XRAY_GROUP, XRAY_SAMPLING_RULE,
};
pub(super) use super::template::list_attribute_element;
use super::StoredResource;

/// `AWS::S3::Bucket`: "Ref returns the bucket name", so a resource of this type is a
/// bucket of Amazon S3 addressed by its physical resource id.
const S3_BUCKET: &str = "AWS::S3::Bucket";

/// `AWS::IAM::Role`: "Ref returns the role name".
const IAM_ROLE: &str = "AWS::IAM::Role";

/// `AWS::IAM::InstanceProfile`: "Ref returns the instance profile name" — the handle every
/// IAM call on one takes (`GetInstanceProfile$InstanceProfileName`) and the name an
/// instance's `IamInstanceProfile` references, which is the physical id the naming rules
/// already compute for the type (its `InstanceProfileName` property), so the type needs no
/// entry in [`super::template::identity_is_service_assigned`].
const IAM_INSTANCE_PROFILE: &str = "AWS::IAM::InstanceProfile";

/// `AWS::IAM::User`: "Ref returns the user name", so a resource of this type is an IAM
/// user addressed by its physical resource id.
const IAM_USER: &str = "AWS::IAM::User";

/// `AWS::IAM::ManagedPolicy`: "Ref returns the ARN of the managed policy" — the handle
/// every IAM operation on a managed policy takes — which is why
/// [`super::template::identity_is_service_assigned`] names this type: the ARN carries the
/// account and the path IAM built it from, and it is what a template passes to another
/// resource's `ManagedPolicyArns` or `PermissionsBoundary`.
const IAM_MANAGED_POLICY: &str = "AWS::IAM::ManagedPolicy";

/// `AWS::RDS::DBInstance`: "Ref returns the DBInstanceIdentifier", so a resource of this
/// type is a DB instance of Amazon RDS addressed by its physical resource id.
const RDS_DB_INSTANCE: &str = "AWS::RDS::DBInstance";
/// `AWS::RDS::DBSubnetGroup`: "Ref returns the DBSubnetGroupName" — the group a DB
/// instance's `DBSubnetGroupName` names, which Amazon RDS resolves before it will create
/// an instance in it.
const RDS_DB_SUBNET_GROUP: &str = "AWS::RDS::DBSubnetGroup";

/// `AWS::RDS::DBParameterGroup`: "Ref returns the DB parameter group name" — the group a DB
/// instance's `DBParameterGroupName` names, and the one `DescribeDBParameters` reports the
/// parameters of.
const RDS_DB_PARAMETER_GROUP: &str = "AWS::RDS::DBParameterGroup";

/// `AWS::RDS::DBClusterParameterGroup`: "Ref returns the DB cluster parameter group name" —
/// the cluster-scoped sibling of the type above, which a DB cluster's
/// `DBClusterParameterGroupName` names.
const RDS_DB_CLUSTER_PARAMETER_GROUP: &str = "AWS::RDS::DBClusterParameterGroup";

/// `AWS::RDS::DBProxy`: "Ref returns the DBProxyName" — the identifier every Amazon RDS
/// call on a proxy takes, which is the physical id the naming rules already compute for the
/// type (its own `DBProxyName` property), so the type needs no entry in
/// [`super::template::identity_is_service_assigned`].
const RDS_DB_PROXY: &str = "AWS::RDS::DBProxy";

/// `AWS::RDS::DBProxyTargetGroup`: not a resource Amazon RDS creates on its own account —
/// a proxy comes up with the one target group it has, named `default` — but the
/// REGISTRATION of the databases that group routes to, plus the connection pool settings
/// of the group. Its identity is the `TargetGroupArn` that service minted, which is what
/// this type's `Ref` returns, so [`super::template::identity_is_service_assigned`] names
/// it.
const RDS_DB_PROXY_TARGET_GROUP: &str = "AWS::RDS::DBProxyTargetGroup";

/// `AWS::BedrockAgentCore::WorkloadIdentity`: "Ref returns the name" of the workload
/// identity — the handle every call of the Amazon Bedrock AgentCore control plane on one
/// takes (`GetWorkloadIdentity`, `UpdateWorkloadIdentity` and `DeleteWorkloadIdentity` each
/// address an identity by its `name`) — so a resource of this type is addressed by its
/// physical resource id.
const BEDROCK_AGENTCORE_WORKLOAD_IDENTITY: &str = "AWS::BedrockAgentCore::WorkloadIdentity";

/// `AWS::Glue::Job`: "Ref returns the job name" — the `Name` the declaration states, which
/// is what `CreateJob` creates the job under and what every AWS Glue operation on a job
/// definition (`GetJob`, `UpdateJob`, `StartJobRun`, `DeleteJob`) addresses it by. That is
/// the physical id the naming rules already compute for a type whose published return
/// values `super::template` carries no entry for, so the type needs none there.
const GLUE_JOB: &str = "AWS::Glue::Job";

/// The code a refusal carries where the change a deployment asks for is one this emulator
/// has not written the owning API call for. It is the same code the scaffold's own 501
/// answers under (`smithy.framework#NotImplemented`), and not a code any AWS service
/// answers, so it is never mistaken for one of theirs.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// How a resource provider built on the CloudFormation resource-provider plugin words a
/// refusal it classifies as that plugin's invalid-request failure: the class's own sentence
/// opener, with the owning service's message after it
/// ([`Provisioner::handler_message`]).
///
/// Read by the per-type half of provisioning too, for a provider whose invalid-request
/// exception is raised around the TYPE it manages as well as the message
/// ([`super::provisioning::logs_resources`]), so there is one spelling of that opener.
pub(in crate::state) const INVALID_REQUEST_PROVIDED: &str = "Invalid request provided: ";

/// How the resource providers of the `AWS::MSK::*` types OPEN the message they report for a
/// refusal of Amazon MSK: the token of the resource request they were answering, in square
/// brackets, before the service's own message
/// ([`Provisioner::handler_message`]).
///
/// Recorded behaviour, read from the `ResourceStatusReason` of a failed resource of each of
/// those types (in the behavioral corpus `simparity-behavioral-aws-v1`, checks
/// `cloudformation__kafka.deploy-cfn-msk-serverless-cluster-rejects-iam-auth-disabled` and
/// `cloudformation__kafka.deploy-msk-configuration-invalid-kafka-version-rejected-and-\
/// rolled-back`, whose reasons read `Resource handler returned message:
/// "[ClientRequestToken: <token>] <the service's sentence> (Service: Kafka, Status Code:
/// 400, Request ID: …) (SDK Attempt Count: …)…"`), where that service's own API answers the
/// sentence alone. The token is the one the event's own `(RequestToken: …)` carries, since
/// it is the same resource request.
const MSK_HANDLER_REQUEST_TOKEN_LABEL: &str = "ClientRequestToken";

/// The services whose resources this emulator creates. One variant per resource type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Provisioner {
    FirehoseDeliveryStream,
    /// `AWS::Kinesis::Stream`: a data stream of Amazon Kinesis Data Streams — a different
    /// service from the Firehose one above — which `CreateStream` creates and
    /// `DeleteStream` deletes.
    KinesisStream,
    CloudWatchMetricStream,
    CloudWatchDashboard,
    /// `AWS::CloudWatch::Alarm`: the alarm Amazon CloudWatch's own `PutMetricAlarm` creates
    /// under the name the stack decided — the name this type's `Ref` returns and the handle
    /// every CloudWatch call about an alarm takes — and `DeleteAlarms` removes. That one
    /// call is documented "Creates or updates an alarm", so it is this type's update as
    /// well as its create ([`cloudwatch_alarms`]).
    CloudWatchAlarm,
    /// `AWS::CloudWatch::CompositeAlarm`: the composite alarm Amazon CloudWatch's own
    /// `PutCompositeAlarm` creates under the name the stack decided — the name this type's
    /// `Ref` returns — and `DeleteAlarms` removes. That one call is documented "Creates or
    /// updates a composite alarm", so it is this type's update as well as its create
    /// ([`cloudwatch_composite_alarms`]).
    CloudWatchCompositeAlarm,
    EcrPublicRepository,
    /// `AWS::ECR::Repository`: a repository of the PRIVATE Amazon ECR registry — a
    /// different service from the public one above — which `CreateRepository` creates
    /// under the repository name its declaration states and `DeleteRepository` deletes,
    /// with the resource-based policy and the lifecycle policy its declaration asks for set
    /// on it. Every setting of a repository that registry writes through a call of its own is
    /// also this type's UPDATE, so a changed declaration is applied to the repository the
    /// stack created rather than dropped ([`update_repository`]), and its
    /// tags travel through that registry's own tagging calls ([`Self::retag`]).
    EcrRepository,
    /// `AWS::EC2::VPC`: a VPC of Amazon EC2, whose `vpc-…` identifier that service assigns.
    Ec2Vpc,
    /// `AWS::EC2::Subnet`: a subnet of one of those VPCs, whose `subnet-…` identifier is
    /// EC2's to assign the same way.
    Ec2Subnet,
    /// `AWS::EC2::SecurityGroup`: a security group of one of those VPCs, whose `sg-…`
    /// identifier is EC2's to assign the same way, with the inline rules its declaration
    /// asks for authorized on it.
    Ec2SecurityGroup,
    /// `AWS::EC2::Volume`: an Amazon EBS volume, whose `vol-…` identifier EC2 assigns the
    /// same way. `CreateVolume` answers the volume `creating`, so its creation is held open
    /// until that service reports it `available`.
    Ec2Volume,
    /// `AWS::EC2::LaunchTemplate`: a launch template of Amazon EC2, whose `lt-…` identifier
    /// that service assigns and whose version numbers it keeps —
    /// `CreateLaunchTemplate` creates it under the name the stack decided and
    /// `DeleteLaunchTemplate` deletes it by the id EC2 answered.
    Ec2LaunchTemplate,
    /// `AWS::AutoScaling::AutoScalingGroup`: a group of Amazon EC2 Auto Scaling, which
    /// `CreateAutoScalingGroup` creates under the name the stack decided — the name this
    /// type's `Ref` returns — and `DeleteAutoScalingGroup` deletes by that name, together
    /// with the instances the group launched.
    AutoScalingGroup,
    /// `AWS::AutoScaling::ScalingPolicy`: a scaling policy of such a group, which
    /// `PutScalingPolicy` creates "or updates" under the name the stack decided — the name
    /// this type's physical id is — and `DeletePolicy` removes by that name. The policy's
    /// ARN is the one Amazon EC2 Auto Scaling minted, published through `Fn::GetAtt`
    /// ([`auto_scaling_policies`]).
    AutoScalingScalingPolicy,
    OpenSearchServerlessCollection,
    OpenSearchServerlessSecurityPolicy,
    /// `AWS::OpenSearchServerless::VpcEndpoint`: a managed interface endpoint of OpenSearch
    /// Serverless, which `CreateVpcEndpoint` creates under the name the stack decided and
    /// `DeleteVpcEndpoint` removes by the `vpce-…` id that service minted — the id this
    /// type's `Ref` returns, because it is what every call on an endpoint takes.
    OpenSearchServerlessVpcEndpoint,
    /// `AWS::OpenSearchService::Domain`: a domain of Amazon OpenSearch Service, which is a
    /// different service from the serverless one above — `CreateDomain` creates it and
    /// `DeleteDomain` deletes it.
    OpenSearchServiceDomain,
    S3Bucket,
    /// `AWS::S3::BucketPolicy`: not a resource of Amazon S3's own but the POLICY of the
    /// bucket it names, which `PutBucketPolicy` writes and `DeleteBucketPolicy` removes —
    /// the queue-policy pair's rule ([`Provisioner::SqsQueuePolicy`]) for the service that
    /// holds a policy as a sub-resource of the bucket.
    S3BucketPolicy,
    /// `AWS::S3Tables::TableBucket`: the table bucket Amazon S3 Tables'
    /// `CreateTableBucket` creates under the name the template states and
    /// `DeleteTableBucket` removes. That service keys a table bucket by its own ARN, which
    /// is this type's physical id and what every call on one takes, so the arms address it
    /// by that ARN rather than by the bucket's name.
    S3TablesTableBucket,
    /// `AWS::S3Tables::Namespace`: the namespace Amazon S3 Tables' `CreateNamespace` creates
    /// INSIDE the table bucket the declaration names and `DeleteNamespace` removes from it.
    /// That service addresses one by the bucket's ARN and the namespace's name together,
    /// which is this type's composite physical id and what the arms read the pair off.
    S3TablesNamespace,
    S3VectorsVectorBucket,
    S3VectorsIndex,
    EmrCluster,
    /// `AWS::EMR::SecurityConfiguration`: the security configuration Amazon EMR's own
    /// `CreateSecurityConfiguration` stores under the name the template states — "Ref returns
    /// the Name of the security configuration", which is the handle every call on one takes
    /// and what a cluster names it by — and `DeleteSecurityConfiguration` removes
    /// ([`emr_resources`]).
    EmrSecurityConfiguration,
    /// `AWS::EMR::Studio`: the Studio `CreateStudio` creates in Amazon EMR, whose `es-…`
    /// identifier that service mints — "Ref returns the Studio ID" — and `DeleteStudio`
    /// removes by it.
    EmrStudio,
    /// `AWS::EMR::StudioSessionMapping`: the mapping of ONE identity onto such a Studio,
    /// which `CreateStudioSessionMapping` creates and `DeleteStudioSessionMapping` removes.
    /// Amazon EMR mints no identifier for one, addressing it by the Studio, the identity type
    /// and the identity name together, which is this type's composite physical id and what
    /// the arms read the three parts off.
    EmrStudioSessionMapping,
    IamRole,
    /// `AWS::IAM::InstanceProfile`: the container that lets an Amazon EC2 instance assume
    /// the role it holds. `CreateInstanceProfile` creates it and
    /// `AddRoleToInstanceProfile` puts the declared role in it.
    IamInstanceProfile,
    IamUser,
    /// `AWS::IAM::AccessKey`: the long-term credential of one of those users, which
    /// `CreateAccessKey` mints and `DeleteAccessKey` deletes. Its id and its secret are
    /// IAM's to generate, so both are read off that create's answer.
    IamAccessKey,
    IamManagedPolicy,
    CodePipelinePipeline,
    /// `AWS::CodePipeline::CustomActionType`: the custom action type a job worker of the
    /// account polls for, which `CreateCustomActionType` registers from the declaration's
    /// own members and `DeleteCustomActionType` removes by the triple that identifies it.
    CodePipelineCustomActionType,
    /// `AWS::CodePipeline::Webhook`: the trigger a third party POSTs to in order to start
    /// one pipeline, which `PutWebhook` creates under the name the template states and
    /// `DeleteWebhook` removes by that name — so the webhook a stack reports created is one
    /// `ListWebhooks` reports and a POST to its published URL really starts
    /// ([`codepipeline_webhooks`]).
    CodePipelineWebhook,
    /// `AWS::CodeBuild::ReportGroup`: a report group of AWS CodeBuild, which
    /// `CreateReportGroup` creates under the name the template states and
    /// `DeleteReportGroup` removes by the ARN that service keyed it by.
    CodeBuildReportGroup,
    /// `AWS::CodeBuild::Project`: a build project of that same service, which
    /// `CreateProject` creates under the name the template states and `DeleteProject`
    /// removes by that name — so the project a stack reports created is one every
    /// CodeBuild read and every `StartBuild` resolves.
    CodeBuildProject,
    /// `AWS::CodeBuild::Fleet`: a compute fleet of that same service, which `CreateFleet`
    /// creates under the name the template decided and `DeleteFleet` removes by the ARN
    /// CodeBuild minted for it — so the fleet a stack reports created is one `ListFleets`
    /// reports and `BatchGetFleets` resolves ([`codebuild_fleets`]). "Ref returns the fleet
    /// ARN", which that create answers and which carries the `<name>:<id>` identifier only
    /// CodeBuild produces.
    CodeBuildFleet,
    EcsCluster,
    /// `AWS::ECS::Service`: the service Amazon ECS's own `CreateService` creates on the
    /// cluster the declaration names, `UpdateService` changes and `DeleteService` removes.
    /// "Ref returns the Amazon Resource Name (ARN)" of the service, which that create
    /// answers and every call about the service takes ([`ecs_services`]).
    EcsService,
    /// `AWS::ECS::CapacityProvider`: the capacity provider Amazon ECS's own
    /// `CreateCapacityProvider` creates under the name the template decided and
    /// `DeleteCapacityProvider` removes by that same name. "Ref returns the capacity
    /// provider name", which is the only handle that service takes for one — a
    /// `DescribeCapacityProviders` read, a `PutClusterCapacityProviders` association and a
    /// `capacityProviderStrategy` entry each name a provider by it
    /// ([`ecs_capacity_providers`]).
    EcsCapacityProvider,
    /// `AWS::ECS::ClusterCapacityProviderAssociations`: the capacity-provider configuration OF
    /// one cluster, which Amazon ECS's own `PutClusterCapacityProviders` sets — the call that
    /// "is used to associate a capacity provider with a cluster", without which no
    /// `capacityProviderStrategy` may name one. "Ref returns the cluster name": the resource IS
    /// that cluster's configuration, so its physical id is the cluster
    /// ([`ecs_cluster_capacity_providers`]).
    EcsClusterCapacityProviderAssociations,
    /// `AWS::ECS::TaskSet`: the task set Amazon ECS's own `CreateTaskSet` creates in a service
    /// whose deployment controller is not the ECS scheduler, `UpdateTaskSet` scales and
    /// `DeleteTaskSet` removes. Its identity is the TRIPLE that service addresses one by — the
    /// cluster, the service and the `id` it minted — and `Fn::GetAtt Id` reads that id, which
    /// is the only handle every call on a task set takes ([`ecs_task_sets`]).
    EcsTaskSet,
    /// `AWS::ECS::PrimaryTaskSet`: which of a service's task sets "is the primary task set",
    /// the one serving production traffic, which Amazon ECS's own
    /// `UpdateServicePrimaryTaskSet` decides. There is one per service, so the resource's
    /// identity is the cluster-and-service pair it is the primary task set OF
    /// ([`ecs_task_sets`]).
    EcsPrimaryTaskSet,
    SqsQueue,
    /// `AWS::SQS::QueuePolicy` and `AWS::SQS::QueueInlinePolicy`, which are not resources
    /// of Amazon SQS's own: each is the `Policy` ATTRIBUTE of the queues it names, so both
    /// are deployed through `SetQueueAttributes` and differ only in which property carries
    /// the queue.
    SqsQueuePolicy,
    SageMakerModelPackageGroup,
    /// `AWS::SageMaker::Model`: the inference model `CreateModel` registers under the name
    /// the template decided — the name every SageMaker call on a model takes — and
    /// `DeleteModel` removes.
    SageMakerModel,
    /// `AWS::SageMaker::EndpointConfig`: the production variants `CreateEndpointConfig`
    /// registers under the name the template decided — the name an endpoint's own
    /// `EndpointConfigName` takes — and `DeleteEndpointConfig` removes.
    SageMakerEndpointConfig,
    /// `AWS::SageMaker::Endpoint`: the hosted endpoint `CreateEndpoint` provisions from one of
    /// those configurations. That create answers the endpoint `Creating`, in which it serves
    /// no inference, so the resource's creation is held open until SageMaker reports it
    /// `InService`, and its deletion is finished once that service no longer holds it.
    SageMakerEndpoint,
    /// `AWS::SageMaker::NotebookInstance`: the managed Jupyter instance
    /// `CreateNotebookInstance` launches under that name. That create answers the instance
    /// `Pending`, in which it serves no session, so the resource's creation is held open
    /// until SageMaker reports it `InService`, and its deletion stops the instance before
    /// `DeleteNotebookInstance` can take it away.
    SageMakerNotebookInstance,
    /// `AWS::SageMaker::NotebookInstanceLifecycleConfig`: the `OnCreate`/`OnStart` scripts a
    /// notebook instance's `LifecycleConfigName` names, which
    /// `CreateNotebookInstanceLifecycleConfig` creates and
    /// `DeleteNotebookInstanceLifecycleConfig` removes.
    SageMakerNotebookInstanceLifecycleConfig,
    SnsTopic,
    /// `AWS::SNS::TopicPolicy` and `AWS::SNS::TopicInlinePolicy`, which are not resources
    /// of Amazon SNS's own: each is the `Policy` ATTRIBUTE of the topics it names, so both
    /// are deployed through `SetTopicAttributes` and differ only in which property carries
    /// the topic — the queue-policy pair's rule ([`Provisioner::SqsQueuePolicy`]) for the
    /// other service that holds a policy as an attribute.
    SnsTopicPolicy,
    /// `AWS::SNS::Subscription`: a subscriber of a topic, which is a resource of Amazon
    /// SNS's own — `Subscribe` creates it and `Unsubscribe` deletes it — beside the inline
    /// `Subscription` list an `AWS::SNS::Topic` may declare.
    SnsSubscription,
    FisExperimentTemplate,
    WafRegionalIpSet,
    Wafv2WebAcl,
    Wafv2RuleGroup,
    Wafv2IpSet,
    Wafv2RegexPatternSet,
    /// `AWS::WAFv2::WebACLAssociation`: not an entity of AWS WAF's own but the BINDING that
    /// puts one of its web ACLs in front of a protected resource, which `AssociateWebACL`
    /// makes and `DisassociateWebACL` removes.
    Wafv2WebAclAssociation,
    GlueJob,
    /// `AWS::Glue::Trigger`: a trigger of AWS Glue — what STARTS the jobs and crawlers of a
    /// deployment, on a schedule, on a predicate, on an event or on a caller — which
    /// `CreateTrigger` creates under the name the declaration states, `UpdateTrigger`
    /// overwrites the definition of and `DeleteTrigger` deletes.
    GlueTrigger,
    /// `AWS::Glue::Database`: a database of the AWS Glue Data Catalog, which
    /// `CreateDatabase` creates under the name its `DatabaseInput` states and
    /// `DeleteDatabase` deletes.
    GlueDatabase,
    /// `AWS::Glue::Table`: a table IN one of those databases, which `CreateTable` creates
    /// under the name its `TableInput` states and `DeleteTable` deletes — addressed by the
    /// PAIR that service keys a table by, its own name and the database that holds it.
    GlueTable,
    /// `AWS::Glue::Crawler`: a crawler of that same catalog, which `CreateCrawler` creates
    /// under the name the declaration states and `DeleteCrawler` deletes.
    GlueCrawler,
    /// `AWS::Glue::TableOptimizer`: the compaction, retention or orphan-file-deletion
    /// optimizer OF one of those tables, which `CreateTableOptimizer` registers and
    /// `DeleteTableOptimizer` removes — addressed by the four values that service keys an
    /// optimizer by (the catalog, the database, the table and the optimizer's own type),
    /// which the declaration carries and which are this resource's physical id.
    GlueTableOptimizer,
    GrafanaWorkspace,
    GuardDutyDetector,
    /// `AWS::GuardDuty::IPSet`: a trusted IP list of one of those detectors, which
    /// `CreateIPSet` creates in the detector its declaration names and `DeleteIPSet`
    /// deletes. GuardDuty mints the `IpSetId` that type's `Ref` returns.
    GuardDutyIpSet,
    KmsKey,
    KmsAlias,
    MskCluster,
    MskServerlessCluster,
    /// `AWS::MSK::Configuration`: the `server.properties` document a cluster of either kind
    /// can be given, which `CreateConfiguration` creates under the name the declaration
    /// states and `DeleteConfiguration` deletes — addressed by the `Arn` Amazon MSK minted,
    /// which is what this type's `Ref` returns. That create is also what ADMITS the Apache
    /// Kafka versions the declaration names, so a template naming one that service does not
    /// offer fails the resource instead of reporting a configuration no cluster could be
    /// given.
    MskConfiguration,
    MqBroker,
    RdsDbInstance,
    /// `AWS::RDS::DBCluster`: a DB cluster of Amazon RDS, which `CreateDBCluster` creates
    /// under the identifier the stack decided — the identifier this type's `Ref` returns —
    /// and `DeleteDBCluster` deletes by it. The create answers the cluster `creating`, in
    /// which it serves no connection, so the resource's creation is held open until Amazon
    /// RDS reports it `available`; a member `AWS::RDS::DBInstance` of the same stack
    /// resolves its `DBClusterIdentifier` against that service's own clusters, which is why
    /// this type has to be created there for real.
    RdsDbCluster,
    /// `AWS::ElastiCache::CacheCluster`: a cache cluster of Amazon ElastiCache, which
    /// `CreateCacheCluster` creates under the `ClusterName` the declaration states — the name
    /// this type's `Ref` returns — and `DeleteCacheCluster` deletes by it. That create
    /// answers the cluster `creating`, in which it serves nothing and carries no endpoint, so
    /// the resource's creation is held open until ElastiCache reports it `available`.
    ElastiCacheCacheCluster,
    RdsDbSubnetGroup,
    /// `AWS::RDS::OptionGroup`: an option group of Amazon RDS, which `CreateOptionGroup`
    /// creates under the name the stack decided — the name this type's `Ref` returns — and
    /// `DeleteOptionGroup` deletes by it. Amazon RDS resolves a DB instance's
    /// `OptionGroupName` against the groups it HOLDS, and a client's own
    /// `DescribeOptionGroups` reports the group a completed stack declared, so a stack
    /// declaring one has to create it there for real.
    RdsOptionGroup,
    RdsDbParameterGroup,
    RdsDbClusterParameterGroup,
    /// `AWS::RDS::DBProxy`: an RDS proxy, which `CreateDBProxy` creates under the name the
    /// template states and `DeleteDBProxy` deletes. The create answers the proxy
    /// `creating`, so its creation is held open until Amazon RDS reports it `available`.
    RdsDbProxy,
    /// `AWS::RDS::DBProxyTargetGroup`: the databases one of those proxies routes to, which
    /// `RegisterDBProxyTargets` registers in the proxy's own target group and
    /// `DeregisterDBProxyTargets` removes, with the connection pool settings the
    /// declaration asks for applied by `ModifyDBProxyTargetGroup`.
    RdsDbProxyTargetGroup,
    RedshiftCluster,
    /// `AWS::Redshift::ClusterSubnetGroup`: the subnet group a provisioned Redshift cluster
    /// is launched into, which `CreateClusterSubnetGroup` creates under the name
    /// CloudFormation generated for the resource and `DeleteClusterSubnetGroup` deletes.
    ///
    /// Amazon Redshift resolves a cluster's `ClusterSubnetGroupName` against the groups it
    /// HOLDS before it will create a cluster in one (`ClusterSubnetGroupNotFoundFault`), so
    /// a stack declaring both has to create this one for real too — exactly the position
    /// `AWS::RDS::DBSubnetGroup` is in ([`Provisioner::RdsDbSubnetGroup`]).
    RedshiftClusterSubnetGroup,
    /// `AWS::Redshift::ClusterParameterGroup`: the parameter group a provisioned Redshift
    /// cluster's `ClusterParameterGroupName` names, which `CreateClusterParameterGroup`
    /// creates under the name the declaration states — the name this type's `Ref` returns —
    /// `ModifyClusterParameterGroup` writes the declared settings of and
    /// `DeleteClusterParameterGroup` deletes.
    ///
    /// Amazon Redshift resolves a cluster's `ClusterParameterGroupName` against the groups it
    /// HOLDS before it will create a cluster configured from one
    /// (`ClusterParameterGroupNotFoundFault`), so a stack declaring both has to create this
    /// one for real too — exactly the position [`Provisioner::RedshiftClusterSubnetGroup`] is
    /// in.
    RedshiftClusterParameterGroup,
    /// `AWS::Redshift::SnapshotCopyGrant`: the grant that permits Amazon Redshift to encrypt
    /// copied snapshots with a Key Management Service key, which `CreateSnapshotCopyGrant`
    /// creates under the name the declaration states — the name this type's `Ref` returns —
    /// `DescribeSnapshotCopyGrants` reports and `DeleteSnapshotCopyGrant` deletes.
    ///
    /// The grant's whole content is the KEY it covers, which Amazon Redshift resolves against
    /// Key Management Service before it holds one, so a declaration naming a key that service
    /// cannot read is that service's own refusal on this resource.
    RedshiftSnapshotCopyGrant,
    EventsRule,
    /// `AWS::Pipes::Pipe`: a pipe of Amazon EventBridge Pipes, which `CreatePipe` creates
    /// under the name the template states, `UpdatePipe` changes and `DeletePipe` deletes.
    /// That create answers the pipe `CREATING`, in which it moves no record, so the
    /// resource's creation is held open until that service reports it resting in the state
    /// its `DesiredState` asked for.
    PipesPipe,
    SsmParameter,
    /// `AWS::ResourceGroups::Group`: a resource group of Resource Groups, which
    /// `CreateGroup` creates under the name the template states — the name this type's `Ref`
    /// returns and the identifier every Resource Groups call on one takes — `UpdateGroup` and
    /// `UpdateGroupQuery` change and `DeleteGroup` removes. The ARN the type publishes is the
    /// one that service mints, under its own hyphenated `arnNamespace`, so it is read off the
    /// create's own answer.
    ResourceGroupsGroup,
    /// `AWS::ApplicationInsights::Application`: an application of Amazon CloudWatch Application
    /// Insights, which `CreateApplication` creates over the resource group the template names —
    /// the group that service resolves in Resource Groups before it holds anything, and the
    /// identifier every call on an application takes — and `DeleteApplication` removes. The
    /// create answers the application `CREATING` while that service onboards the group's
    /// resources, so its creation is held open until it reports the onboarding finished.
    ///
    /// Its identity is the ARN the type's `Ref` returns, which is a total function of the
    /// Region, the account and that group, so the naming rules compute it.
    ApplicationInsightsApplication,
    /// `AWS::SSM::Association`: a State Manager association of AWS Systems Manager, which
    /// `CreateAssociation` creates, `UpdateAssociation` changes and `DeleteAssociation`
    /// removes.
    ///
    /// Its identity is the `AssociationId` that service MINTS: the create takes no identifier
    /// for the association at all — the `Name` a declaration states names the DOCUMENT the
    /// association applies — and every call on one takes that id, which is why
    /// `template::identity_is_service_assigned` names the type and the create replaces the
    /// stand-in the naming rules computed with the id Systems Manager answered.
    SsmAssociation,
    /// `AWS::SSM::MaintenanceWindow`: a maintenance window of AWS Systems Manager, which
    /// `CreateMaintenanceWindow` creates and `DeleteMaintenanceWindow` removes, and
    /// `AWS::SSM::MaintenanceWindowTarget`: one target registered with such a window, which
    /// `RegisterTargetWithMaintenanceWindow` registers and
    /// `DeregisterTargetFromMaintenanceWindow` removes.
    ///
    /// Each one's identity is the id that service MINTS — the `mw-…` `WindowId` and the
    /// target's own UUID — which is why `template::identity_is_service_assigned` names both
    /// types and each create replaces the stand-in the naming rules computed
    /// ([`ssm_maintenance_windows`]).
    SsmMaintenanceWindow,
    SsmMaintenanceWindowTarget,
    /// `AWS::SSM::Document`: a document of AWS Systems Manager — a command document, a
    /// runbook, a session document — which `CreateDocument` creates under the name the stack
    /// decided, `UpdateDocument` adds a new version to and `DeleteDocument` removes.
    ///
    /// The name is the document's identity: every Systems Manager call on a document takes it
    /// (`GetDocument$Name`, and `StartAutomationExecution$DocumentName` for a runbook), and it
    /// is what this type's `Ref` returns — so the stand-in the naming rules computed IS the
    /// identifier, and a stack that reports the resource created is one whose document that
    /// service really holds under that name ([`ssm_documents`]).
    SsmDocument,
    StepFunctionsActivity,
    SchedulerScheduleGroup,
    SchedulerSchedule,
    CognitoUserPoolGroup,
    AcmAcmeEndpoint,
    AcmCertificate,
    /// `AWS::ACMPCA::CertificateAuthority`: a private certificate authority of Amazon Web
    /// Services Private CA, whose ARN that service mints — `CreateCertificateAuthority`
    /// creates it and `DeleteCertificateAuthority` deletes it.
    AcmPcaCertificateAuthority,
    /// `AWS::ACMPCA::Certificate`: a certificate one of those CAs ISSUED, which
    /// `IssueCertificate` creates and whose ARN that service mints. Amazon Web Services
    /// Private CA publishes no call that un-issues one, so the type's deployment has
    /// nothing to delete.
    AcmPcaCertificate,
    /// `AWS::ACMPCA::CertificateAuthorityActivation`: not a resource of that service's own
    /// but the ACTIVATION of one of its CAs — `ImportCertificateAuthorityCertificate` puts
    /// the issued certificate into the CA, and `UpdateCertificateAuthority` applies the
    /// status the declaration asks for.
    AcmPcaCertificateAuthorityActivation,
    /// `AWS::ACMPCA::Permission`: not a resource of that service's own either but the GRANT
    /// one of its CAs makes to a service principal — `CreatePermission` makes it and
    /// `DeletePermission` revokes it — identified by the CA-and-principal pair every call of
    /// that service on one is addressed by.
    AcmPcaPermission,
    Route53ResolverEndpoint,
    Route53ResolverRule,
    Route53ResolverRuleAssociation,
    Route53ResolverFirewallDomainList,
    Route53ResolverFirewallRuleGroup,
    Route53ResolverFirewallRuleGroupAssociation,
    Route53HostedZone,
    Route53RecordSet,
    Route53RecordSetGroup,
    Route53HealthCheck,
    Route53CidrCollection,
    Route53DelegationSet,
    Route53TrafficPolicy,
    ElasticBeanstalkApplication,
    ElasticBeanstalkApplicationVersion,
    ElasticBeanstalkEnvironment,
    TransferServer,
    CodeDeployApplication,
    CodeDeployDeploymentGroup,
    CodeDeployDeploymentConfig,
    BatchComputeEnvironment,
    BatchJobQueue,
    BatchServiceEnvironment,
    /// `AWS::Batch::JobDefinition`: the job-definition REVISION `RegisterJobDefinition`
    /// registers and `DeregisterJobDefinition` takes out of `ACTIVE`, whose arms are
    /// [`batch_job_definitions`]'s.
    ///
    /// Its identity is the ARN that service answered — revision and all, which AWS Batch
    /// allocates — so the create replaces the stand-in the naming rules computed
    /// ([`super::template::identity_is_service_assigned`] names the type).
    BatchJobDefinition,
    EksAccessEntry,
    /// `AWS::EKS::Cluster`: the Kubernetes control plane `CreateCluster` creates under the
    /// name the template states and `DeleteCluster` removes. That create is asynchronous,
    /// so the resource's creation is held open until Amazon EKS reports the cluster
    /// `ACTIVE`.
    EksCluster,
    /// `AWS::EKS::Addon`: the Amazon EKS add-on `CreateAddon` installs ON a cluster and
    /// `DeleteAddon` removes. That install is asynchronous, so the resource's creation is
    /// held open until Amazon EKS reports the add-on `ACTIVE`.
    EksAddon,
    /// `AWS::EKS::Nodegroup`: the managed node group `CreateNodegroup` creates on a cluster
    /// and `DeleteNodegroup` removes. That create is asynchronous too, so the resource's
    /// creation is held open until Amazon EKS reports the group `ACTIVE`.
    EksNodegroup,
    /// `AWS::EKS::PodIdentityAssociation`: the IAM role one Kubernetes service account
    /// assumes, which `CreatePodIdentityAssociation` makes and
    /// `DeletePodIdentityAssociation` removes.
    EksPodIdentityAssociation,
    /// `AWS::ApiGateway::DomainName`: a custom domain name of the REST control plane, which
    /// `CreateDomainName` creates under the name the template states and
    /// `DeleteDomainName` removes.
    ApiGatewayDomainName,
    /// `AWS::ApiGateway::BasePathMapping`: the base path of a custom domain name that a
    /// stage of a REST API answers on, which `CreateBasePathMapping` creates under that
    /// domain name and `DeleteBasePathMapping` removes.
    ApiGatewayBasePathMapping,
    ApiGatewayV2Api,
    ApiGatewayV2DomainName,
    ApiGatewayV2ApiMapping,
    ApiGatewayV2Integration,
    ApiGatewayV2Route,
    ApiGatewayV2Stage,
    ApiGatewayV2Deployment,
    ApiGatewayV2Authorizer,
    /// `AWS::ApiGatewayV2::RoutingRule`: a routing rule of a CUSTOM DOMAIN NAME of the same
    /// control plane, which `CreateRoutingRule` creates on the domain its declaration names,
    /// `PutRoutingRule` changes and `DeleteRoutingRule` removes. That service MINTS the
    /// `routingRuleId` and the `routingRuleArn` the type publishes, so both are read off that
    /// create's own answer and the ARN — the type's primary identifier — is the identity a
    /// `Ref` resolves to ([`apigatewayv2_routing_rules`]).
    ApiGatewayV2RoutingRule,
    CodeCommitRepository,
    /// `AWS::CodeArtifact::Domain`: a CodeArtifact domain, which `CreateDomain` creates
    /// under the name the template states and `DeleteDomain` removes.
    CodeArtifactDomain,
    /// `AWS::CodeArtifact::Repository`: a repository IN such a domain, which
    /// `CreateRepository` creates there and `DeleteRepository` removes — with every external
    /// connection its declaration asks for associated, which is a call of its own.
    CodeArtifactRepository,
    SesConfigurationSet,
    /// `AWS::SES::EmailIdentity`: an email identity of Amazon SES — the domain or the address
    /// an account sends from — which `CreateEmailIdentity` creates under the text the
    /// template states and `DeleteEmailIdentity` removes, with the attributes the declaration
    /// asks for written on it.
    SesEmailIdentity,
    /// `AWS::SES::ReceiptRuleSet`: a receipt rule set of Amazon SES, which
    /// `CreateReceiptRuleSet` creates empty and `DeleteReceiptRuleSet` deletes.
    SesReceiptRuleSet,
    /// `AWS::SES::ReceiptRule`: one rule OF such a set, which `CreateReceiptRule` adds to
    /// the set its declaration names and `DeleteReceiptRule` removes from it.
    SesReceiptRule,
    BackupVault,
    BackupPlan,
    BackupSelection,
    /// `AWS::Bedrock::Guardrail`: a guardrail of Amazon Bedrock's own control plane, which
    /// `CreateGuardrail` creates under the name the template states, `UpdateGuardrail`
    /// changes and `DeleteGuardrail` removes. Amazon Bedrock MINTS the `guardrailId` and
    /// the ARN the type publishes, so both are read off that create's own answer and the
    /// ARN is the identity a `Ref` resolves to.
    BedrockGuardrail,
    /// `AWS::Bedrock::KnowledgeBase`: a knowledge base of Amazon Bedrock Agent's build-time
    /// control plane, which `CreateKnowledgeBase` creates under the name the template states,
    /// `UpdateKnowledgeBase` changes and `DeleteKnowledgeBase` removes. That service MINTS
    /// the ten-character `knowledgeBaseId` the type publishes — the handle every
    /// knowledge-base API takes — so it is read off that create's own answer and is the
    /// identity a `Ref` resolves to.
    BedrockKnowledgeBase,
    /// `AWS::Bedrock::DataSource`: the data source attached to one of those knowledge bases,
    /// which `CreateDataSource` creates under the name the template states,
    /// `UpdateDataSource` changes and `DeleteDataSource` removes. That service MINTS the
    /// ten-character `dataSourceId` the type publishes — the handle every data-source and
    /// ingestion API takes beside the knowledge base's own id — so it is read off that
    /// create's own answer, and the identity a `Ref` resolves to is the knowledge base and
    /// that id joined ([`super::template::bedrock_data_source_identity`]).
    BedrockDataSource,
    /// `AWS::Bedrock::Flow`: a FLOW of Amazon Bedrock Agent's build-time control plane, which
    /// `CreateFlow` creates under the name the template states, `UpdateFlow` changes and
    /// `DeleteFlow` removes. That service MINTS the ten-character `id` and the `flow/<id>`
    /// ARN the type publishes — the handles the build-time calls and the runtime `InvokeFlow`
    /// alike address a flow by — so both are read off that create's own answer and the ARN is
    /// the identity a `Ref` resolves to.
    BedrockFlow,
    /// `AWS::Bedrock::Session`: a SESSION of the Bedrock Agent RUNTIME plane, which
    /// `CreateSession` mints, `UpdateSession` changes and `DeleteSession` removes. That
    /// service MINTS the `sessionId` (a uuid) and the `session/<uuid>` ARN the type publishes
    /// — the two forms every call on a session takes as its `sessionIdentifier` — so both are
    /// read off that create's own answer and the ARN is the identity a `Ref` resolves to.
    BedrockSession,
    /// `AWS::BedrockAgentCore::WorkloadIdentity`: a workload identity of the Amazon Bedrock
    /// AgentCore control plane, which `CreateWorkloadIdentity` creates under the name the
    /// template states, `UpdateWorkloadIdentity` changes and `DeleteWorkloadIdentity`
    /// removes. The name IS the identity's handle — every call on one takes it — so it is
    /// the physical id the naming rules already compute for the type.
    BedrockAgentCoreWorkloadIdentity,
    /// `AWS::BedrockAgentCore::Runtime`: an agent runtime of the same control plane, which
    /// `CreateAgentRuntime` creates under the name the template states, `UpdateAgentRuntime`
    /// changes — publishing a new `agentRuntimeVersion` for it — and `DeleteAgentRuntime`
    /// removes. That service MINTS the `agentRuntimeId` the type publishes, the handle every
    /// runtime API and the data plane's own invoke take, so it is read off that create's own
    /// answer and is the identity a `Ref` resolves to.
    BedrockAgentCoreRuntime,
    /// `AWS::BedrockAgentCore::Memory`: a memory of the same control plane, which
    /// `CreateMemory` creates under the name the template states and `DeleteMemory` removes.
    /// That service MINTS the `memoryId` the type publishes — the handle `GetMemory` and
    /// every event and record API take — so it is read off that create's own answer.
    ///
    /// The type has no [`Self::update`] arm: `UpdateMemory` takes its `memoryStrategies` as a
    /// MODIFY document (the strategies to add, to change and to delete) rather than as the
    /// list a declaration states, so a changed declaration of this type is reported as NOT
    /// applied rather than as a success that sent a document the template does not describe.
    BedrockAgentCoreMemory,
    /// `AWS::BedrockAgentCore::Gateway`: a gateway of the same control plane, which
    /// `CreateGateway` creates under the name the template states, `UpdateGateway` changes
    /// and `DeleteGateway` removes. That service MINTS the `gatewayId` the type publishes —
    /// the handle every call on a gateway and on its targets takes — so it is read off that
    /// create's own answer.
    BedrockAgentCoreGateway,
    /// `AWS::BedrockAgentCore::GatewayTarget`: one TARGET of such a gateway, which
    /// `CreateGatewayTarget` creates on the gateway the declaration names,
    /// `UpdateGatewayTarget` changes and `DeleteGatewayTarget` removes. That service MINTS
    /// the `targetId` — ten alphanumerics, which no name CloudFormation decides could satisfy
    /// — and every call on a target binds the gateway BESIDE it, so this resource's physical
    /// id is the two joined ([`super::template::agentcore_gateway_target_identity`]).
    BedrockAgentCoreGatewayTarget,
    /// `AWS::BedrockAgentCore::CodeInterpreter` and `AWS::BedrockAgentCore::Browser`: the two
    /// SANDBOX types of the same control plane, which `CreateCodeInterpreter` /
    /// `CreateBrowser` create under the name the template states and the matching deletes
    /// remove. That service MINTS the `codeInterpreterId` / `browserId` the type publishes
    /// and builds the ARN that carries it, so both are read off the create's own answer. Each
    /// value names which of the two it is, so the one set of arms serves both.
    BedrockAgentCoreSandbox(bedrock_agentcore_gateways::Sandbox),
    /// `AWS::Braket::SpendingLimit`: a spending limit of Amazon Braket's own control plane,
    /// which `CreateSpendingLimit` makes for the quantum device the template names,
    /// `UpdateSpendingLimit` changes and `DeleteSpendingLimit` removes. That service MINTS
    /// the `spendingLimitArn` the type publishes — the handle every spending-limit API takes
    /// — so it is read off that create's own answer and is the identity a `Ref` resolves to.
    BraketSpendingLimit,
    /// `AWS::DirectConnect::DirectConnectGateway`: the "intermediate object that enables
    /// you to connect a set of virtual interfaces and virtual private gateways" AWS Direct
    /// Connect's own `CreateDirectConnectGateway` makes. Its arms are
    /// [`directconnect_resources`]'.
    DirectConnectGateway,
    /// `AWS::DirectConnect::ResiliencyGroup`: the group that service's own
    /// `CreateResiliencyGroup` makes, which "lets you group Direct Connect connections
    /// together and manage them as a single unit to meet a target resiliency model". Its
    /// arms are [`directconnect_resources`]' too.
    DirectConnectResiliencyGroup,
    /// `AWS::IoTCoreDeviceAdvisor::SuiteDefinition`: the test suite AWS IoT Core Device
    /// Advisor's own `CreateSuiteDefinition` records, `UpdateSuiteDefinition` reconfigures and
    /// `DeleteSuiteDefinition` removes. That service DRAWS the `suiteDefinitionId` the type
    /// publishes — the handle every suite-definition API takes — so it is read off that
    /// create's own answer and is the identity a `Ref` resolves to
    /// ([`iot_core_device_advisor_suite_definitions`]).
    IotCoreDeviceAdvisorSuiteDefinition,
    /// `AWS::BCM::Dashboard` and `AWS::BCM::ScheduledReport`: the Billing and Cost
    /// Management Dashboards dashboard `CreateDashboard` makes under the name the template
    /// states, and the scheduled report `CreateScheduledReport` makes ON one of those
    /// dashboards; each `Update…` changes one and each `Delete…` removes it. That service
    /// MINTS the `arn` either type publishes — the handle every call on the resource takes —
    /// so it is read off that create's own answer and is the identity a `Ref` resolves to
    /// ([`bcm_dashboards`]).
    BcmDashboard,
    BcmScheduledReport,
    /// `AWS::ApplicationSignals::ServiceLevelObjective`: the objective CloudWatch Application
    /// Signals' own `CreateServiceLevelObjective` makes under the name the template states,
    /// `UpdateServiceLevelObjective` changes and `DeleteServiceLevelObjective` removes. Its
    /// ARN carries that name, so it is the identity the naming rules already compute for the
    /// type — the `Id` every call on an objective takes ([`application_signals`]).
    ApplicationSignalsServiceLevelObjective,
    /// `AWS::ApplicationSignals::GroupingConfiguration`: the ONE grouping configuration that
    /// service keeps per account and Region, which `PutGroupingConfiguration` is documented to
    /// create or update and `DeleteGroupingConfiguration` removes. The put is therefore this
    /// type's create and its update alike, and neither call takes an identifier: the resource
    /// is the account's own configuration.
    ApplicationSignalsGroupingConfiguration,
    /// `AWS::ApplicationSignals::InstrumentationConfig`: the dynamic instrumentation point
    /// that service's own `CreateInstrumentationConfiguration` records for one code location
    /// and `DeleteInstrumentationConfiguration` removes. A configuration is immutable after
    /// creation, so the type has no update arm; that service MINTS the ARN the type publishes —
    /// around the `LocationHash` it computes — so it is read off that create's own answer and
    /// is the identity a `Ref` resolves to.
    ApplicationSignalsInstrumentationConfig,
    /// `AWS::CleanRoomsML::TrainingDataset`: the metadata record Clean Rooms ML's own
    /// `CreateTrainingDataset` makes for a Glue table, which `DeleteTrainingDataset` removes by
    /// the `trainingDatasetArn` that service minted — the ARN this type's `Ref` returns and
    /// this resource's physical id ([`cleanrooms_ml_training_datasets`]).
    CleanRoomsMlTrainingDataset,
    /// `AWS::AmplifyUIBuilder::Component`, `::Form` and `::Theme`: the three user-interface
    /// entities of an Amplify app environment, which Amplify UI Builder's own
    /// `CreateComponent`, `CreateForm` and `CreateTheme` make under the name the template
    /// states, the matching `Update…` changes and the matching `Delete…` removes. That service
    /// MINTS the `id` each type publishes — the `Uuid` every call on the entity is addressed by,
    /// beside the app and the environment its URI carries — so it is read off that create's own
    /// answer and is the identity a `Ref` resolves to. One variant per type, each dispatching to
    /// the arms of [`amplify_ui_builder`] with the entity it names.
    AmplifyUiBuilderComponent,
    AmplifyUiBuilderForm,
    AmplifyUiBuilderTheme,
    /// `AWS::Amplify::App`: the hosting application AWS Amplify's own `CreateApp` makes under
    /// the name the template states, `UpdateApp` changes and `DeleteApp` removes — the parent
    /// every branch, domain and Amplify UI Builder entity of an app hangs off. That service
    /// MINTS the `appId` and the `appArn` the type publishes, so both are read off that
    /// create's own answer and the ARN is the identity a `Ref` resolves to.
    AmplifyApp,
    /// `AWS::Amplify::Branch`: the branch of such an app AWS Amplify's own `CreateBranch`
    /// makes under the name the template states, `UpdateBranch` changes and `DeleteBranch`
    /// removes — the unit that service hosts, which every `GetBranch`, `ListBranches` and
    /// deployment call addresses. That service builds the `branchArn` the type publishes around
    /// the `appId` it minted for the app, so it is read off that create's own answer and is the
    /// identity a `Ref` resolves to.
    AmplifyBranch,
    /// `AWS::MWAA::Environment`: the Apache Airflow environment Amazon Managed Workflows for
    /// Apache Airflow's `CreateEnvironment` creates under the name the template states,
    /// `UpdateEnvironment` reconfigures and `DeleteEnvironment` removes. The name IS the
    /// handle every operation on one takes, so it is the physical id the naming rules already
    /// compute for the type; both of those lifecycle changes are asynchronous, so the
    /// resource's creation and its deletion are each held open on that service's own report
    /// of the environment.
    MwaaEnvironment,
    /// `AWS::DirectoryService::MicrosoftAD` and `AWS::DirectoryService::SimpleAD`: the two
    /// directories of Directory Service's own control plane, which `CreateMicrosoftAD` and
    /// `CreateDirectory` make in the caller's VPC, `EnableSso`/`DisableSso` turn single sign-on
    /// over on and `DeleteDirectory` removes. That service MINTS the `DirectoryId` either type
    /// publishes — the handle every operation on a directory takes — so it is read off that
    /// create's own answer and is the identity a `Ref` resolves to. Both lifecycle changes are
    /// asynchronous, so the resource's creation and its deletion are each held open on that
    /// service's own description of the directory. Which of the two types it is travels with
    /// the variant ([`directory_service_directories::DirectoryKind`]).
    DirectoryServiceDirectory(directory_service_directories::DirectoryKind),
    /// `AWS::XRay::SamplingRule`: a sampling rule of AWS X-Ray, which
    /// `CreateSamplingRule` creates under the rule name its declaration states and
    /// `DeleteSamplingRule` deletes. X-Ray mints the `RuleARN` that type's `Ref`
    /// returns and every call on a rule can address it by.
    XraySamplingRule,
    /// `AWS::XRay::Group`: a trace group of AWS X-Ray, which `CreateGroup` creates under
    /// the group name the declaration states and `DeleteGroup` deletes. X-Ray mints the
    /// `GroupARN` that type's `Ref` returns — "Ref returns the group ARN" — which is also
    /// its one published `Fn::GetAtt` attribute and a handle every call on a group takes.
    XrayGroup,
    /// `AWS::Config::ConfigurationRecorder`: the configuration recorder of AWS Config,
    /// which `PutConfigurationRecorder` creates under the name the template states and
    /// `DeleteConfigurationRecorder` deletes.
    ConfigConfigurationRecorder,
    /// `AWS::Config::DeliveryChannel`: the channel that recorder delivers through, which
    /// `PutDeliveryChannel` creates and `DeleteDeliveryChannel` deletes.
    ConfigDeliveryChannel,
    /// `AWS::Config::ConfigRule`: a Config rule, which `PutConfigRule` creates under the
    /// rule name the template states and `DeleteConfigRule` deletes. Config mints the ARN
    /// and the rule id the type publishes, so both are read off that service's own read of
    /// the rule.
    ConfigConfigRule,
    /// `AWS::MemoryDB::ParameterGroup` and `AWS::MemoryDB::SubnetGroup`: the two
    /// configuration groups an Amazon MemoryDB cluster is assembled from, which that
    /// service's own `CreateParameterGroup`/`CreateSubnetGroup` make under the name the
    /// template states and the matching `Delete…` removes. The name IS the handle every
    /// call on a group takes, so it is the physical id the naming rules already compute for
    /// the type; the ARN each type publishes is the one that service minted, read off its
    /// own answer. Which group it is travels with the variant
    /// ([`memorydb_groups::Group`]).
    MemoryDbGroup(memorydb_groups::Group),
    /// `AWS::Events::EventBus`: an Amazon EventBridge event bus, which `CreateEventBus`
    /// creates under the name the template states and `DeleteEventBus` deletes. The name is
    /// the identifier every EventBridge call on a bus takes, so it is the physical id the
    /// naming rules compute for the type — and what a rule, a policy or a rule target of the
    /// same stack resolves the bus by.
    EventsEventBus,
    /// `AWS::Events::EventBusPolicy`: one statement on an event bus's resource policy, which
    /// `PutPermission` installs under the declaration's `StatementId` and `RemovePermission`
    /// revokes by that same id.
    EventsEventBusPolicy,
    /// `AWS::Rbin::Rule`: the Recycle Bin retention rule `CreateRule` makes for the resource
    /// type the declaration names, `UpdateRule` reconfigures and `DeleteRule` removes. That
    /// service MINTS the rule's identifier and the ARN built around it — the create takes no
    /// identifier at all — so the ARN it answered is the resource's physical id, and the
    /// identifier inside it is the handle every call on a rule takes
    /// ([`rbin_rules`]).
    RbinRule,
    /// `AWS::DLM::LifecyclePolicy`: the Amazon Data Lifecycle Manager lifecycle policy
    /// `CreateLifecyclePolicy` makes out of the configuration the declaration states,
    /// `UpdateLifecyclePolicy` reconfigures and `DeleteLifecyclePolicy` removes. That service
    /// MINTS the policy's id — the create takes no identifier at all — so the `PolicyId` it
    /// answered is the resource's physical id and the handle every call on a policy takes, and
    /// the `PolicyArn` it built around that id is the one attribute the type publishes
    /// ([`dlm_lifecycle_policies`]).
    DlmLifecyclePolicy,
    /// `AWS::NetworkSecurityManager::{Rule,Template,Policy,Scope}`: the rule, the template,
    /// the policy and the scope AWS Network Security Manager's own `CreateRule`,
    /// `CreateTemplate`, `CreatePolicy` and `CreateScope` make, the matching `Update…`
    /// reconfigures and `Delete…` removes. That service MINTS each entity's `…Id` — every
    /// create takes the entity's name and no identifier at all — and the ARN built around it
    /// is the `…Identifier` every later call on the entity takes, so that ARN is the
    /// resource's physical id. Which entity it is travels with the variant
    /// ([`network_security_manager::Entity`]).
    NetworkSecurityManagerEntity(network_security_manager::Entity),
    /// `AWS::CustomerProfiles::Domain`: the customer-data domain Amazon Connect Customer
    /// Profiles' own `CreateDomain` makes under the name the template states, `UpdateDomain`
    /// reconfigures and `DeleteDomain` removes. The name IS the handle every call on a domain
    /// takes — it is the `@httpLabel` of each of those operations — so it is the physical id
    /// the naming rules already compute for the type, and the values the type publishes (the
    /// two instants that service recorded, the status of its rule-based matching configuration
    /// and the usage statistics it counts) are read off its own report of the domain.
    CustomerProfilesDomain,
    /// `AWS::NeptuneGraph::Graph`: the Neptune Analytics graph that service's own
    /// `CreateGraph` makes under the name the template states, `UpdateGraph` reconfigures and
    /// `DeleteGraph` removes. That service MINTS the `id` the type publishes
    /// (`^g-[a-z0-9]{10}$`) — the `graphIdentifier` every call on a graph takes — so it is
    /// read off that create's own answer and is the identity a `Ref` resolves to; both of
    /// those lifecycle changes leave the graph coming up, so the resource's creation and its
    /// deletion are each held open on that service's own report of the graph.
    NeptuneGraphGraph,
    /// `AWS::NeptuneGraph::GraphSnapshot`: the snapshot OF such a graph that service's own
    /// `CreateGraphSnapshot` takes under the name the template states and
    /// `DeleteGraphSnapshot` removes. That service MINTS the `id` the type publishes
    /// (`^gs-[a-z0-9]{10}$`), which is the identity a `Ref` resolves to, and publishes no call
    /// that changes a snapshot that exists — so a changed declaration replaces it.
    NeptuneGraphGraphSnapshot,
    /// `AWS::BackupGateway::Hypervisor`: the on-premises hypervisor Backup Gateway's own
    /// `ImportHypervisorConfiguration` connects to, `UpdateHypervisor` reconfigures and
    /// `DeleteHypervisor` removes. That service MINTS the `HypervisorArn` — the import takes no
    /// identifier at all — so the ARN it answered is the resource's physical id, and it is the
    /// `ServerArn` every later call on a hypervisor takes
    /// ([`backup_gateway_hypervisors`]).
    BackupGatewayHypervisor,
    /// `AWS::MediaConvert::CertificateAssociation`: the trust between an AWS Certificate
    /// Manager certificate and the account's MediaConvert service, which that service's own
    /// `AssociateCertificate` makes and `DisassociateCertificate` takes away. The resource IS
    /// the certificate it trusts — neither call takes an identifier of its own — so the
    /// declared `Arn` is the physical id the naming rules already compute for the type, and
    /// the association is read back through the resources Certificate Manager reports as using
    /// the certificate ([`mediaconvert_certificates`]).
    MediaConvertCertificateAssociation,
    /// `AWS::LakeFormation::Permissions` and `AWS::LakeFormation::PrincipalPermissions`: ONE
    /// grant of Lake Formation permissions, which that service's own `GrantPermissions`
    /// records for the principal and the Data Catalog resource the declaration names and
    /// `RevokePermissions` takes away again. A grant declares no name of its own: what
    /// identifies it is the pair it binds, which is what each type's `Ref` returns
    /// ([`lake_formation_permissions`]). Which of the two types the declaration is travels
    /// with the variant, the spelling of the declaration being all that differs between them
    /// ([`lake_formation_permissions::Grant`]).
    LakeFormationPermissions(lake_formation_permissions::Grant),
    /// `AWS::EC2::ClientVpnEndpoint`: the Client VPN endpoint Amazon EC2's own
    /// `CreateClientVpnEndpoint` makes out of the client address range, the certificates and
    /// the authentication options the declaration states, and `DeleteClientVpnEndpoint`
    /// removes. That service MINTS the `cvpn-endpoint-…` id the type's `Ref` returns — the
    /// create takes no identifier at all — so the id it answered is the resource's physical
    /// id and the handle every later Client VPN call takes ([`client_vpn`]).
    Ec2ClientVpnEndpoint,
    /// `AWS::EC2::ClientVpnAuthorizationRule`: ONE ingress authorization rule of such an
    /// endpoint, which `AuthorizeClientVpnIngress` grants and `RevokeClientVpnIngress`
    /// revokes. A rule is no entity of EC2's own and carries no identifier: what identifies
    /// it is the endpoint it is on and the network it grants, which is what the type's `Ref`
    /// returns. The authorize leaves it `authorizing`, granting nothing yet, so the
    /// resource's creation is held open until that service reports it `active`.
    Ec2ClientVpnAuthorizationRule,
}

/// The provisioner that owns a template resource type, or `None` for a type this emulator
/// leaves declared-only.
pub(super) fn provisioner_for(resource_type: &str) -> Option<Provisioner> {
    match resource_type {
        FIREHOSE_DELIVERY_STREAM => Some(Provisioner::FirehoseDeliveryStream),
        KINESIS_STREAM => Some(Provisioner::KinesisStream),
        CLOUDWATCH_METRIC_STREAM => Some(Provisioner::CloudWatchMetricStream),
        cloudwatch_alarms::CLOUDWATCH_ALARM => Some(Provisioner::CloudWatchAlarm),
        cloudwatch_composite_alarms::CLOUDWATCH_COMPOSITE_ALARM => {
            Some(Provisioner::CloudWatchCompositeAlarm)
        }
        CLOUDWATCH_DASHBOARD => Some(Provisioner::CloudWatchDashboard),
        ECR_PUBLIC_REPOSITORY => Some(Provisioner::EcrPublicRepository),
        ECR_REPOSITORY => Some(Provisioner::EcrRepository),
        EC2_VPC => Some(Provisioner::Ec2Vpc),
        EC2_SUBNET => Some(Provisioner::Ec2Subnet),
        EC2_SECURITY_GROUP => Some(Provisioner::Ec2SecurityGroup),
        EC2_VOLUME => Some(Provisioner::Ec2Volume),
        EC2_LAUNCH_TEMPLATE => Some(Provisioner::Ec2LaunchTemplate),
        AUTOSCALING_GROUP => Some(Provisioner::AutoScalingGroup),
        auto_scaling_policies::AUTOSCALING_SCALING_POLICY => {
            Some(Provisioner::AutoScalingScalingPolicy)
        }
        AOSS_COLLECTION => Some(Provisioner::OpenSearchServerlessCollection),
        AOSS_SECURITY_POLICY => Some(Provisioner::OpenSearchServerlessSecurityPolicy),
        AOSS_VPC_ENDPOINT => Some(Provisioner::OpenSearchServerlessVpcEndpoint),
        OPENSEARCH_DOMAIN => Some(Provisioner::OpenSearchServiceDomain),
        S3_BUCKET => Some(Provisioner::S3Bucket),
        S3_BUCKET_POLICY => Some(Provisioner::S3BucketPolicy),
        S3TABLES_TABLE_BUCKET => Some(Provisioner::S3TablesTableBucket),
        S3TABLES_NAMESPACE => Some(Provisioner::S3TablesNamespace),
        S3VECTORS_VECTOR_BUCKET => Some(Provisioner::S3VectorsVectorBucket),
        S3VECTORS_INDEX => Some(Provisioner::S3VectorsIndex),
        EMR_CLUSTER => Some(Provisioner::EmrCluster),
        emr_resources::EMR_SECURITY_CONFIGURATION => Some(Provisioner::EmrSecurityConfiguration),
        emr_resources::EMR_STUDIO => Some(Provisioner::EmrStudio),
        emr_resources::EMR_STUDIO_SESSION_MAPPING => Some(Provisioner::EmrStudioSessionMapping),
        IAM_ROLE => Some(Provisioner::IamRole),
        IAM_INSTANCE_PROFILE => Some(Provisioner::IamInstanceProfile),
        IAM_USER => Some(Provisioner::IamUser),
        IAM_ACCESS_KEY => Some(Provisioner::IamAccessKey),
        IAM_MANAGED_POLICY => Some(Provisioner::IamManagedPolicy),
        CODEPIPELINE_PIPELINE => Some(Provisioner::CodePipelinePipeline),
        CODEPIPELINE_CUSTOM_ACTION_TYPE => Some(Provisioner::CodePipelineCustomActionType),
        CODEPIPELINE_WEBHOOK => Some(Provisioner::CodePipelineWebhook),
        CODEBUILD_REPORT_GROUP => Some(Provisioner::CodeBuildReportGroup),
        CODEBUILD_PROJECT => Some(Provisioner::CodeBuildProject),
        codebuild_fleets::CODEBUILD_FLEET => Some(Provisioner::CodeBuildFleet),
        ECS_CLUSTER => Some(Provisioner::EcsCluster),
        ECS_SERVICE => Some(Provisioner::EcsService),
        ecs_capacity_providers::ECS_CAPACITY_PROVIDER => Some(Provisioner::EcsCapacityProvider),
        ecs_cluster_capacity_providers::ECS_CLUSTER_CAPACITY_PROVIDER_ASSOCIATIONS => {
            Some(Provisioner::EcsClusterCapacityProviderAssociations)
        }
        ecs_task_sets::ECS_TASK_SET => Some(Provisioner::EcsTaskSet),
        ecs_task_sets::ECS_PRIMARY_TASK_SET => Some(Provisioner::EcsPrimaryTaskSet),
        SQS_QUEUE => Some(Provisioner::SqsQueue),
        SQS_QUEUE_POLICY | SQS_QUEUE_INLINE_POLICY => Some(Provisioner::SqsQueuePolicy),
        SAGEMAKER_MODEL_PACKAGE_GROUP => Some(Provisioner::SageMakerModelPackageGroup),
        SAGEMAKER_MODEL => Some(Provisioner::SageMakerModel),
        SAGEMAKER_ENDPOINT_CONFIG => Some(Provisioner::SageMakerEndpointConfig),
        SAGEMAKER_ENDPOINT => Some(Provisioner::SageMakerEndpoint),
        SAGEMAKER_NOTEBOOK_INSTANCE => Some(Provisioner::SageMakerNotebookInstance),
        SAGEMAKER_NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG => {
            Some(Provisioner::SageMakerNotebookInstanceLifecycleConfig)
        }
        SNS_TOPIC => Some(Provisioner::SnsTopic),
        SNS_TOPIC_POLICY | SNS_TOPIC_INLINE_POLICY => Some(Provisioner::SnsTopicPolicy),
        SNS_SUBSCRIPTION => Some(Provisioner::SnsSubscription),
        FIS_EXPERIMENT_TEMPLATE => Some(Provisioner::FisExperimentTemplate),
        WAF_REGIONAL_IP_SET => Some(Provisioner::WafRegionalIpSet),
        WAFV2_WEB_ACL => Some(Provisioner::Wafv2WebAcl),
        WAFV2_RULE_GROUP => Some(Provisioner::Wafv2RuleGroup),
        WAFV2_IP_SET => Some(Provisioner::Wafv2IpSet),
        WAFV2_REGEX_PATTERN_SET => Some(Provisioner::Wafv2RegexPatternSet),
        WAFV2_WEB_ACL_ASSOCIATION => Some(Provisioner::Wafv2WebAclAssociation),
        GLUE_JOB => Some(Provisioner::GlueJob),
        GLUE_TRIGGER => Some(Provisioner::GlueTrigger),
        GLUE_DATABASE => Some(Provisioner::GlueDatabase),
        GLUE_TABLE => Some(Provisioner::GlueTable),
        GLUE_CRAWLER => Some(Provisioner::GlueCrawler),
        GLUE_TABLE_OPTIMIZER => Some(Provisioner::GlueTableOptimizer),
        GRAFANA_WORKSPACE => Some(Provisioner::GrafanaWorkspace),
        GUARDDUTY_DETECTOR => Some(Provisioner::GuardDutyDetector),
        GUARDDUTY_IP_SET => Some(Provisioner::GuardDutyIpSet),
        KMS_KEY => Some(Provisioner::KmsKey),
        KMS_ALIAS => Some(Provisioner::KmsAlias),
        MSK_CLUSTER => Some(Provisioner::MskCluster),
        MSK_SERVERLESS_CLUSTER => Some(Provisioner::MskServerlessCluster),
        MSK_CONFIGURATION => Some(Provisioner::MskConfiguration),
        MQ_BROKER => Some(Provisioner::MqBroker),
        RDS_DB_INSTANCE => Some(Provisioner::RdsDbInstance),
        RDS_DB_CLUSTER => Some(Provisioner::RdsDbCluster),
        ELASTICACHE_CACHE_CLUSTER => Some(Provisioner::ElastiCacheCacheCluster),
        RDS_DB_SUBNET_GROUP => Some(Provisioner::RdsDbSubnetGroup),
        rds_option_groups::RDS_OPTION_GROUP => Some(Provisioner::RdsOptionGroup),
        RDS_DB_PARAMETER_GROUP => Some(Provisioner::RdsDbParameterGroup),
        RDS_DB_CLUSTER_PARAMETER_GROUP => Some(Provisioner::RdsDbClusterParameterGroup),
        RDS_DB_PROXY => Some(Provisioner::RdsDbProxy),
        RDS_DB_PROXY_TARGET_GROUP => Some(Provisioner::RdsDbProxyTargetGroup),
        REDSHIFT_CLUSTER => Some(Provisioner::RedshiftCluster),
        REDSHIFT_CLUSTER_SUBNET_GROUP => Some(Provisioner::RedshiftClusterSubnetGroup),
        REDSHIFT_CLUSTER_PARAMETER_GROUP => Some(Provisioner::RedshiftClusterParameterGroup),
        REDSHIFT_SNAPSHOT_COPY_GRANT => Some(Provisioner::RedshiftSnapshotCopyGrant),
        EVENTS_RULE => Some(Provisioner::EventsRule),
        EVENTS_EVENT_BUS => Some(Provisioner::EventsEventBus),
        EVENTS_EVENT_BUS_POLICY => Some(Provisioner::EventsEventBusPolicy),
        PIPES_PIPE => Some(Provisioner::PipesPipe),
        RESOURCE_GROUPS_GROUP => Some(Provisioner::ResourceGroupsGroup),
        APPLICATION_INSIGHTS_APPLICATION => Some(Provisioner::ApplicationInsightsApplication),
        SSM_PARAMETER => Some(Provisioner::SsmParameter),
        SSM_ASSOCIATION => Some(Provisioner::SsmAssociation),
        ssm_maintenance_windows::SSM_MAINTENANCE_WINDOW => Some(Provisioner::SsmMaintenanceWindow),
        ssm_maintenance_windows::SSM_MAINTENANCE_WINDOW_TARGET => {
            Some(Provisioner::SsmMaintenanceWindowTarget)
        }
        ssm_documents::SSM_DOCUMENT => Some(Provisioner::SsmDocument),
        STEP_FUNCTIONS_ACTIVITY => Some(Provisioner::StepFunctionsActivity),
        SCHEDULER_SCHEDULE_GROUP => Some(Provisioner::SchedulerScheduleGroup),
        SCHEDULER_SCHEDULE => Some(Provisioner::SchedulerSchedule),
        COGNITO_USER_POOL_GROUP => Some(Provisioner::CognitoUserPoolGroup),
        ACM_ACME_ENDPOINT => Some(Provisioner::AcmAcmeEndpoint),
        ACM_CERTIFICATE => Some(Provisioner::AcmCertificate),
        ACMPCA_CERTIFICATE_AUTHORITY => Some(Provisioner::AcmPcaCertificateAuthority),
        ACMPCA_CERTIFICATE => Some(Provisioner::AcmPcaCertificate),
        ACMPCA_CERTIFICATE_AUTHORITY_ACTIVATION => {
            Some(Provisioner::AcmPcaCertificateAuthorityActivation)
        }
        ACMPCA_PERMISSION => Some(Provisioner::AcmPcaPermission),
        ROUTE53RESOLVER_ENDPOINT => Some(Provisioner::Route53ResolverEndpoint),
        ROUTE53RESOLVER_RULE => Some(Provisioner::Route53ResolverRule),
        ROUTE53RESOLVER_RULE_ASSOCIATION => Some(Provisioner::Route53ResolverRuleAssociation),
        ROUTE53RESOLVER_FIREWALL_DOMAIN_LIST => {
            Some(Provisioner::Route53ResolverFirewallDomainList)
        }
        ROUTE53RESOLVER_FIREWALL_RULE_GROUP => Some(Provisioner::Route53ResolverFirewallRuleGroup),
        ROUTE53RESOLVER_FIREWALL_RULE_GROUP_ASSOCIATION => {
            Some(Provisioner::Route53ResolverFirewallRuleGroupAssociation)
        }
        ROUTE53_HOSTED_ZONE => Some(Provisioner::Route53HostedZone),
        ROUTE53_RECORD_SET => Some(Provisioner::Route53RecordSet),
        ROUTE53_RECORD_SET_GROUP => Some(Provisioner::Route53RecordSetGroup),
        ROUTE53_HEALTH_CHECK => Some(Provisioner::Route53HealthCheck),
        ROUTE53_CIDR_COLLECTION => Some(Provisioner::Route53CidrCollection),
        ROUTE53_DELEGATION_SET => Some(Provisioner::Route53DelegationSet),
        ROUTE53_TRAFFIC_POLICY => Some(Provisioner::Route53TrafficPolicy),
        ELASTIC_BEANSTALK_APPLICATION => Some(Provisioner::ElasticBeanstalkApplication),
        ELASTIC_BEANSTALK_APPLICATION_VERSION => {
            Some(Provisioner::ElasticBeanstalkApplicationVersion)
        }
        ELASTIC_BEANSTALK_ENVIRONMENT => Some(Provisioner::ElasticBeanstalkEnvironment),
        TRANSFER_SERVER => Some(Provisioner::TransferServer),
        CODEDEPLOY_APPLICATION => Some(Provisioner::CodeDeployApplication),
        CODEDEPLOY_DEPLOYMENT_GROUP => Some(Provisioner::CodeDeployDeploymentGroup),
        CODEDEPLOY_DEPLOYMENT_CONFIG => Some(Provisioner::CodeDeployDeploymentConfig),
        BATCH_COMPUTE_ENVIRONMENT => Some(Provisioner::BatchComputeEnvironment),
        BATCH_JOB_QUEUE => Some(Provisioner::BatchJobQueue),
        BATCH_JOB_DEFINITION => Some(Provisioner::BatchJobDefinition),
        BATCH_SERVICE_ENVIRONMENT => Some(Provisioner::BatchServiceEnvironment),
        EKS_ACCESS_ENTRY => Some(Provisioner::EksAccessEntry),
        EKS_CLUSTER => Some(Provisioner::EksCluster),
        EKS_ADDON => Some(Provisioner::EksAddon),
        EKS_NODEGROUP => Some(Provisioner::EksNodegroup),
        EKS_POD_IDENTITY_ASSOCIATION => Some(Provisioner::EksPodIdentityAssociation),
        APIGATEWAY_DOMAIN_NAME => Some(Provisioner::ApiGatewayDomainName),
        APIGATEWAY_BASE_PATH_MAPPING => Some(Provisioner::ApiGatewayBasePathMapping),
        APIGATEWAYV2_API => Some(Provisioner::ApiGatewayV2Api),
        APIGATEWAYV2_DOMAIN_NAME => Some(Provisioner::ApiGatewayV2DomainName),
        APIGATEWAYV2_API_MAPPING => Some(Provisioner::ApiGatewayV2ApiMapping),
        APIGATEWAYV2_INTEGRATION => Some(Provisioner::ApiGatewayV2Integration),
        APIGATEWAYV2_ROUTE => Some(Provisioner::ApiGatewayV2Route),
        APIGATEWAYV2_STAGE => Some(Provisioner::ApiGatewayV2Stage),
        APIGATEWAYV2_DEPLOYMENT => Some(Provisioner::ApiGatewayV2Deployment),
        APIGATEWAYV2_AUTHORIZER => Some(Provisioner::ApiGatewayV2Authorizer),
        APIGATEWAYV2_ROUTING_RULE => Some(Provisioner::ApiGatewayV2RoutingRule),
        CODECOMMIT_REPOSITORY => Some(Provisioner::CodeCommitRepository),
        CODEARTIFACT_DOMAIN => Some(Provisioner::CodeArtifactDomain),
        CODEARTIFACT_REPOSITORY => Some(Provisioner::CodeArtifactRepository),
        SES_CONFIGURATION_SET => Some(Provisioner::SesConfigurationSet),
        SES_EMAIL_IDENTITY => Some(Provisioner::SesEmailIdentity),
        SES_RECEIPT_RULE_SET => Some(Provisioner::SesReceiptRuleSet),
        SES_RECEIPT_RULE => Some(Provisioner::SesReceiptRule),
        BACKUP_VAULT => Some(Provisioner::BackupVault),
        BACKUP_PLAN => Some(Provisioner::BackupPlan),
        BACKUP_SELECTION => Some(Provisioner::BackupSelection),
        BEDROCK_GUARDRAIL => Some(Provisioner::BedrockGuardrail),
        BEDROCK_KNOWLEDGE_BASE => Some(Provisioner::BedrockKnowledgeBase),
        BEDROCK_DATA_SOURCE => Some(Provisioner::BedrockDataSource),
        BEDROCK_FLOW => Some(Provisioner::BedrockFlow),
        BEDROCK_SESSION => Some(Provisioner::BedrockSession),
        BEDROCK_AGENTCORE_WORKLOAD_IDENTITY => {
            Some(Provisioner::BedrockAgentCoreWorkloadIdentity)
        }
        BRAKET_SPENDING_LIMIT => Some(Provisioner::BraketSpendingLimit),
        DIRECT_CONNECT_GATEWAY => Some(Provisioner::DirectConnectGateway),
        DIRECT_CONNECT_RESILIENCY_GROUP => Some(Provisioner::DirectConnectResiliencyGroup),
        IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION => {
            Some(Provisioner::IotCoreDeviceAdvisorSuiteDefinition)
        }
        BCM_DASHBOARD => Some(Provisioner::BcmDashboard),
        BCM_SCHEDULED_REPORT => Some(Provisioner::BcmScheduledReport),
        APPLICATION_SIGNALS_SERVICE_LEVEL_OBJECTIVE => {
            Some(Provisioner::ApplicationSignalsServiceLevelObjective)
        }
        APPLICATION_SIGNALS_GROUPING_CONFIGURATION => {
            Some(Provisioner::ApplicationSignalsGroupingConfiguration)
        }
        APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG => {
            Some(Provisioner::ApplicationSignalsInstrumentationConfig)
        }
        CLEANROOMSML_TRAINING_DATASET => Some(Provisioner::CleanRoomsMlTrainingDataset),
        AMPLIFYUIBUILDER_COMPONENT => Some(Provisioner::AmplifyUiBuilderComponent),
        AMPLIFYUIBUILDER_FORM => Some(Provisioner::AmplifyUiBuilderForm),
        AMPLIFYUIBUILDER_THEME => Some(Provisioner::AmplifyUiBuilderTheme),
        AMPLIFY_APP => Some(Provisioner::AmplifyApp),
        AMPLIFY_BRANCH => Some(Provisioner::AmplifyBranch),
        MWAA_ENVIRONMENT => Some(Provisioner::MwaaEnvironment),
        DIRECTORY_SERVICE_MICROSOFT_AD => Some(Provisioner::DirectoryServiceDirectory(
            directory_service_directories::DirectoryKind::MicrosoftAd,
        )),
        DIRECTORY_SERVICE_SIMPLE_AD => Some(Provisioner::DirectoryServiceDirectory(
            directory_service_directories::DirectoryKind::SimpleAd,
        )),

        XRAY_SAMPLING_RULE => Some(Provisioner::XraySamplingRule),
        XRAY_GROUP => Some(Provisioner::XrayGroup),
        CONFIG_CONFIGURATION_RECORDER => Some(Provisioner::ConfigConfigurationRecorder),
        CONFIG_DELIVERY_CHANNEL => Some(Provisioner::ConfigDeliveryChannel),
        CONFIG_CONFIG_RULE => Some(Provisioner::ConfigConfigRule),
        // Each MemoryDB group names which of the two it is, so the one pair of arms serves
        // both types; the catalogue still lists every provisioned type here.
        MEMORYDB_PARAMETER_GROUP => {
            Some(Provisioner::MemoryDbGroup(memorydb_groups::Group::Parameter))
        }
        MEMORYDB_SUBNET_GROUP => Some(Provisioner::MemoryDbGroup(memorydb_groups::Group::Subnet)),
        CUSTOMER_PROFILES_DOMAIN => Some(Provisioner::CustomerProfilesDomain),
        NEPTUNE_GRAPH_GRAPH => Some(Provisioner::NeptuneGraphGraph),
        NEPTUNE_GRAPH_GRAPH_SNAPSHOT => Some(Provisioner::NeptuneGraphGraphSnapshot),
        RBIN_RULE => Some(Provisioner::RbinRule),
        DLM_LIFECYCLE_POLICY => Some(Provisioner::DlmLifecyclePolicy),
        // Each AWS Network Security Manager entity names which of the four it is, so the one
        // set of arms serves every type; the catalogue still lists every provisioned type here.
        NETWORK_SECURITY_MANAGER_RULE => Some(Provisioner::NetworkSecurityManagerEntity(
            network_security_manager::Entity::Rule,
        )),
        NETWORK_SECURITY_MANAGER_TEMPLATE => Some(Provisioner::NetworkSecurityManagerEntity(
            network_security_manager::Entity::Template,
        )),
        NETWORK_SECURITY_MANAGER_POLICY => Some(Provisioner::NetworkSecurityManagerEntity(
            network_security_manager::Entity::Policy,
        )),
        NETWORK_SECURITY_MANAGER_SCOPE => Some(Provisioner::NetworkSecurityManagerEntity(
            network_security_manager::Entity::Scope,
        )),
        BACKUP_GATEWAY_HYPERVISOR => Some(Provisioner::BackupGatewayHypervisor),
        MEDIACONVERT_CERTIFICATE_ASSOCIATION => {
            Some(Provisioner::MediaConvertCertificateAssociation)
        }
        // Each Lake Formation grant names which of the two types it is, so the one set of
        // arms serves both; the catalogue still lists every provisioned type here.
        LAKEFORMATION_PERMISSIONS => Some(Provisioner::LakeFormationPermissions(
            lake_formation_permissions::Grant::Legacy,
        )),
        LAKEFORMATION_PRINCIPAL_PERMISSIONS => Some(Provisioner::LakeFormationPermissions(
            lake_formation_permissions::Grant::Principal,
        )),
        EC2_CLIENT_VPN_ENDPOINT => Some(Provisioner::Ec2ClientVpnEndpoint),
        EC2_CLIENT_VPN_AUTHORIZATION_RULE => Some(Provisioner::Ec2ClientVpnAuthorizationRule),
        // The Amazon Bedrock AgentCore family's entries are catalogued beside that family's
        // own type names and arms ([`type_registry`]), so adding one of its types is a change
        // to that file alone; every other type answers `None` and is left declared-only.
        _ => agentcore_provisioner_for(resource_type),
    }
}
impl Provisioner {
    /// Create one resource in the service that owns it, and report back what that service
    /// decided about it ([`Created`]): the attributes it reported (an ARN it minted, say),
    /// which replace the ones CloudFormation's naming rules computed, and — for a type
    /// whose identity is that service's
    /// ([`super::template::identity_is_service_assigned`]) — the physical id it assigned.
    /// `Err` is that service's own refusal.
    ///
    /// `propagated` are the tags CloudFormation gives the resource — the stack's own plus
    /// the three that name the stack and this resource's place in it
    /// ([`super::provisioning::StackProvenance::tags_for`]) — and every arm whose type is
    /// taggable applies them ([`tags_of`]) beside the ones the declaration itself asks for,
    /// so the owning service's tag read on the handle the stack publishes answers with
    /// them. The one arm that applies none is `AWS::WAFRegional::IPSet`, whose type
    /// publishes no `Tags` property at all: CloudFormation propagates no tags to a resource
    /// type that is not taggable.
    pub(super) async fn create(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        propagated: &[(String, String)],
    ) -> Result<Created, Refusal> {
        let propagated = &self.applicable_tags(propagated);
        match self {
            Self::FirehoseDeliveryStream => {
                create_delivery_stream(ctx, region, resource, propagated).await
            }
            Self::KinesisStream => {
                create_kinesis_stream(ctx, region, resource, propagated).await
            }
            Self::CloudWatchMetricStream => {
                put_metric_stream(ctx, region, resource, propagated).await
            }
            Self::CloudWatchDashboard => {
                put_dashboard(ctx, region, resource, propagated).await
            }
            // `AWS::CloudWatch::Alarm` publishes no `Tags` property at all — CloudWatch holds
            // an alarm's tags through its own tagging calls and `PutMetricAlarm` takes a
            // `Tags` member the type does not declare — so no tags are propagated to one.
            Self::CloudWatchAlarm => put_metric_alarm(ctx, region, resource, propagated).await,
            // A composite alarm's own `Tags` member is one `PutCompositeAlarm` takes, so the
            // tags CloudFormation propagates travel on the create like the declaration's own.
            Self::CloudWatchCompositeAlarm => {
                put_composite_alarm(ctx, region, resource, propagated).await
            }
            Self::EmrCluster => run_job_flow(ctx, region, resource, propagated).await,
            // `AWS::EMR::SecurityConfiguration` publishes no `Tags` property and
            // `CreateSecurityConfiguration` takes none, so no tags are propagated to one.
            Self::EmrSecurityConfiguration => {
                create_emr_security_configuration(ctx, region, resource).await
            }
            // `AWS::EMR::Studio` publishes `Tags`, and `CreateStudio` takes a `TagList` of
            // its own, so a Studio a stack creates carries the tags CloudFormation gives it
            // beside the ones the declaration asks for.
            Self::EmrStudio => create_emr_studio(ctx, region, resource, propagated).await,
            // `AWS::EMR::StudioSessionMapping` publishes no `Tags` property and
            // `CreateStudioSessionMapping` takes none: a mapping is no taggable resource, so
            // no tags are propagated to one.
            Self::EmrStudioSessionMapping => {
                create_emr_studio_session_mapping(ctx, region, resource).await
            }
            Self::EcrPublicRepository => {
                create_public_repository(ctx, region, resource, propagated).await
            }
            Self::EcrRepository => create_repository(ctx, region, resource, propagated).await,
            Self::Ec2Vpc => create_ec2_vpc(ctx, region, resource, propagated).await,
            Self::Ec2Subnet => create_ec2_subnet(ctx, region, resource, propagated).await,
            Self::Ec2SecurityGroup => {
                create_ec2_security_group(ctx, region, resource, propagated).await
            }
            Self::Ec2Volume => create_ec2_volume(ctx, region, resource, propagated).await,
            Self::Ec2LaunchTemplate => {
                create_ec2_launch_template(ctx, region, resource, propagated).await
            }
            Self::AutoScalingGroup => {
                create_auto_scaling_group(ctx, region, resource, propagated).await
            }
            // `AWS::AutoScaling::ScalingPolicy` publishes no `Tags` property and
            // `PutScalingPolicy` takes none, so no tag is propagated to a scaling policy.
            Self::AutoScalingScalingPolicy => put_scaling_policy(ctx, region, resource).await,
            // Boxed, as the deeper arms of this match are: a bucket's create sends the
            // declared access controls and notification configuration after the create,
            // so this arm's own future is one of the largest here and a stack it shares
            // with the provisioning walk above it is the scarce resource.
            Self::S3Bucket => Box::pin(create_bucket(ctx, region, resource, propagated)).await,
            // `AWS::S3::BucketPolicy` publishes `Bucket` and `PolicyDocument` and no
            // `Tags`: a bucket's policy is no taggable resource, so no tags are
            // propagated to one.
            Self::S3BucketPolicy => put_bucket_policy(ctx, region, resource).await,
            // `AWS::S3Tables::TableBucket` publishes `Tags`, and `CreateTableBucket` takes
            // a tag MAP of its own, so a table bucket a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for.
            Self::S3TablesTableBucket => {
                create_table_bucket(ctx, region, resource, propagated).await
            }
            // `AWS::S3Tables::Namespace` publishes no `Tags` property and `CreateNamespace`
            // takes none, so no tags are propagated to a namespace.
            Self::S3TablesNamespace => create_namespace(ctx, region, resource).await,
            Self::S3VectorsVectorBucket => {
                create_vector_bucket(ctx, region, resource, propagated).await
            }
            Self::S3VectorsIndex => create_index(ctx, region, resource, propagated).await,
            Self::IamRole => create_role(ctx, region, resource, propagated).await,
            // `AWS::IAM::InstanceProfile` publishes no `Tags` property and
            // `CreateInstanceProfile` takes none from this arm, so no tags are propagated to
            // a profile.
            Self::IamInstanceProfile => create_instance_profile(ctx, region, resource).await,
            Self::IamUser => create_iam_user(ctx, region, resource, propagated).await,
            // `AWS::IAM::AccessKey` publishes no `Tags` property and `CreateAccessKey` takes
            // none, so no tags are propagated to an access key.
            Self::IamAccessKey => create_iam_access_key(ctx, region, resource).await,
            // `AWS::IAM::ManagedPolicy` publishes no `Tags` property and `CreatePolicy`
            // takes none from this arm, so no tags are propagated to a policy.
            Self::IamManagedPolicy => create_managed_policy(ctx, region, resource).await,
            Self::CodePipelinePipeline => {
                create_pipeline(ctx, region, resource, propagated).await
            }
            Self::CodePipelineCustomActionType => {
                create_custom_action_type(ctx, region, resource, propagated).await
            }
            // `AWS::CodePipeline::Webhook` publishes `Tags`, and `PutWebhook` takes a `tags`
            // list of its own, so a webhook a stack creates carries the tags CloudFormation
            // gives it beside the ones the declaration asks for.
            Self::CodePipelineWebhook => put_webhook(ctx, region, resource, propagated).await,
            Self::EcsCluster => create_cluster(ctx, region, resource, propagated).await,
            // `AWS::ECS::Service` publishes `Tags`, which `CreateService` takes as its own
            // `tags` list, so a service a stack creates carries the tags CloudFormation gives
            // it beside the ones the declaration asks for.
            // BOXED, for the reason the other heavy arms of this match are: the call's future
            // is the owning service's whole handler, which schedules the service's tasks.
            Self::EcsService => {
                Box::pin(create_ecs_service(ctx, region, resource, propagated)).await
            }
            // `AWS::ECS::CapacityProvider` publishes `Tags`, which `CreateCapacityProvider`
            // takes as its own `tags` list, so a provider a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for.
            Self::EcsCapacityProvider => {
                create_ecs_capacity_provider(ctx, region, resource, propagated).await
            }
            // `AWS::ECS::ClusterCapacityProviderAssociations` publishes no `Tags` property at
            // all — the configuration is the CLUSTER's, and the cluster carries the tags — so
            // no tags are propagated to one. The create IS the put: that one call sets the
            // whole configuration of the cluster it addresses.
            Self::EcsClusterCapacityProviderAssociations => {
                put_cluster_capacity_providers(ctx, region, resource, &resource.properties).await
            }
            // `CreateTaskSet` takes a `tags` list of its own, so a task set a stack creates
            // carries the tags CloudFormation gives it beside the ones the declaration asks
            // for.
            // BOXED, for the reason the other heavy arms of this match are: the call's future
            // is the owning service's whole handler, which places the task set's tasks.
            Self::EcsTaskSet => {
                Box::pin(create_ecs_task_set(ctx, region, resource, propagated)).await
            }
            // `AWS::ECS::PrimaryTaskSet` publishes no `Tags` property and
            // `UpdateServicePrimaryTaskSet` takes none: the resource is which task set of a
            // service serves production traffic, so no tags are propagated to one.
            Self::EcsPrimaryTaskSet => {
                update_service_primary_task_set(ctx, region, resource, &resource.properties).await
            }
            Self::SqsQueue => create_queue(ctx, region, resource, propagated).await,
            // Neither policy type publishes a `Tags` property — a policy is an attribute of
            // the queues it names, and those carry the tags — so no tags are propagated to
            // one.
            Self::SqsQueuePolicy => put_queue_policy(ctx, region, resource).await,
            Self::SageMakerModelPackageGroup => {
                create_model_package_group(ctx, region, resource, propagated).await
            }
            // Each of the three creates takes a `Tags` list, so a model, a notebook instance
            // and a lifecycle configuration a stack creates each carry the tags
            // CloudFormation gives them beside the ones their declaration asks for.
            Self::SageMakerModel => {
                create_sagemaker_model(ctx, region, resource, propagated).await
            }
            // Both endpoint types take a `Tags` list of their own, so a configuration and an
            // endpoint a stack creates each carry the tags CloudFormation gives them beside
            // the ones their declaration asks for.
            Self::SageMakerEndpointConfig => {
                create_sagemaker_endpoint_config(ctx, region, resource, propagated).await
            }
            Self::SageMakerEndpoint => {
                create_sagemaker_endpoint(ctx, region, resource, propagated).await
            }
            Self::SageMakerNotebookInstance => {
                create_sagemaker_notebook_instance(ctx, region, resource, propagated).await
            }
            Self::SageMakerNotebookInstanceLifecycleConfig => {
                create_sagemaker_notebook_instance_lifecycle_config(
                    ctx, region, resource, propagated,
                )
                .await
            }
            Self::SnsTopic => create_topic(ctx, region, resource, propagated).await,
            // Neither topic-policy type publishes a `Tags` property — a policy is an
            // attribute of the topics it names, and those carry the tags — so no tags are
            // propagated to one.
            Self::SnsTopicPolicy => put_topic_policy(ctx, region, resource).await,
            // A subscription publishes no `Tags` property — SNS holds tags on topics alone —
            // so no tags are propagated to one.
            Self::SnsSubscription => subscribe(ctx, region, resource).await,
            Self::FisExperimentTemplate => {
                create_experiment_template(ctx, region, resource, propagated).await
            }
            // `AWS::WAFRegional::IPSet` publishes `Name` and `IPSetDescriptors` and no
            // `Tags`: the type is not taggable, so no tags are propagated to it.
            Self::WafRegionalIpSet => create_ip_set(ctx, region, resource).await,
            // Each WAFv2 type publishes a `Tags` property and its create takes that list as
            // its own member, so a stack's resources in WAF carry the tags CloudFormation
            // gives them. Every arm is the same call sequence under the type's own
            // operation names ([`Wafv2Calls`]).
            Self::Wafv2WebAcl => {
                create_wafv2_entity(ctx, region, resource, propagated, WAFV2_WEB_ACL_CALLS).await
            }
            Self::Wafv2RuleGroup => {
                create_wafv2_entity(ctx, region, resource, propagated, WAFV2_RULE_GROUP_CALLS)
                    .await
            }
            Self::Wafv2IpSet => {
                create_wafv2_entity(ctx, region, resource, propagated, WAFV2_IP_SET_CALLS).await
            }
            Self::Wafv2RegexPatternSet => {
                create_wafv2_entity(
                    ctx,
                    region,
                    resource,
                    propagated,
                    WAFV2_REGEX_PATTERN_SET_CALLS,
                )
                .await
            }
            // An association publishes no `Tags` property and `AssociateWebACL` takes none:
            // the binding is not a taggable resource — AWS WAF holds tags on its own
            // entities — so no tags are propagated to one.
            Self::Wafv2WebAclAssociation => associate_web_acl(ctx, region, resource).await,
            // An association publishes no `Tags` property and `AssociateCertificate` takes
            // none: the trust is not a taggable resource of MediaConvert's — that service
            // holds tags on its queues, presets, job templates and jobs — so no tags are
            // propagated to one.
            Self::MediaConvertCertificateAssociation => {
                associate_mediaconvert_certificate(ctx, region, resource).await
            }
            // Neither Lake Formation permission type publishes a `Tags` property and neither
            // `GrantPermissions` nor `RevokePermissions` takes one: a grant is not a taggable
            // resource — that service holds tags on the LF-tags its own tagging calls name —
            // so no tags are propagated to one, and the declared document IS the grant
            // ([`lake_formation_permissions`]).
            Self::LakeFormationPermissions(grant) => {
                grant_lake_formation_permissions(ctx, region, grant, resource).await
            }
            // A Client VPN endpoint's properties ARE `CreateClientVpnEndpoint`'s own members,
            // each carried onto the call under the member of the same name in the spelling
            // Amazon EC2's query protocol gives it; the tags CloudFormation propagates ride
            // on that call's own `TagSpecifications` member, which is where this type states
            // its own ([`client_vpn`]).
            Self::Ec2ClientVpnEndpoint => {
                create_client_vpn_endpoint(ctx, region, resource, propagated).await
            }
            // An authorization rule publishes no `Tags` property at all — it is no entity of
            // Amazon EC2's own but a grant ON an endpoint, and `AuthorizeClientVpnIngress`
            // takes no tags — so no tags are propagated to one and the declaration IS the
            // request.
            Self::Ec2ClientVpnAuthorizationRule => {
                authorize_client_vpn_ingress(ctx, region, resource).await
            }
            // An `AWS::Glue::Job`'s properties ARE `CreateJob`'s own members, so the
            // declared document is the request, with the one member CloudFormation decides
            // (the job's name) set on the way out and the tag MAP that call takes merged
            // from the declaration's own and the stack's.
            Self::GlueJob => create_glue_job(ctx, region, resource, propagated).await,
            // A trigger IS taggable: its `Tags` property is the tag MAP `CreateTrigger`'s own
            // `Tags` member takes, so it carries the stack's tags beside its own, and every
            // other property of the type is a member of that same call.
            Self::GlueTrigger => create_glue_trigger(ctx, region, resource, propagated).await,
            // The two catalog types publish no `Tags` property at all — neither is a
            // taggable resource, AWS Glue holding tags on the catalog entities its own
            // tagging calls name — so no tags are propagated to one, and the declared
            // document IS the create's request with the name CloudFormation decided set
            // inside the input structure that call takes it in.
            Self::GlueDatabase => create_glue_database(ctx, region, resource).await,
            Self::GlueTable => create_glue_table(ctx, region, resource).await,
            // A crawler IS taggable: its `Tags` property is the tag MAP `CreateCrawler`'s
            // own `Tags` member takes, so it carries the stack's tags beside its own.
            Self::GlueCrawler => create_glue_crawler(ctx, region, resource, propagated).await,
            // An optimizer publishes no `Tags` property either: it is a resource OF a table
            // rather than one AWS Glue holds tags on, so no tags are propagated to one and
            // the declared document is the create's request with its configuration re-cased
            // to the members that call takes.
            Self::GlueTableOptimizer => {
                create_glue_table_optimizer(ctx, region, resource).await
            }
            // `AWS::Grafana::Workspace` publishes no `Tags` property either, so the type
            // is not taggable and no stack tags are propagated to it.
            Self::GrafanaWorkspace => create_workspace(ctx, region, resource).await,
            // `AWS::OpenSearchServerless::SecurityPolicy` publishes no `Tags` property and
            // `CreateSecurityPolicy` takes none, so a policy carries no propagated tag.
            Self::OpenSearchServerlessSecurityPolicy => {
                create_security_policy(ctx, region, resource).await
            }
            Self::OpenSearchServerlessCollection => {
                create_collection(ctx, region, resource, propagated).await
            }
            // `AWS::OpenSearchServerless::VpcEndpoint` publishes no `Tags` property and
            // `CreateVpcEndpoint` takes none, so an endpoint carries no propagated tag.
            Self::OpenSearchServerlessVpcEndpoint => {
                create_vpc_endpoint(ctx, region, resource).await
            }
            Self::OpenSearchServiceDomain => {
                create_domain(ctx, region, resource, propagated).await
            }
            Self::GuardDutyDetector => {
                create_detector(ctx, region, resource, propagated).await
            }
            Self::GuardDutyIpSet => {
                create_guardduty_ip_set(ctx, region, resource, propagated).await
            }
            Self::KmsKey => create_kms_key(ctx, region, resource, propagated).await,
            // `AWS::KMS::Alias` publishes `AliasName` and `TargetKeyId` and no `Tags`: an
            // alias is not a taggable resource — KMS holds tags on KEYS — so no tags are
            // propagated to one.
            Self::KmsAlias => create_kms_alias(ctx, region, resource).await,
            Self::MskCluster => create_msk_cluster(ctx, region, resource, propagated).await,
            Self::MskServerlessCluster => {
                create_msk_serverless_cluster(ctx, region, resource, propagated).await
            }
            // `AWS::MSK::Configuration` publishes no `Tags` property — a configuration is not
            // a taggable Amazon MSK resource — so no tags are propagated to one.
            Self::MskConfiguration => create_msk_configuration(ctx, region, resource).await,
            Self::MqBroker => create_mq_broker(ctx, region, resource, propagated).await,
            Self::RdsDbInstance => {
                create_db_instance(ctx, region, resource, propagated).await
            }
            // `AWS::RDS::DBCluster` publishes `Tags` as the `{Key, Value}` list
            // `CreateDBCluster`'s own `Tags` member takes, so a cluster a stack creates
            // carries the tags CloudFormation gives it beside the declared ones.
            Self::RdsDbCluster => {
                // Boxed, like the other arms of this catalogue: the enclosing future holds
                // one pointer instead of this arm's whole state.
                Box::pin(create_db_cluster(ctx, region, resource, propagated)).await
            }
            // `AWS::ElastiCache::CacheCluster` publishes `Tags` as the `{Key, Value}` list
            // `CreateCacheCluster`'s own `Tags` member takes, so a cluster a stack creates
            // carries the tags CloudFormation gives it beside the declared ones.
            Self::ElastiCacheCacheCluster => {
                // Boxed, like every other arm of this catalogue should be as it grows: the
                // enclosing future holds one pointer instead of this arm's whole state, so
                // adding a type does not enlarge the future a nested stack's provisioning
                // walk keeps on the stack.
                Box::pin(create_cache_cluster(ctx, region, resource, propagated)).await
            }
            Self::RedshiftCluster => {
                create_redshift_cluster(ctx, region, resource, propagated).await
            }
            Self::RedshiftClusterSubnetGroup => {
                create_cluster_subnet_group(ctx, region, resource, propagated).await
            }
            // `AWS::Redshift::ClusterParameterGroup` publishes `Tags` as the `{Key, Value}`
            // list `CreateClusterParameterGroup`'s own `Tags` member takes, so a group a
            // stack creates carries the tags CloudFormation gives it beside the declared ones.
            Self::RedshiftClusterParameterGroup => {
                Box::pin(create_cluster_parameter_group(
                    ctx,
                    region,
                    resource,
                    propagated,
                ))
                .await
            }
            // `AWS::Redshift::SnapshotCopyGrant` publishes `Tags` as the `{Key, Value}` list
            // `CreateSnapshotCopyGrant`'s own `Tags` member takes, so a grant a stack creates
            // carries the tags CloudFormation gives it beside the declared ones.
            Self::RedshiftSnapshotCopyGrant => {
                Box::pin(create_snapshot_copy_grant(ctx, region, resource, propagated)).await
            }
            Self::RdsDbSubnetGroup => {
                create_db_subnet_group(ctx, region, resource, propagated).await
            }
            // `AWS::RDS::OptionGroup` publishes `Tags` as the `{Key, Value}` list
            // `CreateOptionGroup`'s own `Tags` member takes, so a group a stack creates
            // carries the tags CloudFormation gives it beside the declared ones.
            Self::RdsOptionGroup => {
                Box::pin(rds_option_groups::create_option_group(
                    ctx,
                    region,
                    resource,
                    propagated,
                ))
                .await
            }
            Self::RdsDbParameterGroup => {
                create_parameter_group(
                    ctx,
                    region,
                    resource,
                    propagated,
                    DB_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            Self::RdsDbClusterParameterGroup => {
                create_parameter_group(
                    ctx,
                    region,
                    resource,
                    propagated,
                    DB_CLUSTER_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            Self::RdsDbProxy => create_db_proxy(ctx, region, resource, propagated).await,
            // `AWS::RDS::DBProxyTargetGroup` publishes no `Tags` property — Amazon RDS
            // holds tags on the PROXY, not on the registrations in its target group — so
            // the type is not taggable and no tags are propagated to one.
            Self::RdsDbProxyTargetGroup => {
                create_db_proxy_target_group(ctx, region, resource).await
            }
            // `AWS::Events::Rule` publishes `Description`, `EventBusName`, `EventPattern`,
            // `Name`, `RoleArn`, `ScheduleExpression`, `State` and `Targets` and no
            // `Tags`: the type is not taggable, so no tags are propagated to it.
            Self::EventsRule => put_rule(ctx, region, resource).await,
            // `AWS::Events::EventBus` publishes `Tags` as the `[{Key, Value}]` list most
            // types do, which is the shape `CreateEventBus`'s own `Tags` member takes, so a
            // bus a stack creates carries the tags CloudFormation gives it beside the ones
            // the declaration asks for.
            Self::EventsEventBus => {
                event_buses::create_event_bus(ctx, region, resource, propagated).await
            }
            // `AWS::Events::EventBusPolicy` publishes no `Tags` property — a grant on a bus's
            // resource policy is not a resource EventBridge tags — so the type is not taggable
            // and no tags are propagated to one.
            Self::EventsEventBusPolicy => {
                event_buses::put_event_bus_policy(ctx, region, resource).await
            }
            // `AWS::Pipes::Pipe` publishes `Tags` as a MAP, which is the shape
            // `CreatePipe`'s own `Tags` member takes, so a pipe a stack creates carries the
            // tags CloudFormation gives it beside the ones the declaration asks for.
            Self::PipesPipe => event_pipes::create_pipe(ctx, region, resource, propagated).await,
            // `AWS::ResourceGroups::Group` publishes `Tags` as the `[{Key, Value}]` list most
            // types do, which `CreateGroup` takes as its own `Tags` MAP, so a group a stack
            // creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::ResourceGroupsGroup => {
                create_resource_group(ctx, region, resource, propagated).await
            }
            // `AWS::ApplicationInsights::Application` publishes `Tags` as the `[{Key, Value}]`
            // list most types do, which is the shape `CreateApplication`'s own `Tags` member
            // takes, so an application a stack creates carries the tags CloudFormation gives it
            // beside the ones the declaration asks for.
            Self::ApplicationInsightsApplication => {
                create_application(ctx, region, resource, propagated).await
            }
            Self::SsmParameter => put_parameter(ctx, region, resource, propagated).await,
            // `AWS::SSM::Association` publishes no `Tags` property — Systems Manager tags
            // documents and managed nodes rather than the associations between them — so the
            // type is not taggable and no tags are propagated to one.
            Self::SsmAssociation => create_ssm_association(ctx, region, resource).await,
            // `AWS::SSM::MaintenanceWindow` takes its own `Tags` property as a member of the
            // create itself, and `AWS::SSM::MaintenanceWindowTarget` publishes no `Tags`
            // property at all — Systems Manager tags the WINDOW, not the targets registered
            // with it — so neither type has tags propagated to it here.
            Self::SsmMaintenanceWindow => {
                create_ssm_maintenance_window(ctx, region, resource).await
            }
            Self::SsmMaintenanceWindowTarget => {
                create_ssm_maintenance_window_target(ctx, region, resource).await
            }
            // `AWS::SSM::Document` publishes `Tags` as the `[{Key, Value}]` list most types
            // do, which is the shape `CreateDocument`'s own `Tags` member takes, so a
            // document a stack creates carries the tags CloudFormation gives it beside the
            // ones the declaration asks for.
            Self::SsmDocument => {
                create_ssm_document(ctx, region, resource, propagated).await
            }
            Self::StepFunctionsActivity => {
                create_activity(ctx, region, resource, propagated).await
            }
            Self::SchedulerScheduleGroup => {
                create_schedule_group(ctx, region, resource, propagated).await
            }
            // `AWS::Scheduler::Schedule` publishes no `Tags` property — EventBridge
            // Scheduler tags a schedule GROUP, not a schedule — so the type is not
            // taggable and no tags are propagated to it.
            Self::SchedulerSchedule => create_schedule(ctx, region, resource).await,
            // `AWS::Cognito::UserPoolGroup` publishes `Description`, `GroupName`,
            // `Precedence`, `RoleArn` and `UserPoolId` and no `Tags`: the type is not
            // taggable, so no tags are propagated to a group.
            Self::CognitoUserPoolGroup => create_group(ctx, region, resource).await,
            Self::AcmAcmeEndpoint => create_acme_endpoint(ctx, region, resource, propagated).await,
            Self::AcmCertificate => request_certificate(ctx, region, resource, propagated).await,
            Self::AcmPcaCertificateAuthority => {
                create_certificate_authority(ctx, region, resource, propagated).await
            }
            // Neither `AWS::ACMPCA::Certificate` nor
            // `AWS::ACMPCA::CertificateAuthorityActivation` publishes a `Tags` property:
            // Amazon Web Services Private CA tags the certificate AUTHORITY, not a
            // certificate it issued or the import of one, so no tags are propagated to
            // either.
            Self::AcmPcaCertificate => issue_certificate(ctx, region, resource).await,
            Self::AcmPcaCertificateAuthorityActivation => {
                activate_certificate_authority(ctx, region, resource).await
            }
            // `AWS::ACMPCA::Permission` publishes no `Tags` property either: a grant on a CA
            // is no taggable resource, so no tags are propagated to one.
            Self::AcmPcaPermission => create_ca_permission(ctx, region, resource).await,
            Self::Route53ResolverEndpoint => {
                create_resolver_endpoint(ctx, region, resource, propagated).await
            }
            Self::Route53ResolverRule => {
                create_resolver_rule(ctx, region, resource, propagated).await
            }
            // `AWS::Route53Resolver::ResolverRuleAssociation` publishes `Name`,
            // `ResolverRuleId` and `VPCId` and no `Tags`: the type is not taggable, so no
            // tags are propagated to an association.
            Self::Route53ResolverRuleAssociation => {
                associate_resolver_rule(ctx, region, resource).await
            }
            Self::Route53ResolverFirewallDomainList => {
                create_firewall_domain_list(ctx, region, resource, propagated).await
            }
            Self::Route53ResolverFirewallRuleGroup => {
                create_firewall_rule_group(ctx, region, resource, propagated).await
            }
            Self::Route53ResolverFirewallRuleGroupAssociation => {
                associate_firewall_rule_group(ctx, region, resource, propagated).await
            }
            Self::Route53HostedZone => {
                create_hosted_zone(ctx, region, resource, propagated).await
            }
            // `AWS::Route53::RecordSet` publishes no `Tags` property — Route 53 tags a
            // hosted zone, not a record set — so the type is not taggable and no tags are
            // propagated to one.
            Self::Route53RecordSet => create_record_set(ctx, region, resource).await,
            // The same for a GROUP of record sets, which publishes no `Tags` property
            // either: the group IS the record sets it declares, applied in one change batch.
            Self::Route53RecordSetGroup => create_record_set_group(ctx, region, resource).await,
            // `AWS::Route53::HealthCheck` publishes `HealthCheckTags`, which the health
            // check carries beside the ones CloudFormation gives it — applied through
            // `ChangeTagsForResource`, since `CreateHealthCheck` takes no tags.
            Self::Route53HealthCheck => {
                create_health_check(ctx, region, resource, propagated).await
            }
            // None of the three types below publishes a `Tags` property — Route 53 tags a
            // hosted zone and a health check, not a CIDR collection, a delegation set or a
            // traffic policy — so no tags are propagated to one.
            Self::Route53CidrCollection => {
                create_cidr_collection(ctx, region, resource, propagated).await
            }
            Self::Route53DelegationSet => {
                create_delegation_set(ctx, region, resource, propagated).await
            }
            Self::Route53TrafficPolicy => create_traffic_policy(ctx, region, resource).await,
            // `AWS::ElasticBeanstalk::Application` publishes `ApplicationName`,
            // `Description` and `ResourceLifecycleConfig` and no `Tags`: the type is not
            // taggable, so no tags are propagated to it. The same goes for
            // `AWS::ElasticBeanstalk::ApplicationVersion`, whose properties are the
            // application it belongs to, its description and its source bundle.
            Self::ElasticBeanstalkApplication => create_eb_application(ctx, region, resource).await,
            Self::ElasticBeanstalkApplicationVersion => {
                create_eb_application_version(ctx, region, resource).await
            }
            Self::ElasticBeanstalkEnvironment => {
                create_eb_environment(ctx, region, resource, propagated).await
            }
            Self::TransferServer => {
                create_transfer_server(ctx, region, resource, propagated).await
            }
            Self::CodeDeployApplication => {
                create_codedeploy_application(ctx, region, resource, propagated).await
            }
            Self::CodeDeployDeploymentGroup => {
                create_codedeploy_deployment_group(ctx, region, resource, propagated).await
            }
            // `AWS::CodeDeploy::DeploymentConfig` publishes `ComputePlatform`,
            // `DeploymentConfigName`, `MinimumHealthyHosts`, `TrafficRoutingConfig` and
            // `ZonalConfig` and no `Tags`: the type is not taggable, so no tags are
            // propagated to a deployment configuration.
            Self::CodeDeployDeploymentConfig => {
                create_codedeploy_deployment_config(ctx, region, resource).await
            }
            Self::BatchComputeEnvironment => {
                create_compute_environment(ctx, region, resource, propagated).await
            }
            Self::BatchJobQueue => create_job_queue(ctx, region, resource, propagated).await,
            // A job-definition revision IS taggable: its `Tags` property is the tag map
            // `RegisterJobDefinition` takes, so the revision carries the tags CloudFormation
            // gives the resource beside its own.
            Self::BatchJobDefinition => {
                create_job_definition(ctx, region, resource, propagated).await
            }
            Self::BatchServiceEnvironment => {
                create_service_environment(ctx, region, resource, propagated).await
            }
            Self::EksAccessEntry => create_access_entry(ctx, region, resource, propagated).await,
            Self::EksCluster => create_eks_cluster(ctx, region, resource, propagated).await,
            // An add-on and a node group are taggable types whose create takes the tag set
            // as its own member, so the tags CloudFormation gives the resource reach Amazon
            // EKS on the call that makes it.
            Self::EksAddon => create_eks_addon(ctx, region, resource, propagated).await,
            Self::EksNodegroup => create_eks_nodegroup(ctx, region, resource, propagated).await,
            Self::EksPodIdentityAssociation => {
                create_pod_identity_association(ctx, region, resource, propagated).await
            }
            Self::ApiGatewayDomainName => {
                create_apigateway_domain_name(ctx, region, resource, propagated).await
            }
            // `AWS::ApiGateway::BasePathMapping` publishes `BasePath`, `DomainName`,
            // `DomainNameId`, `RestApiId` and `Stage` and no `Tags`: the type is not
            // taggable, so no tags are propagated to a mapping.
            Self::ApiGatewayBasePathMapping => {
                create_apigateway_base_path_mapping(ctx, region, resource).await
            }
            Self::ApiGatewayV2Api => {
                create_apigatewayv2_api(ctx, region, resource, propagated).await
            }
            Self::ApiGatewayV2DomainName => {
                create_apigatewayv2_domain_name(ctx, region, resource, propagated).await
            }
            // `AWS::ApiGatewayV2::ApiMapping` publishes `ApiId`, `ApiMappingKey`,
            // `DomainName` and `Stage` and no `Tags`: the type is not taggable, so no tags
            // are propagated to a mapping.
            Self::ApiGatewayV2ApiMapping => {
                create_apigatewayv2_api_mapping(ctx, region, resource).await
            }
            // The five types whose resources live INSIDE an api, each created in the
            // collection its api holds ([`ApiGatewayV2Child`]). Only the stage's own
            // property set publishes `Tags`, and the table is what says so, so the tags
            // CloudFormation gives the resource reach the one create that takes them.
            Self::ApiGatewayV2Integration => {
                create_apigatewayv2_child(
                    ctx,
                    region,
                    resource,
                    propagated,
                    &APIGATEWAYV2_INTEGRATION_CHILD,
                )
                .await
            }
            Self::ApiGatewayV2Route => {
                create_apigatewayv2_child(ctx, region, resource, propagated, &APIGATEWAYV2_ROUTE_CHILD)
                    .await
            }
            Self::ApiGatewayV2Stage => {
                create_apigatewayv2_child(ctx, region, resource, propagated, &APIGATEWAYV2_STAGE_CHILD)
                    .await
            }
            Self::ApiGatewayV2Deployment => {
                create_apigatewayv2_child(
                    ctx,
                    region,
                    resource,
                    propagated,
                    &APIGATEWAYV2_DEPLOYMENT_CHILD,
                )
                .await
            }
            Self::ApiGatewayV2Authorizer => {
                create_apigatewayv2_child(
                    ctx,
                    region,
                    resource,
                    propagated,
                    &APIGATEWAYV2_AUTHORIZER_CHILD,
                )
                .await
            }
            // `AWS::ApiGatewayV2::RoutingRule` publishes `Actions`, `Conditions`,
            // `DomainNameArn` and `Priority` and no `Tags`: a routing rule is no taggable
            // resource, so no tags are propagated to one.
            Self::ApiGatewayV2RoutingRule => {
                create_apigatewayv2_routing_rule(ctx, region, resource).await
            }
            Self::CodeCommitRepository => {
                create_codecommit_repository(ctx, region, resource, propagated).await
            }
            Self::CodeArtifactDomain => {
                create_codeartifact_domain(ctx, region, resource, propagated).await
            }
            Self::CodeArtifactRepository => {
                create_codeartifact_repository(ctx, region, resource, propagated).await
            }
            Self::CodeBuildReportGroup => {
                create_codebuild_report_group(ctx, region, resource, propagated).await
            }
            // `AWS::CodeBuild::Project` publishes `Tags`, and `CreateProject` takes a tag
            // list of its own, so a project a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for.
            Self::CodeBuildProject => {
                create_codebuild_project(ctx, region, resource, propagated).await
            }
            // `AWS::CodeBuild::Fleet` publishes `Tags`, and `CreateFleet` takes a tag list
            // of its own, so a fleet a stack creates carries the tags CloudFormation gives
            // it beside the ones the declaration asks for.
            Self::CodeBuildFleet => {
                create_codebuild_fleet(ctx, region, resource, propagated).await
            }
            // `AWS::SES::ConfigurationSet` publishes `Name`, `DeliveryOptions`,
            // `ReputationOptions`, `SendingOptions`, `SuppressionOptions`,
            // `TrackingOptions` and `VdmOptions` and no `Tags`: the type is not taggable,
            // so no tags are propagated to a configuration set.
            Self::SesConfigurationSet => {
                create_ses_configuration_set(ctx, region, resource).await
            }
            // `AWS::SES::EmailIdentity` publishes `EmailIdentity`,
            // `ConfigurationSetAttributes`, `DkimAttributes`, `DkimSigningAttributes`,
            // `FeedbackAttributes` and `MailFromAttributes` and no `Tags`: the type is not
            // taggable, so no tags are propagated to an email identity.
            Self::SesEmailIdentity => create_ses_email_identity(ctx, region, resource).await,
            // `AWS::SES::ReceiptRuleSet` publishes `RuleSetName` alone and
            // `AWS::SES::ReceiptRule` publishes `After`, `Rule` and `RuleSetName`: neither
            // type publishes a `Tags` property — Amazon SES holds no tags on either — so no
            // tags are propagated to one.
            Self::SesReceiptRuleSet => {
                create_ses_receipt_rule_set(ctx, region, resource).await
            }
            Self::SesReceiptRule => create_ses_receipt_rule(ctx, region, resource).await,
            Self::BackupVault => {
                create_backup_vault(ctx, region, resource, propagated).await
            }
            Self::BackupPlan => create_backup_plan(ctx, region, resource, propagated).await,
            // `AWS::Backup::BackupSelection` publishes `BackupPlanId` and
            // `BackupSelection` and no tags: AWS Backup tags a plan and a vault, not a
            // selection, so no tags are propagated to one.
            Self::BackupSelection => create_backup_selection(ctx, region, resource).await,
            // `AWS::Bedrock::Guardrail` publishes `Tags`, and `CreateGuardrail` takes the
            // `TagList` of its own, so a guardrail a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for.
            Self::BedrockGuardrail => {
                create_bedrock_guardrail(ctx, region, resource, propagated).await
            }
            // `AWS::Bedrock::KnowledgeBase` publishes `Tags` as a MAP, which is the shape
            // `CreateKnowledgeBase`'s own `tags` member takes, so a knowledge base a stack
            // creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::BedrockKnowledgeBase => {
                create_bedrock_knowledge_base(ctx, region, resource, propagated).await
            }
            // `AWS::Bedrock::DataSource` publishes no `Tags` property and `CreateDataSource`
            // takes no `tags` member, so no tags are propagated to a data source.
            Self::BedrockDataSource => create_bedrock_data_source(ctx, region, resource).await,
            // `AWS::Bedrock::Flow` publishes `Tags` as a MAP, which is the shape
            // `CreateFlow`'s own `tags` member takes, so a flow a stack creates carries the
            // tags CloudFormation gives it beside the ones the declaration asks for.
            Self::BedrockFlow => create_bedrock_flow(ctx, region, resource, propagated).await,
            // `AWS::Bedrock::Session` publishes `Tags` as a MAP, which is the shape
            // `CreateSession`'s own `tags` member takes, so a session a stack creates carries
            // the tags CloudFormation gives it beside the ones the declaration asks for.
            Self::BedrockSession => {
                create_bedrock_session(ctx, region, resource, propagated).await
            }
            // `AWS::BedrockAgentCore::WorkloadIdentity` publishes `Tags`, and
            // `CreateWorkloadIdentity` takes a tag MAP of its own, so an identity a stack
            // creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::BedrockAgentCoreWorkloadIdentity => {
                create_workload_identity(ctx, region, resource, propagated).await
            }
            // Every other `AWS::BedrockAgentCore::*` type publishes `Tags` and its create
            // takes a tag MAP of its own, so each resource a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for. A gateway
            // TARGET is the exception: `CreateGatewayTarget` takes no `tags` member and the
            // type publishes no `Tags` property, so no tag is propagated to one.
            Self::BedrockAgentCoreRuntime => {
                create_agentcore_runtime(ctx, region, resource, propagated).await
            }
            Self::BedrockAgentCoreMemory => {
                create_agentcore_memory(ctx, region, resource, propagated).await
            }
            Self::BedrockAgentCoreGateway => {
                create_agentcore_gateway(ctx, region, resource, propagated).await
            }
            Self::BedrockAgentCoreGatewayTarget => {
                create_agentcore_gateway_target(ctx, region, resource).await
            }
            Self::BedrockAgentCoreSandbox(sandbox) => {
                create_agentcore_sandbox(ctx, region, sandbox, resource, propagated).await
            }
            // `AWS::Braket::SpendingLimit` publishes `Tags` as a MAP, which is the shape
            // `CreateSpendingLimit`'s own `tags` member takes, so a spending limit a stack
            // creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::BraketSpendingLimit => {
                create_braket_spending_limit(ctx, region, resource, propagated).await
            }
            // Both `AWS::DirectConnect::*` types publish `Tags`, and each of their creates
            // takes a `TagList` of its own, so a gateway and a resiliency group a stack
            // creates each carry the tags CloudFormation gives them beside the ones their
            // declaration asks for.
            Self::DirectConnectGateway => {
                create_direct_connect_gateway(ctx, region, resource, propagated).await
            }
            Self::DirectConnectResiliencyGroup => {
                create_resiliency_group(ctx, region, resource, propagated).await
            }
            // `AWS::IoTCoreDeviceAdvisor::SuiteDefinition` publishes `Tags` as a list of
            // pairs, which `CreateSuiteDefinition` takes as the `TagMap` of its own `tags`
            // member, so a suite a stack creates carries the tags CloudFormation gives it
            // beside the ones the declaration asks for.
            Self::IotCoreDeviceAdvisorSuiteDefinition => {
                create_suite_definition(ctx, region, resource, propagated).await
            }
            // Both `AWS::BCM::*` types publish `Tags`, and each of their creates takes a
            // `resourceTags` list of its own, so a dashboard and a scheduled report a stack
            // creates each carry the tags CloudFormation gives them beside the ones their
            // declaration asks for.
            Self::BcmDashboard => create_bcm_dashboard(ctx, region, resource, propagated).await,
            Self::BcmScheduledReport => {
                create_bcm_scheduled_report(ctx, region, resource, propagated).await
            }
            // `AWS::ApplicationSignals::ServiceLevelObjective` publishes `Tags`, and
            // `CreateServiceLevelObjective` takes the `TagList` of its own, so an objective a
            // stack creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for. The same holds for an
            // `AWS::ApplicationSignals::InstrumentationConfig`, whose create takes a `TagList`
            // too. A GROUPING CONFIGURATION publishes no `Tags` property and its put takes no
            // tag member at all — it is the account's own configuration rather than a named
            // resource — so no tags are propagated to one.
            Self::ApplicationSignalsServiceLevelObjective => {
                create_slo(ctx, region, resource, propagated).await
            }
            Self::ApplicationSignalsGroupingConfiguration => {
                put_grouping_configuration(ctx, region, resource, &resource.properties).await
            }
            Self::ApplicationSignalsInstrumentationConfig => {
                create_instrumentation_config(ctx, region, resource, propagated).await
            }
            // `AWS::CleanRoomsML::TrainingDataset` publishes `Tags`, which
            // `CreateTrainingDataset` takes as its own tag MAP, so a dataset a stack creates
            // carries the tags CloudFormation gives it beside the ones the declaration asks
            // for.
            Self::CleanRoomsMlTrainingDataset => {
                create_training_dataset(ctx, region, resource, propagated).await
            }
            // Each of the three `AWS::AmplifyUIBuilder::*` types publishes `Tags` as a MAP,
            // which is the shape its create's own `tags` member takes, so an entity a stack
            // creates carries the tags CloudFormation gives it beside the ones the declaration
            // asks for.
            Self::AmplifyUiBuilderComponent => {
                create_ui_entity(ctx, region, UiEntity::Component, resource, propagated).await
            }
            Self::AmplifyUiBuilderForm => {
                create_ui_entity(ctx, region, UiEntity::Form, resource, propagated).await
            }
            Self::AmplifyUiBuilderTheme => {
                create_ui_entity(ctx, region, UiEntity::Theme, resource, propagated).await
            }
            // `AWS::Amplify::App` publishes `Tags` as a LIST of pairs, which `CreateApp` takes
            // as the `TagMap` of its own `tags` member, so an app a stack creates carries the
            // tags CloudFormation gives it beside the ones the declaration asks for.
            Self::AmplifyApp => create_amplify_app(ctx, region, resource, propagated).await,
            // `AWS::Amplify::Branch` publishes `Tags` as a LIST of pairs too, which
            // `CreateBranch` takes as the `TagMap` of its own `tags` member, so a branch a stack
            // creates carries the tags CloudFormation gives it beside the ones the declaration
            // asks for.
            Self::AmplifyBranch => {
                create_amplify_branch(ctx, region, resource, propagated).await
            }
            Self::XraySamplingRule => {
                create_sampling_rule(ctx, region, resource, propagated).await
            }
            // `AWS::XRay::Group` publishes `Tags` as a LIST of pairs, which is the shape
            // `CreateGroup`'s own `Tags` member takes, so a group a stack creates carries
            // the tags CloudFormation gives it beside the ones the declaration asks for.
            Self::XrayGroup => create_xray_group(ctx, region, resource, propagated).await,
            // `AWS::MWAA::Environment` publishes `Tags` as a MAP, which is the shape
            // `CreateEnvironment`'s own `Tags` member takes, so an environment a stack
            // creates carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::MwaaEnvironment => {
                create_mwaa_environment(ctx, region, resource, propagated).await
            }
            // Neither `AWS::DirectoryService::*` type publishes a `Tags` property — Directory
            // Service holds a directory's tags through its own `AddTagsToResource` and takes
            // none on either create — so no tags are propagated to a directory.
            Self::DirectoryServiceDirectory(kind) => {
                create_ds_directory(ctx, region, kind, resource).await
            }

            // None of the three `AWS::Config::*` types publishes a `Tags` property — AWS
            // Config holds tags on a recorder and on a rule through its own tagging call
            // and takes none on either put — so no tags are propagated to one.
            Self::ConfigConfigurationRecorder => {
                put_configuration_recorder(ctx, region, resource).await
            }
            Self::ConfigDeliveryChannel => put_delivery_channel(ctx, region, resource).await,
            Self::ConfigConfigRule => put_config_rule(ctx, region, resource).await,
            // Each group's create takes a `Tags` list of its own, so a parameter group and
            // a subnet group a stack creates each carry the tags CloudFormation gives them
            // beside the ones their declaration asks for.
            Self::MemoryDbGroup(group) => {
                create_memorydb_group(ctx, region, group, resource, propagated).await
            }
            // `AWS::CustomerProfiles::Domain` publishes `Tags`, and `CreateDomain` takes a
            // `TagMap` of its own, so a domain a stack creates carries the tags CloudFormation
            // gives it beside the ones the declaration asks for.
            Self::CustomerProfilesDomain => {
                create_customer_profiles_domain(ctx, region, resource, propagated).await
            }
            // `AWS::Rbin::Rule` publishes `Tags`, and `CreateRule` takes a `TagList` of its
            // own, so a retention rule a stack creates carries the tags CloudFormation gives
            // it beside the ones the declaration asks for.
            Self::RbinRule => create_rbin_rule(ctx, region, resource, propagated).await,
            // `AWS::DLM::LifecyclePolicy` publishes `Tags`, and `CreateLifecyclePolicy` takes a
            // `TagMap` of its own, so a lifecycle policy a stack creates carries the tags
            // CloudFormation gives it beside the ones the declaration asks for.
            Self::DlmLifecyclePolicy => {
                create_dlm_lifecycle_policy(ctx, region, resource, propagated).await
            }
            // Every `AWS::NetworkSecurityManager::*` type publishes `Tags`, and each of their
            // creates takes a `tags` MAP of its own, so an entity a stack creates carries the
            // tags CloudFormation gives it beside the ones the declaration asks for.
            Self::NetworkSecurityManagerEntity(entity) => {
                create_entity(ctx, region, entity, resource, propagated).await
            }
            // Both `AWS::NeptuneGraph::*` types publish `Tags`, and each of their creates
            // takes a `TagMap` of its own, so a graph and a snapshot a stack creates each
            // carry the tags CloudFormation gives them beside the ones their declaration asks
            // for.
            Self::NeptuneGraphGraph => {
                create_neptune_graph(ctx, region, resource, propagated).await
            }
            Self::NeptuneGraphGraphSnapshot => {
                create_graph_snapshot(ctx, region, resource, propagated).await
            }
            // `AWS::BackupGateway::Hypervisor` publishes `Tags`, and
            // `ImportHypervisorConfiguration` takes a `Tags` list of its own, so a hypervisor a
            // stack imports carries the tags CloudFormation gives it beside the ones the
            // declaration asks for.
            Self::BackupGatewayHypervisor => {
                create_backup_gateway_hypervisor(ctx, region, resource, propagated).await
            }
        }
    }

    /// Those of the tags CloudFormation gives a resource that the owning service's own API
    /// will take.
    ///
    /// Every create here is made through the owning service's PUBLIC API — the same path a
    /// client's own call takes, which is what makes a stack's resource indistinguishable
    /// from one a client created. Some of those APIs refuse a tag key in AWS's reserved
    /// `aws:` namespace, which is AWS's rule for a CALLER's request ("don't use the `aws:`
    /// prefix ... it is reserved for AWS use"): IAM's `CreateRole`, CodePipeline's
    /// `CreatePipeline` and EventBridge Scheduler's `CreateScheduleGroup` all do, in their
    /// own crates' tag validation. AWS sets the
    /// `aws:cloudformation:*` tags from inside itself, through no public API: here that
    /// inside path is the in-process call this provisioning request IS, which the framework
    /// stamps and a client's request cannot be — so a type whose owning crate reads that
    /// stamp takes the three (see the paragraph below), and for a type whose crate does not
    /// they are left off rather than sent to be refused: a stack must not fail over tags
    /// CloudFormation adds of its own accord. The stack's own tags, which are ordinary keys,
    /// are applied either
    /// way. ACM is the same case: its `CreateAcmeEndpoint` refuses a reserved key on either
    /// half of a pair, and so do Amazon ECS's `CreateCluster`, Amazon MSK's two cluster
    /// creates, Amazon SQS's `CreateQueue` — which refuses it in its own crate's
    /// `state::tag_refusal`, the one check both `CreateQueue`'s `tags` map and `TagQueue`
    /// run — and Amazon Data Firehose's `CreateDeliveryStream`, whose `TagKey` shape is
    /// modelled with the reserved prefix excluded, the same check its `TagDeliveryStream`
    /// makes. CodeCommit's `CreateRepository` is in the same position: a tag key in that
    /// namespace is its `InvalidSystemTagUsageException`, and Amazon ECR Public's
    /// `CreateRepository` is the same case: a key in that namespace is the
    /// `InvalidTagParameterException` its own crate's tag validation answers, the one check
    /// its `TagResource` runs too. AWS WAF's four WAFv2 entity
    /// creates are the same case again: each takes a `TagList` and refuses a key in the
    /// reserved namespace in its own crate's tag validation, the one check `TagResource`
    /// runs too.
    ///
    /// A type whose owning service ADMITS the three on that in-process path — Amazon S3,
    /// whose crate refuses a reserved key to an API caller and takes one from a provisioning
    /// call, Amazon S3
    /// Vectors, whose crate does the same, and Amazon EventBridge Pipes, whose crate reads
    /// the same signal —
    /// is NOT in the set below: the three travel with the create, and that service's own
    /// read of the resource's tags on the handle the stack publishes answers with them. A
    /// type stays in the set only until its own crate reads that path too.
    fn applicable_tags(self, propagated: &[(String, String)]) -> Vec<(String, String)> {
        let refuses_reserved_keys = matches!(
            self,
            Self::IamRole
                | Self::IamUser
                | Self::CodePipelinePipeline
                // CodePipeline refuses a tag key in the reserved namespace on
                // `CreateCustomActionType` too, in the same tag validation its
                // `CreatePipeline` runs.
                | Self::CodePipelineCustomActionType
                // And on `PutWebhook`'s own `tags` list, in that same validation, so a
                // webhook carries the stack's own tags without the three.
                | Self::CodePipelineWebhook
                | Self::EcsCluster
                // Amazon ECS refuses a tag key in the reserved namespace on every call that
                // takes tags — `CreateService`'s own `tags` list as much as `CreateCluster`'s
                // and `TagResource`, in the one check that crate's tag validation makes ("Tag
                // key must not start with aws:") — so a service carries the stack's own tags
                // without the three.
                | Self::EcsService
                // Amazon ECS refuses one on `CreateCapacityProvider`'s `tags` list too, in
                // that same check, so a capacity provider carries the stack's own tags
                // without the three.
                | Self::EcsCapacityProvider
                // And on `CreateTaskSet`'s `tags` list too, in that same check, so a task set
                // carries the stack's own tags without the three.
                | Self::EcsTaskSet
                | Self::EcrPublicRepository
                // Amazon ECR refuses a tag key in the reserved namespace on every call
                // that takes tags — `CreateRepository`'s `tags` and `TagResource` alike —
                // so a repository carries the stack's own tags without the three.
                | Self::EcrRepository
                | Self::FirehoseDeliveryStream
                // Amazon Kinesis Data Streams refuses a tag key in the reserved namespace
                // on every call that takes tags — `CreateStream`'s `Tags`, `TagResource` and
                // `AddTagsToStream` alike — so a stream carries the stack's own tags
                // without the three.
                | Self::KinesisStream
                | Self::SchedulerScheduleGroup
                | Self::AcmAcmeEndpoint
                // ACM refuses a tag key in the reserved namespace on every call that takes
                // tags, in the one check its own crate's tag validation makes, so a
                // certificate's are left off the same way an ACME endpoint's are.
                | Self::AcmCertificate
                // Amazon Web Services Private CA refuses a tag key in the reserved
                // namespace on `CreateCertificateAuthority` and on
                // `TagCertificateAuthority` alike, in the one check its own crate's tag
                // validation makes.
                | Self::AcmPcaCertificateAuthority
                | Self::MskCluster
                | Self::MskServerlessCluster
                | Self::SqsQueue
                | Self::CodeCommitRepository
                // KMS refuses a tag key in the reserved namespace on `CreateKey` and on
                // `TagResource` alike, in its own crate's tag validation.
                | Self::KmsKey
                | Self::Wafv2WebAcl
                | Self::Wafv2RuleGroup
                | Self::Wafv2IpSet
                | Self::Wafv2RegexPatternSet
                // Amazon API Gateway refuses a tag key in the reserved namespace on every
                // call that takes tags — the `tags` member of each create and `TagResource`
                // alike, in the one check its own crate's tag validation makes ("The tag
                // key … must not start with aws:") — so a custom domain name carries the
                // stack's own tags without the three.
                | Self::ApiGatewayDomainName
                // AWS X-Ray refuses a tag key in the reserved namespace on every call that
                // takes tags — `CreateSamplingRule`'s `Tags` and `TagResource` alike, in
                // the one check its own crate's tag validation makes ("Don't use `aws:` as
                // a prefix for keys; it's reserved for Amazon Web Services use") — so a
                // sampling rule carries the stack's own tags without the three.
                | Self::XraySamplingRule
                // The same check guards `CreateGroup`'s own `Tags` member, so a group is in
                // exactly that position.
                | Self::XrayGroup
                // Amazon MWAA refuses a tag key in the reserved namespace on every call that
                // takes tags — `CreateEnvironment`'s `Tags` map and `TagResource` alike, in
                // the one check its own crate's tag validation makes — so an environment
                // carries the stack's own tags without the three.
                | Self::MwaaEnvironment
                // Amazon ElastiCache refuses a tag key in the reserved namespace on every
                // call that takes tags — `CreateCacheCluster`'s `Tags` member and
                // `AddTagsToResource` alike, in the one check its own crate's tag validation
                // makes — so a cache cluster carries the stack's own tags without the three.
                | Self::ElastiCacheCacheCluster
                // Resource Groups refuses a tag key in the reserved namespace on every call
                // that takes tags — `CreateGroup`'s `Tags` map and `Tag` alike, in the one
                // check its own crate's tag validation makes — so a group carries the stack's
                // own tags without the three.
                | Self::ResourceGroupsGroup
                // Amazon EventBridge refuses a tag key in the reserved namespace on every
                // call that takes tags — `CreateEventBus`'s `Tags` list and `TagResource`
                // alike, in the one check its own crate's tag merge makes — so a bus carries
                // the stack's own tags without the three.
                | Self::EventsEventBus
                // Amazon Redshift refuses a tag key in the reserved namespace on
                // `CreateSnapshotCopyGrant`'s own `Tags` member and on `CreateTags` alike, in
                // the one check its own crate's tag validation makes — so a grant carries the
                // stack's own tags without the three.
                | Self::RedshiftSnapshotCopyGrant
                // Amazon MemoryDB refuses a tag key in the reserved namespace on every call
                // that takes tags — the `Tags` member of each group create and `TagResource`
                // alike, in the one check its own crate's tag validation makes — so a
                // parameter group and a subnet group each carry the stack's own tags without
                // the three.
                | Self::MemoryDbGroup(_)
                // Amazon Connect Customer Profiles refuses a tag key in the reserved namespace
                // on every call that takes tags: its `TagKey` shape is modelled with the
                // reserved prefix excluded (`^(?!aws:)…$`), which is the one check both
                // `CreateDomain`'s `Tags` map and `UpdateDomain`'s run — so a domain carries
                // the stack's own tags without the three.
                | Self::CustomerProfilesDomain
                // Recycle Bin refuses a tag key in the reserved namespace on every call that
                // takes tags — `CreateRule`'s own `Tags` list and `TagResource` alike, in the
                // one check its own crate's tag validation makes — so a retention rule carries
                // the stack's own tags without the three.
                | Self::RbinRule
                // Amazon Data Lifecycle Manager refuses a tag key in the reserved namespace on
                // every call that takes tags — `CreateLifecyclePolicy`'s own `Tags` map and
                // `TagResource` alike, in the one check its own crate's tag validation makes — so
                // a lifecycle policy carries the stack's own tags without the three.
                | Self::DlmLifecyclePolicy
                // Neptune Analytics refuses a tag key in the reserved namespace on every call
                // that takes tags — the `tags` map of each create and `TagResource` alike, in
                // the one check its own crate's tag validation makes — so a graph and a
                // snapshot each carry the stack's own tags without the three.
                | Self::NeptuneGraphGraph
                | Self::NeptuneGraphGraphSnapshot
                // Backup Gateway refuses a tag key in the reserved namespace on every call that
                // takes tags — `ImportHypervisorConfiguration`'s own `Tags` list and
                // `TagResource` alike, in the one check its own crate's tag validation makes
                // ("The key can't start with `aws:`") — so a hypervisor carries the stack's own
                // tags without the three.
                | Self::BackupGatewayHypervisor
                // Billing and Cost Management Dashboards refuses a tag key in the reserved
                // namespace on every call that takes tags — the `resourceTags` list of each
                // create and `TagResource` alike, in the one check its own crate's tag
                // validation makes — so a dashboard and a scheduled report each carry the
                // stack's own tags without the three.
                | Self::BcmDashboard
                | Self::BcmScheduledReport
        );
        propagated
            .iter()
            .filter(|(key, _)| !(refuses_reserved_keys && is_reserved_tag_key(key)))
            .cloned()
            .collect()
    }

    /// How THIS type's resource provider words a refusal of the owning service, which is the
    /// message CloudFormation quotes in the failed resource's status reason
    /// ([`super::provisioning`]).
    ///
    /// A provider reports the refusal of the service it called, and most of them report that
    /// service's message as it came — the diagnostic a client of that service would read,
    /// carrying that service's own name and status. A provider written against the
    /// CloudFormation resource-provider plugin reports it through the exception class of the
    /// `HandlerErrorCode` it classified the refusal under, and THAT class's wording is part
    /// of the message rather than only of the envelope: an `InvalidRequest` refusal reaches
    /// the reason as `Invalid request provided: <the service's message>`, which is what an
    /// `AWS::OpenSearchService::Domain` whose cluster its service will not provision is
    /// reported with.
    ///
    /// Which of the two a type is, is that type's provider's and cannot be derived from the
    /// type's name, so it is stated per type here — the same per-type half of provisioning
    /// the create and delete arms are. A type not named below reports the message unchanged.
    ///
    /// `token` is the token of the resource request this refusal answered, which the provider
    /// of an `AWS::MSK::*` type states at the head of its message
    /// ([`MSK_HANDLER_REQUEST_TOKEN_LABEL`]) — the same token the event's own
    /// `(RequestToken: …)` reports, since one resource request earned both.
    pub(super) fn handler_message(self, class: &str, message: &str, token: &str) -> String {
        let plugin_worded = matches!(self, Self::OpenSearchServiceDomain);
        if plugin_worded && class == super::provisioning::HANDLER_ERROR_INVALID_REQUEST {
            return format!("{INVALID_REQUEST_PROVIDED}{message}");
        }
        // Every Amazon MSK type's provider opens its message with the resource request's own
        // token, whatever class the refusal falls in: it is how that provider reports a
        // refusal rather than something it says about one kind of refusal.
        if matches!(
            self,
            Self::MskCluster | Self::MskServerlessCluster | Self::MskConfiguration
        ) {
            return format!("[{MSK_HANDLER_REQUEST_TOKEN_LABEL}: {token}] {message}");
        }
        message.to_string()
    }

    /// Which `HandlerErrorCode` class this type's provider puts a refusal carrying `code` in,
    /// where that is NOT the class the code itself falls in
    /// ([`super::provisioning::handler_error_code`] computes `classified`).
    ///
    /// `NotFound` is the class of a refusal about the resource the handler MANAGES — the one
    /// it was asked to read, update or delete. A refusal naming a DIFFERENT resource, one the
    /// declaration merely REFERENCES, says the declaration named something that is not there:
    /// the request the handler was given is the thing at fault, so its provider reports it as
    /// `InvalidRequest` and a client reading the class sees a template to fix rather than a
    /// resource that vanished.
    ///
    /// Which of a type's refusals are about a referenced resource is that type's own, so it is
    /// stated per type here beside the rest of the per-type half of provisioning. A type not
    /// named below reports the class its code falls in.
    ///
    /// A type whose provider does not map one of the owning service's own error codes is
    /// named here too: the plugin reports an exception it has no mapping for under its class
    /// for a service that failed ([`super::provisioning::HANDLER_ERROR_SERVICE_FAILED`]),
    /// whatever the code itself reads like — which is the class the recorded reason of a
    /// failed `AWS::CodePipeline::Pipeline` carries for CodePipeline's
    /// `InvalidStructureException` (in the behavioral corpus `simparity-behavioral-aws-v1`,
    /// check
    /// `cloudformation__codepipeline.deploy-codepipeline-pipeline-unassumable-role-rolled-back`,
    /// whose reason closes `HandlerErrorCode: GeneralServiceException`). It is that CODE that
    /// decides the class, as everywhere else here, and never the message the refusal carried:
    /// every refusal that service answers under it is reported the same way, while its other
    /// declaration exceptions keep the class their own codes fall in.
    pub(super) fn handler_error_class(self, code: &str, classified: &'static str) -> &'static str {
        if matches!(self, Self::CodePipelinePipeline) && code == CODEPIPELINE_INVALID_STRUCTURE {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        let references_another_resource = match self {
            // A record set is written INSIDE a hosted zone its declaration names
            // (`HostedZoneId`/`HostedZoneName`), and the record set itself is the resource
            // this handler manages: a zone Route 53 does not hold (`NoSuchHostedZone`) is a
            // declaration naming a zone that is not there. A GROUP of record sets names its
            // zone the same way and is in the same position.
            Self::Route53RecordSet | Self::Route53RecordSetGroup => {
                code.contains(NO_SUCH_HOSTED_ZONE)
            }
            _ => false,
        };
        if references_another_resource {
            return super::provisioning::HANDLER_ERROR_INVALID_REQUEST;
        }
        classified
    }

    /// Delete one resource from the service that owns it. A resource that service no
    /// longer holds is already in the state the delete asks for, so each arm decides that
    /// on the answer's own terms: on its not-found CODE where the service models one —
    /// never on a message, which carries the request's own name — and, where it models
    /// none (SageMaker's model group), by READING the resource back.
    ///
    /// The whole record is passed, not just its physical id: a type whose identity is a
    /// PAIR (a security policy's `(type, name)`) needs the property that carries the other
    /// half.
    ///
    /// `final_snapshot` is the identifier CloudFormation asks for a SNAPSHOT of the resource
    /// under, for a resource whose `DeletionPolicy` is `Snapshot`
    /// ([`super::provisioning::final_snapshot_identifier`]). It reaches the arms of the types
    /// whose owning service takes a snapshot ON THE WAY OUT, as a member of the delete call
    /// itself (Amazon RDS's `FinalDBSnapshotIdentifier`, Amazon Redshift's
    /// `FinalClusterSnapshotIdentifier`) — the published list of types `Snapshot` supports is
    /// wider than that, and a type whose snapshot is a call of its own is not one of these
    /// arms yet, so it is deleted with no snapshot taken rather than with one invented for
    /// it.
    pub(super) async fn delete(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        final_snapshot: Option<&str>,
    ) -> Result<(), Refusal> {
        let physical_resource_id = &resource.physical_resource_id;
        match self {
            Self::FirehoseDeliveryStream => {
                delete_delivery_stream(ctx, region, physical_resource_id).await
            }
            Self::KinesisStream => {
                delete_kinesis_stream(ctx, region, physical_resource_id).await
            }
            Self::CloudWatchMetricStream => {
                delete_metric_stream(ctx, region, physical_resource_id).await
            }
            Self::CloudWatchDashboard => {
                delete_dashboard(ctx, region, physical_resource_id).await
            }
            Self::EcrPublicRepository => {
                delete_public_repository(ctx, region, physical_resource_id).await
            }
            Self::EcrRepository => delete_repository(ctx, region, resource).await,
            Self::Ec2Vpc => delete_ec2_vpc(ctx, region, physical_resource_id).await,
            Self::Ec2Subnet => delete_ec2_subnet(ctx, region, physical_resource_id).await,
            Self::Ec2SecurityGroup => {
                delete_ec2_security_group(ctx, region, physical_resource_id).await
            }
            Self::Ec2Volume => delete_ec2_volume(ctx, region, physical_resource_id).await,
            Self::Ec2LaunchTemplate => {
                delete_ec2_launch_template(ctx, region, physical_resource_id).await
            }
            Self::AutoScalingGroup => {
                delete_auto_scaling_group(ctx, region, physical_resource_id).await
            }
            // The policy is addressed by the group it belongs to and the name it was created
            // under, and only the resource's own declaration says which group that is.
            Self::AutoScalingScalingPolicy => delete_scaling_policy(ctx, region, resource).await,
            Self::OpenSearchServerlessSecurityPolicy => {
                delete_security_policy(ctx, region, resource).await
            }
            Self::OpenSearchServerlessCollection => {
                delete_collection(ctx, region, physical_resource_id).await
            }
            // The endpoint is addressed by the `vpce-…` id that service minted, which is
            // this resource's physical id.
            Self::OpenSearchServerlessVpcEndpoint => {
                delete_vpc_endpoint(ctx, region, physical_resource_id).await
            }
            Self::OpenSearchServiceDomain => {
                delete_domain(ctx, region, physical_resource_id).await
            }
            Self::S3Bucket => delete_bucket(ctx, region, physical_resource_id).await,
            // The policy is the named BUCKET's, and only the resource's own declaration
            // says which bucket that is.
            Self::S3BucketPolicy => delete_bucket_policy(ctx, region, resource).await,
            Self::S3TablesTableBucket => {
                delete_table_bucket(ctx, region, physical_resource_id).await
            }
            // The namespace is addressed by the PAIR S3 Tables keys one by — the table bucket
            // holding it and its own name — which the resource's own declaration carries.
            Self::S3TablesNamespace => delete_namespace(ctx, region, resource).await,
            Self::S3VectorsVectorBucket => {
                delete_vector_bucket(ctx, region, physical_resource_id).await
            }
            Self::S3VectorsIndex => delete_index(ctx, region, physical_resource_id).await,
            Self::EmrCluster => terminate_job_flow(ctx, region, physical_resource_id).await,
            // The configuration and the Studio are each removed by the identity they were
            // created under, which is this resource's physical id; a session mapping is
            // addressed by the triple only its own declaration carries.
            Self::EmrSecurityConfiguration => {
                delete_emr_security_configuration(ctx, region, physical_resource_id).await
            }
            Self::EmrStudio => delete_emr_studio(ctx, region, physical_resource_id).await,
            Self::EmrStudioSessionMapping => {
                delete_emr_studio_session_mapping(ctx, region, resource).await
            }
            // The role's policies have to go first, and only the resource's own record
            // says which ones there are.
            Self::IamRole => delete_role(ctx, region, resource).await,
            // The roles the profile holds have to be taken out of it first, and only the
            // resource's own record says which ones there are.
            Self::IamInstanceProfile => delete_instance_profile(ctx, region, resource).await,
            // The user's password, policies and memberships have to go first, and only the
            // resource's own record says which ones there are.
            Self::IamUser => delete_iam_user(ctx, region, resource).await,
            // The key is addressed by the PAIR IAM keys one by — the user that holds it and
            // the id IAM minted — and the user half is on the resource's own declaration.
            Self::IamAccessKey => delete_iam_access_key(ctx, region, resource).await,
            // The policy's attachments and its non-default versions have to go first, and
            // only the resource's own record says which entities it was attached to.
            Self::IamManagedPolicy => delete_managed_policy(ctx, region, resource).await,
            Self::CodePipelinePipeline => {
                delete_pipeline(ctx, region, physical_resource_id).await
            }
            // The registration is addressed by the triple its declaration states, not by
            // a name, so the delete takes the whole record rather than the id.
            Self::CodePipelineCustomActionType => {
                delete_custom_action_type(ctx, region, resource).await
            }
            // The webhook is removed by the name it was created under, which is this
            // resource's physical id.
            Self::CodePipelineWebhook => {
                delete_webhook(ctx, region, physical_resource_id).await
            }
            Self::EcsCluster => delete_cluster(ctx, region, physical_resource_id).await,
            // The provider is deleted by the name it was created under, which is this
            // resource's physical id.
            Self::EcsCapacityProvider => {
                delete_ecs_capacity_provider(ctx, region, resource).await
            }
            // The association is REMOVED by the same put with nothing associated: "any
            // existing capacity providers that are associated with a cluster that are omitted
            // from a PutClusterCapacityProviders API call will be disassociated with the
            // cluster".
            Self::EcsClusterCapacityProviderAssociations => {
                disassociate_cluster_capacity_providers(ctx, region, resource).await
            }
            // The task set is deleted by the triple Amazon ECS addresses one by, whose id is
            // the last part of this resource's physical id.
            Self::EcsTaskSet => Box::pin(delete_ecs_task_set(ctx, region, resource)).await,
            // A service HAS a primary task set for as long as it has task sets at all:
            // Amazon ECS publishes no call that un-sets one, and the task sets a stack
            // deletes are what take it away, so there is nothing to ask of that service for
            // the record going away.
            Self::EcsPrimaryTaskSet => Ok(()),
            Self::SqsQueue => delete_queue(ctx, region, physical_resource_id).await,
            Self::SqsQueuePolicy => delete_queue_policy(ctx, region, resource).await,
            Self::SageMakerModelPackageGroup => {
                delete_model_package_group(ctx, region, physical_resource_id).await
            }
            Self::SageMakerModel => {
                delete_sagemaker_model(ctx, region, physical_resource_id).await
            }
            Self::SageMakerEndpointConfig => {
                delete_sagemaker_endpoint_config(ctx, region, physical_resource_id).await
            }
            // The endpoint is deleted by name; the removal above waits for SageMaker to free
            // the resources it deployed.
            Self::SageMakerEndpoint => {
                delete_sagemaker_endpoint(ctx, region, physical_resource_id).await
            }
            // The instance has to be stopped before SageMaker will take it away, which is
            // what that arm asks for; the removal below finishes the deletion.
            Self::SageMakerNotebookInstance => {
                delete_sagemaker_notebook_instance(ctx, region, physical_resource_id).await
            }
            Self::SageMakerNotebookInstanceLifecycleConfig => {
                delete_sagemaker_notebook_instance_lifecycle_config(
                    ctx,
                    region,
                    physical_resource_id,
                )
                .await
            }
            Self::SnsTopic => delete_topic(ctx, region, physical_resource_id).await,
            Self::SnsTopicPolicy => delete_topic_policy(ctx, region, resource).await,
            Self::SnsSubscription => unsubscribe(ctx, region, resource).await,
            Self::FisExperimentTemplate => {
                delete_experiment_template(ctx, region, physical_resource_id).await
            }
            Self::WafRegionalIpSet => delete_ip_set(ctx, region, physical_resource_id).await,
            // WAF addresses an entity by the triple its create fixed — the declared name
            // and scope and the id that service assigned — so the delete takes the whole
            // record rather than the id.
            Self::Wafv2WebAcl => {
                delete_wafv2_entity(ctx, region, resource, WAFV2_WEB_ACL_CALLS).await
            }
            Self::Wafv2RuleGroup => {
                delete_wafv2_entity(ctx, region, resource, WAFV2_RULE_GROUP_CALLS).await
            }
            Self::Wafv2IpSet => {
                delete_wafv2_entity(ctx, region, resource, WAFV2_IP_SET_CALLS).await
            }
            Self::Wafv2RegexPatternSet => {
                delete_wafv2_entity(ctx, region, resource, WAFV2_REGEX_PATTERN_SET_CALLS).await
            }
            // The binding is addressed by the PROTECTED resource alone — "a resource has at
            // most one web ACL in front of it" — which the resource's own declaration
            // carries.
            Self::Wafv2WebAclAssociation => disassociate_web_acl(ctx, region, resource).await,
            Self::GlueJob => delete_glue_job(ctx, region, physical_resource_id).await,
            Self::GlueTrigger => delete_glue_trigger(ctx, region, physical_resource_id).await,
            // A database is addressed by its name in the catalog its declaration names, and
            // a table by that catalog, the database that holds it and its own name — so both
            // deletes take the whole record rather than the id.
            Self::GlueDatabase => delete_glue_database(ctx, region, resource).await,
            Self::GlueTable => delete_glue_table(ctx, region, resource).await,
            // A grant is revoked by the very request that made it — the principal, the
            // resource and the permissions it carries — so the revoke takes the whole record
            // rather than the id, and it takes away exactly what the stack granted.
            Self::LakeFormationPermissions(grant) => {
                revoke_lake_formation_permissions(ctx, region, grant, resource).await
            }
            // A Client VPN endpoint is addressed by the `cvpn-endpoint-…` id Amazon EC2
            // minted for it, which is this resource's physical id.
            Self::Ec2ClientVpnEndpoint => {
                delete_client_vpn_endpoint(ctx, region, physical_resource_id).await
            }
            // An authorization rule is revoked by the endpoint it is on, the network it
            // grants and the group it grants that network to — which only the resource's own
            // declaration carries, so the revoke takes the whole record rather than the id.
            Self::Ec2ClientVpnAuthorizationRule => {
                revoke_client_vpn_ingress(ctx, region, resource).await
            }
            Self::GlueCrawler => delete_glue_crawler(ctx, region, physical_resource_id).await,
            // An optimizer is addressed by the four values that service keys one by, which
            // the resource's own declaration carries — so the delete takes the whole record
            // rather than the id.
            Self::GlueTableOptimizer => {
                delete_glue_table_optimizer(ctx, region, resource).await
            }
            Self::GrafanaWorkspace => {
                delete_workspace(ctx, region, physical_resource_id).await
            }
            Self::GuardDutyDetector => delete_detector(ctx, region, physical_resource_id).await,
            // A trusted IP list is addressed by the PAIR that identifies it — the detector
            // its declaration names and the id GuardDuty minted — so the delete takes the
            // whole record rather than the id.
            Self::GuardDutyIpSet => delete_guardduty_ip_set(ctx, region, resource).await,
            // A KMS key is not deleted at once: its deletion is SCHEDULED, for the waiting
            // period the declaration asks for.
            Self::KmsKey => {
                let properties = resource.properties.as_object().cloned().unwrap_or_default();
                schedule_kms_key_deletion(ctx, region, physical_resource_id, &properties).await
            }
            // An alias is deleted at once, and deleting it leaves the key it pointed at as
            // it is ("These operations don't affect the underlying KMS key").
            Self::KmsAlias => delete_kms_alias(ctx, region, physical_resource_id).await,
            // One `DeleteCluster` deletes a cluster of either kind, addressed by the ARN
            // Amazon MSK minted for it.
            Self::MskCluster | Self::MskServerlessCluster => {
                delete_msk_cluster(ctx, region, physical_resource_id).await
            }
            // A configuration is deleted by its own call, addressed by the ARN Amazon MSK
            // minted for it.
            Self::MskConfiguration => {
                delete_msk_configuration(ctx, region, physical_resource_id).await
            }
            Self::MqBroker => delete_mq_broker(ctx, region, physical_resource_id).await,
            // The delete carries one member off the resource's own declaration
            // (`DeleteAutomatedBackups`), so it takes the record rather than the id — and
            // the final snapshot a `Snapshot` deletion policy asks for is a member of that
            // same call.
            Self::RdsDbInstance => {
                delete_db_instance(ctx, region, resource, final_snapshot).await
            }
            // The final snapshot a `Snapshot` deletion policy asks for is a member of the
            // delete itself, so the arm takes it beside the cluster's identifier.
            Self::RdsDbCluster => {
                Box::pin(delete_db_cluster(
                    ctx,
                    region,
                    physical_resource_id,
                    final_snapshot,
                ))
                .await
            }
            // The final snapshot a `Snapshot` deletion policy asks for is a member of the
            // delete itself, so the arm takes it beside the cluster's identifier.
            Self::ElastiCacheCacheCluster => {
                Box::pin(delete_cache_cluster(
                    ctx,
                    region,
                    physical_resource_id,
                    final_snapshot,
                ))
                .await
            }
            Self::RedshiftCluster => {
                delete_redshift_cluster(ctx, region, physical_resource_id, final_snapshot)
                    .await
            }
            Self::RedshiftClusterParameterGroup => {
                delete_cluster_parameter_group(ctx, region, physical_resource_id).await
            }
            Self::RedshiftClusterSubnetGroup => {
                delete_cluster_subnet_group(ctx, region, physical_resource_id).await
            }
            Self::RedshiftSnapshotCopyGrant => {
                delete_snapshot_copy_grant(ctx, region, physical_resource_id).await
            }
            Self::RdsDbSubnetGroup => {
                delete_db_subnet_group(ctx, region, physical_resource_id).await
            }
            Self::RdsOptionGroup => {
                rds_option_groups::delete_option_group(ctx, region, physical_resource_id).await
            }
            Self::RdsDbParameterGroup => {
                delete_parameter_group(
                    ctx,
                    region,
                    physical_resource_id,
                    DB_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            Self::RdsDbClusterParameterGroup => {
                delete_parameter_group(
                    ctx,
                    region,
                    physical_resource_id,
                    DB_CLUSTER_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            // The proxy is addressed by the name it was created under, which is this
            // resource's physical id.
            Self::RdsDbProxy => delete_db_proxy(ctx, region, physical_resource_id).await,
            // A registration is addressed by the PROXY and the target group that hold it,
            // and only the resource's own declaration carries either, so the delete takes
            // the record rather than its id — which is the group's ARN.
            Self::RdsDbProxyTargetGroup => {
                delete_db_proxy_target_group(ctx, region, resource).await
            }
            // A rule still carrying targets cannot be deleted, and only the rule's own
            // record says which bus it is on, so the delete takes the record.
            Self::EventsRule => delete_rule(ctx, region, resource).await,
            // A bus is addressed by the name it was created under, which is this resource's
            // physical id.
            Self::EventsEventBus => {
                event_buses::delete_event_bus(ctx, region, physical_resource_id).await
            }
            // A grant is addressed by the bus AND the statement id that identify it, and only
            // the resource's own declaration carries either, so the delete takes the record.
            Self::EventsEventBusPolicy => {
                event_buses::remove_event_bus_policy(ctx, region, resource).await
            }
            Self::SsmParameter => delete_parameter(ctx, region, physical_resource_id).await,
            Self::SsmAssociation => {
                delete_ssm_association(ctx, region, physical_resource_id).await
            }
            // A window is addressed by the id Systems Manager minted for it, which is this
            // resource's physical id; a TARGET is addressed by that window AND its own minted
            // id, and only the resource's own record carries both, so that delete takes the
            // record.
            Self::SsmMaintenanceWindow => {
                delete_ssm_maintenance_window(ctx, region, physical_resource_id).await
            }
            Self::SsmMaintenanceWindowTarget => {
                delete_ssm_maintenance_window_target(ctx, region, resource).await
            }
            // A document is addressed by its name, which is this resource's physical id.
            Self::SsmDocument => delete_ssm_document(ctx, region, physical_resource_id).await,
            Self::StepFunctionsActivity => {
                delete_activity(ctx, region, physical_resource_id).await
            }
            Self::SchedulerScheduleGroup => {
                delete_schedule_group(ctx, region, physical_resource_id).await
            }
            // A resource group is addressed by its name, which is this resource's physical id.
            Self::ResourceGroupsGroup => {
                delete_resource_group(ctx, region, physical_resource_id).await
            }
            // An application is addressed by the resource group it monitors, which its own
            // declaration carries rather than the ARN that is this resource's physical id.
            Self::ApplicationInsightsApplication => {
                delete_application(ctx, region, resource).await
            }
            // A schedule is addressed by its name AND the group that holds it, which only
            // the resource's own record carries, so the delete takes the record.
            Self::SchedulerSchedule => delete_schedule(ctx, region, resource).await,
            // A pipe is addressed by its name alone, which is this resource's physical id.
            Self::PipesPipe => {
                event_pipes::delete_pipe(ctx, region, physical_resource_id).await
            }
            // The delete addresses the group by the PAIR that identifies it, and the pool
            // half of that pair is on the resource's own declaration.
            Self::CognitoUserPoolGroup => delete_group(ctx, region, resource).await,
            Self::AcmAcmeEndpoint => delete_acme_endpoint(ctx, region, physical_resource_id).await,
            Self::AcmCertificate => delete_certificate(ctx, region, physical_resource_id).await,
            Self::AcmPcaCertificateAuthority => {
                delete_certificate_authority(ctx, region, physical_resource_id).await
            }
            // Amazon Web Services Private CA publishes no call that un-issues a
            // certificate one of its CAs signed, so removing the resource that asked for
            // it removes the stack's record of it and nothing else.
            Self::AcmPcaCertificate => Ok(()),
            // An activation is not a resource of that service's own: what it DID was put
            // the CA's certificate into it and leave it able to issue. Removing it
            // withdraws that, which is `UpdateCertificateAuthority` putting the CA back in
            // `DISABLED` — the status a CA that holds a certificate but may not use it is
            // in, and the one the CA's own resource can then be deleted from.
            Self::AcmPcaCertificateAuthorityActivation => {
                deactivate_certificate_authority(ctx, region, physical_resource_id).await
            }
            // A grant is revoked by the CA-and-principal pair that identifies it, both
            // halves of which are on the resource's own declaration.
            Self::AcmPcaPermission => revoke_ca_permission(ctx, region, resource).await,
            Self::Route53ResolverEndpoint => {
                delete_resolver_endpoint(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverRule => {
                delete_resolver_rule(ctx, region, physical_resource_id).await
            }
            // The disassociate addresses the association by the PAIR that identifies it,
            // which only the resource's own record carries.
            Self::Route53ResolverRuleAssociation => {
                disassociate_resolver_rule(ctx, region, resource).await
            }
            Self::Route53ResolverFirewallDomainList => {
                delete_firewall_domain_list(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverFirewallRuleGroup => {
                delete_firewall_rule_group(ctx, region, physical_resource_id).await
            }
            // The disassociate addresses the association by the id Route 53 Resolver minted
            // for it, which is this resource's physical id.
            Self::Route53ResolverFirewallRuleGroupAssociation => {
                disassociate_firewall_rule_group(ctx, region, physical_resource_id).await
            }
            Self::Route53HostedZone => {
                delete_hosted_zone(ctx, region, physical_resource_id).await
            }
            // Route 53 removes a record set by its own values, which only the resource's
            // own record carries — as is the zone its declaration names.
            Self::Route53RecordSet => delete_record_set(ctx, region, resource).await,
            // The same batch that created the group's record sets, removing them.
            Self::Route53RecordSetGroup => delete_record_set_group(ctx, region, resource).await,
            Self::Route53HealthCheck => {
                delete_health_check(ctx, region, physical_resource_id).await
            }
            // Each of the three is addressed by the identifier Route 53 minted for it, which
            // is this resource's physical id.
            Self::Route53CidrCollection => {
                delete_cidr_collection(ctx, region, physical_resource_id).await
            }
            Self::Route53DelegationSet => {
                delete_delegation_set(ctx, region, physical_resource_id).await
            }
            Self::Route53TrafficPolicy => {
                delete_traffic_policy(ctx, region, physical_resource_id).await
            }
            Self::ElasticBeanstalkApplication => {
                delete_eb_application(ctx, region, physical_resource_id).await
            }
            // An application version is keyed by the PAIR that identifies it, and the
            // application half of that pair is on the resource's own declaration.
            Self::ElasticBeanstalkApplicationVersion => {
                delete_eb_application_version(ctx, region, resource).await
            }
            Self::ElasticBeanstalkEnvironment => {
                terminate_eb_environment(ctx, region, physical_resource_id).await
            }
            // The server is addressed by the `ServerId` Transfer Family assigned it,
            // which this type's `ServerId` attribute carries — its physical id is the
            // server's ARN.
            Self::TransferServer => delete_transfer_server(ctx, region, resource).await,
            Self::CodeDeployApplication => {
                delete_codedeploy_application(ctx, region, physical_resource_id).await
            }
            // The delete addresses the group by the PAIR that identifies it, and the
            // application half of that pair is on the resource's own declaration.
            Self::CodeDeployDeploymentGroup => {
                delete_codedeploy_deployment_group(ctx, region, resource).await
            }
            Self::CodeDeployDeploymentConfig => {
                delete_codedeploy_deployment_config(ctx, region, physical_resource_id).await
            }
            Self::BatchComputeEnvironment => {
                delete_batch_compute_environment(ctx, region, physical_resource_id).await
            }
            Self::BatchJobQueue => {
                delete_batch_job_queue(ctx, region, physical_resource_id).await
            }
            Self::BatchJobDefinition => {
                delete_batch_job_definition(ctx, region, physical_resource_id).await
            }
            Self::BatchServiceEnvironment => {
                delete_batch_service_environment(ctx, region, physical_resource_id).await
            }
            // The delete addresses the entry by the PAIR Amazon EKS keys one by, both
            // halves of which are on the resource's own declaration.
            Self::EksAccessEntry => delete_access_entry(ctx, region, resource).await,
            Self::EksCluster => delete_eks_cluster(ctx, region, physical_resource_id).await,
            // Each addressed by the cluster that holds it and its own name, the cluster half
            // of which is on the resource's own declaration.
            Self::EksAddon => delete_eks_addon(ctx, region, resource).await,
            Self::EksNodegroup => delete_eks_nodegroup(ctx, region, resource).await,
            // The delete addresses the association by the cluster that holds it and the id
            // Amazon EKS assigned it, both of which are on the resource's own record.
            Self::EksPodIdentityAssociation => {
                delete_pod_identity_association(ctx, region, resource).await
            }
            Self::ApiGatewayDomainName => {
                delete_apigateway_domain_name(ctx, region, physical_resource_id).await
            }
            // The delete addresses the mapping by the PAIR API Gateway keys one by — the
            // domain name and the base path — both of which are on the resource's own
            // declaration.
            Self::ApiGatewayBasePathMapping => {
                delete_apigateway_base_path_mapping(ctx, region, resource).await
            }
            Self::ApiGatewayV2Api => {
                delete_apigatewayv2_api(ctx, region, physical_resource_id).await
            }
            Self::ApiGatewayV2DomainName => {
                delete_apigatewayv2_domain_name(ctx, region, physical_resource_id).await
            }
            // The delete addresses the mapping by the PAIR API Gateway v2 keys one by, and
            // the domain half of that pair is on the resource's own declaration.
            Self::ApiGatewayV2ApiMapping => {
                delete_apigatewayv2_api_mapping(ctx, region, resource).await
            }
            // Each addressed by the api that holds it and its own identifier, both of
            // which the resource's record carries.
            Self::ApiGatewayV2Integration => {
                delete_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_INTEGRATION_CHILD)
                    .await
            }
            Self::ApiGatewayV2Route => {
                delete_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_ROUTE_CHILD).await
            }
            Self::ApiGatewayV2Stage => {
                delete_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_STAGE_CHILD).await
            }
            Self::ApiGatewayV2Deployment => {
                delete_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_DEPLOYMENT_CHILD)
                    .await
            }
            Self::ApiGatewayV2Authorizer => {
                delete_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_AUTHORIZER_CHILD)
                    .await
            }
            // The delete addresses the rule by the ARN API Gateway v2 minted for it, which is
            // this resource's physical id and carries both of the labels that delete takes:
            // the custom domain name the rule lives on and the rule's own id.
            Self::ApiGatewayV2RoutingRule => {
                delete_apigatewayv2_routing_rule(ctx, region, resource).await
            }
            Self::CodeCommitRepository => {
                delete_codecommit_repository(ctx, region, physical_resource_id).await
            }
            // Neither CodeArtifact type is addressed by its physical id: each type's `Ref`
            // is the resource's ARN, so the names its delete takes come off the resource's
            // own record ([`declared_name`], [`codeartifact_property`]).
            Self::CodeArtifactDomain => {
                delete_codeartifact_domain(ctx, region, &declared_name(resource)).await
            }
            Self::CodeArtifactRepository => {
                delete_codeartifact_repository(
                    ctx,
                    region,
                    &codeartifact_property(resource, CODEARTIFACT_DOMAIN_PROPERTY),
                    &codeartifact_property(resource, CODEARTIFACT_DOMAIN_OWNER_PROPERTY),
                    &declared_name(resource),
                )
                .await
            }
            // The delete addresses the group by the ARN CodeBuild keyed it by, which the
            // resource's own record carries.
            Self::CodeBuildReportGroup => {
                delete_codebuild_report_group(ctx, region, resource).await
            }
            // A project is addressed by the NAME it was created under, which is this
            // resource's physical id.
            Self::CodeBuildProject => {
                delete_codebuild_project(ctx, region, physical_resource_id).await
            }
            // A fleet is addressed by the ARN CodeBuild keyed it by, which this resource's
            // own record carries as its physical id.
            Self::CodeBuildFleet => delete_codebuild_fleet(ctx, region, resource).await,
            Self::SesConfigurationSet => {
                delete_ses_configuration_set(ctx, region, physical_resource_id).await
            }
            // An identity is addressed by its own TEXT, which is this resource's physical id.
            Self::SesEmailIdentity => {
                delete_ses_email_identity(ctx, region, physical_resource_id).await
            }
            Self::SesReceiptRuleSet => {
                delete_ses_receipt_rule_set(ctx, region, physical_resource_id).await
            }
            // A rule is addressed by its own name AND the set that holds it, and only the
            // resource's own declaration carries that set.
            Self::SesReceiptRule => delete_ses_receipt_rule(ctx, region, resource).await,
            Self::BackupVault => {
                delete_backup_vault(ctx, region, physical_resource_id).await
            }
            Self::BackupPlan => delete_backup_plan(ctx, region, physical_resource_id).await,
            // The delete addresses the selection by the PAIR AWS Backup keys one by, and
            // the plan half of that pair is on the resource's own declaration.
            Self::BackupSelection => delete_backup_selection(ctx, region, resource).await,
            // The delete addresses the guardrail by the identifier Amazon Bedrock keyed it
            // by, which is this resource's physical id.
            Self::BedrockGuardrail => {
                delete_bedrock_guardrail(ctx, region, physical_resource_id).await
            }
            // The delete addresses the knowledge base by the id Amazon Bedrock Agent minted
            // for it, which is this resource's physical id.
            Self::BedrockKnowledgeBase => {
                delete_bedrock_knowledge_base(ctx, region, physical_resource_id).await
            }
            // Removed by the same knowledge-base-and-data-source pair the create was answered
            // with, which is this resource's physical id.
            Self::BedrockDataSource => delete_bedrock_data_source(ctx, region, resource).await,
            // The delete addresses the flow by the ARN Amazon Bedrock Agent minted for it,
            // which is this resource's physical id.
            Self::BedrockFlow => delete_bedrock_flow(ctx, region, physical_resource_id).await,
            // The delete addresses the session by the ARN the Bedrock Agent runtime minted for
            // it, which is this resource's physical id.
            Self::BedrockSession => {
                delete_bedrock_session(ctx, region, physical_resource_id).await
            }
            Self::BedrockAgentCoreWorkloadIdentity => {
                delete_workload_identity(ctx, region, physical_resource_id).await
            }
            // Each delete addresses the resource by the identifier that control plane minted
            // for it, which is this resource's physical id — a gateway target by the gateway
            // and the target id that id joins.
            Self::BedrockAgentCoreRuntime => {
                delete_agentcore_runtime(ctx, region, physical_resource_id).await
            }
            Self::BedrockAgentCoreMemory => {
                delete_agentcore_memory(ctx, region, physical_resource_id).await
            }
            Self::BedrockAgentCoreGateway => {
                delete_agentcore_gateway(ctx, region, physical_resource_id).await
            }
            Self::BedrockAgentCoreGatewayTarget => {
                delete_agentcore_gateway_target(ctx, region, physical_resource_id).await
            }
            Self::BedrockAgentCoreSandbox(sandbox) => {
                delete_agentcore_sandbox(ctx, region, sandbox, physical_resource_id).await
            }
            // The delete addresses the alarm by the name it was created under, which is this
            // resource's physical id.
            Self::CloudWatchAlarm => {
                delete_metric_alarm(ctx, region, physical_resource_id).await
            }
            // The same delete, addressing the composite alarm by the name it was created
            // under, which is this resource's physical id.
            Self::CloudWatchCompositeAlarm => {
                delete_composite_alarm(ctx, region, physical_resource_id).await
            }
            // The delete addresses the service by its own name inside the cluster it runs
            // on, which is what the ARN that is this resource's physical id carries.
            Self::EcsService => Box::pin(delete_ecs_service(ctx, region, resource)).await,
            // The delete addresses the spending limit by the ARN Amazon Braket minted for
            // it, which is this resource's physical id.
            Self::BraketSpendingLimit => {
                delete_braket_spending_limit(ctx, region, physical_resource_id).await
            }
            // Each delete addresses its resource by the identifier AWS Direct Connect minted
            // for it, which the ARN that is this resource's physical id carries.
            Self::DirectConnectGateway => {
                delete_direct_connect_gateway(ctx, region, physical_resource_id).await
            }
            Self::DirectConnectResiliencyGroup => {
                delete_resiliency_group(ctx, region, physical_resource_id).await
            }
            // The delete addresses the suite by the id AWS IoT Core Device Advisor drew for
            // it, which is this resource's physical id.
            Self::IotCoreDeviceAdvisorSuiteDefinition => {
                delete_suite_definition(ctx, region, physical_resource_id).await
            }
            // Each delete addresses its resource by the ARN that service minted for it, which
            // is this resource's physical id.
            Self::BcmDashboard => delete_bcm_dashboard(ctx, region, physical_resource_id).await,
            Self::BcmScheduledReport => {
                delete_bcm_scheduled_report(ctx, region, physical_resource_id).await
            }
            // The objective is deleted by the ARN it is addressed by, which is this resource's
            // physical id; the account's grouping configuration by the one call that takes it
            // away, which carries no identifier; and the instrumentation point by the identity
            // the stack holds for it — the four dimensions its declaration states and the
            // location hash that service computed.
            Self::ApplicationSignalsServiceLevelObjective => {
                delete_slo(ctx, region, physical_resource_id).await
            }
            Self::ApplicationSignalsGroupingConfiguration => {
                delete_grouping_configuration(ctx, region).await
            }
            Self::ApplicationSignalsInstrumentationConfig => {
                delete_instrumentation_config(ctx, region, resource).await
            }
            // The delete addresses the dataset by the ARN Clean Rooms ML minted for it.
            Self::CleanRoomsMlTrainingDataset => {
                delete_training_dataset(ctx, region, physical_resource_id).await
            }
            // The delete addresses the entity by the id Amplify UI Builder minted for it,
            // which is this resource's physical id, inside the app environment its declaration
            // names.
            Self::AmplifyUiBuilderComponent => {
                delete_ui_entity(ctx, region, UiEntity::Component, resource).await
            }
            Self::AmplifyUiBuilderForm => {
                delete_ui_entity(ctx, region, UiEntity::Form, resource).await
            }
            Self::AmplifyUiBuilderTheme => {
                delete_ui_entity(ctx, region, UiEntity::Theme, resource).await
            }
            // The delete addresses the app by the id AWS Amplify minted for it, which is the
            // id inside the ARN that is this resource's physical id.
            Self::AmplifyApp => delete_amplify_app(ctx, region, resource).await,
            // The delete addresses the branch by the name it was created under, inside the app
            // the ARN that is this resource's physical id carries.
            Self::AmplifyBranch => delete_amplify_branch(ctx, region, resource).await,
            // The delete addresses the environment by the name Amazon MWAA keyed it by,
            // which is this resource's physical id.
            Self::MwaaEnvironment => {
                delete_mwaa_environment(ctx, region, physical_resource_id).await
            }
            // The delete addresses the directory by the id Directory Service minted for it,
            // which is this resource's physical id.
            Self::DirectoryServiceDirectory(_) => {
                delete_ds_directory(ctx, region, physical_resource_id).await
            }

            // A rule is addressed by its name or by the ARN X-Ray minted — "either name
            // or ARN, but not both" — and the ARN is this resource's physical id.
            Self::XraySamplingRule => delete_sampling_rule(ctx, region, resource).await,
            // A group is addressed by its name or by the ARN X-Ray minted, and the ARN is
            // this resource's physical id.
            Self::XrayGroup => delete_xray_group(ctx, region, resource).await,
            // Each of the three AWS Config types is addressed by its own name, which is
            // this resource's physical id.
            Self::ConfigConfigurationRecorder => {
                delete_configuration_recorder(ctx, region, physical_resource_id).await
            }
            Self::ConfigDeliveryChannel => {
                delete_delivery_channel(ctx, region, physical_resource_id).await
            }
            Self::ConfigConfigRule => {
                delete_config_rule(ctx, region, physical_resource_id).await
            }
            // Each group is addressed by its own name, which is this resource's physical
            // id.
            Self::MemoryDbGroup(group) => {
                delete_memorydb_group(ctx, region, group, physical_resource_id).await
            }
            // The delete addresses the domain by the name Amazon Connect Customer Profiles
            // keyed it by, which is this resource's physical id.
            Self::CustomerProfilesDomain => {
                delete_customer_profiles_domain(ctx, region, physical_resource_id).await
            }
            // The delete addresses the rule by the identifier inside the ARN Recycle Bin
            // minted for it, which is this resource's physical id.
            Self::RbinRule => delete_rbin_rule(ctx, region, physical_resource_id).await,
            // The delete addresses the policy by the id Amazon Data Lifecycle Manager minted for
            // it, which is this resource's physical id.
            Self::DlmLifecyclePolicy => {
                delete_dlm_lifecycle_policy(ctx, region, physical_resource_id).await
            }
            // The delete addresses the entity by the ARN AWS Network Security Manager minted
            // for it, which is this resource's physical id.
            Self::NetworkSecurityManagerEntity(entity) => {
                delete_entity(ctx, region, entity, physical_resource_id).await
            }
            // Each delete addresses its resource by the id Neptune Analytics minted for it,
            // which is this resource's physical id.
            Self::NeptuneGraphGraph => {
                delete_neptune_graph(ctx, region, physical_resource_id).await
            }
            Self::NeptuneGraphGraphSnapshot => {
                delete_graph_snapshot(ctx, region, physical_resource_id).await
            }
            // The delete addresses the hypervisor by the ARN Backup Gateway minted for it,
            // which is this resource's physical id.
            Self::BackupGatewayHypervisor => {
                delete_backup_gateway_hypervisor(ctx, region, physical_resource_id).await
            }
            // The delete addresses the association by the CERTIFICATE's ARN, which is this
            // resource's physical id and the one handle `DisassociateCertificate` takes.
            Self::MediaConvertCertificateAssociation => {
                disassociate_mediaconvert_certificate(ctx, region, physical_resource_id).await
            }
        }
    }

}

// ---------------------------------------------------------------------------
// Amazon Data Firehose: AWS::KinesisFirehose::DeliveryStream
// ---------------------------------------------------------------------------

#[cfg(test)]
// The per-type provisioning arms, grouped by the services that own the resources: each file
// holds the create, update, delete, read and drift arms of one group of types, and the enum
// above dispatches to them. Re-exported into this module, which is the one path every caller
// of an arm uses.
mod tests;
mod config_recording;
/// The Amazon CloudWatch alarm arms: visible to the rest of this service's state because
/// the type's own NAME lives here, beside the arms that provision it.
pub(in crate::state) mod cloudwatch_alarms;
pub(in crate::state) mod cloudwatch_composite_alarms;
/// The Amazon ECS SERVICE arms, beside the cluster arms [`application_services`] holds for
/// the same owning service.
mod ecs_services;
/// The Amazon ECS CAPACITY PROVIDER arms, beside the cluster and service arms the same owning
/// service has: visible to the rest of this service's state because the type's own NAME lives
/// here, beside the arms that provision it.
pub(in crate::state) mod ecs_capacity_providers;
/// The Amazon ECS CLUSTER CAPACITY PROVIDER ASSOCIATION arms: the configuration an
/// `AWS::ECS::ClusterCapacityProviderAssociations` declares between a cluster and the providers
/// that run its tasks. Visible to the rest of this service's state because the type's own NAME
/// and the property its identity IS live here, beside the arms that apply it.
pub(in crate::state) mod ecs_cluster_capacity_providers;
/// The Amazon ECS TASK SET arms: the task set an `AWS::ECS::TaskSet` creates in a service whose
/// deployment controller is not the ECS scheduler, and the promotion of one an
/// `AWS::ECS::PrimaryTaskSet` declares. Visible to the rest of this service's state because the
/// type NAMES and the identities they are known by live here, beside the arms that provision
/// them.
pub(in crate::state) mod ecs_task_sets;
mod event_pipes;
/// The Amazon EventBridge EVENT BUS arms: the bus an `AWS::Events::EventBus` declares, which
/// the rules, policies and rule targets of the same stack resolve by name, and the grant an
/// `AWS::Events::EventBusPolicy` installs on one.
mod event_buses;
mod bedrock_guardrails;
mod bedrock_knowledge_bases;
/// The Amazon Bedrock DATA SOURCE arms: the data source an `AWS::Bedrock::DataSource` attaches
/// to one of those knowledge bases, created in the build-time control plane that owns it so the
/// data source the stack reports created is the one `ListDataSources` finds and `GetDataSource`
/// resolves.
mod bedrock_data_sources;
/// The Amazon Bedrock FLOW arms: the flow an `AWS::Bedrock::Flow` declares, created in the
/// build-time control plane that owns it so the handles the stack publishes resolve — for
/// that plane's own reads and for the runtime plane that INVOKES the flow.
mod bedrock_flows;
/// The Amazon Bedrock SESSION arms: the session an `AWS::Bedrock::Session` declares, minted in
/// the Bedrock Agent RUNTIME plane that owns it so the identity the stack publishes is one
/// that service answers for — for its own reads of the session and for every invocation that
/// stores a checkpoint in it.
mod bedrock_sessions;
/// The Amazon SageMaker AI types a stack declares beside the model registry's: the model,
/// the notebook instance and the lifecycle configuration one of those runs. Visible to the
/// rest of this service's state because the type NAMES it owns are what the naming rules and
/// the return-value catalogue key their entries for those types by.
pub(crate) mod sagemaker_resources;
mod braket_spending_limits;
/// The AWS IoT Core Device Advisor SUITE DEFINITION arms: the test suite an
/// `AWS::IoTCoreDeviceAdvisor::SuiteDefinition` declares, recorded in the service that owns it
/// so a client's own reads — and a suite run started against it — resolve the id the stack
/// publishes.
mod iot_core_device_advisor_suite_definitions;
/// The Billing and Cost Management Dashboards arms: the dashboard an `AWS::BCM::Dashboard`
/// declares and the scheduled report an `AWS::BCM::ScheduledReport` declares, created in the
/// service that owns them so a client's own reads — and an execution of the report — resolve
/// the ARNs the stack publishes.
mod bcm_dashboards;
/// The CloudWatch Application Signals arms: the service level objective, the account's
/// grouping configuration and the dynamic instrumentation point a template declares there,
/// created in the service that owns them so a client's own reads — and the SDKs that fetch an
/// instrumentation configuration — resolve what the stack publishes.
mod application_signals;
/// The Clean Rooms ML TRAINING DATASET arms: the dataset an
/// `AWS::CleanRoomsML::TrainingDataset` declares, recorded in the service that owns it so a
/// client's own reads resolve the ARN the stack publishes.
mod cleanrooms_ml_training_datasets;
/// The AWS Backup Gateway HYPERVISOR arms: the on-premises host an
/// `AWS::BackupGateway::Hypervisor` declares, imported into the service that owns it so a
/// client's own reads — and the gateway a later declaration associates with it — resolve the
/// ARN the stack publishes.
mod backup_gateway_hypervisors;
mod amplify_ui_builder;
mod amplify_apps;
mod amplify_branches;
mod mwaa_environments;
/// The Amazon Connect Customer Profiles DOMAIN arms: the customer-data domain an
/// `AWS::CustomerProfiles::Domain` declares, created in the service that owns it so a client's
/// own reads and the domain's data plane resolve the name the stack publishes.
mod customer_profiles_domains;
/// The Recycle Bin RETENTION RULE arms: the rule an `AWS::Rbin::Rule` declares, created in the
/// service that owns it so a client's own reads resolve the identity the stack publishes.
/// Visible to the rest of this service's state because the LOCK STATE a declaration asks for
/// is read by the return-value catalogue too, beside the arms that provision the rule.
pub(in crate::state) mod rbin_rules;
/// The Amazon Data Lifecycle Manager LIFECYCLE POLICY arms: the policy an
/// `AWS::DLM::LifecyclePolicy` declares, created in the service that owns it so a client's own
/// `GetLifecyclePolicy` and `GetLifecyclePolicies` resolve the identity the stack publishes.
mod dlm_lifecycle_policies;
/// The AWS Network Security Manager arms: the rule, the template, the policy and the scope an
/// `AWS::NetworkSecurityManager::*` declares, created in the service that owns them so a
/// client's own `ListRules`, `ListTemplates`, `ListPolicies` and `Get…` resolve the identities
/// the stack publishes. Visible to the rest of this service's state because WHICH of the four
/// entities a resource is travels with the dispatch variant.
pub(in crate::state) mod network_security_manager;
/// The Neptune Analytics arms: the graph an `AWS::NeptuneGraph::Graph` declares and the
/// snapshot of one an `AWS::NeptuneGraph::GraphSnapshot` declares, created in the service that
/// owns them so a client's own reads resolve the identities the stack publishes.
mod neptune_graphs;
/// The AWS Glue TABLE OPTIMIZER arms: the optimizer an `AWS::Glue::TableOptimizer` declares
/// on one of the Data Catalog tables [`platform_services`] creates, which that service keys
/// by the catalog, the database, the table and the optimizer's own type together.
mod glue_table_optimizers;
/// The AWS Glue TRIGGER arms: the trigger an `AWS::Glue::Trigger` declares, which is what
/// starts the jobs [`platform_services`] creates. Visible to the rest of this service's state
/// because the type's own NAME lives here, beside the arms that provision it, and the table of
/// properties a change REPLACES the resource over keys its entry by it.
pub(in crate::state) mod glue_triggers;
/// The AWS Elemental MediaConvert CERTIFICATE ASSOCIATION arm: the trust an
/// `AWS::MediaConvert::CertificateAssociation` declares between an AWS Certificate Manager
/// certificate and the account's MediaConvert service, made in the service that owns it so
/// that service really accepts the certificate and Certificate Manager's own report names it
/// as using one.
mod mediaconvert_certificates;
/// The two LAKE FORMATION PERMISSION types' arms: the one grant an
/// `AWS::LakeFormation::Permissions` or an `AWS::LakeFormation::PrincipalPermissions`
/// declares, made in the service that owns it so a client's own `ListPermissions` reads back
/// what the stack granted. Visible to the rest of this service's state because both types'
/// own NAMES live here, beside the arms that provision them, and the return-value catalogue
/// computes each type's published identity from the same rules.
pub(in crate::state) mod lake_formation_permissions;
mod data_and_storage;
/// The in-place CHANGE of an `AWS::KinesisFirehose::DeliveryStream`, beside the create and
/// delete arms [`data_and_storage`] holds for the same owning service: applying a changed
/// destination declaration is one concern of its own, because Amazon Data Firehose addresses
/// a destination by the version and the id IT keeps rather than by anything the template
/// states.
mod firehose_delivery_streams;
mod iam_identities;
mod application_services;
mod platform_services;
/// The Amazon SES email IDENTITY arms, beside the configuration-set and receipt-rule arms
/// [`platform_services`] holds for the same owning service: an identity is created through
/// that service's API v2, which owns the type's properties, so its create, delete and read
/// are their own concern.
mod ses_email_identities;
/// The AWS X-Ray GROUP arms, beside the sampling-rule arms of the same service in
/// [`platform_services`]: a group's identity is an ARN X-Ray draws per group, so its create,
/// delete and read are their own concern.
mod xray_groups;
mod route53_policies;
/// The FLEET types a stack launches instances with — the Amazon EC2 launch template and the
/// Auto Scaling group that launches from one: visible to the rest of this service's state
/// because the INSTANCES a group holds are read there too, where the signals a resource's
/// `CreationPolicy` waits for are accounted for
/// ([`super::provisioning::instance_signals`]).
pub(super) mod compute_fleet;
/// The scaling policies a stack attaches to an Auto Scaling group of [`compute_fleet`]'s.
pub(in crate::state) mod auto_scaling_policies;
mod network_and_delivery;
/// Applying a CHANGED declaration to an Amazon EC2 resource that already exists: the calls
/// each of that service's types takes a new property value through, and the tag reconciliation
/// every one of them shares. Visible to the rest of this service's state because the EC2 types
/// whose arms live beside the deployment algorithm reach the same calls
/// ([`super::provisioning::ec2_vpc_network`], [`super::provisioning::ec2_instance`]).
pub(in crate::state) mod ec2_in_place;
/// The Amazon EKS arms of the two types a stack declares ON a cluster — the add-on and the
/// managed node group — beside the cluster's own arms in [`network_and_delivery`]: each is a
/// child resource addressed by the cluster that holds it and its own name, which is a
/// concern of its own.
mod eks_cluster_workloads;
/// The Amazon S3 Tables arms: the table bucket a stack creates in that service, which is
/// keyed by its own ARN rather than by a name.
mod s3_table_buckets;
mod ssm_associations;
/// The AWS Systems Manager MAINTENANCE WINDOW arms: visible to the rest of this service's
/// state because both types' own NAMES live here, beside the arms that provision them.
pub(in crate::state) mod ssm_maintenance_windows;
/// The AWS Systems Manager DOCUMENT arms: visible to the rest of this service's state because
/// the type's own NAME lives here, beside the arms that provision it.
pub(in crate::state) mod ssm_documents;
mod resource_groups;
/// The Amazon CloudWatch Application Insights arm: the application an
/// `AWS::ApplicationInsights::Application` declares, created in the service that owns it so a
/// client's own read of that service resolves the application the stack publishes.
mod application_insights;
/// The AWS Batch JOB DEFINITION arms, beside the compute environment, job queue and service
/// environment arms the same owning service has in [`network_and_delivery`]: a revision's
/// identity is the revision-qualified ARN that service allocates, which makes its create,
/// delete and read a concern of their own.
mod batch_job_definitions;
pub(super) mod elasticache_caches;
/// The Amazon RDS DB CLUSTER arm: the cluster an `AWS::RDS::DBCluster` declares, created in
/// the service that owns it so a member DB instance of that cluster resolves it.
pub(super) mod rds_db_clusters;
/// The IAM role associations an `AWS::RDS::DBInstance` or an `AWS::RDS::DBCluster` declares,
/// which are calls of their own on the resource the type's own arms create.
mod rds_role_associations;
/// `AWS::RDS::DBInstance`'s half of the role-association record, beside the walk that takes
/// it: the DB instance is the type whose create and modify carry no `AssociatedRoles` member
/// at all.
mod rds_db_instance_roles;
/// The endpoints an `AWS::RDS::DBCluster` and an `AWS::RDS::DBInstance` publish, read off
/// Amazon RDS once it reports the resource serving so a stack output carries the address a
/// client really connects to.
mod rds_published_endpoints;
/// The Amazon RDS OPTION GROUP arm: the group an `AWS::RDS::OptionGroup` declares, created in
/// the service that owns it so a DB instance's `OptionGroupName` and a client's own read of
/// that service resolve the name the stack publishes.
pub(super) mod rds_option_groups;
pub(super) mod redshift_parameter_groups;
/// The Amazon Redshift SNAPSHOT COPY GRANT arm: the grant an
/// `AWS::Redshift::SnapshotCopyGrant` declares, created in the service that owns it so a
/// client's own read of that service resolves the name the stack publishes.
pub(super) mod redshift_snapshot_copy_grants;
/// The Amazon MemoryDB group arms: the parameter group and the subnet group a cluster's
/// configuration is declared with, created in the service that owns them so a client's own
/// read of that service resolves the names and ARNs the stack publishes.
pub(super) mod memorydb_groups;
/// The AWS CodePipeline WEBHOOK arms, beside the pipeline and custom-action-type arms the
/// same owning service has in [`application_services`]: a webhook is a resource of its own in
/// that API, created by `PutWebhook` and reported by `ListWebhooks`, so its create, delete
/// and read are their own concern.
mod codepipeline_webhooks;
/// The AWS CodeBuild COMPUTE FLEET arms: the fleet an `AWS::CodeBuild::Fleet` declares,
/// created in CodeBuild itself so the ARN the stack publishes is one `ListFleets` reports.
/// Visible to the rest of this service's state because the type's own NAME lives here, beside
/// the arms that provision it, and the naming rules and the identity catalogue key their
/// entries for it by that name.
pub(in crate::state) mod codebuild_fleets;
/// The per-type READ arm of every type here — what the owning service reports a
/// provisioned resource's configuration to be now, which is what drift detection compares
/// a stack's expected configuration against. One `impl Provisioner` of its own, so this
/// file holds the dispatch enum and the write arms.
mod reported_configuration;
/// The Amazon EMR arms of the types a stack declares BESIDE a cluster — the security
/// configuration, the Studio and a Studio's session mapping — whose own arms are
/// [`data_and_storage`]'s. Visible to the rest of this service's state because those three
/// types' own NAMES live here, beside the arms that provision them, and the naming rules and
/// the return-value catalogue key their entries for them by those names.
pub(in crate::state) mod emr_resources;
use codebuild_fleets::*;
/// The catalogue entries of the Amazon Bedrock AgentCore family, beside that family's own
/// resource-type NAMES. Visible to the rest of this service's state for the reason
/// [`emr_resources`]' are: the registry of recognised types, the return-value catalogue and
/// the primary-identifier table each key an entry by one of those names.
pub(in crate::state) mod type_registry;
pub(in crate::state) use type_registry::*;
/// The Amazon Bedrock AgentCore arms: the agent runtime's and the memory's, plus the readers
/// every arm of that family shares. Visible to the rest of this service's state because this
/// family's own resource-type names live beside those arms.
pub(in crate::state) mod bedrock_agentcore;
/// The same family's gateway, gateway-target and sandbox arms.
pub(in crate::state) mod bedrock_agentcore_gateways;
use bedrock_agentcore::*;
use bedrock_agentcore_gateways::*;
use codepipeline_webhooks::*;
use memorydb_groups::*;
use elasticache_caches::*;
use rds_db_clusters::*;
use redshift_parameter_groups::*;
use redshift_snapshot_copy_grants::*;
use ssm_associations::*;
use ssm_maintenance_windows::*;
use ssm_documents::*;
use resource_groups::*;
use application_insights::*;
use auto_scaling_policies::*;
use batch_job_definitions::*;
use compute_fleet::*;
pub(crate) use network_and_delivery::*;
use ec2_in_place::{
    applied_in_place, retag_ec2_resource, update_ec2_security_group, update_ec2_subnet,
    update_ec2_vpc,
};
use eks_cluster_workloads::*;
use s3_table_buckets::*;
use platform_services::*;
use ses_email_identities::*;
use xray_groups::*;
use route53_policies::*;
pub(crate) use application_services::*;
use cloudwatch_alarms::*;
use cloudwatch_composite_alarms::*;
use ecs_services::*;
use ecs_capacity_providers::*;
use ecs_cluster_capacity_providers::*;
use ecs_task_sets::*;
use config_recording::*;
use bedrock_guardrails::*;
use bedrock_knowledge_bases::*;
use bedrock_data_sources::*;
use bedrock_flows::*;
use bedrock_sessions::*;
use sagemaker_resources::*;
/// The AWS Direct Connect arms: the gateway an `AWS::DirectConnect::DirectConnectGateway`
/// declares and the group an `AWS::DirectConnect::ResiliencyGroup` declares, created in the
/// service that owns them so a client's own `DescribeDirectConnectGateways`,
/// `GetResiliencyGroup` and `ListResiliencyGroups` resolve the identities the stack publishes.
mod directconnect_resources;
use directconnect_resources::*;
use braket_spending_limits::*;
use iot_core_device_advisor_suite_definitions::*;
use bcm_dashboards::*;
use application_signals::*;
use cleanrooms_ml_training_datasets::*;
use backup_gateway_hypervisors::*;
use amplify_ui_builder::*;
use amplify_apps::*;
use amplify_branches::*;
use mwaa_environments::*;
/// The two Directory Service directory arms, whose own `DirectoryKind` says which type an arm
/// is working on. Visible to the rest of this service's state because that kind travels on the
/// `Provisioner` variant the catalogue above decides.
pub(in crate::state) mod directory_service_directories;
use directory_service_directories::*;
use customer_profiles_domains::*;
use rbin_rules::*;
use dlm_lifecycle_policies::*;
use network_security_manager::*;
use neptune_graphs::*;
use glue_table_optimizers::*;
use glue_triggers::*;
use mediaconvert_certificates::*;
use lake_formation_permissions::*;
use iam_identities::*;
use data_and_storage::*;
use firehose_delivery_streams::*;
use emr_resources::*;

/// The Amazon API Gateway v2 ROUTING RULE arms: the rule an `AWS::ApiGatewayV2::RoutingRule`
/// declares on a custom domain name of that control plane, created there so a client's own
/// `GetRoutingRule` and `ListRoutingRules` resolve the identity the stack publishes. Visible
/// to the rest of this service's state because the return-value catalogue reads the
/// declaration's domain-name ARN through the same reading these arms address the rule by.
pub(in crate::state) mod apigatewayv2_routing_rules;
pub(in crate::state) use apigatewayv2_routing_rules::*;

/// The AWS Client VPN family of Amazon EC2 — the endpoint and the ingress authorization rule
/// of one — whose arms are [`client_vpn`]'s. Visible to the rest of this service's state
/// because both types' own NAMES live there, beside the arms that provision them, and the
/// catalogue of declarable types, the naming rules and the return-value catalogue key their
/// entries for them by those names.
pub(in crate::state) mod client_vpn;
use client_vpn::*;

/// What the owning service says about a resource's LIFECYCLE PROGRESS — whether a created
/// resource has come up, what it publishes once it has, and whether a deleted one is gone —
/// whose arms are [`lifecycle_progress`]'s: one concern of this catalogue, in a file of its
/// own so this one stays inside the line bound a change may touch.
mod lifecycle_progress;
/// Applying a CHANGED declaration to a resource the stack ALREADY has — the configuration
/// update and the re-tagging arms of this catalogue — which is the concern beside its create,
/// its lifecycle progress above and its delete, in a file of its own for the same reason.
mod in_place_changes;

/// Amazon CloudWatch's wire identity, for the ROLLBACK TRIGGER monitoring beside this
/// catalogue ([`super::rollback_triggers`]): that module reads a trigger alarm's state out of
/// the same service this catalogue's `AWS::CloudWatch::*` arms write to, so the signing name
/// and the target prefix are read in ONE place and the two cannot drift apart.
pub(in crate::state) use data_and_storage::{CLOUDWATCH_SIGNING_NAME, CLOUDWATCH_TARGET_PREFIX};

/// The STATIC-WEBSITE configuration an `AWS::S3::Bucket` declares, read by the per-type
/// provider's UPDATE half beside this catalogue ([`super::provisioning::s3_bucket`]): the
/// create and a changed declaration send the SAME translated request, so the template's
/// spelling is read in ONE place and the two halves cannot drift apart.
pub(in crate::state) use data_and_storage::{declared_website_configuration, WEBSITE_CONFIGURATION};
