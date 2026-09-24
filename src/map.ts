// Builds the traffic-path rows from a snapshot and renders them as SVG.
// Columns follow a request: entry (ingress or load balancer) -> service ->
// workload -> pods -> nodes.

import type { ClusterGraph, IngressInfo, Issue, PodInfo, Route, ServiceInfo, WorkloadInfo } from "./types";
import { clip, esc } from "./util";

export interface Row {
  key: string;
  namespace: string;
  svc: ServiceInfo | null;
  /** Set when an ingress points at a service that doesn't exist. */
  missing: string | null;
  workloads: WorkloadInfo[];
  pods: PodInfo[];
  routes: { ing: IngressInfo; route: Route }[];
}

export interface Health { byTarget: Map<string, "critical" | "warning">; pods: Set<string> }

export const tkey = (kind: string, ns: string, name: string) => `${kind}/${ns}/${name}`;

export function health(issues: Issue[]): Health {
  const byTarget = new Map<string, "critical" | "warning">();
  const pods = new Set<string>();
  for (const i of issues) {
    const k = tkey(i.target.kind, i.target.namespace, i.target.name);
    if (byTarget.get(k) !== "critical") byTarget.set(k, i.severity);
    i.pods.forEach((p) => pods.add(`${i.target.namespace}/${p}`));
  }
  return { byTarget, pods };
}

export function buildRows(g: ClusterGraph, inScope: (ns: string) => boolean): Row[] {
  const rows: Row[] = [];
  const podsByKey = new Map(g.pods.map((p) => [`${p.namespace}/${p.name}`, p]));
  const wlById = new Map(g.workloads.map((w) => [w.id, w]));
  const covered = new Set<string>();

  const services = g.services.filter(
    (s) => inScope(s.namespace) && s.type !== "ExternalName" && !(s.namespace === "default" && s.name === "kubernetes"),
  );
  for (const s of services) {
    const routes = g.ingresses
      .filter((i) => i.namespace === s.namespace)
      .flatMap((ing) => ing.routes.filter((r) => r.service === s.name).map((route) => ({ ing, route })));
    const workloads = s.workloads.map((id) => wlById.get(id)).filter((w): w is WorkloadInfo => !!w);
    workloads.forEach((w) => covered.add(w.id));
    const pods = s.pods.map((n) => podsByKey.get(`${s.namespace}/${n}`)).filter((p): p is PodInfo => !!p);
    if (!routes.length && !workloads.length && !pods.length && !Object.keys(s.selector).length) continue;
    rows.push({ key: tkey("Service", s.namespace, s.name), namespace: s.namespace, svc: s, missing: null, workloads, pods, routes });
  }
  // Ingress backends that point at services that don't exist.
  const svcNames = new Set(g.services.map((s) => `${s.namespace}/${s.name}`));
  for (const ing of g.ingresses.filter((i) => inScope(i.namespace))) {
    const missing = new Map<string, Route[]>();
    ing.routes.filter((r) => !svcNames.has(`${ing.namespace}/${r.service}`)).forEach((r) => missing.set(r.service, [...(missing.get(r.service) ?? []), r]));
    for (const [name, routes] of missing) {
      rows.push({ key: `missing/${ing.namespace}/${name}`, namespace: ing.namespace, svc: null, missing: name, workloads: [], pods: [], routes: routes.map((route) => ({ ing, route })) });
    }
  }
  // Workloads that no service selects still get a row.
  for (const w of g.workloads.filter((w) => inScope(w.namespace) && !covered.has(w.id))) {
    const pods = g.pods.filter((p) => p.workload === w.id);
    rows.push({ key: w.id, namespace: w.namespace, svc: null, missing: null, workloads: [w], pods, routes: [] });
  }
  const rank = (r: Row) => (r.routes.length ? 0 : r.svc ? 1 : 2);
  rows.sort((a, b) => rank(a) - rank(b) || a.namespace.localeCompare(b.namespace) || a.key.localeCompare(b.key));
  return rows;
}

const hex = (cx: number, cy: number, r: number) =>
  Array.from({ length: 6 }, (_, k) => {
    const a = (Math.PI / 180) * (60 * k - 30);
    return `${k ? "L" : "M"}${(cx + r * Math.cos(a)).toFixed(1)},${(cy + r * Math.sin(a)).toFixed(1)}`;
  }).join("") + "Z";

export const podClass = (p: PodInfo): string =>
  p.phase === "Succeeded" ? "done" : p.status === "Running" && p.ready ? "ok" : p.phase === "Pending" && !p.status.includes("Err") && !p.status.includes("BackOff") ? "warn" : "bad";

export function renderMap(g: ClusterGraph, rows: Row[], h: Health, animate: boolean): string {
  const top = 46, rowH = 70, BW = 160, BH = 44, MAXPODS = 8;
  const X = { entry: 12, svc: 206, wl: 400, pods: 594, node: 818 };
  const cls = (k: string) => {
    const s = h.byTarget.get(k);
    return s === "critical" ? "bad" : s === "warning" ? "warn" : "";
  };
  const box = (x: number, cy: number, kind: string, id: string, t: string, s: string, c: string) =>
    `<g class="bx clk ${c}" tabindex="0" role="button" data-kind="${kind}" data-id="${esc(id)}" aria-label="${esc(`${kind} ${t}, ${s}`)}"><title>${esc(t)}</title><rect x="${x}" y="${cy - BH / 2}" width="${BW}" height="${BH}" rx="8"/><text class="t" x="${x + 12}" y="${cy - 3}">${esc(clip(t, 21))}</text><text class="s" x="${x + 12}" y="${cy + 13}">${esc(clip(s, 26))}</text></g>`;

  const rowY = rows.map((_, i) => top + i * rowH + rowH / 2);
  let H = top + rows.length * rowH + 12;

  // Entry points: one per ingress, plus LoadBalancer/NodePort services.
  type Entry = { id: string; kind: string; title: string; sub: string; rows: number[]; bad: string };
  const entries: Entry[] = [];
  const ingIdx = new Map<string, Entry>();
  rows.forEach((r, i) => {
    for (const { ing } of r.routes) {
      const id = `${ing.namespace}/${ing.name}`;
      let e = ingIdx.get(id);
      if (!e) {
        e = { id, kind: "ingress", title: ing.name, sub: ing.routes[0]?.host ?? ing.addresses[0] ?? ing.namespace, rows: [], bad: cls(tkey("Ingress", ing.namespace, ing.name)) };
        ingIdx.set(id, e);
        entries.push(e);
      }
      if (!e.rows.includes(i)) e.rows.push(i);
    }
    if (r.svc && (r.svc.type === "LoadBalancer" || r.svc.type === "NodePort")) {
      entries.push({ id: r.key, kind: "service", title: r.svc.type === "LoadBalancer" ? "Load balancer" : "NodePort", sub: r.svc.external[0] ?? r.svc.name, rows: [i], bad: "" });
    }
  });
  const entryY: number[] = [];
  entries
    .map((e, i) => ({ i, y: e.rows.reduce((a, r) => a + rowY[r], 0) / e.rows.length }))
    .sort((a, b) => a.y - b.y)
    .reduce((prev, cur) => {
      const y = Math.max(cur.y, prev + BH + 10);
      entryY[cur.i] = y;
      return y;
    }, top - BH);
  if (entryY.length) H = Math.max(H, Math.max(...entryY) + BH);

  // Nodes that host visible pods.
  const nodeNames = [...new Set(rows.flatMap((r) => r.pods.map((p) => p.node).filter((n): n is string => !!n)))].sort();
  const nodeInfo = new Map(g.nodes.map((n) => [n.name, n]));
  H = Math.max(H, top + nodeNames.length * (BH + 12) + 12);
  const span = H - top - 12;
  const nodeY = new Map(nodeNames.map((n, k) => [n, top + (k + 0.5) * (span / Math.max(1, nodeNames.length))]));

  let links = "", boxes = "", pods = "", pk = "", packets = 0;
  entries.forEach((e, i) => {
    const y = entryY[i];
    boxes += box(X.entry, y, e.kind, e.id, e.title, e.sub, e.bad);
    for (const r of e.rows) {
      const row = rows[r];
      const broken = !!row.missing || (row.svc !== null && row.svc.readyEndpoints === 0);
      const pid = `pe-${i}-${r}`;
      links += `<path id="${pid}" class="lnk ${broken ? "bad" : ""}" d="M${X.entry + BW},${y} C${X.entry + BW + 24},${y} ${X.svc - 24},${rowY[r]} ${X.svc},${rowY[r]}"/>`;
      if (!broken && animate && packets++ < 40) pk += `<circle r="3" class="pkt"><animateMotion dur="2.2s" begin="-${((r * 0.37) % 2.2).toFixed(2)}s" repeatCount="indefinite"><mpath href="#${pid}"/></animateMotion></circle>`;
    }
  });

  rows.forEach((r, i) => {
    const cy = rowY[i];
    if (r.missing) {
      boxes += box(X.svc, cy, "missing", r.key, r.missing, "service doesn't exist", "bad ghost");
      return;
    }
    if (r.svc) {
      const s = r.svc;
      const sub = r.routes.length ? r.routes.map((x) => x.route.path).join(" ") : `${s.type}, ${s.readyEndpoints} ready`;
      boxes += box(X.svc, cy, "service", r.key, s.name, sub, cls(r.key) || (s.readyEndpoints === 0 && Object.keys(s.selector).length ? "warn" : ""));
    }
    const w = r.workloads[0];
    if (w) {
      if (r.svc) links += `<path class="lnk" d="M${X.svc + BW},${cy} L${X.wl},${cy}"/>`;
      const extra = r.workloads.length > 1 ? `, +${r.workloads.length - 1} more` : "";
      const wc = cls(w.id) || (w.ready < w.desired ? "warn" : "");
      boxes += box(X.wl, cy, "workload", w.id, w.name, `${w.kind} ${w.ready}/${w.desired}${extra}`, wc);
    } else if (r.svc) {
      links += `<path class="lnk bad" d="M${X.svc + BW},${cy} L${X.wl},${cy}"/>`;
      boxes += box(X.wl, cy, "service", r.key, "No matching pods", "selector matches nothing", "bad ghost");
    }
    if (!r.pods.length) return;
    links += `<path class="lnk" d="M${X.wl + BW},${cy} L${X.pods + 4},${cy}"/>`;
    const shown = r.pods.slice(0, MAXPODS);
    shown.forEach((p, j) => {
      const px = X.pods + 14 + j * 22;
      pods += `<g class="clk" tabindex="0" role="button" data-kind="pod" data-id="${esc(`${p.namespace}/${p.name}`)}" aria-label="Pod ${esc(p.name)}, ${esc(p.status)}"><title>${esc(p.name)}: ${esc(p.status)}</title><path class="pod ${podClass(p)}" d="${hex(px, cy, 9)}"/></g>`;
    });
    if (r.pods.length > MAXPODS) pods += `<text class="more" x="${X.pods + 14 + MAXPODS * 22 - 6}" y="${cy + 4}">+${r.pods.length - MAXPODS}</text>`;
    const endX = X.pods + 14 + (shown.length - 1) * 22 + 10;
    for (const n of new Set(r.pods.map((p) => p.node).filter((n): n is string => !!n))) {
      const ny = nodeY.get(n)!;
      links += `<path class="lnk faint" d="M${endX},${cy} C${endX + 30},${cy} ${X.node - 30},${ny} ${X.node},${ny}"/>`;
    }
  });

  for (const n of nodeNames) {
    const info = nodeInfo.get(n);
    const count = rows.reduce((a, r) => a + r.pods.filter((p) => p.node === n).length, 0);
    const c = cls(tkey("Node", "", n)) || (info && !info.ready ? "bad" : "");
    boxes += box(X.node, nodeY.get(n)!, "node", n, n, `${info?.instanceType ?? "node"}, ${count} ${count === 1 ? "pod" : "pods"} here`, c);
  }

  const heads = ([["entry", "Entry"], ["svc", "Service"], ["wl", "Workload"], ["pods", "Pods"], ["node", "Nodes"]] as const)
    .map(([k, l]) => `<text class="colh" x="${X[k] + 2}" y="24">${l}</text>`)
    .join("");
  return `<svg viewBox="0 0 990 ${H}" role="group" aria-label="Traffic paths from entry points to nodes">${heads}${links}${pk}${boxes}${pods}</svg>`;
}
