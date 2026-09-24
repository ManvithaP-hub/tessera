# Security policy

Tessera runs with your Kubernetes credentials, so security reports are taken seriously.

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private vulnerability reporting
("Report a vulnerability" under the Security tab of this repository). You
should get a response within five working days.

## What Tessera does with your credentials

- It reads your kubeconfig the same way kubectl does, including exec credential
  plugins, and keeps tokens in memory only.
- By default it only calls `get` and `list` on the resources listed in
  `docs/rbac.yaml`, plus reading pod logs.
- Network tests are the only feature that creates anything. They run only
  after you review and approve the exact pod manifests. The webview can't
  submit its own manifest: it can only approve a plan the Rust side built and
  keeps. Probe pods are non-root, drop all capabilities, have a read-only
  filesystem and no service account token, stop after 90 seconds, and are
  always deleted afterwards.
- Production contexts (detected by name, or marked in Settings) block network
  tests unless an organisation policy allows them, and then require typing the
  context name. This is enforced in the Rust backend.
- An optional system-wide policy file can lock these rules for all users and
  fails safe if it can't be read. See `docs/multi-environment.md`.
- Network tests and blocked attempts are written to a local activity log,
  including which probe pods were created and confirmed deleted.
- Cloud checks are off by default. When on, Tessera runs only these `aws`
  commands: `elbv2 describe-load-balancers`, `elbv2 describe-target-groups`,
  `elbv2 describe-target-health`, `elb describe-load-balancers` and
  `elb describe-instance-health`. The list is enforced in code.
- It sends nothing anywhere except to the API servers in your kubeconfig and,
  if you turn on cloud checks, your cloud provider's API through its CLI. There
  is no telemetry, crash reporting or update check in this version.
- The webview has no filesystem, shell or network permissions; all cluster
  access happens in the Rust process.
