import React, { useEffect, useState } from "react";
import { Plus, Save, Settings, Trash2 } from "lucide-react";
import {
  api,
  ApiError,
  FerrixConfig,
  LogTarget,
  StreamConfig,
  StreamProtocol,
  ValidationIssue,
  VariableDef,
} from "../services/api";
import { Button } from "../components/ui/Button";
import { Field } from "../components/ui/Field";
import { Input, Select, Toggle } from "../components/ui/Input";
import { KeyValueEditor } from "../components/ui/KeyValueEditor";
import { PairListEditor, Section } from "../components/ui/ListEditors";
import { CidrListEditor } from "../components/ui/CidrListEditor";

interface Props {
  config: FerrixConfig | null;
  onRefreshConfig: () => Promise<void>;
}

const LOG_VARS =
  "$remote_addr $remote_user $time_local $time_iso8601 $msec $request $request_method $request_uri $uri $args $status $body_bytes_sent $request_time $host $http_host $http_referer $http_user_agent $upstream_addr $scheme $request_id";

function newVariable(type: VariableDef["type"]): VariableDef {
  switch (type) {
    case "map":
      return { type, source: "$http_user_agent", entries: [{ pattern: "~*bot", value: "1" }], default: "0" };
    case "geo":
      return { type, source: null, ranges: [{ network: "10.0.0.0/8", value: "lan" }], default: "wan" };
    case "json":
      return { type, source: "$request_body", path: ".user.id" };
    default:
      return { type: "split_clients", source: "$remote_addr", buckets: [{ percent: 10, value: "a" }, { percent: 0, value: "b" }] };
  }
}

function newTarget(kind: LogTarget["kind"]): LogTarget {
  switch (kind) {
    case "file":
      return { kind, path: "/var/log/ferrix/ferrix.log", max_bytes: 104857600, keep: 5 };
    case "syslog":
      return { kind, addr: "127.0.0.1:514", facility: "local7", tag: "ferrix" };
    case "http":
      return { kind, url: "http://127.0.0.1:8686/", batch_size: 500, flush_ms: 1000 };
    default:
      return { kind };
  }
}

const LogTargetsEditor: React.FC<{ value: LogTarget[]; onChange: (v: LogTarget[]) => void }> = ({ value, onChange }) => {
  const patch = (i: number, t: LogTarget) => onChange(value.map((v, j) => (j === i ? t : v)));
  return (
    <div className="space-y-2">
      {value.map((t, i) => (
        <div key={i} className="flex flex-wrap items-end gap-2">
          <Field label="Destination" className="w-32">
            <Select value={t.kind} onChange={(e) => patch(i, newTarget(e.target.value as LogTarget["kind"]))}>
              {["stderr", "stdout", "file", "syslog", "http", "off"].map((k) => (
                <option key={k} value={k}>{k}</option>
              ))}
            </Select>
          </Field>
          {t.kind === "file" && (
            <>
              <Field label="Path" className="flex-1 min-w-48">
                <Input value={t.path} onChange={(e) => patch(i, { ...t, path: e.target.value })} mono className="text-xs" />
              </Field>
              <Field label="Rotate at (bytes)" hint="0 = never" className="w-36">
                <Input type="number" value={t.max_bytes ?? 0} onChange={(e) => patch(i, { ...t, max_bytes: parseInt(e.target.value) || 0 })} mono />
              </Field>
              <Field label="Keep" className="w-20">
                <Input type="number" value={t.keep ?? 0} onChange={(e) => patch(i, { ...t, keep: parseInt(e.target.value) || 0 })} mono />
              </Field>
            </>
          )}
          {t.kind === "syslog" && (
            <>
              <Field label="Address" hint="host:port (UDP) or /path" className="flex-1 min-w-48">
                <Input value={t.addr ?? ""} onChange={(e) => patch(i, { ...t, addr: e.target.value || null })} mono className="text-xs" />
              </Field>
              <Field label="Facility" className="w-28">
                <Input value={t.facility ?? ""} onChange={(e) => patch(i, { ...t, facility: e.target.value || null })} mono />
              </Field>
              <Field label="Tag" className="w-28">
                <Input value={t.tag ?? ""} onChange={(e) => patch(i, { ...t, tag: e.target.value || null })} mono />
              </Field>
            </>
          )}
          {t.kind === "http" && (
            <>
              <Field label="URL (http://)" className="flex-1 min-w-48">
                <Input value={t.url} onChange={(e) => patch(i, { ...t, url: e.target.value })} mono className="text-xs" />
              </Field>
              <Field label="Batch" className="w-24">
                <Input type="number" value={t.batch_size ?? ""} onChange={(e) => patch(i, { ...t, batch_size: e.target.value ? parseInt(e.target.value) : null })} mono />
              </Field>
              <Field label="Flush (ms)" className="w-28">
                <Input type="number" value={t.flush_ms ?? ""} onChange={(e) => patch(i, { ...t, flush_ms: e.target.value ? parseInt(e.target.value) : null })} mono />
              </Field>
            </>
          )}
          <button type="button" onClick={() => onChange(value.filter((_, j) => j !== i))} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove destination">
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      ))}
      <Button variant="secondary" icon={<Plus className="w-4 h-4" />} onClick={() => onChange([...value, newTarget("file")])}>
        Add destination
      </Button>
    </div>
  );
};

const VariableCard: React.FC<{
  name: string;
  def: VariableDef;
  onRename: (n: string) => void;
  onChange: (d: VariableDef) => void;
  onRemove: () => void;
}> = ({ name, def, onRename, onChange, onRemove }) => (
  <div className="rounded-lg bg-surface-1 border border-border-subtle p-3 space-y-3">
    <div className="flex flex-wrap items-end gap-3">
      <Field label="Name (without $)" className="w-48">
        <Input value={name} onChange={(e) => onRename(e.target.value.replace(/^\$/, ""))} mono />
      </Field>
      <Field label="Type" className="w-44">
        <Select value={def.type} onChange={(e) => onChange(newVariable(e.target.value as VariableDef["type"]))}>
          <option value="map">map</option>
          <option value="geo">geo</option>
          <option value="split_clients">split_clients</option>
          <option value="json">json (json_set)</option>
        </Select>
      </Field>
      <Field label={def.type === "geo" ? "Source (empty = $remote_addr)" : "Source"} className="flex-1 min-w-[160px]">
        <Input
          value={def.source ?? ""}
          onChange={(e) => onChange({ ...def, source: e.target.value } as VariableDef)}
          mono
        />
      </Field>
      {def.type === "json" && (
        <Field label="Path" className="w-48">
          <Input value={def.path} onChange={(e) => onChange({ ...def, path: e.target.value })} placeholder=".user.id" mono />
        </Field>
      )}
      {(def.type === "map" || def.type === "geo") && (
        <Field label="Default" className="w-36">
          <Input value={def.default} onChange={(e) => onChange({ ...def, default: e.target.value })} mono />
        </Field>
      )}
      <button type="button" onClick={onRemove} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove variable">
        <Trash2 className="w-4 h-4" />
      </button>
    </div>
    {def.type === "map" && (
      <PairListEditor
        value={def.entries.map((e) => [e.pattern, e.value] as [string, string])}
        onChange={(v) => onChange({ ...def, entries: v.map(([pattern, value]) => ({ pattern, value })) })}
        leftPlaceholder="value, ~regex or ~*regex"
        rightPlaceholder="result"
        addLabel="Add entry"
      />
    )}
    {def.type === "geo" && (
      <PairListEditor
        value={def.ranges.map((r) => [r.network, r.value] as [string, string])}
        onChange={(v) => onChange({ ...def, ranges: v.map(([network, value]) => ({ network, value })) })}
        leftPlaceholder="10.0.0.0/8"
        rightPlaceholder="result"
        addLabel="Add network"
      />
    )}
    {def.type === "split_clients" && (
      <>
        <PairListEditor
          value={def.buckets.map((b) => [String(b.percent), b.value] as [string, string])}
          onChange={(v) =>
            onChange({ ...def, buckets: v.map(([p, value]) => ({ percent: parseFloat(p) || 0, value })) })
          }
          leftPlaceholder="percent (0 = the rest)"
          rightPlaceholder="value"
          addLabel="Add bucket"
        />
        <p className="text-[11px] text-content-muted">
          Total: {def.buckets.reduce((a, b) => a + b.percent, 0)}% — a bucket with 0% receives the rest (nginx <code>*</code>).
        </p>
      </>
    )}
  </div>
);

export const ServerSettings: React.FC<Props> = ({ config, onRefreshConfig }) => {
  const [draft, setDraft] = useState<FerrixConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [issues, setIssues] = useState<ValidationIssue[]>([]);

  useEffect(() => {
    if (config) setDraft(structuredClone(config));
  }, [config]);

  if (!draft) {
    return <p className="text-sm text-content-muted">Loading configuration...</p>;
  }

  const http = draft.http;
  const logging = draft.logging ?? {};
  const errLog = logging.error ?? {};
  const accLog = logging.access ?? {};
  const setLogging = (p: Partial<typeof logging>) => setDraft({ ...draft, logging: { ...logging, ...p } });
  const vars = http.variables ?? {};
  const streams = draft.streams ?? [];
  const upstreamNames = Object.keys(draft.upstreams);
  const setHttp = (p: Partial<typeof http>) => setDraft({ ...draft, http: { ...http, ...p } });
  const setVars = (v: Record<string, VariableDef>) => setHttp({ variables: v });
  const setStreams = (s: StreamConfig[]) => setDraft({ ...draft, streams: s });
  const patchStream = (i: number, p: Partial<StreamConfig>) =>
    setStreams(streams.map((s, j) => (j === i ? { ...s, ...p } : s)));

  const renameVar = (from: string, to: string) => {
    const next: Record<string, VariableDef> = {};
    for (const [k, v] of Object.entries(vars)) next[k === from ? to : k] = v;
    setVars(next);
  };

  const save = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    setIssues([]);
    try {
      await api.updateConfig(draft);
      await onRefreshConfig();
      setSuccess("Saved and hot-reloaded.");
      setTimeout(() => setSuccess(null), 4000);
    } catch (err: any) {
      if (err instanceof ApiError && Array.isArray(err.body?.report?.issues)) {
        setIssues(err.body.report.issues);
        setError(err.body.report.summary || "Invalid configuration");
      } else {
        setError(err.message || "Failed to save");
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <Settings className="w-6 h-6 text-brand-400" />
          <div>
            <h1 className="text-xl font-bold text-content-primary">Server settings</h1>
            <p className="text-xs text-content-muted">Access log, variables (map / geo / split_clients) and TCP/UDP streams</p>
          </div>
        </div>
        <Button variant="primary" icon={<Save className="w-4 h-4" />} onClick={save} loading={saving}>
          Save and reload
        </Button>
      </div>

      {error && (
        <div className="p-3 rounded-lg bg-danger-500/10 border border-danger-500/30 text-danger-400 text-xs space-y-1">
          <div>{error}</div>
          {issues.map((i, k) => (
            <div key={k} className="font-mono">
              [{i.severity}] {i.path}: {i.message}
            </div>
          ))}
        </div>
      )}
      {success && (
        <div className="p-3 rounded-lg bg-success-500/10 border border-success-500/30 text-success-400 text-xs">{success}</div>
      )}

      <Section
        title="Access log to a file (access_log / log_format)"
        hint="Works with metrics on or off; routes with access log off are skipped"
        enabled={!!http.access_log}
        onToggle={(on) => setHttp({ access_log: on ? { path: "/var/log/ferrix/access.log", format: "combined" } : null })}
      >
        {http.access_log && (
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            <Field label="File path">
              <Input value={http.access_log.path} onChange={(e) => setHttp({ access_log: { ...http.access_log!, path: e.target.value } })} mono className="text-xs" />
            </Field>
            <Field label="Format" hint={`"combined" or variables: ${LOG_VARS}`} className="md:col-span-2">
              <Input value={http.access_log.format} onChange={(e) => setHttp({ access_log: { ...http.access_log!, format: e.target.value } })} mono className="text-xs" />
            </Field>
          </div>
        )}
      </Section>

      <Section title="Error log (logging.error)" hint="Default: info level, text, stderr. RUST_LOG overrides the level">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Field label="Level" hint='error, warn, info, debug or "warn,ferrix=debug"'>
            <Input value={errLog.level ?? ""} placeholder="info" onChange={(e) => setLogging({ error: { ...errLog, level: e.target.value || null } })} mono />
          </Field>
          <Field label="Format">
            <Select value={errLog.format ?? "text"} onChange={(e) => setLogging({ error: { ...errLog, format: e.target.value as "text" | "json" } })}>
              <option value="text">text</option>
              <option value="json">json</option>
            </Select>
          </Field>
        </div>
        <Toggle
          checked={Array.isArray(errLog.targets)}
          onChange={(on) => setLogging({ error: { ...errLog, targets: on ? [{ kind: "stderr" }] : null } })}
          label="Choose destinations"
          description="Off = stderr. On with an empty list = error log disabled"
        />
        {Array.isArray(errLog.targets) && (
          <LogTargetsEditor value={errLog.targets} onChange={(t) => setLogging({ error: { ...errLog, targets: t } })} />
        )}
      </Section>

      <Section title="Access log destinations (logging.access)" hint="When set, replaces the access log file above. No destination = no access log">
        <Field label="Format" hint={`"combined", "json" or variables: ${LOG_VARS}`}>
          <Input value={accLog.format ?? ""} placeholder="combined" onChange={(e) => setLogging({ access: { ...accLog, format: e.target.value } })} mono className="text-xs" />
        </Field>
        <LogTargetsEditor value={accLog.targets ?? []} onChange={(t) => setLogging({ access: { ...accLog, targets: t } })} />
      </Section>

      <Section title="Workers and protocols" hint="worker_processes, worker_connections, worker_rlimit_nofile, server_tokens; workers and protocols apply after a restart">
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <Field label="Worker threads">
            <Input type="number" value={http.worker_threads} onChange={(e) => setHttp({ worker_threads: parseInt(e.target.value) || 1 })} mono />
          </Field>
          <Field label="Connections per worker">
            <Input type="number" value={http.worker_connections ?? 4096} onChange={(e) => setHttp({ worker_connections: parseInt(e.target.value) || 1 })} mono />
          </Field>
          <Field label="Open files limit" hint="empty = system default">
            <Input type="number" value={http.worker_rlimit_nofile ?? ""} onChange={(e) => setHttp({ worker_rlimit_nofile: e.target.value ? parseInt(e.target.value) : null })} mono />
          </Field>
          <Field label="CPU affinity" hint='"off", "auto" or 0,2,4'>
            <Input
              value={Array.isArray(http.worker_cpu_affinity) ? http.worker_cpu_affinity.join(",") : http.worker_cpu_affinity ?? "off"}
              onChange={(e) => {
                const v = e.target.value.trim();
                setHttp({ worker_cpu_affinity: /^[\d,\s]+$/.test(v) ? v.split(/[\s,]+/).filter(Boolean).map(Number) : v });
              }}
              mono
            />
          </Field>
        </div>
        <div className="flex flex-wrap gap-4">
          {(["http1", "http2", "http3"] as const).map((p) => (
            <Toggle
              key={p}
              checked={(http.protocols ?? ["http1", "http2"]).includes(p)}
              onChange={(on) => {
                const cur = http.protocols ?? ["http1", "http2"];
                setHttp({ protocols: on ? [...cur.filter((x) => x !== p), p] : cur.filter((x) => x !== p) });
              }}
              label={p.toUpperCase().replace("HTTP", "HTTP/")}
            />
          ))}
        </div>
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Toggle checked={http.server_tokens !== false} onChange={(v) => setHttp({ server_tokens: v })} label="Show the version (server_tokens)" />
          <Toggle checked={http.default_page !== false} onChange={(v) => setHttp({ default_page: v })} label="Welcome page for unknown hosts" />
          <Toggle checked={!!http.request_id} onChange={(v) => setHttp({ request_id: v })} label="X-Request-Id on every request" />
        </div>
      </Section>

      <Section title="Request validation" hint="underscores_in_headers, ignore_invalid_headers, large_client_header_buffers, allowed methods">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Field label="Max header line (bytes)">
            <Input type="number" value={http.max_header_line_bytes ?? 8192} onChange={(e) => setHttp({ max_header_line_bytes: parseInt(e.target.value) || 8192 })} mono />
          </Field>
          <Field label="Allowed methods" hint="empty = GET HEAD POST PUT DELETE PATCH OPTIONS CONNECT">
            <Input value={(http.allowed_methods ?? []).join(" ")} onChange={(e) => { const m = e.target.value.toUpperCase().split(/[\s,]+/).filter(Boolean); setHttp({ allowed_methods: m.length ? m : null }); }} mono className="text-xs" />
          </Field>
        </div>
        <Toggle checked={!!http.underscores_in_headers} onChange={(v) => setHttp({ underscores_in_headers: v })} label="Accept headers with _ in the name" />
        <Toggle checked={http.ignore_invalid_headers !== false} onChange={(v) => setHttp({ ignore_invalid_headers: v })} label="Ignore invalid header lines (off = 400)" />
      </Section>

      <Section title="Static files" hint="In-memory cache, default_type and extra MIME types">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Field label="Cache files up to (bytes)" hint="0 = cache off">
            <Input type="number" value={http.static_cache?.max_file_bytes ?? 1048576} onChange={(e) => setHttp({ static_cache: { max_total_bytes: http.static_cache?.max_total_bytes ?? 67108864, max_file_bytes: parseInt(e.target.value) || 0 } })} mono />
          </Field>
          <Field label="Cache total (bytes)">
            <Input type="number" value={http.static_cache?.max_total_bytes ?? 67108864} onChange={(e) => setHttp({ static_cache: { max_file_bytes: http.static_cache?.max_file_bytes ?? 1048576, max_total_bytes: parseInt(e.target.value) || 0 } })} mono />
          </Field>
          <Field label="default_type" hint="unknown extensions and return bodies">
            <Input value={http.default_type ?? ""} placeholder="text/plain" onChange={(e) => setHttp({ default_type: e.target.value || null })} mono className="text-xs" />
          </Field>
        </div>
        <Field label="Extra MIME types (extension → type)">
          <KeyValueEditor value={http.mime_types ?? {}} onChange={(v) => setHttp({ mime_types: v })} keyPlaceholder="webmanifest" valuePlaceholder="application/manifest+json" addLabel="Add type" emptyText="No extra types." />
        </Field>
      </Section>

      <Section title="Let's Encrypt account (acme)" hint="Used by virtual hosts with tls.acme on">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Field label="E-mail">
            <Input value={draft.acme?.email ?? ""} onChange={(e) => setDraft({ ...draft, acme: { ...draft.acme, email: e.target.value || null } })} mono className="text-xs" />
          </Field>
          <Field label="Directory URL">
            <Input value={draft.acme?.directory_url ?? "https://acme-v02.api.letsencrypt.org/directory"} onChange={(e) => setDraft({ ...draft, acme: { ...draft.acme, directory_url: e.target.value } })} mono className="text-xs" />
          </Field>
          <Field label="Storage folder">
            <Input value={draft.acme?.storage_dir ?? "./acme"} onChange={(e) => setDraft({ ...draft, acme: { ...draft.acme, storage_dir: e.target.value } })} mono className="text-xs" />
          </Field>
          <Field label="Renew days before expiry">
            <Input type="number" value={draft.acme?.renew_before_days ?? 30} onChange={(e) => setDraft({ ...draft, acme: { ...draft.acme, renew_before_days: parseInt(e.target.value) || 30 } })} mono />
          </Field>
        </div>
      </Section>

      <Section
        title="Compression (gzip, brotli, zstd)"
        hint="Negotiated from Accept-Encoding; a route can override the switch"
        enabled={!!http.compression?.enabled}
        onToggle={(on) => setHttp({ compression: { ...http.compression, enabled: on } })}
      >
        {http.compression?.enabled && (
          <>
            <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
              <Field label="Level">
                <Select value={http.compression.level ?? "default"} onChange={(e) => setHttp({ compression: { ...http.compression, level: e.target.value as "fastest" | "default" | "best" } })}>
                  <option value="fastest">fastest</option>
                  <option value="default">default</option>
                  <option value="best">best</option>
                </Select>
              </Field>
              <Field label="Min length (bytes)">
                <Input type="number" value={http.compression.min_length ?? 1024} onChange={(e) => setHttp({ compression: { ...http.compression, min_length: parseInt(e.target.value) || 0 } })} mono />
              </Field>
              {(["gzip_level", "brotli_level", "zstd_level"] as const).map((k) => (
                <Field key={k} label={k} hint="empty = use Level">
                  <Input type="number" value={http.compression![k] ?? ""} onChange={(e) => setHttp({ compression: { ...http.compression, [k]: e.target.value === "" ? null : parseInt(e.target.value) } })} mono />
                </Field>
              ))}
            </div>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              <Field label="Algorithms" hint="in order of preference: zstd br gzip">
                <Input value={(http.compression.algorithms ?? ["zstd", "br", "gzip"]).join(" ")} onChange={(e) => setHttp({ compression: { ...http.compression, algorithms: e.target.value.split(/[\s,]+/).filter(Boolean) } })} mono className="text-xs" />
              </Field>
              <Field label="MIME types" hint="space-separated; text/* style prefixes allowed">
                <Input value={(http.compression.types ?? []).join(" ")} onChange={(e) => setHttp({ compression: { ...http.compression, types: e.target.value.split(/[\s,]+/).filter(Boolean) } })} mono className="text-xs" />
              </Field>
            </div>
          </>
        )}
      </Section>

      <Section title="Connection limits and shutdown" hint="keepalive_requests, keepalive_time, etag, if_modified_since, max_ranges, worker_shutdown_timeout">
        <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
          <Field label="Keep-alive requests" hint="0 = no limit">
            <Input type="number" value={http.keepalive_requests ?? 1000} onChange={(e) => setHttp({ keepalive_requests: parseInt(e.target.value) || 0 })} mono />
          </Field>
          <Field label="Keep-alive time (ms)" hint="0 = no limit">
            <Input type="number" value={http.keepalive_time_ms ?? 3600000} onChange={(e) => setHttp({ keepalive_time_ms: parseInt(e.target.value) || 0 })} mono />
          </Field>
          <Field label="Shutdown timeout (ms)" hint="wait for requests in flight">
            <Input type="number" value={http.shutdown_timeout_ms ?? 30000} onChange={(e) => setHttp({ shutdown_timeout_ms: parseInt(e.target.value) || 0 })} mono />
          </Field>
          <Field label="If-Modified-Since">
            <Select value={http.if_modified_since ?? "exact"} onChange={(e) => setHttp({ if_modified_since: e.target.value as "off" | "exact" | "before" })}>
              <option value="exact">exact</option>
              <option value="before">before</option>
              <option value="off">off</option>
            </Select>
          </Field>
          <Field label="Max ranges" hint="empty = default, 0 = off">
            <Input type="number" value={http.max_ranges ?? ""} onChange={(e) => setHttp({ max_ranges: e.target.value === "" ? null : parseInt(e.target.value) })} mono />
          </Field>
        </div>
        <Toggle checked={http.etag !== false} onChange={(v) => setHttp({ etag: v })} label="ETag on static files" />
      </Section>

      <Section title="TLS (http.tls)" hint="ssl_protocols, ssl_ciphers and session tickets for every HTTPS port; applies after a restart">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Field label="Protocols" hint="Empty = TLSv1.2 TLSv1.3">
            <Input value={(http.tls?.protocols ?? []).join(" ")} placeholder="TLSv1.2 TLSv1.3" onChange={(e) => setHttp({ tls: { ...http.tls, protocols: e.target.value.split(/[\s,]+/).filter(Boolean) } })} mono className="text-xs" />
          </Field>
          <Field label="Ciphers" hint="Empty = defaults; OpenSSL or IANA names separated by : or space">
            <Input value={(http.tls?.ciphers ?? []).join(":")} onChange={(e) => setHttp({ tls: { ...http.tls, ciphers: e.target.value.split(/[\s:,]+/).filter(Boolean) } })} mono className="text-xs" />
          </Field>
          <Field label="Session ticket key file" hint="ssl_session_ticket_key (80 random bytes)">
            <Input value={http.tls?.session_ticket_key ?? ""} onChange={(e) => setHttp({ tls: { ...http.tls, session_ticket_key: e.target.value || null } })} mono className="text-xs" />
          </Field>
        </div>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Toggle checked={http.tls?.session_tickets !== false} onChange={(v) => setHttp({ tls: { ...http.tls, session_tickets: v } })} label="Session tickets" />
          <Toggle checked={!!http.tls?.early_data} onChange={(v) => setHttp({ tls: { ...http.tls, early_data: v } })} label="0-RTT early data (HTTP/3 only)" />
        </div>
      </Section>

      <Section title="Cache storage (cache_store)" hint="Where route caches live: memory, disk (proxy_cache_path) or Redis / Dragonfly / KeyDB / Valkey">
        <div className="grid grid-cols-1 md:grid-cols-4 gap-3">
          <Field label="Kind">
            <Select
              value={http.cache_store?.kind ?? "memory"}
              onChange={(e) => setHttp({ cache_store: { ...(http.cache_store ?? { kind: "memory" }), kind: e.target.value as any } })}
            >
              <option value="memory">memory</option>
              <option value="disk">disk</option>
              <option value="redis">redis / dragonfly</option>
            </Select>
          </Field>
          {http.cache_store?.kind === "disk" && (
            <>
              <Field label="Directory" className="md:col-span-2">
                <Input value={http.cache_store.path ?? ""} onChange={(e) => setHttp({ cache_store: { ...http.cache_store!, path: e.target.value || null } })} placeholder="/var/cache/ferrix" mono className="text-xs" />
              </Field>
              <Field label="Max bytes" hint="0 = no limit">
                <Input type="number" value={http.cache_store.max_bytes ?? 0} onChange={(e) => setHttp({ cache_store: { ...http.cache_store!, max_bytes: parseInt(e.target.value) || 0 } })} mono />
              </Field>
            </>
          )}
          {http.cache_store?.kind === "redis" && (
            <>
              <Field label="URL" className="md:col-span-2">
                <Input value={http.cache_store.url ?? ""} onChange={(e) => setHttp({ cache_store: { ...http.cache_store!, url: e.target.value || null } })} placeholder="redis://:password@127.0.0.1:6379/0" mono className="text-xs" />
              </Field>
              <Field label="Key prefix">
                <Input value={http.cache_store.prefix ?? ""} onChange={(e) => setHttp({ cache_store: { ...http.cache_store!, prefix: e.target.value || null } })} placeholder="ferrix:cache:" mono className="text-xs" />
              </Field>
            </>
          )}
        </div>
      </Section>

      <Section title="Connections and URLs">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Toggle checked={!!http.proxy_protocol} onChange={(v) => setHttp({ proxy_protocol: v })} label="PROXY protocol on HTTP listeners" description="listen ... proxy_protocol (AWS NLB, HAProxy); applies after a restart" />
          <Toggle checked={http.merge_slashes !== false} onChange={(v) => setHttp({ merge_slashes: v })} label="merge_slashes" description="//a///b is handled as /a/b" />
          <Toggle checked={http.absolute_redirect !== false} onChange={(v) => setHttp({ absolute_redirect: v })} label="absolute_redirect" description="Ferrix's own redirects get scheme://host" />
          <Toggle checked={http.port_in_redirect !== false} onChange={(v) => setHttp({ port_in_redirect: v })} label="port_in_redirect" />
          <Toggle checked={!!http.server_name_in_redirect} onChange={(v) => setHttp({ server_name_in_redirect: v })} label="server_name_in_redirect" />
          <Toggle checked={!!http.proxy_forwarded_headers} onChange={(v) => setHttp({ proxy_forwarded_headers: v })} label="Automatic X-Forwarded-* headers" description="Off = nginx behaviour: only the route's proxy_set_headers" />
          <Toggle checked={!!http.x_accel_redirect} onChange={(v) => setHttp({ x_accel_redirect: v })} label="Follow X-Accel-Redirect" description="Backends can hand a request to another route" />
        </div>
      </Section>

      <Section
        title="GeoIP (MaxMind .mmdb)"
        hint="$geoip_country_code, $geoip_country_name, $geoip_city, $geoip_region, $geoip_postal_code, $geoip_latitude, $geoip_longitude, $geoip_org, $geoip_asn"
        enabled={!!http.geoip}
        onToggle={(on) => setHttp({ geoip: on ? { country_db: "/usr/share/GeoIP/GeoLite2-Country.mmdb", city_db: null, asn_db: null } : null })}
      >
        {http.geoip && (
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            {(["country_db", "city_db", "asn_db"] as const).map((k) => (
              <Field key={k} label={k}>
                <Input value={http.geoip![k] ?? ""} onChange={(e) => setHttp({ geoip: { ...http.geoip!, [k]: e.target.value || null } })} mono className="text-xs" />
              </Field>
            ))}
          </div>
        )}
      </Section>

      <Section title="HTTP/2 and HTTP/3 limits" hint="Empty = default; applies after a restart">
        <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
          {(["max_concurrent_streams", "max_header_list_size", "initial_stream_window", "initial_connection_window", "max_frame_size", "keepalive_interval_ms"] as const).map((k) => (
            <Field key={k} label={`http2.${k}`}>
              <Input type="number" value={http.http2?.[k] ?? ""} onChange={(e) => setHttp({ http2: { ...(http.http2 ?? {}), [k]: e.target.value === "" ? null : parseInt(e.target.value) } })} mono />
            </Field>
          ))}
          {(["max_concurrent_streams", "stream_buffer_size", "idle_timeout_ms"] as const).map((k) => (
            <Field key={k} label={`http3.${k}`}>
              <Input type="number" value={http.http3?.[k] ?? ""} onChange={(e) => setHttp({ http3: { ...(http.http3 ?? {}), [k]: e.target.value === "" ? null : parseInt(e.target.value) } })} mono />
            </Field>
          ))}
        </div>
      </Section>

      <Section title="Variables (http.variables)" hint="Use them as $name in rewrites, if, set, proxy headers and upstream: $name">
        <div className="space-y-3">
          {Object.entries(vars).map(([name, def], i) => (
            <VariableCard
              key={i}
              name={name}
              def={def}
              onRename={(n) => renameVar(name, n)}
              onChange={(d) => setVars({ ...vars, [name]: d })}
              onRemove={() => {
                const { [name]: _, ...rest } = vars;
                setVars(rest);
              }}
            />
          ))}
          <Button
            variant="secondary"
            size="sm"
            icon={<Plus className="w-3.5 h-3.5" />}
            onClick={() => {
              let n = 1;
              while (vars[`var${n}`]) n++;
              setVars({ ...vars, [`var${n}`]: newVariable("map") });
            }}
          >
            Add variable
          </Button>
        </div>
      </Section>

      <Section title="TCP/UDP streams (stream {})" hint="Layer-4 proxy; TLS streams can route by server name (ssl_preread) without terminating TLS">
        <div className="space-y-3">
          {streams.map((s, i) => (
            <div key={i} className="rounded-lg bg-surface-1 border border-border-subtle p-3 space-y-3">
              <div className="flex flex-wrap items-end gap-3">
                <Field label="Id" className="w-36">
                  <Input value={s.id} onChange={(e) => patchStream(i, { id: e.target.value })} mono />
                </Field>
                <Field label="Listen" className="w-44">
                  <Input value={s.listen} onChange={(e) => patchStream(i, { listen: e.target.value })} placeholder="0.0.0.0:443" mono />
                </Field>
                <Field label="Protocol" className="w-24">
                  <Select value={s.protocol} onChange={(e) => patchStream(i, { protocol: e.target.value as StreamProtocol })}>
                    <option value="tcp">TCP</option>
                    <option value="udp">UDP</option>
                  </Select>
                </Field>
                <Field label="Upstream (default)" className="w-44">
                  <Select value={s.upstream} onChange={(e) => patchStream(i, { upstream: e.target.value })}>
                    {upstreamNames.map((u) => (
                      <option key={u} value={u}>
                        {u}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Connect timeout (ms)" className="w-36">
                  <Input type="number" value={s.connect_timeout_ms} onChange={(e) => patchStream(i, { connect_timeout_ms: parseInt(e.target.value) || 5000 })} mono />
                </Field>
                <Field label="Idle timeout (ms)" className="w-36">
                  <Input type="number" value={s.idle_timeout_ms} onChange={(e) => patchStream(i, { idle_timeout_ms: parseInt(e.target.value) || 600000 })} mono />
                </Field>
                <button type="button" onClick={() => setStreams(streams.filter((_, j) => j !== i))} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove stream">
                  <Trash2 className="w-4 h-4" />
                </button>
              </div>
              {s.protocol === "tcp" && (
                <>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    <Toggle checked={!!s.accept_proxy_protocol} onChange={(v) => patchStream(i, { accept_proxy_protocol: v })} label="Clients send the PROXY protocol" />
                    <Toggle checked={!!s.send_proxy_protocol} onChange={(v) => patchStream(i, { send_proxy_protocol: v })} label="Send the PROXY protocol upstream" />
                    <Toggle checked={!!s.tls} onChange={(on) => patchStream(i, { tls: on ? { cert_path: "", key_path: "" } : null })} label="Terminate TLS (listen ... ssl)" />
                    <Toggle checked={!!s.upstream_tls} onChange={(on) => patchStream(i, { upstream_tls: on ? { verify: true } : null })} label="TLS to the upstream (proxy_ssl)" />
                  </div>
                  {s.tls && (
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                      <Field label="Certificate (PEM)"><Input value={s.tls.cert_path} onChange={(e) => patchStream(i, { tls: { ...s.tls!, cert_path: e.target.value } })} mono className="text-xs" /></Field>
                      <Field label="Key (PEM)"><Input value={s.tls.key_path} onChange={(e) => patchStream(i, { tls: { ...s.tls!, key_path: e.target.value } })} mono className="text-xs" /></Field>
                    </div>
                  )}
                  {s.upstream_tls && (
                    <div className="grid grid-cols-2 md:grid-cols-5 gap-3 items-end">
                      <Toggle checked={s.upstream_tls.verify} onChange={(v) => patchStream(i, { upstream_tls: { ...s.upstream_tls!, verify: v } })} label="Verify" />
                      {(["server_name", "trusted_ca", "client_cert", "client_key"] as const).map((k) => (
                        <Field key={k} label={k}><Input value={s.upstream_tls![k] ?? ""} onChange={(e) => patchStream(i, { upstream_tls: { ...s.upstream_tls!, [k]: e.target.value || null } })} mono className="text-xs" /></Field>
                      ))}
                    </div>
                  )}
                  <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
                    <Field label="Max conns per IP"><Input type="number" value={s.limit_conn ?? ""} onChange={(e) => patchStream(i, { limit_conn: e.target.value ? parseInt(e.target.value) : null })} mono /></Field>
                    <Field label="Upload rate (B/s)"><Input type="number" value={s.upload_rate ?? 0} onChange={(e) => patchStream(i, { upload_rate: parseInt(e.target.value) || 0 })} mono /></Field>
                    <Field label="Download rate (B/s)"><Input type="number" value={s.download_rate ?? 0} onChange={(e) => patchStream(i, { download_rate: parseInt(e.target.value) || 0 })} mono /></Field>
                    <Field label="Return text" hint="answer and close"><Input value={s.return_text ?? ""} onChange={(e) => patchStream(i, { return_text: e.target.value || null })} mono className="text-xs" /></Field>
                    <Field label="Pass to" hint="local host:port"><Input value={s.pass ?? ""} onChange={(e) => patchStream(i, { pass: e.target.value || null })} mono className="text-xs" /></Field>
                  </div>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    <div><span className="block text-xs font-medium text-content-secondary mb-1">Allowed IPs</span><CidrListEditor value={s.allowed_ips ?? []} onChange={(v) => patchStream(i, { allowed_ips: v })} /></div>
                    <div><span className="block text-xs font-medium text-content-secondary mb-1">Denied IPs</span><CidrListEditor value={s.denied_ips ?? []} onChange={(v) => patchStream(i, { denied_ips: v })} /></div>
                  </div>
                  <Toggle
                    checked={Object.keys(s.sni_routes ?? {}).length > 0}
                    onChange={(on) => patchStream(i, { sni_routes: on ? { default: s.upstream } : {} })}
                    label="Route by TLS server name (ssl_preread)"
                    description="Exact name, *.suffix or default → upstream"
                  />
                  {Object.keys(s.sni_routes ?? {}).length > 0 && (
                    <KeyValueEditor
                      value={s.sni_routes ?? {}}
                      onChange={(v) => patchStream(i, { sni_routes: v })}
                      keyPlaceholder="api.example.com"
                      valuePlaceholder="upstream name"
                      addLabel="Add server name"
                    />
                  )}
                </>
              )}
            </div>
          ))}
          <Button
            variant="secondary"
            size="sm"
            icon={<Plus className="w-3.5 h-3.5" />}
            onClick={() =>
              setStreams([
                ...streams,
                {
                  id: `stream-${streams.length + 1}`,
                  listen: "0.0.0.0:5432",
                  protocol: "tcp",
                  upstream: upstreamNames[0] ?? "",
                  connect_timeout_ms: 5000,
                  idle_timeout_ms: 600000,
                  sni_routes: {},
                },
              ])
            }
          >
            Add stream
          </Button>
        </div>
      </Section>
    </div>
  );
};
