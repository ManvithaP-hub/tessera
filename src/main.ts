import "@fontsource/instrument-sans/400.css";
import "@fontsource/instrument-sans/500.css";
import "@fontsource/instrument-sans/600.css";
import "@fontsource/instrument-sans/700.css";
import "@fontsource/jetbrains-mono/400.css";
import "./styles.css";

import * as api from "./api";
import { buildRows, health, podClass, renderMap, tkey, type Row } from "./map";
import type { ClusterGraph, Issue, PodInfo } from "./types";
import { ago, clip, esc, fmtBytes, fmtCpu, SYSTEM_NS, toast } from "./util";

type View = "map" | "issues" | "workloads" | "events";
const VIEWS: [View, string, string][] = [
  ["map", "Traffic map", "Every path from an entry point to the node a pod runs on. Select anything to inspect it."],
  ["issues", "Issues", "Problems found in this snapshot, each traced through the layers a request passes."],
  ["workloads", "Workloads", "Deployments, StatefulSets, DaemonSets and their pods."],
  ["events", "Warning events", "Recent Warning events reported by the cluster."],
];
const MAX_ROWS = 200;
const REFRESH_MS = 15000;
const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
const store = {
  get: (k: string) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k: string, v: string) => { try { localStorage.setItem(k, v); } catch { /* storage unavailable */ } },
};

const S = {
  contexts: [] as string[],
  ctx: "",
  g: null as ClusterGraph | null,
  loading: false,
  error: "",
  setupError: "",
  view: ((store.get("tessera.view") as View) ?? "map") as View,
  ns: "",
  showSystem: false,
  auto: store.get("tessera.auto") !== "off",
  selIssue: "",
  podQuery: "",
};
if (!VIEWS.some((v) => v[0] === S.view)) S.view = "map";

const $ = <T extends HTMLElement = HTMLElement>(sel: string) => document.querySelector<T>(sel)!;
const inScope = (ns: string) => (S.ns ? ns === S.ns : S.showSystem || !SYSTEM_NS(ns));
const scopedIssues = (): Issue[] => (S.g?.issues ?? []).filter((i) => i.target.kind === "Node" || inScope(i.target.namespace));

/* ---------------- Shell ---------------- */

const LOGO = `<svg width="26" height="26" viewBox="0 0 26 26" aria-hidden="true"><path d="M7 2.5l5 2.9v5.8l-5 2.9-5-2.9V5.4z" fill="var(--accent)"/><path d="M19 2.5l5 2.9v5.8l-5 2.9-5-2.9V5.4z" fill="var(--ok)"/><path d="M13 12.8l5 2.9v5.8l-5 2.9-5-2.9v-5.8z" fill="var(--warn)"/></svg>`;

function shell() {
  $("#app").innerHTML = `<div class="app">
    <aside class="side">
      <div class="brand">${LOGO} Tessera</div>
      <label class="field">Context <select id="ctx" aria-label="Kubernetes context"></select><small id="ctxmeta"></small></label>
      <label class="field">Namespace <select id="ns" aria-label="Namespace"></select></label>
      <label class="check"><input type="checkbox" id="sys"> Include system namespaces</label>
      <nav id="nav" aria-label="Views"></nav>
      <div class="side-foot">
        <label class="check" style="padding:0"><input type="checkbox" id="auto"> Refresh every 15 seconds</label>
        <span id="fresh"></span>
        <span>Read-only. Tessera never changes your cluster.</span>
      </div>
    </aside>
    <main>
      <header class="top">
        <div><h1 id="title"></h1><p class="sub" id="sub"></p></div>
        <div class="row"><button class="btn" id="refresh">Refresh</button><button class="btn" id="theme">Switch theme</button></div>
      </header>
      <div id="notices"></div>
      <section id="view"></section>
    </main>
  </div><div id="overlay"></div>`;
  ($("#auto") as HTMLInputElement).checked = S.auto;
}

function renderSide() {
  $("#ctx").innerHTML = S.contexts.map((c) => `<option ${c === S.ctx ? "selected" : ""}>${esc(c)}</option>`).join("");
  const g = S.g;
  $("#ctxmeta").textContent = api.demoMode ? "Demo cluster, no kubeconfig used" : g?.serverVersion ? `Kubernetes ${g.serverVersion}` : "";
  const nss = [...new Set((g?.pods ?? []).map((p) => p.namespace).concat((g?.services ?? []).map((s) => s.namespace)))]
    .filter((n) => S.showSystem || !SYSTEM_NS(n) || n === S.ns)
    .sort();
  $("#ns").innerHTML = `<option value="">All namespaces</option>` + nss.map((n) => `<option ${n === S.ns ? "selected" : ""}>${esc(n)}</option>`).join("");
  ($("#sys") as HTMLInputElement).checked = S.showSystem;
  const crit = scopedIssues().filter((i) => i.severity === "critical").length;
  $("#nav").innerHTML = VIEWS.map(([k, l]) => `<button data-view="${k}" ${k === S.view ? 'aria-current="page"' : ""}><span>${l}</span>${k === "issues" && crit ? `<span class="badge" aria-label="${crit} critical issues">${crit}</span>` : ""}</button>`).join("");
  $("#fresh").innerHTML = S.loading ? `<span><span class="spin"></span> Reading cluster…</span>` : g ? `Updated ${ago(g.fetchedAt)} ago` : "";
  const meta = VIEWS.find((v) => v[0] === S.view)!;
  $("#title").textContent = meta[1];
  $("#sub").textContent = meta[2];
  const n: string[] = [];
  if (S.error) n.push(`<div class="notice bad">${esc(S.error)}${S.g ? " Showing the last successful snapshot." : ""}</div>`);
  for (const w of g?.warnings ?? []) n.push(`<div class="notice">${esc(w)}</div>`);
  $("#notices").innerHTML = n.join("");
}

function render() {
  renderSide();
  const v = $("#view");
  if (S.setupError) {
    v.innerHTML = `<div class="panel empty"><h2>No clusters to show yet</h2><p>${esc(S.setupError)}</p><p>Tessera uses the same kubeconfig as kubectl. Once <span class="mono">kubectl get pods</span> works in your terminal, select Refresh.</p></div>`;
    return;
  }
  if (!S.g) {
    v.innerHTML = S.loading ? `<div class="panel empty"><span class="spin"></span> Reading ${esc(S.ctx)}…</div>` : `<div class="panel empty"><h2>Couldn't read ${esc(S.ctx)}</h2><p>Check that your credentials are current (for example, run <span class="mono">aws sso login</span> or <span class="mono">gcloud auth login</span>), then select Refresh.</p></div>`;
    return;
  }
  ({ map: viewMap, issues: viewIssues, workloads: viewWorkloads, events: viewEvents })[S.view]();
}

/* ---------------- Views ---------------- */

let rowsCache: Row[] = [];

function viewMap() {
  const g = S.g!;
  const all = buildRows(g, inScope);
  rowsCache = all;
  const rows = all.slice(0, MAX_ROWS);
  const issues = scopedIssues();
  const crit = issues.filter((i) => i.severity === "critical");
  const chips = crit.length
    ? `<span>${crit.length} critical ${crit.length === 1 ? "issue" : "issues"}:</span>` +
      crit.slice(0, 8).map((i) => `<button class="chip" data-issue="${esc(i.id)}"><span class="st bad"></span>${esc(clip(i.title, 60))}</button>`).join("")
    : `<span class="st ok">No critical issues in ${S.ns ? esc(S.ns) : "these namespaces"}.</span>`;
  const more = all.length > MAX_ROWS ? `<div class="notice">Showing ${MAX_ROWS} of ${all.length} paths. Pick a namespace to see the rest.</div>` : "";
  $("#view").innerHTML = `<div class="chips">${chips}</div>${more}
    ${rows.length ? `<div class="panel"><div class="map-wrap">${renderMap(g, rows, health(issues), !reduceMotion)}</div>
    <div class="legend"><span><i class="sw" style="background:var(--ok)"></i>Pod ready</span><span><i class="sw" style="background:var(--warn)"></i>Pod pending</span><span><i class="sw" style="background:var(--bad)"></i>Pod failing</span><span><svg width="26" height="8" aria-hidden="true"><path d="M0 4H26" class="lnk bad"/></svg>Broken hop</span><span><svg width="10" height="10" aria-hidden="true"><circle cx="5" cy="5" r="3" class="pkt"/></svg>Healthy route</span></div></div>`
    : `<div class="panel empty"><h2>Nothing to map here</h2><p>No services or workloads in ${S.ns ? esc(S.ns) : "the selected namespaces"}. Try including system namespaces or choosing another namespace.</p></div>`}`;
}

type LState = "ok" | "warn" | "bad" | "skip";
interface TraceLayer { layer: Issue["layer"]; name: string; status: LState; finding: string }

function findRow(i: Issue): Row | undefined {
  const t = i.target;
  if (t.kind === "Service") return rowsCache.find((r) => r.key === tkey("Service", t.namespace, t.name));
  if (t.kind === "Ingress") return rowsCache.find((r) => r.routes.some((x) => x.ing.name === t.name && x.ing.namespace === t.namespace) && (r.missing !== null || i.layer !== "entry"));
  if (t.kind === "Pod") return rowsCache.find((r) => r.pods.some((p) => p.namespace === t.namespace && p.name === t.name));
  const id = tkey(t.kind, t.namespace, t.name);
  return rowsCache.find((r) => r.workloads.some((w) => w.id === id));
}

function trace(i: Issue): TraceLayer[] {
  const r = findRow(i);
  const g = S.g!;
  if (!r) {
    return [{ layer: i.layer, name: i.layer === "node" ? "Node" : i.target.kind, status: i.severity === "critical" ? "bad" : "warn", finding: i.title }];
  }
  const L: TraceLayer[] = [];
  const paths = r.routes.map((x) => `${x.route.host ?? "*"}${x.route.path}`).join(", ");
  L.push(
    r.routes.length
      ? { layer: "entry", name: "Entry", status: r.missing ? "bad" : "ok", finding: r.missing ? `${r.routes[0].ing.name} sends ${paths} to a service that doesn't exist.` : `${[...new Set(r.routes.map((x) => x.ing.name))].join(", ")} routes ${paths}.` }
      : r.svc && (r.svc.type === "LoadBalancer" || r.svc.type === "NodePort")
        ? { layer: "entry", name: "Entry", status: "ok", finding: `Exposed directly as a ${r.svc.type} service.` }
        : { layer: "entry", name: "Entry", status: "skip", finding: "Not exposed through an ingress or load balancer." },
  );
  if (r.missing) L.push({ layer: "service", name: "Service", status: "bad", finding: `Service ${r.missing} doesn't exist.` });
  else if (r.svc) {
    const s = r.svc;
    L.push(s.readyEndpoints > 0
      ? { layer: "service", name: "Service", status: "ok", finding: `${s.name} has ${s.readyEndpoints} ready endpoints.` }
      : s.pods.length
        ? { layer: "service", name: "Service", status: "warn", finding: `${s.name} matches ${s.pods.length} pods, but none are ready.` }
        : { layer: "service", name: "Service", status: "bad", finding: `${s.name} selects ${Object.entries(s.selector).map(([k, v]) => `${k}=${v}`).join(",")}, which matches no pods.` });
  } else L.push({ layer: "service", name: "Service", status: "skip", finding: "No service selects this workload." });
  const w = r.workloads[0];
  if (w) L.push({ layer: "workload", name: "Workload", status: w.ready >= w.desired ? "ok" : w.ready === 0 ? "bad" : "warn", finding: `${w.kind} ${w.name}: ${w.ready} of ${w.desired} ready.` });
  if (r.pods.length) {
    const counts = new Map<string, number>();
    r.pods.forEach((p) => counts.set(p.status, (counts.get(p.status) ?? 0) + 1));
    const bad = r.pods.some((p) => podClass(p) === "bad"), warn = r.pods.some((p) => podClass(p) === "warn");
    L.push({ layer: "pod", name: "Pods", status: bad ? "bad" : warn ? "warn" : "ok", finding: [...counts].map(([s, n]) => `${n} ${s}`).join(", ") + "." });
    const unscheduled = r.pods.filter((p) => !p.node && p.phase === "Pending").length;
    const nodes = [...new Set(r.pods.map((p) => p.node).filter((n): n is string => !!n))];
    const notReady = nodes.filter((n) => g.nodes.find((x) => x.name === n)?.ready === false);
    L.push(unscheduled
      ? { layer: "node", name: "Nodes", status: "bad", finding: `${unscheduled} ${unscheduled === 1 ? "pod isn't" : "pods aren't"} scheduled on any node.` }
      : notReady.length
        ? { layer: "node", name: "Nodes", status: "bad", finding: `Not ready: ${notReady.join(", ")}.` }
        : { layer: "node", name: "Nodes", status: "ok", finding: `Running on ${nodes.length} ${nodes.length === 1 ? "node" : "nodes"}, all Ready.` });
  }
  return L;
}

function viewIssues() {
  if (!rowsCache.length) rowsCache = buildRows(S.g!, inScope);
  const issues = scopedIssues();
  if (!issues.length) {
    $("#view").innerHTML = `<div class="panel empty"><h2>No issues found</h2><p>Every route, service, workload and node in ${S.ns ? esc(S.ns) : "these namespaces"} looks healthy in this snapshot.</p></div>`;
    return;
  }
  if (!issues.some((i) => i.id === S.selIssue)) S.selIssue = issues[0].id;
  const sel = issues.find((i) => i.id === S.selIssue)!;
  const ICON: Record<LState, string> = { ok: "✓", warn: "!", bad: "✕", skip: "–" };
  const NAMES: Record<LState, string> = { ok: "passed", warn: "degraded", bad: "failing", skip: "not applicable" };
  const layers = trace(sel)
    .map((l) => `<div class="layer ${l.layer === sel.layer ? "root" : ""}"><span class="ic ${l.status}" aria-label="${NAMES[l.status]}">${ICON[l.status]}</span><span class="lname">${l.name}</span><div>${esc(l.finding)}${l.layer === sel.layer ? ' <span class="muted">Root cause is here.</span>' : ""}</div></div>`)
    .join("");
  const targetBtn = sel.target.kind === "Node"
    ? `<button class="linkbtn" data-kind="node" data-id="${esc(sel.target.name)}">${esc(sel.target.name)}</button>`
    : sel.target.kind === "Service"
      ? `<button class="linkbtn" data-kind="service" data-id="${esc(tkey("Service", sel.target.namespace, sel.target.name))}">${esc(sel.target.namespace)}/${esc(sel.target.name)}</button>`
      : sel.target.kind === "Ingress"
        ? `<button class="linkbtn" data-kind="ingress" data-id="${esc(`${sel.target.namespace}/${sel.target.name}`)}">${esc(sel.target.namespace)}/${esc(sel.target.name)}</button>`
        : `<button class="linkbtn" data-kind="workload" data-id="${esc(tkey(sel.target.kind, sel.target.namespace, sel.target.name))}">${esc(sel.target.namespace)}/${esc(sel.target.name)}</button>`;
  $("#view").innerHTML = `<div class="issues">
    <div class="ilist">${issues.map((i) => `<button data-issue="${esc(i.id)}" aria-pressed="${i.id === sel.id}"><div class="nm"><span class="st ${i.severity === "critical" ? "bad" : "warn"}"></span> ${esc(i.title)}</div><div class="ds">${esc(i.target.kind)} ${esc(i.target.namespace ? `${i.target.namespace}/` : "")}${esc(i.target.name)}</div></button>`).join("")}</div>
    <div class="panel pad">
      <h2>${esc(sel.title)}</h2>
      <p class="muted" style="margin:4px 0 14px">${sel.severity === "critical" ? "Critical" : "Warning"} on ${esc(sel.target.kind)} ${targetBtn}</p>
      <p style="margin:0 0 14px">${esc(sel.detail)}</p>
      <h3>Request path</h3><div class="layers">${layers}</div>
      ${sel.evidence.length ? `<h3>Evidence</h3><pre class="ev" style="margin-bottom:18px">${esc(sel.evidence.join("\n"))}</pre>` : ""}
      <h3>Suggested fix</h3><p class="fix">${esc(sel.suggestion)}</p>
      <h3>Confirm it yourself</h3>${sel.commands.map((c) => `<div class="cmd"><code>${esc(c)}</code><button class="btn sm" data-copy="${esc(c)}">Copy</button></div>`).join("")}
      ${sel.pods.length ? `<h3 class="gap">Affected pods</h3><ul class="plist">${sel.pods.map((p) => `<li><button class="linkbtn mono" data-kind="pod" data-id="${esc(`${sel.target.namespace}/${p}`)}">${esc(p)}</button></li>`).join("")}</ul>` : ""}
    </div></div>`;
}

function viewWorkloads() {
  const g = S.g!;
  const q = S.podQuery.toLowerCase();
  const wls = g.workloads.filter((w) => inScope(w.namespace));
  const pods = g.pods.filter((p) => inScope(p.namespace) && (!q || p.name.includes(q) || p.status.toLowerCase().includes(q) || (p.node ?? "").includes(q)));
  $("#view").innerHTML = `
  <div class="panel"><div class="tbl-wrap"><table class="tbl"><thead><tr><th>Workload</th><th>Kind</th><th>Namespace</th><th>Ready</th><th>Image</th></tr></thead><tbody>
  ${wls.map((w) => `<tr><td><button class="linkbtn mono" data-kind="workload" data-id="${esc(w.id)}">${esc(w.name)}</button></td><td>${w.kind}</td><td>${esc(w.namespace)}</td><td><span class="st ${w.ready >= w.desired ? "ok" : w.ready === 0 && w.desired > 0 ? "bad" : "warn"}">${w.ready}/${w.desired}</span></td><td class="mono">${esc(clip(w.containers[0]?.image.split("/").pop() ?? "", 44))}</td></tr>`).join("") || `<tr><td colspan="5" class="muted">No workloads in scope.</td></tr>`}
  </tbody></table></div></div>
  <div class="filters gap"><input id="podq" type="search" placeholder="Filter pods by name, status or node" value="${esc(S.podQuery)}" aria-label="Filter pods"></div>
  <div class="panel"><div class="tbl-wrap"><table class="tbl"><thead><tr><th>Pod</th><th>Namespace</th><th>Status</th><th>Restarts</th><th>Node</th><th>Age</th></tr></thead><tbody>
  ${pods.slice(0, 500).map((p) => `<tr><td><button class="linkbtn mono" data-kind="pod" data-id="${esc(`${p.namespace}/${p.name}`)}">${esc(p.name)}</button></td><td>${esc(p.namespace)}</td><td><span class="st ${podClass(p) === "done" ? "ok" : podClass(p)}">${esc(p.status)}</span></td><td class="num">${p.restarts}</td><td class="mono">${esc(p.node ?? "—")}</td><td class="num muted">${ago(p.created)}</td></tr>`).join("") || `<tr><td colspan="6" class="muted">No pods match “${esc(S.podQuery)}”.</td></tr>`}
  </tbody></table></div></div>${pods.length > 500 ? `<p class="muted">Showing 500 of ${pods.length} pods. Narrow the filter to see more.</p>` : ""}`;
}

function viewEvents() {
  const ev = S.g!.events.filter((e) => !e.namespace || inScope(e.namespace));
  $("#view").innerHTML = ev.length
    ? `<div class="panel"><div class="tbl-wrap"><table class="tbl"><thead><tr><th>Reason</th><th>Object</th><th>Message</th><th>Count</th><th>Last seen</th></tr></thead><tbody>
      ${ev.slice(0, 300).map((e) => `<tr><td><span class="ev-type">${esc(e.reason)}</span></td><td class="mono">${e.kind === "Pod" ? `<button class="linkbtn mono" data-kind="pod" data-id="${esc(`${e.namespace}/${e.name}`)}">${esc(clip(`pod/${e.name}`, 48))}</button>` : esc(clip(`${e.kind.toLowerCase()}/${e.name}`, 48))}</td><td>${esc(e.message)}</td><td class="num">${e.count}</td><td class="num muted">${e.lastSeen ? ago(e.lastSeen) : ""}</td></tr>`).join("")}
      </tbody></table></div></div>`
    : `<div class="panel empty"><h2>No warning events</h2><p>The cluster hasn't reported any Warning events in these namespaces recently.</p></div>`;
}

/* ---------------- Drawer ---------------- */

let drawerPod: PodInfo | null = null;

function relatedIssues(match: (i: Issue) => boolean): string {
  const list = (S.g?.issues ?? []).filter(match);
  return list.length
    ? `<h3 class="gap">Issues</h3><ul class="plist">${list.map((i) => `<li><button class="linkbtn" data-issue="${esc(i.id)}">${esc(i.title)}</button><span class="st ${i.severity === "critical" ? "bad" : "warn"}"></span></li>`).join("")}</ul>`
    : "";
}

function podList(pods: PodInfo[]): string {
  return `<ul class="plist">${pods.map((p) => `<li><button class="linkbtn mono" data-kind="pod" data-id="${esc(`${p.namespace}/${p.name}`)}">${esc(clip(p.name, 44))}</button><span class="st ${podClass(p) === "done" ? "ok" : podClass(p)}">${esc(p.status)}</span></li>`).join("") || '<li class="muted">No pods.</li>'}</ul>`;
}

function openDrawer(kind: string, id: string) {
  const g = S.g;
  if (!g) return;
  drawerPod = null;
  let h = "";
  if (kind === "ingress") {
    const [ns, name] = id.split("/");
    const ing = g.ingresses.find((i) => i.namespace === ns && i.name === name);
    if (!ing) return;
    const svcs = new Set(g.services.filter((s) => s.namespace === ns).map((s) => s.name));
    h = `<h2>${esc(ing.name)}</h2><p class="muted" style="margin:4px 0 0">Ingress in ${esc(ns)}</p>
      <dl><dt>Class</dt><dd>${esc(ing.className ?? "default")}</dd><dt>Address</dt><dd class="mono">${esc(ing.addresses.join(", ") || "not assigned yet")}</dd></dl>
      <h3>Routes</h3><ul class="plist">${ing.routes.map((r) => `<li><span class="mono">${esc(`${r.host ?? "*"}${r.path}`)}</span>${svcs.has(r.service) ? `<button class="linkbtn" data-kind="service" data-id="${esc(tkey("Service", ns, r.service))}">${esc(r.service)}</button>` : `<span class="st bad">${esc(r.service)} missing</span>`}</li>`).join("")}</ul>
      ${relatedIssues((i) => i.target.kind === "Ingress" && i.target.namespace === ns && i.target.name === name)}`;
  } else if (kind === "service" || kind === "missing") {
    const s = g.services.find((x) => tkey("Service", x.namespace, x.name) === id);
    if (!s) {
      const [, ns, name] = id.split("/");
      h = `<h2>${esc(name)}</h2><p class="muted" style="margin:4px 0 0">Service in ${esc(ns)}</p><p><span class="st bad">This service doesn't exist</span></p><p>An ingress routes traffic to it, so those requests fail.</p>${relatedIssues((i) => i.id.endsWith(`/${name}`) && i.layer === "entry")}`;
    } else {
      const pods = g.pods.filter((p) => p.namespace === s.namespace && s.pods.includes(p.name));
      h = `<h2>${esc(s.name)}</h2><p class="muted" style="margin:4px 0 0">${esc(s.type)} service in ${esc(s.namespace)}</p>
        <dl><dt>Selector</dt><dd class="mono">${esc(Object.entries(s.selector).map(([k, v]) => `${k}=${v}`).join(", ") || "none")}</dd>
        <dt>Cluster IP</dt><dd class="mono">${esc(s.clusterIp ?? "")}</dd><dt>Ports</dt><dd class="mono">${esc(s.ports.join(", "))}</dd>
        ${s.external.length ? `<dt>External</dt><dd class="mono">${esc(s.external.join(", "))}</dd>` : ""}
        <dt>Endpoints</dt><dd><span class="st ${s.readyEndpoints ? "ok" : "bad"}">${s.readyEndpoints} ready</span>${s.notReadyEndpoints ? `, ${s.notReadyEndpoints} not ready` : ""}</dd></dl>
        ${s.workloads.length ? `<h3>Workloads</h3><ul class="plist">${s.workloads.map((w) => `<li><button class="linkbtn" data-kind="workload" data-id="${esc(w)}">${esc(w.split("/").slice(-1)[0])}</button><span class="muted">${esc(w.split("/")[0])}</span></li>`).join("")}</ul>` : ""}
        <h3 class="gap">Pods</h3>${podList(pods)}
        ${relatedIssues((i) => i.target.kind === "Service" && i.target.namespace === s.namespace && i.target.name === s.name)}`;
    }
  } else if (kind === "workload") {
    const w = g.workloads.find((x) => x.id === id);
    if (!w) return;
    const pods = g.pods.filter((p) => p.workload === w.id);
    h = `<h2>${esc(w.name)}</h2><p class="muted" style="margin:4px 0 0">${esc(w.kind)} in ${esc(w.namespace)}</p>
      <dl><dt>Ready</dt><dd><span class="st ${w.ready >= w.desired ? "ok" : "warn"}">${w.ready} of ${w.desired}</span></dd>
      ${w.containers.map((c) => `<dt>${esc(c.name)}</dt><dd><span class="mono">${esc(c.image)}</span><br><span class="muted">CPU request ${fmtCpu(c.cpuRequestMilli)}, memory request ${fmtBytes(c.memoryRequestBytes)}, limit ${fmtBytes(c.memoryLimitBytes)}</span></dd>`).join("")}</dl>
      <h3>Pods</h3>${podList(pods)}
      ${relatedIssues((i) => tkey(i.target.kind, i.target.namespace, i.target.name) === w.id)}`;
  } else if (kind === "pod") {
    const p = g.pods.find((x) => `${x.namespace}/${x.name}` === id);
    if (!p) return;
    drawerPod = p;
    h = `<h2 class="mono" style="font-size:15px;padding-right:36px">${esc(p.name)}</h2><p class="muted" style="margin:4px 0 0">Pod in ${esc(p.namespace)}</p>
      <dl><dt>Status</dt><dd><span class="st ${podClass(p) === "done" ? "ok" : podClass(p)}">${esc(p.status)}</span></dd>
      <dt>Node</dt><dd>${p.node ? `<button class="linkbtn" data-kind="node" data-id="${esc(p.node)}">${esc(p.node)}</button>` : "Not scheduled"}</dd>
      <dt>Pod IP</dt><dd class="mono">${esc(p.podIp ?? "none")}</dd><dt>Restarts</dt><dd>${p.restarts}</dd>
      ${p.workload ? `<dt>Owner</dt><dd><button class="linkbtn" data-kind="workload" data-id="${esc(p.workload)}">${esc(p.workload.split("/").join(" "))}</button></dd>` : ""}
      ${p.containers.map((c) => `<dt>${esc(c.name)}</dt><dd>${esc(c.state)}${c.reason ? `: ${esc(c.reason)}` : ""}${c.lastReason ? `<br><span class="muted">last exit: ${esc(c.lastReason)} (${c.lastExitCode ?? "?"})</span>` : ""}${c.message ? `<br><span class="muted">${esc(clip(c.message, 200))}</span>` : ""}</dd>`).join("")}</dl>
      ${relatedIssues((i) => i.target.namespace === p.namespace && (i.pods.includes(p.name) || (i.target.kind === "Pod" && i.target.name === p.name)))}
      <h3 class="gap">Logs</h3>
      <div class="logbar"><select id="logc" aria-label="Container">${p.containers.map((c) => `<option>${esc(c.name)}</option>`).join("")}</select>
      <label class="check" style="padding:0"><input type="checkbox" id="logprev" ${p.containers.some((c) => c.lastReason) ? "checked" : ""}> Previous container</label>
      <button class="btn sm" id="logload">Reload</button></div>
      <pre class="logs" id="logout">Loading…</pre>`;
  } else if (kind === "node") {
    const n = g.nodes.find((x) => x.name === id);
    if (!n) return;
    const pods = g.pods.filter((p) => p.node === n.name);
    const req = pods.reduce((a, p) => a + p.containers.reduce((b, c) => b + (c.cpuRequestMilli ?? 0), 0), 0);
    h = `<h2>${esc(n.name)}</h2><p class="muted" style="margin:4px 0 0">Node${n.instanceType ? `, ${esc(n.instanceType)}` : ""}</p>
      <dl><dt>Status</dt><dd><span class="st ${n.ready ? "ok" : "bad"}">${n.ready ? "Ready" : "Not ready"}</span>${n.unschedulable ? ", cordoned" : ""}</dd>
      <dt>Allocatable CPU</dt><dd>${fmtCpu(n.cpuAllocatableMilli)}</dd><dt>CPU requested</dt><dd>${fmtCpu(req)} (${n.cpuAllocatableMilli ? Math.round((req / n.cpuAllocatableMilli) * 100) : 0}%)</dd>
      <dt>Allocatable memory</dt><dd>${fmtBytes(n.memoryAllocatableBytes)}</dd>${n.pressure.length ? `<dt>Pressure</dt><dd class="st warn">${esc(n.pressure.join(", "))}</dd>` : ""}</dl>
      <h3>Pods on this node</h3>${podList(pods)}
      ${relatedIssues((i) => i.target.kind === "Node" && i.target.name === n.name)}`;
  } else return;
  $("#overlay").innerHTML = `<div class="scrim" data-close></div><aside class="drawer" role="dialog" aria-modal="true" aria-label="Details"><button class="close" data-close aria-label="Close">×</button>${h}</aside>`;
  $(".drawer .close").focus();
  if (drawerPod) loadLogs();
}

async function loadLogs() {
  const p = drawerPod;
  const out = document.querySelector<HTMLElement>("#logout");
  if (!p || !out) return;
  const container = ($("#logc") as HTMLSelectElement).value || null;
  const previous = ($("#logprev") as HTMLInputElement).checked;
  out.textContent = "Loading…";
  try {
    const text = await api.podLogs(S.ctx, p.namespace, p.name, container, previous);
    if (drawerPod !== p) return;
    out.textContent = text.trim() || (previous ? "No logs from a previous container." : "The container hasn't written any logs yet.");
    out.scrollTop = out.scrollHeight;
  } catch (e) {
    out.textContent = String(e);
  }
}

const closeOverlay = () => { $("#overlay").innerHTML = ""; drawerPod = null; };

/* ---------------- Data ---------------- */

let timer: number | undefined;
let seq = 0;

async function refresh() {
  if (!S.ctx || S.loading) return;
  const mine = ++seq;
  S.loading = true;
  renderSide();
  if (!S.g) render();
  try {
    const g = await api.snapshot(S.ctx);
    if (mine !== seq) return;
    S.g = g;
    S.error = "";
  } catch (e) {
    if (mine !== seq) return;
    S.error = `Couldn't read ${S.ctx}: ${String(e)}`;
  } finally {
    if (mine === seq) {
      S.loading = false;
      render();
    }
  }
}

function schedule() {
  clearInterval(timer);
  if (S.auto) timer = window.setInterval(() => { if (!document.hidden && !$("#overlay").innerHTML) refresh(); }, REFRESH_MS);
}

async function init() {
  shell();
  try {
    const c = await api.listContexts();
    S.contexts = c.contexts.map((x) => x.name);
    const saved = store.get("tessera.ctx");
    S.ctx = saved && S.contexts.includes(saved) ? saved : c.current && S.contexts.includes(c.current) ? c.current : S.contexts[0] ?? "";
    if (!S.ctx) S.setupError = "Your kubeconfig has no contexts.";
  } catch (e) {
    S.setupError = String(e);
  }
  render();
  schedule();
  await refresh();
}

function switchContext(ctx: string) {
  S.ctx = ctx;
  S.g = null;
  S.ns = "";
  S.error = "";
  S.selIssue = "";
  seq++;
  S.loading = false;
  store.set("tessera.ctx", ctx);
  closeOverlay();
  refresh();
}

function go(v: View) {
  S.view = v;
  store.set("tessera.view", v);
  render();
  $("main").scrollTop = 0;
}

function toggleTheme() {
  const cur = document.documentElement.dataset.theme ?? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  store.set("tessera.theme", next);
}

/* ---------------- Events ---------------- */

document.addEventListener("click", (e) => {
  const t = (e.target as HTMLElement).closest<HTMLElement>("[data-view],[data-kind],[data-issue],[data-copy],[data-close],#refresh,#theme,#logload");
  if (!t) return;
  if (t.dataset.view) go(t.dataset.view as View);
  else if (t.dataset.kind) openDrawer(t.dataset.kind, t.dataset.id ?? "");
  else if (t.dataset.issue) { closeOverlay(); S.selIssue = t.dataset.issue; go("issues"); }
  else if (t.dataset.copy) navigator.clipboard?.writeText(t.dataset.copy).then(() => toast("Copied"), () => toast("Copy isn't available here"));
  else if (t.dataset.close !== undefined) closeOverlay();
  else if (t.id === "refresh") api.resetConnection(S.ctx).then(refresh);
  else if (t.id === "theme") toggleTheme();
  else if (t.id === "logload") loadLogs();
});

document.addEventListener("change", (e) => {
  const t = e.target as HTMLInputElement;
  if (t.id === "ctx") switchContext(t.value);
  else if (t.id === "ns") { S.ns = t.value; rowsCache = []; render(); }
  else if (t.id === "sys") { S.showSystem = t.checked; rowsCache = []; render(); }
  else if (t.id === "auto") { S.auto = t.checked; store.set("tessera.auto", t.checked ? "on" : "off"); schedule(); }
  else if (t.id === "logc" || t.id === "logprev") loadLogs();
});

document.addEventListener("input", (e) => {
  const t = e.target as HTMLInputElement;
  if (t.id === "podq") {
    S.podQuery = t.value;
    const pos = t.selectionStart ?? t.value.length;
    render();
    const el = $("#podq") as HTMLInputElement;
    el.focus();
    el.setSelectionRange(pos, pos);
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && $("#overlay").innerHTML) { closeOverlay(); return; }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") { e.preventDefault(); refresh(); return; }
  const g = (e.target as Element).closest?.("g.clk") as SVGGElement | null;
  if (g && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); openDrawer(g.dataset.kind ?? "", g.dataset.id ?? ""); }
});

const savedTheme = store.get("tessera.theme");
if (savedTheme === "dark" || savedTheme === "light") document.documentElement.dataset.theme = savedTheme;
init();
