# Security policy

Tessera runs with your Kubernetes credentials, so security reports are taken seriously.

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private vulnerability reporting
("Report a vulnerability" under the Security tab of this repository). You
should get a response within five working days.

## What Tessera does with your credentials

- It reads your kubeconfig the same way kubectl does, including exec credential
  plugins, and keeps tokens in memory only.
- It only calls `get` and `list` on the resources listed in `docs/rbac.yaml`,
  plus reading pod logs.
- It sends nothing anywhere except to the API servers in your kubeconfig. There
  is no telemetry, crash reporting or update check in this version.
- The webview has no filesystem, shell or network permissions; all cluster
  access happens in the Rust process.
