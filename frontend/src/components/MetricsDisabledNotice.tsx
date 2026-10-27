import React from "react";
import { BarChart3 } from "lucide-react";

/** Shown when the core runs with `metrics.enabled = false` (the default). */
export const MetricsDisabledNotice: React.FC = () => (
  <div className="flex items-start gap-3 rounded-2xl border border-border bg-surface-1/60 p-4">
    <div className="p-2 rounded-lg bg-surface-2 text-content-muted">
      <BarChart3 size={18} />
    </div>
    <div className="text-sm">
      <p className="font-semibold text-content-primary">Metrics are disabled</p>
      <p className="text-xs text-content-muted mt-1 leading-relaxed">
        Ferrix skips per-request bookkeeping by default to stay lightweight. To see traffic
        here, add <code className="font-mono">metrics: {"{"} enabled: true {"}"}</code> to
        ferrix.yaml (Config Editor) and reload.
      </p>
    </div>
  </div>
);
