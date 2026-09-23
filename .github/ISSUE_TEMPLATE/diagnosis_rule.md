---
name: Diagnosis rule idea
about: A failure Tessera should detect, or detected wrongly
labels: diagnosis
---

**The failure**
What breaks, and how it shows up for users (503s, timeouts, crash loops...).

**How you'd spot it from the API**
Which objects and fields show it (pod status, events, endpoints, node conditions...).

**Which layer is the root cause**
Entry, service, workload, pod or node.

**Suggested fix text**
What should Tessera tell the user to do?
