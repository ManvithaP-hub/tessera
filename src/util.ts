export const esc = (s: unknown): string =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

export const clip = (s: string, n: number): string => (s.length > n ? s.slice(0, n - 1) + "…" : s);

export function fmtBytes(b: number | null | undefined): string {
  if (b == null) return "not set";
  const units: [string, number][] = [["Gi", 2 ** 30], ["Mi", 2 ** 20], ["Ki", 2 ** 10]];
  for (const [u, size] of units) {
    if (b >= size) {
      const v = b / size;
      return (Math.abs(v - Math.round(v)) < 0.05 ? String(Math.round(v)) : v.toFixed(1)) + u;
    }
  }
  return `${b}B`;
}

export function fmtCpu(m: number | null | undefined): string {
  if (m == null) return "not set";
  if (m % 1000 === 0) return String(m / 1000);
  return m > 1000 ? (m / 1000).toFixed(2) : `${m}m`;
}

export function ago(unixOrIso: string | null | undefined): string {
  if (!unixOrIso) return "";
  const t = /^\d+$/.test(unixOrIso) ? Number(unixOrIso) * 1000 : Date.parse(unixOrIso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

export const SYSTEM_NS = (ns: string): boolean => ns.startsWith("kube-") || ns === "local-path-storage";

export function toast(msg: string): void {
  const t = document.createElement("div");
  t.className = "toast";
  t.setAttribute("role", "status");
  t.textContent = msg;
  document.body.appendChild(t);
  setTimeout(() => t.remove(), 2400);
}
