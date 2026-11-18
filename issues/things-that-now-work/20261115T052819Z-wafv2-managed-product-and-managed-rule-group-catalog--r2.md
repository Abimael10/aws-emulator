Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-catalog-describe-all-managed-products
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-catalog-describe-all-managed-products wafv2-catalog-describe-all-managed-products-cloudfront wafv2-catalog-list-available-managed-rule-groups wafv2-catalog-list-available-managed-rule-groups-cloudfront wafv2-managed-rule-group-and-marketplace-product-discovery wafv2-webacl-full-lifecycle-with-managed-rules
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|DescribeAllManagedProducts|2xx->2xx|$.ManagedProducts[*]
Issue-Key: wafv2|ListAvailableManagedRuleGroups|2xx->2xx|$.ManagedRuleGroups[Name="AWSManagedRulesACFPRuleSet"].Description
Issue-Key: wafv2|ListAvailableManagedRuleGroups|2xx->2xx|$.ManagedRuleGroups[Name="AWSManagedRulesATPRuleSet"].Description
Issue-Key: wafv2|ListAvailableManagedRuleGroups|2xx->2xx|$.ManagedRuleGroups[Name="AWSManagedRulesAntiDDoSRuleSet"].Description
Issue-Commit: d2d81c0034aa9e73de1da910ad0ee37749603f85

# wafv2 managed-product and managed-rule-group catalog content diverges from AWS's published catalog

> _Note: this issue was identified against the code base at commit `d2d81c0034aa9e73de1da910ad0ee37749603f85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAllManagedProducts

## Description
Two net-new root causes in the wafv2 read-only managed-rule catalog, neither marked OWNED nor blocked by a stub in the replay report. They are bundled because they share one service token (wafv2) and one underlying fault: the bundled catalog the emulator serves is not AWS's published managed-rule/managed-product catalog. A fixer MUST close BOTH causes before submitting.

=== Cause 1 of 2: DescribeAllManagedProducts serves only AWS-vendor rule sets and omits every Amazon Web Services Marketplace managed product ===
Fixture/step: wafv2-catalog-describe-all-managed-products#task#1 (also wafv2-catalog-describe-all-managed-products-cloudfront#task#1).

AWS: Smithy (wafv2, com-amazonaws-wafv2-awswaf-20190729) — DescribeAllManagedProducts takes only Scope (required, enum) and returns ManagedProducts (list of ManagedProductDescriptor). ManagedProductDescriptor is documented as "The properties of a managed product, such as an Amazon Web Services Managed Rules rule group OR an Amazon Web Services Marketplace managed rule group", and its ProductLink member is documented "For Amazon Web Services Marketplace managed rule groups only, the link to the rule group product page." The response is therefore the union of both vendor families: AWS-vendor rule sets AND third-party Marketplace seller products, each descriptor carrying VendorName, ManagedRuleSetName (@pattern ^[\w\-]+$, @length 1-128), ProductId, ProductTitle, ProductDescription, SnsTopicArn (@length 20-2048), IsVersioningSupported and IsAdvancedManagedRuleSet, plus ProductLink for Marketplace entries. The recording shows the catalog AWS serves for this Scope: a 46-entry list whose members include third-party seller products (the VendorName is the seller, the SnsTopicArn is the SELLER's notification topic in the seller's own account, and ProductLink points at that product's AWS Marketplace page), interleaved with the AWS-vendor rule sets. The set of products, their vendors and their identifiers are the requirement; a particular recording's ProductId GUIDs, marketplace URLs and seller account numbers are values filling that catalog, not part of the contract.

Emulator: the response is a 200 whose ManagedProducts list bears no relation to AWS's catalog — EVERY index from $.ManagedProducts[0] through $.ManagedProducts[45] diverges. The emulator emits only AWS-vendor descriptors (VendorName "AWS", ManagedRuleSetName/ProductId both set to the rule-set name, e.g. ManagedRuleSetName "AWSManagedRulesCommonRuleSet" / ProductId "AWSManagedRulesCommonRuleSet"), with SnsTopicArn pointed at a single emulator-local topic in the zero account rather than each vendor's own topic, and with ProductLink omitted throughout. No Marketplace seller product appears at any index, and ProductId is not a distinct unique identifier but a copy of the rule-set name.

Why: DescribeAllManagedProducts is the discovery call a client makes to enumerate what it may subscribe to and reference from a ManagedRuleGroupStatement. A client that enumerates the catalog to find a third-party vendor's group finds none, so any code path that selects a non-AWS vendor, follows ProductLink to a Marketplace listing, or subscribes to a vendor's SnsTopicArn for change notifications finds nothing to act on and silently takes its empty-result branch. Because ProductId duplicates the rule-set name rather than being a separate identifier, a client that round-trips a ProductId into update/delete calls also keys on the wrong value.

Fix: the wafv2 DescribeAllManagedProducts handler and the bundled managed-product catalog in wafv2 state that it reads — the catalog must carry both vendor families, with per-vendor SnsTopicArn, a ProductId distinct from ManagedRuleSetName, and ProductLink populated on Marketplace entries.

Traces: wafv2-catalog-describe-all-managed-products#task#1, wafv2-catalog-describe-all-managed-products-cloudfront#task#1.

=== Cause 2 of 2: ListAvailableManagedRuleGroups returns stale Descriptions for the three intelligent-threat-mitigation rule groups ===
Fixture/step: wafv2-catalog-list-available-managed-rule-groups#task#1 (also wafv2-catalog-list-available-managed-rule-groups-cloudfront#task#1, wafv2-managed-rule-group-and-marketplace-product-discovery#task#1, wafv2-webacl-full-lifecycle-with-managed-rules#task#1).

AWS: Smithy — ListAvailableManagedRuleGroups returns ManagedRuleGroups (list of ManagedRuleGroupSummary) whose Description member targets EntityDescription (@length 1-256, @pattern ^[\w+=:#@/\-,\.][\w+=:#@/\-,\.\s]+[\w+=:#@/\-,\.]$) and is documented as "The description of the managed rule group, provided by Amazon Web Services Managed Rules or the Amazon Web Services Marketplace seller who manages it." The Description is thus not free-form emulator prose: it is the vendor-published text for that group, keyed by VendorName+Name, and is fixed text with no request-derived parts. The recording carries AWS's current published text for three groups: AWSManagedRulesATPRuleSet ("...With account takeover prevention, you can prevent unauthorized access that may lead to fraudulent activities, or inform legitimate users to take a preventive action."), AWSManagedRulesACFPRuleSet ("...Fraudulent accounts can be used for activities such as obtaining sign-up bonuses and impersonating legitimate users.") and AWSManagedRulesAntiDDoSRuleSet ("Provides protection against DDoS attacks targeting the application layer, also known as Layer 7 attacks.").

Emulator: the call answers 200 and the group names, vendors and VersioningSupported flags all match, but the Description for those same three groups is an older/paraphrased variant of the vendor text — e.g. for AWSManagedRulesAntiDDoSRuleSet it returns "Provides protection against distributed denial of service (DDoS) events. This rule group contains rules that detect requests that are contributing to a DDoS event against your protected resources and challenge or block them." where AWS returns "Provides protection against DDoS attacks targeting the application layer, also known as Layer 7 attacks."; the ATP and ACFP entries diverge the same way, each rewording the clause that names what the protection prevents. Every other group's Description in the same list matches, so the catalog is right in shape and wrong only in these three vendor strings.

Why: the Description is the only human-readable text a console, a catalog browser or a `wafv2 list-available-managed-rule-groups` caller shows when an operator chooses a group to add to a web ACL, and it is the string a golden-file or snapshot assertion over the catalog pins. Serving superseded vendor text makes the emulator's catalog disagree with AWS for exactly the three intelligent-threat-mitigation groups whose selection matters most, and any client that matches on the documented description to pick a group selects nothing.

Fix: the bundled managed-rule-group catalog in wafv2 state that ListAvailableManagedRuleGroups serializes — the Description for each group must be the vendor-published text for that VendorName+Name, refreshed for the ATP, ACFP and AntiDDoS rule sets.

Traces: wafv2-catalog-list-available-managed-rule-groups#task#1, wafv2-catalog-list-available-managed-rule-groups-cloudfront#task#1, wafv2-managed-rule-group-and-marketplace-product-discovery#task#1, wafv2-webacl-full-lifecycle-with-managed-rules#task#1.

Related open issue that does NOT own either cause (the replay report marks both clusters with an empty owned_by): 20261029T221828Z-apigateway-wafv2-2-net-new-root-causes-describemanagedrulegroup.md concerns DescribeManagedRuleGroup's AvailableLabels for a single group, a different operation and a different member; its replay key covers neither of these catalog clusters.

## Evidence
Cause 1 — DescribeAllManagedProducts catalog is the wrong list (verdict: mismatch, expected_status 200, actual_status 200):
  divergence_key: service=wafv2|DescribeAllManagedProducts|status=200->200|keys=$.ManagedProducts[0] ... $.ManagedProducts[45]  (all 46 indices diverge)
  fixture/step: wafv2-catalog-describe-all-managed-products#task#1; affected also wafv2-catalog-describe-all-managed-products-cloudfront#task#1 (affected_count 2)

  Expected (AWS), $.ManagedProducts[0]: {'VendorName': 'Cyber Security Cloud Inc.', 'ManagedRuleSetName': 'CyberSecurityCloud-APIGatewayServerless-', 'ProductId': '8bbf295b-34b3-40c0-bee7-5ba8f602705f', 'ProductTitle': 'Cyber Security Cloud Managed Rules for AWS WAF -API Gateway/Serverless-', 'ProductDescription': 'The Cyber Security Cloud OWASP ruleset is designed to mitigate and minimize vulnerabilities, including all those on OWASP API Security/Serverless Top 10 Threats.', 'ProductLink': 'http://aws.amazon.com/marketplace/pp/B081TKTBQ8', 'SnsTopicArn': 'arn:aws:sns:us-east-1:343255486711:CyberSecurityCloud-ManagedRule', 'IsVersioningSupported': True, 'IsAdvancedManagedRuleSet': False}
  Actual (emulator), $.ManagedProducts[0]: {'VendorName': 'AWS', 'ManagedRuleSetName': 'AWSManagedRulesCommonRuleSet', 'ProductId': 'AWSManagedRulesCommonRuleSet', 'ProductTitle': 'Core rule set (CRS)', 'ProductDescription': 'Provides protection against exploitation of a wide range of vulnerabilities, including high risk and commonly occurring vulnerabilities described in OWASP publications.', 'SnsTopicArn': 'arn:aws:sns:us-east-1:000000000000:aws-managed-waf-rule-notifications', 'IsVersioningSupported': True, 'IsAdvancedManagedRuleSet': False}  — no ProductLink member at all.
  Expected $.ManagedProducts[1] is likewise a Marketplace seller entry ('CyberSecurityCloud-HighSecurityOWASPSet-', ProductLink http://aws.amazon.com/marketplace/pp/B081TD6XL1, seller SnsTopicArn), where the emulator returns 'AWSManagedRulesAdminProtectionRuleSet'.
  Structural summary: expected list = AWS-vendor rule sets UNION Marketplace seller products, each with its own vendor SnsTopicArn and (for Marketplace) ProductLink, and a ProductId distinct from ManagedRuleSetName. Actual list = AWS-vendor rule sets only, ProductId == ManagedRuleSetName, one shared zero-account SnsTopicArn, ProductLink never emitted. (The specific GUIDs, marketplace URLs and seller account ids above are the recording's values filling the catalog, not the requirement.)

Cause 2 — ListAvailableManagedRuleGroups Description is stale vendor text (verdict: mismatch, expected_status 200, actual_status 200):
  divergence_key: service=wafv2|ListAvailableManagedRuleGroups|status=200->200|keys=$.ManagedRuleGroups[Name="AWSManagedRulesACFPRuleSet"].Description,$.ManagedRuleGroups[Name="AWSManagedRulesATPRuleSet"].Description,$.ManagedRuleGroups[Name="AWSManagedRulesAntiDDoSRuleSet"].Description
  fixture/step: wafv2-catalog-list-available-managed-rule-groups#task#1; affected also wafv2-catalog-list-available-managed-rule-groups-cloudfront#task#1, wafv2-managed-rule-group-and-marketplace-product-discovery#task#1, wafv2-webacl-full-lifecycle-with-managed-rules#task#1 (affected_count 4)

  AWSManagedRulesATPRuleSet.Description
    Expected: 'Provides protection for your login page against stolen credentials, credential stuffing attacks, brute force login attempts, and other anomalous login activities. With account takeover prevention, you can prevent unauthorized access that may lead to fraudulent activities, or inform legitimate users to take a preventive action.'
    Actual:   'Provides protection for your login page against stolen credentials, credential stuffing attacks, brute force login attempts, and other anomalous login activities. With AWS WAF Fraud Control account takeover prevention (ATP), you can prevent unauthorized access that might lead to fraudulent activities, or inform legitimate users to take a preventive action.'
  AWSManagedRulesACFPRuleSet.Description
    Expected: 'Provides protection against the creation of fraudulent accounts on your site. Fraudulent accounts can be used for activities such as obtaining sign-up bonuses and impersonating legitimate users.'
    Actual:   'Provides protection against the creation of fraudulent accounts on your site. Fraudulent accounts can be used for activities such as abusing promotion and sign-up bonuses, impersonating someone, and to carry out phishing and spam attacks.'
  AWSManagedRulesAntiDDoSRuleSet.Description
    Expected: 'Provides protection against DDoS attacks targeting the application layer, also known as Layer 7 attacks.'
    Actual:   'Provides protection against distributed denial of service (DDoS) events. This rule group contains rules that detect requests that are contributing to a DDoS event against your protected resources and challenge or block them.'
  Each Description is fixed vendor text keyed by VendorName+Name with no request-derived parts (Smithy: ManagedRuleGroupSummary.Description -> EntityDescription, @length 1-256). All other groups' Descriptions in the same response matched.

## Steps to Reproduce
Cause 1 (DescribeAllManagedProducts catalog):
1. Call wafv2 DescribeAllManagedProducts with Scope='REGIONAL' (and again with Scope='CLOUDFRONT' — both fixtures diverge identically).
2. Inspect the ManagedProducts list: AWS returns the union of AWS-vendor rule sets and Amazon Web Services Marketplace seller products, where each Marketplace descriptor carries its seller's VendorName, a ProductId distinct from ManagedRuleSetName, a ProductLink to the Marketplace product page, and the SELLER's own SnsTopicArn.
3. Observe the emulator returns only VendorName='AWS' descriptors, with ProductId copied from ManagedRuleSetName, ProductLink absent, and a single zero-account SnsTopicArn — every index 0..45 diverges and no Marketplace product appears.
4. Or replay fixture wafv2-catalog-describe-all-managed-products step task#1 (and wafv2-catalog-describe-all-managed-products-cloudfront step task#1).

Cause 2 (ListAvailableManagedRuleGroups stale Descriptions):
1. Call wafv2 ListAvailableManagedRuleGroups with Scope='REGIONAL' (and Scope='CLOUDFRONT').
2. Locate the entries whose Name is AWSManagedRulesATPRuleSet, AWSManagedRulesACFPRuleSet and AWSManagedRulesAntiDDoSRuleSet.
3. Compare each Description against the vendor-published text AWS returns for that VendorName+Name: all three differ, each rewording the clause naming what the protection prevents (the AntiDDoS entry diverges wholesale). Every other group's Description matches.
4. Or replay fixture wafv2-catalog-list-available-managed-rule-groups step task#1 (also wafv2-catalog-list-available-managed-rule-groups-cloudfront#task#1, wafv2-managed-rule-group-and-marketplace-product-discovery#task#1, wafv2-webacl-full-lifecycle-with-managed-rules#task#1).

## Resolution

Cause 1 — FIXED as to the rule, with the Marketplace family's MEMBERSHIP limited to the listings any
source available here states. The rule: `DescribeAllManagedProducts` reports one descriptor per managed
product of EITHER vendor family — the Amazon Web Services Managed Rules rule groups and the Amazon Web
Services Marketplace sellers' managed rule groups — each carrying its publishing vendor's identity: a
`ProductId` that identifies the product independently of its `ManagedRuleSetName`, the ARN of the
topic that VENDOR announces its changes on, and, for a Marketplace product only, the `ProductLink` to
its product page; what a web ACL may RUN stays the narrower set, because
`ListAvailableManagedRuleGroups` lists only the Amazon Web Services Managed Rules groups and the
Marketplace groups the account is SUBSCRIBED to. The product catalogue in
`services/service-com-amazonaws-wafv2-awswaf-20190729/src/state/managed_products.rs` now carries the
Marketplace family beside the Amazon Web Services Managed Rules rule sets, and both describes project
that one catalogue. A seller's group is published but takes a subscription to use, and nothing in this
API subscribes this account to a seller, so a rule that runs one earns the modelled
`WAFSubscriptionNotFoundException` on `CreateWebACL`, `UpdateWebACL` and `CheckCapacity` — a pair no
vendor publishes stays `WAFNonexistentItemException` — and `ListAvailableManagedRuleGroups` still lists
only the groups this account may run.

Every Marketplace entry is CITED rather than composed: the two listings this report's evidence states
(`CyberSecurityCloud-HighSecurityOWASPSet-` and `CyberSecurityCloud-APIGatewayServerless-`), with the
seller's vendor name, each group's name, each product page's listing code, the seller's notification
topic and whatever of the listing's own text the report states — a title or summary it does not state
is omitted rather than worded by this emulator. The catalogue publishes, per entry, the product's
identifier (derived from the pair that identifies it: stable per product, distinct per product, never
the rule-set name) and the ARN of its seller's topic through the emulator's own ARN builder, because a
seller's GUIDs and account are not observable here. The rest of AWS's 46-entry membership is NOT
served: it is a snapshot of third-party seller inventory no rule of the service produces and no source
available here states, so the replay key for this operation stays red rather than being closed with an
invented inventory. Adding a seller's listing is a data change in that one list.

Cause 2 — FIXED. A catalogue entry's `Description` is the sentence the group's publisher states for
that entry, carried per group and reported verbatim; `src/state/catalog.rs` now holds the publisher's
wording for `AWSManagedRulesATPRuleSet`, `AWSManagedRulesACFPRuleSet` and
`AWSManagedRulesAntiDDoSRuleSet`, and all three replay keys pass.
