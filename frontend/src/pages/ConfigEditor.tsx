import React, { useState, useEffect } from "react";
import {
  FileCode,
  Save,
  RotateCcw,
  Download,
  Upload,
  CheckCircle2,
  AlertCircle,
  ShieldCheck,
} from "lucide-react";
import { api, ApiError, FerrixConfig, ValidationIssue } from "../services/api";
import { Button } from "../components/ui/Button";
import { Badge } from "../components/ui/Badge";

interface ConfigEditorProps {
  config: FerrixConfig | null;
  onRefreshConfig: () => Promise<void>;
}

// The editor works on the same YAML that is stored in ferrix.yaml; the core parses and
// validates it, so the browser needs no YAML library.
export const ConfigEditor: React.FC<ConfigEditorProps> = ({ config, onRefreshConfig }) => {
  const [yamlText, setYamlText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [issues, setIssues] = useState<ValidationIssue[]>([]);
  const [saving, setSaving] = useState(false);
  const [validating, setValidating] = useState(false);

  const loadYaml = async () => {
    try {
      setYamlText(await api.getConfigYaml());
    } catch (err: any) {
      setError(err.message || "Failed to load configuration");
    }
  };

  useEffect(() => {
    if (config) {
      loadYaml();
    }
  }, [config]);

  const showIssuesFrom = (err: any, fallback: string) => {
    if (err instanceof ApiError && Array.isArray(err.body?.report?.issues)) {
      setIssues(err.body.report.issues);
      setError(err.body.report.summary || fallback);
    } else {
      setError(err.message || fallback);
    }
  };

  const handleValidate = async () => {
    setError(null);
    setSuccess(null);
    setIssues([]);
    setValidating(true);
    try {
      const report = await api.validateConfigYaml(yamlText);
      setIssues(report.issues);
      if (report.valid) {
        setSuccess(
          report.issues.length > 0
            ? "Valid configuration — warnings only, no errors."
            : "Valid configuration, no issues found.",
        );
      } else {
        setError("Configuration has errors and cannot be applied yet.");
      }
    } catch (err: any) {
      showIssuesFrom(err, "Failed to validate configuration");
    } finally {
      setValidating(false);
    }
  };

  const handleApply = async () => {
    setError(null);
    setSuccess(null);
    setIssues([]);
    setSaving(true);
    try {
      await api.updateConfigYaml(yamlText);
      await onRefreshConfig();
      setSuccess("Configuration validated, saved to disk, and hot-reloaded!");
      setTimeout(() => setSuccess(null), 5000);
    } catch (err: any) {
      showIssuesFrom(err, "Failed to apply configuration");
    } finally {
      setSaving(false);
    }
  };

  const handleReset = () => {
    if (config) {
      loadYaml();
      setError(null);
      setSuccess(null);
      setIssues([]);
    }
  };

  const handleExport = () => {
    const blob = new Blob([yamlText], { type: "application/yaml" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "ferrix.yaml";
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleImport = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (event) => {
      try {
        const text = event.target?.result as string;
        setYamlText(text);
        setError(null);
        setIssues([]);
      } catch (err: any) {
        setError(`Failed to parse file: ${err.message}`);
      }
    };
    reader.readAsText(file);
  };

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold text-content-primary tracking-tight">
            Configuration Editor
          </h2>
          <p className="text-xs text-content-muted">
            Full ferrix.yaml configuration with validation equivalent to "nginx -t" and zero-downtime hot-reload
          </p>
        </div>

        <div className="flex items-center gap-2">
          <label className="flex items-center gap-1.5 px-3 py-1.5 rounded-xl bg-surface-2 hover:bg-surface-3 text-content-secondary text-xs font-medium cursor-pointer transition border border-border">
            <Upload className="w-3.5 h-3.5" />
            Import YAML
            <input type="file" accept=".yaml,.yml" onChange={handleImport} className="hidden" />
          </label>
          <Button variant="secondary" size="sm" icon={<Download className="w-3.5 h-3.5" />} onClick={handleExport}>
            Export
          </Button>
          <Button variant="secondary" size="sm" icon={<RotateCcw className="w-3.5 h-3.5" />} onClick={handleReset}>
            Reset
          </Button>
          <Button
            variant="secondary"
            size="sm"
            icon={<ShieldCheck className="w-3.5 h-3.5" />}
            loading={validating}
            onClick={handleValidate}
          >
            Validate
          </Button>
          <Button variant="primary" size="sm" icon={<Save className="w-4 h-4" />} loading={saving} onClick={handleApply}>
            Apply (hot-reload)
          </Button>
        </div>
      </div>

      {success && (
        <div className="p-4 rounded-xl bg-success-500/10 border border-success-500/40 flex items-center gap-3 text-success-400 text-sm animate-fade-in">
          <CheckCircle2 className="w-5 h-5 flex-shrink-0" />
          <span>{success}</span>
        </div>
      )}

      {error && (
        <div className="p-4 rounded-xl bg-danger-500/10 border border-danger-500/40 flex items-center gap-3 text-danger-400 text-sm animate-fade-in">
          <AlertCircle className="w-5 h-5 flex-shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {issues.length > 0 && (
        <div className="rounded-xl border border-border overflow-hidden">
          <table className="w-full text-xs">
            <thead className="bg-surface-2 text-content-muted">
              <tr>
                <th className="text-left font-medium px-3 py-2">Severity</th>
                <th className="text-left font-medium px-3 py-2">Path</th>
                <th className="text-left font-medium px-3 py-2">Message</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border-subtle">
              {issues.map((issue, i) => (
                <tr key={i}>
                  <td className="px-3 py-2">
                    <Badge tone={issue.severity === "error" ? "danger" : "warning"}>
                      {issue.severity === "error" ? "Error" : "Warning"}
                    </Badge>
                  </td>
                  <td className="px-3 py-2 font-mono text-content-primary whitespace-nowrap">{issue.path}</td>
                  <td className="px-3 py-2 text-content-secondary">{issue.message}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="bg-surface-0 border border-border rounded-2xl p-4 shadow-card">
        <textarea
          value={yamlText}
          onChange={(e) => setYamlText(e.target.value)}
          spellCheck={false}
          rows={28}
          className="w-full bg-transparent font-mono text-xs text-content-primary resize-y focus:outline-none leading-relaxed"
        />
      </div>
    </div>
  );
};

