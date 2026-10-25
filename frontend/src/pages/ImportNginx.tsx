import React, { useState } from "react";
import { UploadCloud, FileWarning, CheckCircle2, AlertTriangle } from "lucide-react";
import { Button } from "../components/ui/Button";
import { Textarea } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";
import { CodeBlock } from "../components/ui/CodeBlock";
import { useToast, useConfirm } from "../components/ui/Overlays";
import { api, FerrixConfig, ImportNginxResult, MigrationNote } from "../services/api";

const PLACEHOLDER = `# Paste the contents of your nginx.conf here, or a site config block (server { ... }).
#
# Note: "include" directives to other files are not traversed here — paste pre-expanded content,
# or use "ferrix import --from nginx.conf --out ferrix.json --report report.md" on the
# host where the files reside, which resolves includes automatically.

server {
    listen 80;
    server_name mysite.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
    }
}`;

const LEVEL_META: Record<
  MigrationNote["level"],
  { label: string; tone: "danger" | "warning" | "info" | "neutral" }
> = {
  unsupported: { label: "Unsupported", tone: "danger" },
  partial: { label: "Partially converted", tone: "warning" },
  converted: { label: "Converted", tone: "info" },
  ignored: { label: "Ignored by design", tone: "neutral" },
};

interface ImportNginxProps {
  onApplied: () => void;
}

export const ImportNginx: React.FC<ImportNginxProps> = ({ onApplied }) => {
  const toast = useToast();
  const confirm = useConfirm();

  const [source, setSource] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<ImportNginxResult | null>(null);
  const [applying, setApplying] = useState(false);

  const runImport = async () => {
    if (!source.trim()) {
      toast.error("Paste the nginx.conf content before converting.");
      return;
    }
    setLoading(true);
    setResult(null);
    try {
      const r = await api.importNginx(source, "nginx.conf");
      setResult(r);
      const unsupported = r.report.notes.filter((n) => n.level === "unsupported").length;
      if (unsupported > 0) {
        toast.info(`Converted with ${unsupported} unsupported directive(s) — check report below.`);
      } else {
        toast.success("Configuration converted cleanly with no issues.");
      }
    } catch (err: any) {
      toast.error(err.message || "Failed to convert nginx.conf");
    } finally {
      setLoading(false);
    }
  };

  const applyConfig = async (imported: FerrixConfig) => {
    const ok = await confirm({
      title: "Add this configuration to the active configuration?",
      description:
        "Converted virtual hosts and upstreams will be merged into the running configuration with " +
        "immediate zero-downtime hot-reload — authentication, timeouts, and existing settings remain untouched. " +
        "Review the migration report before confirming.",
      confirmLabel: "Add now",
      tone: "danger",
    });
    if (!ok) return;

    setApplying(true);
    try {
      const live = await api.getConfig();
      const merged = mergeImportedConfig(live, imported);
      await api.updateConfig(merged);
      toast.success("Imported virtual hosts and upstreams added successfully.");
      onApplied();
    } catch (err: any) {
      const report = err.body?.report;
      toast.error(
        report?.summary
          ? `Could not apply: ${report.summary}`
          : err.message || "Failed to apply configuration",
      );
    } finally {
      setApplying(false);
    }
  };

  const notesByLevel = result
    ? {
        unsupported: result.report.notes.filter((n) => n.level === "unsupported"),
        partial: result.report.notes.filter((n) => n.level === "partial"),
        ignored: result.report.notes.filter((n) => n.level === "ignored"),
      }
    : null;

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-xl font-bold text-content-primary tracking-tight">
          Import Nginx Configuration
        </h2>
        <p className="text-xs text-content-muted mt-1">
          Paste an <code className="text-brand-400">nginx.conf</code> (or a{" "}
          <code className="text-brand-400">server {"{ }"}</code> block) and see what Ferrix
          converts, what needs manual adjustment, and what is unsupported — before applying
          anything.
        </p>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <div className="space-y-3">
          <Textarea
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder={PLACEHOLDER}
            mono
            rows={20}
            className="text-xs leading-relaxed"
          />
          <Button variant="primary" icon={<UploadCloud className="w-4 h-4" />} loading={loading} onClick={runImport}>
            Convert
          </Button>
        </div>

        <div className="space-y-4">
          {!result && (
            <div className="h-full flex flex-col items-center justify-center text-center py-16 border border-dashed border-border rounded-2xl text-content-muted">
              <FileWarning className="w-8 h-8 mb-3" />
              <p className="text-xs max-w-xs">
                Conversion results and migration report will appear here, alongside the generated JSON.
              </p>
            </div>
          )}

          {result && (
            <>
              <div className="grid grid-cols-3 gap-2 text-center">
                <StatCard label="Virtual hosts" value={result.report.vhosts_created} />
                <StatCard label="Routes" value={result.report.routes_created} />
                <StatCard label="Upstreams" value={result.report.upstreams_created} />
              </div>

              {result.validation.issues.filter((i) => !i.path.startsWith("admin") && !i.path.startsWith("real_ip")).length > 0 && (
                <div className="flex items-start gap-2 p-3 rounded-lg bg-warning-500/10 border border-warning-500/30 text-warning-400 text-xs">
                  <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
                  <span>
                    The conversion has pending issues beyond what Ferrix can resolve
                    automatically (e.g. reference to a missing upstream) — inspect details
                    in the Configuration Editor after applying.
                  </span>
                </div>
              )}

              {notesByLevel &&
                (notesByLevel.unsupported.length > 0 ||
                  notesByLevel.partial.length > 0 ||
                  notesByLevel.ignored.length > 0) && (
                <div className="rounded-xl border border-border overflow-hidden max-h-64 overflow-y-auto">
                  <table className="w-full text-xs">
                    <thead className="bg-surface-2 text-content-muted sticky top-0">
                      <tr>
                        <th className="text-left font-medium px-3 py-2">Directive</th>
                        <th className="text-left font-medium px-3 py-2">Line</th>
                        <th className="text-left font-medium px-3 py-2">Status</th>
                        <th className="text-left font-medium px-3 py-2">Reason</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-border-subtle">
                      {[...notesByLevel.unsupported, ...notesByLevel.partial, ...notesByLevel.ignored].map((n, i) => {
                        const meta = LEVEL_META[n.level];
                        return (
                          <tr key={i} className="align-top">
                            <td className="px-3 py-2 font-mono text-content-primary whitespace-nowrap">
                              {n.directive}
                            </td>
                            <td className="px-3 py-2 text-content-muted">{n.line}</td>
                            <td className="px-3 py-2">
                              <Badge tone={meta.tone}>{meta.label}</Badge>
                            </td>
                            <td className="px-3 py-2 text-content-secondary">{n.reason}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}

              <CodeBlock code={JSON.stringify(result.config, null, 2)} maxHeight="16rem" />

              <Button
                variant="primary"
                icon={<CheckCircle2 className="w-4 h-4" />}
                loading={applying}
                onClick={() => applyConfig(result.config)}
                className="w-full"
              >
                Apply this configuration
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
};

function uniqueId(base: string, taken: Set<string>): string {
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

function mergeImportedConfig(live: FerrixConfig, imported: FerrixConfig): FerrixConfig {
  const takenVhostIds = new Set(live.vhosts.map((v) => v.id));

  const mergedVhosts = imported.vhosts.map((vhost) => {
    const newId = uniqueId(vhost.id, takenVhostIds);
    takenVhostIds.add(newId);

    const takenRouteIds = new Set<string>();
    const routes = vhost.routes.map((route) => {
      const routeId = uniqueId(route.id, takenRouteIds);
      takenRouteIds.add(routeId);
      return { ...route, id: routeId };
    });

    return { ...vhost, id: newId, routes };
  });

  return {
    ...live,

    upstreams: { ...live.upstreams, ...imported.upstreams },
    vhosts: [...live.vhosts, ...mergedVhosts],
  };
}

const StatCard: React.FC<{ label: string; value: number }> = ({ label, value }) => (
  <div className="rounded-xl border border-border-subtle bg-surface-1 p-3">
    <div className="text-lg font-bold text-brand-400">{value}</div>
    <div className="text-[11px] text-content-muted">{label}</div>
  </div>
);

