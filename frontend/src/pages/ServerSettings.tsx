import React, { useEffect, useState } from "react";
import { Plus, Save, Settings, Trash2 } from "lucide-react";
import {
  api,
  ApiError,
  FerrixConfig,
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
