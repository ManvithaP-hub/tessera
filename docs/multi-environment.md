# Multiple environments, accounts and regions

This guide covers using Tessera across many clusters (dev, staging,
production), many cloud accounts and many regions: how it keeps them apart,
the guardrails that apply, and the RBAC and IAM setup we recommend.

- [How Tessera separates clusters](#how-tessera-separates-clusters)
- [Setting up many clusters](#setting-up-many-clusters)
- [Environments and production protection](#environments-and-production-protection)
- [Per-cluster settings](#per-cluster-settings)
- [Recommended RBAC](#recommended-rbac)
- [Recommended AWS IAM](#recommended-aws-iam)
- [Organisation policy file](#organisation-policy-file)
- [Activity log](#activity-log)
- [All guardrails at a glance](#all-guardrails-at-a-glance)
- [Troubleshooting](#troubleshooting)

## How Tessera separates clusters

- **One cluster at a time, fully isolated.** Each kubeconfig context gets its
  own connection, so switching from `dev` to `prod` never mixes data.
- **Your identity, per cluster.** Tessera uses the credentials each context
  already has in your kubeconfig, including exec plugins such as
  `aws eks get-token`. It can never do more than `kubectl` could with the same
  context.
- **The right AWS account per cluster.** For cloud checks, Tessera reads the
  `--profile` and `--region` from each context's kubeconfig entry. If your prod
  cluster's kubeconfig uses `--profile prod`, prod load balancers are checked
  with the prod account automatically.
- **The right region per load balancer.** The region is taken from each load
  balancer's DNS name (for example `...us-west-2.elb.amazonaws.com`), so
  clusters in different regions work without extra setup.

## Setting up many clusters

Give each cluster a clear alias that includes its environment and region, and
its own AWS profile:

```sh
aws eks update-kubeconfig --name payments --region us-east-1 --profile dev-sso  --alias dev-use1-payments
aws eks update-kubeconfig --name payments --region us-west-2 --profile prod-sso --alias prod-usw2-payments
```

Tessera then shows `dev-use1-payments` and `prod-usw2-payments` in the
Context menu, and each one uses the right account. Because the names contain
`dev` and `prod`, Tessera also classifies them automatically (see below).

With AWS IAM Identity Center (SSO), define one profile per account in
`~/.aws/config`:

```ini
[sso-session company]
sso_start_url = https://company.awsapps.com/start
sso_region = us-east-1

[profile dev-sso]
sso_session = company
sso_account_id = 111111111111
sso_role_name = ReadOnly

[profile prod-sso]
sso_session = company
sso_account_id = 222222222222
sso_role_name = ReadOnly
```

Run `aws sso login --sso-session company` once a day, and every cluster that
uses those profiles works in Tessera. New contexts appear when you select
**Refresh**.

## Environments and production protection

Tessera sorts every context into **production**, **staging**, **development**
or **unclassified**, based on its name:

| Environment | Default name patterns |
|---|---|
| Production | `*prod*`, `*prd*`, `*production*`, `*live*` |
| Staging | `*stag*`, `*stg*`, `*uat*`, `*preprod*`, `*qa*` |
| Development | `*dev*`, `*test*`, `*sandbox*`, `*lab*`, `kind-*`, `minikube`, `docker-desktop`, `*local*` |

Production patterns win, so `dev-copy-of-prod` counts as production. An
organisation can change the patterns in the [policy file](#organisation-policy-file).

What changes by environment:

| | Production | Staging | Development |
|---|---|---|---|
| Coloured banner and sidebar stripe | Red | Amber | Green |
| Environment shown in the Context menu | Yes | Yes | Yes |
| Reading the cluster | Yes | Yes | Yes |
| Network tests | **Off** by default. If an administrator allows them, you must type the context name to confirm | After you approve the plan | After you approve the plan |

In **Settings → This cluster → Environment** you can mark a cluster as *more*
sensitive than its name suggests (for example, mark an unclassified cluster as
production). You can't mark a production-named cluster as less sensitive from
the app; that takes a change to the patterns in a policy file.

These rules are enforced in the Tessera backend, not just the screen: a
production network test is refused unless the policy allows it **and** the
exact context name is typed.

## Per-cluster settings

**Settings → This cluster** holds settings that apply only to the selected
context:

- AWS profile and region overrides for cloud checks (normally not needed; they
  come from the kubeconfig)
- Probe image for network tests (for clusters that can't pull from Docker Hub)
- Cluster domain (if it isn't `cluster.local`)
- Environment (raise only)

A profile set for one cluster is never used for another.

## Recommended RBAC

**Everyday use: read-only.** Bind users to the `tessera-viewer` ClusterRole in
[`rbac.yaml`](rbac.yaml), or on EKS use the built-in read-only access policy:

```sh
aws eks create-access-entry --cluster-name payments \
  --principal-arn arn:aws:iam::<ACCOUNT_ID>:role/<ReadOnlyRole>

aws eks associate-access-policy --cluster-name payments \
  --principal-arn arn:aws:iam::<ACCOUNT_ID>:role/<ReadOnlyRole> \
  --policy-arn arn:aws:eks::aws:cluster-access-policy/AmazonEKSViewPolicy \
  --access-scope type=cluster
```

**Network tests: only where needed.** Grant `create` and `delete` on `pods` only
in specific namespaces, using a **Role** (not a ClusterRole), and preferably
only on non-production clusters. See
[`rbac-network-tests.yaml`](rbac-network-tests.yaml).

Even if someone has broader rights, Tessera itself only reads, apart from
network tests you approve.

## Recommended AWS IAM

Cloud checks need only these read-only actions ([`iam-policy.json`](iam-policy.json)):

```json
{
  "Version": "2012-10-17",
  "Statement": [{
    "Sid": "TesseraLoadBalancerReadOnly",
    "Effect": "Allow",
    "Action": [
      "elasticloadbalancing:DescribeLoadBalancers",
      "elasticloadbalancing:DescribeTargetGroups",
      "elasticloadbalancing:DescribeTargetHealth",
      "elasticloadbalancing:DescribeInstanceHealth"
    ],
    "Resource": "*"
  }]
}
```

These four actions cover both the newer and the classic load balancer APIs.
Nothing in this policy can change anything. Attach it to the read-only role
each account's profile uses. Tessera additionally refuses to run any AWS
command outside a fixed allow-list of five `describe` calls.

## Organisation policy file

A JSON file lets you set rules for Tessera. Only include the rules you want to
set; everything else keeps its default. Full example:
[`policy.example.json`](policy.example.json).

```json
{
  "allowNetworkTests": true,
  "allowNetworkTestsInProduction": false,
  "allowCloudChecks": true,
  "productionContextPatterns": ["*prod*", "*prd*", "*live*"],
  "allowedContexts": ["*-payments", "*-orders"],
  "hiddenContexts": ["*-legacy-*"],
  "auditLog": true
}
```

| Rule | Default | Effect |
|---|---|---|
| `allowNetworkTests` | `true` | Turn network tests off everywhere |
| `allowNetworkTestsInProduction` | `false` | Allow network tests on production contexts (still needs typed confirmation) |
| `allowCloudChecks` | `true` | Turn cloud load balancer checks off |
| `productionContextPatterns`, `stagingContextPatterns`, `developmentContextPatterns` | see above | How contexts are classified; `*` matches anything |
| `allowedContexts` | all | If set, only matching contexts are shown or usable |
| `hiddenContexts` | none | Matching contexts are hidden and refused |
| `auditLog` | `true` | Record network tests and blocked actions |

**Where it's read from**, in order (later wins):

1. `~/.tessera/policy.json`: your own file
2. The system file, normally managed by IT or MDM, which **overrides** the user's
   file for every rule it sets:
   - macOS: `/Library/Application Support/Tessera/policy.json`
   - Linux: `/etc/tessera/policy.json`
   - Windows: `%ProgramData%\Tessera\policy.json`

Rules set by the system file show as **set by your organisation** in Settings.
If the system file can't be read (for example, invalid JSON), Tessera **fails
safe**: network tests and cloud checks are turned off and a warning is shown.
Policy is re-read on every action, so changes apply without restarting.

## Activity log

Tessera records, on your computer only:

- every network test: when, who (OS user), cluster, environment, namespace,
  target service, probe pods **created** and **confirmed deleted**, and the result
- network tests refused by policy

It warns if a probe pod couldn't be confirmed deleted. See **Settings →
Activity log**. The file is JSON lines, one entry per line, at:

- macOS: `~/Library/Application Support/io.github.manvithap-hub.tessera/audit.log`
- Linux: `~/.local/share/io.github.manvithap-hub.tessera/audit.log`
- Windows: `%APPDATA%\io.github.manvithap-hub.tessera\audit.log`

## All guardrails at a glance

| Area | Guardrail |
|---|---|
| Cluster access | Read-only by default: only `get`, `list` and pod logs |
| Identity | Your kubeconfig identity per context; never more than `kubectl` could do |
| RBAC denials | Skipped with a warning; no workarounds |
| Secrets | Contents never read; names only if you grant it (optional) |
| Network tests | Plan shown first; nothing runs until approved; UI can only approve backend-built plans |
| Probe pods | Non-root, no token, no capabilities, read-only filesystem, 90-second limit, always deleted |
| Production | Detected by name; red banner; network tests off by default; typed confirmation when allowed; enforced in the backend |
| Per-cluster settings | AWS profile, region, probe image and domain kept separately for each context |
| Organisation policy | System file can lock rules, limit or hide contexts; fails safe if unreadable |
| Activity log | Local record of network tests, pods created and deleted, and blocked attempts |
| Cloud checks | Opt-in; five allow-listed read-only AWS commands; the cluster's own profile |
| Credentials | Never stored by Tessera |
| App sandbox | UI has no file, shell or network access; everything goes through the backend |
| Privacy | No telemetry |

## Troubleshooting

| You see | Likely cause |
|---|---|
| "Can't reach this cluster's API server" | Cluster deleted or stopped, or you need a VPN |
| "Your cloud login has expired" | Run `aws sso login` (or your provider's login), then Refresh |
| A context is missing from the menu | It's hidden or not in `allowedContexts` in a policy file |
| "Test connectivity" is greyed out | Production context, or network tests turned off by policy |
| Cloud checks use the wrong account | Check the context's `--profile` in your kubeconfig, or set one in Settings → This cluster |
| "Policy problem" warning | The system policy file isn't valid JSON; active features are off until it's fixed |
