import React from "react";
import { Plus, Trash2 } from "lucide-react";
import {
  RouteRule,
  RewriteRule,
  RewriteFlag,
  ConditionConfig,
  CgiProtocol,
  ImageOp,
  UpstreamWithHealth,
} from "../services/api";
import { Field } from "./ui/Field";
import { Input, Select, Toggle } from "./ui/Input";
import { Button } from "./ui/Button";
import { KeyValueEditor } from "./ui/KeyValueEditor";
import { StringListEditor, PairListEditor, Section } from "./ui/ListEditors";

type Patch = (patch: Partial<RouteRule>) => void;

const num = (v: string) => (v === "" ? null : parseInt(v));

const DAV_METHODS = ["PUT", "DELETE", "MKCOL", "COPY", "MOVE"];
const REWRITE_FLAGS: { value: RewriteFlag; label: string }[] = [
  { value: "none", label: "(none) — keep going" },
  { value: "last", label: "last — match routes again" },
  { value: "break", label: "break — stop rewriting" },
  { value: "redirect", label: "redirect (302)" },
  { value: "permanent", label: "permanent (301)" },
];

export const newRewrite = (): RewriteRule => ({ pattern: "^/old/(.*)$", replacement: "/new/$1", flag: "none" });
export const newCondition = (): ConditionConfig => ({ condition: "$http_user_agent ~* bot", set: [], return_code: 403, return_value: null, rewrite: null });

// ---------- target-specific panels ----------

/** Fields for `target_type: return` and `target_type: cgi`. */
export const ExtraTargetFields: React.FC<{ route: RouteRule; upstreams: UpstreamWithHealth[]; onChange: Patch }> = ({
  route,
  upstreams,
  onChange,
}) => {
  if (route.target_type === "return") {
    return (
      <div className="grid grid-cols-1 md:grid-cols-4 gap-3">
        <Field label="Status code">
          <Input
            type="number"
            value={route.redirect_code ?? 200}
            onChange={(e) => onChange({ redirect_code: parseInt(e.target.value) || 200 })}
            mono
          />
        </Field>
        <Field label="Body" hint="$scheme, $host and $request_uri are expanded" className="md:col-span-3">
          <Input value={route.return_body ?? ""} onChange={(e) => onChange({ return_body: e.target.value })} placeholder="ok" mono className="text-xs" />
        </Field>
      </div>
    );
  }
  if (route.target_type === "cgi") {
    const cgi = route.cgi ?? { protocol: "fastcgi" as CgiProtocol, pass: "", root: null, index: null, params: {} };
    const set = (p: Partial<typeof cgi>) => onChange({ cgi: { ...cgi, ...p } });
    return (
      <div className="space-y-3">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Field label="Protocol">
            <Select value={cgi.protocol} onChange={(e) => set({ protocol: e.target.value as CgiProtocol })}>
              <option value="fastcgi">FastCGI (PHP-FPM)</option>
              <option value="scgi">SCGI</option>
              <option value="uwsgi">uwsgi</option>
            </Select>
          </Field>
          <Field label="Backend" hint="host:port, unix:/path.sock or an upstream name" className="md:col-span-2">
            <Input value={cgi.pass} onChange={(e) => set({ pass: e.target.value })} placeholder="unix:/run/php/php-fpm.sock" mono className="text-xs" list="cgi-upstreams" />
            <datalist id="cgi-upstreams">
              {upstreams.map((u) => (
                <option key={u.name} value={u.name} />
              ))}
            </datalist>
          </Field>
        </div>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Field label="Document root" hint="$document_root — defaults to the route root">
            <Input value={cgi.root ?? route.static_root ?? ""} onChange={(e) => set({ root: e.target.value || null })} placeholder="/var/www/html" mono className="text-xs" />
          </Field>
          <Field label="Index" hint="fastcgi_index">
            <Input value={cgi.index ?? ""} onChange={(e) => set({ index: e.target.value || null })} placeholder="index.php" mono className="text-xs" />
          </Field>
        </div>
        <div>
          <span className="block text-[11px] font-medium text-content-secondary mb-1.5">Extra params (fastcgi_param)</span>
          <KeyValueEditor value={cgi.params} onChange={(v) => set({ params: v })} keyPlaceholder="SCRIPT_FILENAME" valuePlaceholder="$document_root$fastcgi_script_name" addLabel="Add param" />
        </div>
      </div>
    );
  }
  return null;
};

// ---------- rewrite phase (route and vhost) ----------

export const RewriteRulesEditor: React.FC<{ value: RewriteRule[]; onChange: (v: RewriteRule[]) => void }> = ({ value, onChange }) => (
  <div className="space-y-2">
    {value.map((r, i) => {
      const set = (p: Partial<RewriteRule>) => onChange(value.map((x, j) => (j === i ? { ...x, ...p } : x)));
      return (
        <div key={i} className="flex flex-wrap items-center gap-2">
          <Input value={r.pattern} onChange={(e) => set({ pattern: e.target.value })} placeholder="^/old/(.*)$" mono className="flex-1 min-w-[140px] text-xs" />
          <Input value={r.replacement} onChange={(e) => set({ replacement: e.target.value })} placeholder="/new/$1" mono className="flex-1 min-w-[140px] text-xs" />
          <Select value={r.flag} onChange={(e) => set({ flag: e.target.value as RewriteFlag })} className="w-auto text-xs">
            {REWRITE_FLAGS.map((f) => (
              <option key={f.value} value={f.value}>
                {f.label}
              </option>
            ))}
          </Select>
          <button type="button" onClick={() => onChange(value.filter((_, j) => j !== i))} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove rewrite">
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        </div>
      );
    })}
    <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={() => onChange([...value, newRewrite()])}>
      Add rewrite
    </Button>
  </div>
);

export const ConditionsEditor: React.FC<{ value: ConditionConfig[]; onChange: (v: ConditionConfig[]) => void }> = ({ value, onChange }) => (
  <div className="space-y-3">
    {value.map((c, i) => {
      const set = (p: Partial<ConditionConfig>) => onChange(value.map((x, j) => (j === i ? { ...x, ...p } : x)));
      return (
        <div key={i} className="rounded-lg bg-surface-1 border border-border-subtle p-3 space-y-2">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-mono text-content-muted">if (</span>
            <Input value={c.condition} onChange={(e) => set({ condition: e.target.value })} placeholder="$http_user_agent ~* bot" mono className="flex-1 text-xs" />
            <span className="text-[11px] font-mono text-content-muted">)</span>
            <button type="button" onClick={() => onChange(value.filter((_, j) => j !== i))} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove condition">
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          </div>
          <div className="grid grid-cols-1 md:grid-cols-4 gap-2">
            <Field label="Return code" hint="empty = don't return">
              <Input type="number" value={c.return_code ?? ""} onChange={(e) => set({ return_code: num(e.target.value) })} mono />
            </Field>
            <Field label="Return URL (3xx) / body" className="md:col-span-3">
              <Input value={c.return_value ?? ""} onChange={(e) => set({ return_value: e.target.value || null })} mono className="text-xs" />
            </Field>
          </div>
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1">set (inside the if)</span>
            <PairListEditor value={c.set} onChange={(v) => set({ set: v })} leftPlaceholder="$var" rightPlaceholder="value" addLabel="Add set" />
          </div>
          <Toggle
            checked={!!c.rewrite}
            onChange={(on) => set({ rewrite: on ? newRewrite() : null })}
            label="Rewrite when it matches"
          />
          {c.rewrite && <RewriteRulesEditor value={[c.rewrite]} onChange={(v) => set({ rewrite: v[0] ?? null })} />}
        </div>
      );
    })}
    <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={() => onChange([...value, newCondition()])}>
      Add condition
    </Button>
  </div>
);

const VARS_HINT =
  "Variables: $uri $request_uri $args $arg_x $http_x $cookie_x $host $remote_addr $scheme $1..$9 and http.variables";

// ---------- all nginx-module options of a route ----------

export const RouteModulesEditor: React.FC<{ route: RouteRule; onChange: Patch }> = ({ route, onChange }) => {
  const isStatic = route.target_type === "static_files";
  const isProxy = route.target_type === "proxy_upstream";
  const authReq = route.auth_request;
  const sl = route.secure_link;
  const sf = route.sub_filter;
  const dav = route.dav;
  const img = route.image_filter;

  return (
    <div className="space-y-3">
      <Section title="Rewrite phase (if / set / rewrite)" hint={VARS_HINT}>
        <div>
          <span className="block text-[11px] font-medium text-content-secondary mb-1.5">Conditions (if)</span>
          <ConditionsEditor value={route.conditions ?? []} onChange={(v) => onChange({ conditions: v })} />
        </div>
        <div>
          <span className="block text-[11px] font-medium text-content-secondary mb-1.5">set</span>
          <PairListEditor value={route.set ?? []} onChange={(v) => onChange({ set: v })} leftPlaceholder="$tier" rightPlaceholder="$network" addLabel="Add set" />
        </div>
        <div>
          <span className="block text-[11px] font-medium text-content-secondary mb-1.5">Rewrites</span>
          <RewriteRulesEditor value={route.rewrites ?? []} onChange={(v) => onChange({ rewrites: v })} />
        </div>
      </Section>

      {isProxy && (
        <Section title="gRPC (grpc_pass)" hint="HTTP/2 to the upstream; http:// targets = h2c, https:// = TLS" enabled={!!route.grpc} onToggle={(on) => onChange({ grpc: on, websocket_enabled: on ? false : route.websocket_enabled })} />
      )}

      <Section
        title="Auth request (auth_request)"
        hint="Asks this URL first: 2xx passes, 401/403 go to the client, anything else is 500"
        enabled={!!authReq}
        onToggle={(on) => onChange({ auth_request: on ? { url: "http://auth:9000/check", set_headers: [], timeout_ms: null } : null })}
      >
        {authReq && (
          <>
            <div className="grid grid-cols-1 md:grid-cols-4 gap-3">
              <Field label="Auth URL" className="md:col-span-3">
                <Input value={authReq.url} onChange={(e) => onChange({ auth_request: { ...authReq, url: e.target.value } })} mono className="text-xs" />
              </Field>
              <Field label="Timeout (ms)">
                <Input type="number" value={authReq.timeout_ms ?? ""} onChange={(e) => onChange({ auth_request: { ...authReq, timeout_ms: num(e.target.value) } })} placeholder="5000" mono />
              </Field>
            </div>
            <div>
              <span className="block text-[11px] font-medium text-content-secondary mb-1.5">Headers copied to the backend (auth_request_set)</span>
              <StringListEditor value={authReq.set_headers} onChange={(v) => onChange({ auth_request: { ...authReq, set_headers: v } })} placeholder="X-User" addLabel="Add header" />
            </div>
          </>
        )}
      </Section>

      <Section title="Valid referers (hotlink protection)" hint="none, blocked, server_names, *.example.com, example.*, example.com/path, ~regex — others get 403">
        <StringListEditor value={route.valid_referers ?? []} onChange={(v) => onChange({ valid_referers: v })} placeholder="*.example.com" emptyText="No list — any Referer is accepted." />
      </Section>

      <Section
        title="Secure link (secure_link_md5)"
        hint="?md5=<hash>&expires=<unix>; wrong hash = 403, expired = 410"
        enabled={!!sl}
        onToggle={(on) => onChange({ secure_link: on ? { md5: "$secure_link_expires$uri$remote_addr secret", hash_arg: null, expires_arg: null } : null })}
      >
        {sl && (
          <div className="grid grid-cols-1 md:grid-cols-4 gap-3">
            <Field label="MD5 template" className="md:col-span-2">
              <Input value={sl.md5} onChange={(e) => onChange({ secure_link: { ...sl, md5: e.target.value } })} mono className="text-xs" />
            </Field>
            <Field label="Hash argument">
              <Input value={sl.hash_arg ?? ""} onChange={(e) => onChange({ secure_link: { ...sl, hash_arg: e.target.value || null } })} placeholder="md5" mono />
            </Field>
            <Field label="Expires argument">
              <Input value={sl.expires_arg ?? ""} onChange={(e) => onChange({ secure_link: { ...sl, expires_arg: e.target.value || null } })} placeholder="expires" mono />
            </Field>
          </div>
        )}
      </Section>

      <Section title="Mirror (mirror)" hint="Each request is copied (no body) to these base URLs; responses are ignored">
        <StringListEditor value={route.mirror ?? []} onChange={(v) => onChange({ mirror: v })} placeholder="http://shadow:8080" emptyText="No mirrors." />
      </Section>

      <Section
        title="Response text replacement (sub_filter)"
        hint="text/html (plus extra types); uncompressed bodies up to 8 MB"
        enabled={!!sf}
        onToggle={(on) => onChange({ sub_filter: on ? { replacements: [["", ""]], once: true, types: [] } : null })}
      >
        {sf && (
          <>
            <PairListEditor value={sf.replacements} onChange={(v) => onChange({ sub_filter: { ...sf, replacements: v } })} leftPlaceholder="http://old" rightPlaceholder="https://new" addLabel="Add replacement" />
            <Toggle checked={sf.once} onChange={(v) => onChange({ sub_filter: { ...sf, once: v } })} label="Only the first match (sub_filter_once)" />
            <Field label="Extra MIME types" hint="comma-separated; * = all">
              <Input value={sf.types.join(", ")} onChange={(e) => onChange({ sub_filter: { ...sf, types: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) } })} placeholder="application/javascript" mono className="text-xs" />
            </Field>
          </>
        )}
      </Section>

      <Section title="Server-side includes (ssi)" hint="include, echo, set, if/elif/else/endif in text/html" enabled={!!route.ssi} onToggle={(on) => onChange({ ssi: on })}>
        <Field label="Extra MIME types (ssi_types)" hint="comma-separated">
          <Input value={(route.ssi_types ?? []).join(", ")} onChange={(e) => onChange({ ssi_types: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} mono className="text-xs" />
        </Field>
      </Section>

      <Section
        title="Image filter (image_filter)"
        hint="Needs a binary built with --features image-filter"
        enabled={!!img}
        onToggle={(on) => onChange({ image_filter: on ? { op: "resize", width: 300, height: 0, rotate: 0, jpeg_quality: 75, buffer_bytes: 1048576 } : null })}
      >
        {img && (
          <div className="grid grid-cols-2 md:grid-cols-6 gap-3">
            <Field label="Operation">
              <Select value={img.op} onChange={(e) => onChange({ image_filter: { ...img, op: e.target.value as ImageOp } })}>
                <option value="resize">resize</option>
                <option value="crop">crop</option>
                <option value="rotate">rotate</option>
                <option value="size">size (JSON)</option>
                <option value="test">test</option>
              </Select>
            </Field>
            <Field label="Width" hint="0 = any">
              <Input type="number" value={img.width} onChange={(e) => onChange({ image_filter: { ...img, width: parseInt(e.target.value) || 0 } })} mono />
            </Field>
            <Field label="Height" hint="0 = any">
              <Input type="number" value={img.height} onChange={(e) => onChange({ image_filter: { ...img, height: parseInt(e.target.value) || 0 } })} mono />
            </Field>
            <Field label="Rotate">
              <Select value={String(img.rotate)} onChange={(e) => onChange({ image_filter: { ...img, rotate: parseInt(e.target.value) } })}>
                <option value="0">0</option>
                <option value="90">90</option>
                <option value="180">180</option>
                <option value="270">270</option>
              </Select>
            </Field>
            <Field label="JPEG quality">
              <Input type="number" value={img.jpeg_quality} onChange={(e) => onChange({ image_filter: { ...img, jpeg_quality: parseInt(e.target.value) || 75 } })} mono />
            </Field>
            <Field label="Buffer (bytes)">
              <Input type="number" value={img.buffer_bytes} onChange={(e) => onChange({ image_filter: { ...img, buffer_bytes: parseInt(e.target.value) || 1048576 } })} mono />
            </Field>
          </div>
        )}
      </Section>

      {isStatic && (
        <>
          <Section title="Directory listing (autoindex)" hint="Lists directories without index.html" enabled={!!route.autoindex} onToggle={(on) => onChange({ autoindex: on })} />
          <Section title="FLV pseudo-streaming (flv)" hint="video.flv?start=<byte>" enabled={!!route.flv} onToggle={(on) => onChange({ flv: on })} />
          <Section
            title="WebDAV (dav_methods)"
            hint="Lets clients change files — protect the route with auth or an IP list"
            enabled={!!dav}
            onToggle={(on) => onChange({ dav: on ? { methods: [...DAV_METHODS], create_full_put_path: true, access: null } : null })}
          >
            {dav && (
              <>
                <div className="flex flex-wrap gap-1.5">
                  {DAV_METHODS.map((m) => {
                    const active = dav.methods.includes(m);
                    return (
                      <button
                        key={m}
                        type="button"
                        onClick={() => onChange({ dav: { ...dav, methods: active ? dav.methods.filter((x) => x !== m) : [...dav.methods, m] } })}
                        className={`px-2 py-1 rounded-md text-[11px] font-mono border transition ${
                          active ? "bg-brand-500/10 text-brand-400 border-brand-500/40" : "bg-surface-2 text-content-muted border-border"
                        }`}
                      >
                        {m}
                      </button>
                    );
                  })}
                </div>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3 items-end">
                  <Toggle checked={dav.create_full_put_path} onChange={(v) => onChange({ dav: { ...dav, create_full_put_path: v } })} label="Create missing directories on PUT" />
                  <Field label="File mode (octal)" hint="dav_access, e.g. 644">
                    <Input
                      value={dav.access != null ? dav.access.toString(8) : ""}
                      onChange={(e) => {
                        const v = parseInt(e.target.value, 8);
                        onChange({ dav: { ...dav, access: Number.isNaN(v) ? null : v } });
                      }}
                      placeholder="644"
                      mono
                    />
                  </Field>
                </div>
              </>
            )}
          </Section>
        </>
      )}

      <Toggle checked={route.access_log !== false} onChange={(v) => onChange({ access_log: v })} label="Access log" description="Off = this route is not written to logs (access_log off)" />
    </div>
  );
};
