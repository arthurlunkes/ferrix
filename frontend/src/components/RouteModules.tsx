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
  ErrorRedirect,
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
  if (route.target_type === "memcached") {
    const m = route.memcached ?? { pass: "127.0.0.1:11211", key: null, content_type: null, gzip_flag: null };
    const set = (p: Partial<typeof m>) => onChange({ memcached: { ...m, ...p } });
    return (
      <div className="grid grid-cols-1 md:grid-cols-4 gap-3">
        <Field label="memcached" hint="host:port">
          <Input value={m.pass} onChange={(e) => set({ pass: e.target.value })} mono className="text-xs" />
        </Field>
        <Field label="Key" hint="$memcached_key, default $uri">
          <Input value={m.key ?? ""} onChange={(e) => set({ key: e.target.value || null })} placeholder="$uri?$args" mono className="text-xs" />
        </Field>
        <Field label="Content-Type">
          <Input value={m.content_type ?? ""} onChange={(e) => set({ content_type: e.target.value || null })} placeholder="text/html" mono className="text-xs" />
        </Field>
        <Field label="gzip flag" hint="memcached_gzip_flag">
          <Input type="number" value={m.gzip_flag ?? ""} onChange={(e) => set({ gzip_flag: num(e.target.value) })} mono />
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

export const ErrorRedirectsEditor: React.FC<{ value: ErrorRedirect[]; onChange: (v: ErrorRedirect[]) => void }> = ({ value, onChange }) => (
  <div className="space-y-2">
    {value.map((r, i) => {
      const set = (p: Partial<ErrorRedirect>) => onChange(value.map((x, j) => (j === i ? { ...x, ...p } : x)));
      return (
        <div key={i} className="flex flex-wrap items-center gap-2">
          <Input
            value={r.codes.join(", ")}
            onChange={(e) => set({ codes: e.target.value.split(",").map((c) => parseInt(c.trim())).filter((n) => !Number.isNaN(n)) })}
            placeholder="404, 500"
            mono
            className="w-32 text-xs"
          />
          <Input value={r.status ?? ""} onChange={(e) => set({ status: e.target.value || null })} placeholder="= or 200 (empty: keep)" mono className="w-40 text-xs" />
          <Input value={r.target} onChange={(e) => set({ target: e.target.value })} placeholder="@fallback, /50x.html or https://..." mono className="flex-1 min-w-[160px] text-xs" />
          <button type="button" onClick={() => onChange(value.filter((_, j) => j !== i))} className="p-2 text-content-muted hover:text-danger-400" aria-label="Remove">
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        </div>
      );
    })}
    <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={() => onChange([...value, { codes: [404], target: "@fallback", status: null }])}>
      Add error redirect
    </Button>
  </div>
);

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

      {isProxy && (
        <Section title="Backend responses (proxy_redirect / proxy_cookie_* / intercept)" hint="Rewrite Location and Set-Cookie coming from the backend">
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">proxy_redirect (from → to; "default" or ~regex)</span>
            <PairListEditor value={route.proxy_redirect ?? []} onChange={(v) => onChange({ proxy_redirect: v })} leftPlaceholder="http://backend:8080/" rightPlaceholder="/app/" addLabel="Add redirect rule" />
          </div>
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">proxy_cookie_path</span>
            <PairListEditor value={route.proxy_cookie_path ?? []} onChange={(v) => onChange({ proxy_cookie_path: v })} leftPlaceholder="/" rightPlaceholder="/app/" addLabel="Add path rule" />
          </div>
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">proxy_cookie_domain</span>
            <PairListEditor value={route.proxy_cookie_domain ?? []} onChange={(v) => onChange({ proxy_cookie_domain: v })} leftPlaceholder="internal.local" rightPlaceholder="example.com" addLabel="Add domain rule" />
          </div>
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">proxy_cookie_flags (cookie, ~regex or * → flags)</span>
            <PairListEditor value={route.proxy_cookie_flags ?? []} onChange={(v) => onChange({ proxy_cookie_flags: v })} leftPlaceholder="*" rightPlaceholder="secure httponly samesite=strict" addLabel="Add flag rule" />
          </div>
          <Toggle checked={!!route.proxy_intercept_errors} onChange={(v) => onChange({ proxy_intercept_errors: v })} label="Use the vhost error pages for backend errors (proxy_intercept_errors)" />
        </Section>
      )}

      {isProxy && (
        <Section
          title="Response cache (proxy_cache)"
          hint="x-cache-status: HIT, MISS, STALE, UPDATING, REVALIDATED"
          enabled={!!route.cache}
          onToggle={(on) =>
            onChange({
              cache: on
                ? { valid_secs: 60, bypass_headers: [], bypass_cookies: [], bypass_query_params: [], use_stale: [], stale_keep_secs: 86400, lock: false, lock_timeout_ms: null, revalidate: false, background_update: false }
                : null,
            })
          }
        >
          {route.cache && (() => {
            const c = route.cache!;
            const set = (p: Partial<typeof c>) => onChange({ cache: { ...c, ...p } });
            const STALE = ["error", "timeout", "updating", "http_500", "http_502", "http_503", "http_504", "http_403", "http_404", "http_429"];
            const csv = (v: string) => v.split(",").map((s) => s.trim()).filter(Boolean);
            return (
              <>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
                  <Field label="Valid for (s)">
                    <Input type="number" value={c.valid_secs} onChange={(e) => set({ valid_secs: parseInt(e.target.value) || 0 })} mono />
                  </Field>
                  <Field label="Keep stale (s)">
                    <Input type="number" value={c.stale_keep_secs} onChange={(e) => set({ stale_keep_secs: parseInt(e.target.value) || 0 })} mono />
                  </Field>
                  <Field label="Lock timeout (ms)">
                    <Input type="number" value={c.lock_timeout_ms ?? ""} onChange={(e) => set({ lock_timeout_ms: num(e.target.value) })} placeholder="5000" mono />
                  </Field>
                </div>
                <div>
                  <span className="block text-[11px] font-medium text-content-secondary mb-1.5">Serve stale on (proxy_cache_use_stale)</span>
                  <div className="flex flex-wrap gap-1.5">
                    {STALE.map((s) => {
                      const on = c.use_stale.includes(s);
                      return (
                        <button key={s} type="button" onClick={() => set({ use_stale: on ? c.use_stale.filter((x) => x !== s) : [...c.use_stale, s] })}
                          className={`px-2 py-1 rounded-md text-[11px] font-mono border transition ${on ? "bg-brand-500/10 text-brand-400 border-brand-500/40" : "bg-surface-2 text-content-muted border-border"}`}>
                          {s}
                        </button>
                      );
                    })}
                  </div>
                </div>
                <div className="flex flex-wrap gap-6">
                  <Toggle checked={c.lock} onChange={(v) => set({ lock: v })} label="Lock (one request fills)" />
                  <Toggle checked={c.revalidate} onChange={(v) => set({ revalidate: v })} label="Revalidate (If-None-Match)" />
                  <Toggle checked={c.background_update} onChange={(v) => set({ background_update: v })} label="Background update" />
                </div>
                <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                  <Field label="Bypass headers" hint="comma-separated">
                    <Input value={c.bypass_headers.join(", ")} onChange={(e) => set({ bypass_headers: csv(e.target.value) })} mono className="text-xs" />
                  </Field>
                  <Field label="Bypass cookies">
                    <Input value={c.bypass_cookies.join(", ")} onChange={(e) => set({ bypass_cookies: csv(e.target.value) })} mono className="text-xs" />
                  </Field>
                  <Field label="Bypass query params">
                    <Input value={c.bypass_query_params.join(", ")} onChange={(e) => set({ bypass_query_params: csv(e.target.value) })} mono className="text-xs" />
                  </Field>
                </div>
              </>
            );
          })()}
        </Section>
      )}

      <Section
        title="Allowed methods (limit_except)"
        hint="Other methods get 403 unless the client IP is allowed; GET also allows HEAD"
        enabled={!!route.limit_except}
        onToggle={(on) => onChange({ limit_except: on ? { methods: ["GET"], allow: [] } : null })}
      >
        {route.limit_except && (
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            <Field label="Methods" hint="comma-separated">
              <Input
                value={route.limit_except.methods.join(", ")}
                onChange={(e) => onChange({ limit_except: { ...route.limit_except!, methods: e.target.value.split(",").map((s) => s.trim().toUpperCase()).filter(Boolean) } })}
                mono
                className="text-xs"
              />
            </Field>
            <div>
              <span className="block text-xs font-medium text-content-secondary mb-1">IPs allowed any method</span>
              <StringListEditor value={route.limit_except.allow} onChange={(v) => onChange({ limit_except: { ...route.limit_except!, allow: v } })} placeholder="10.0.0.0/8" />
            </div>
          </div>
        )}
      </Section>

      <Section title="Bandwidth (limit_rate)" hint="Bytes per second for each response, after the first N bytes; 0 = off">
        <div className="grid grid-cols-2 gap-3">
          <Field label="Rate (bytes/s)">
            <Input type="number" value={route.limit_rate ?? 0} onChange={(e) => onChange({ limit_rate: parseInt(e.target.value) || 0 })} mono />
          </Field>
          <Field label="After (bytes)">
            <Input type="number" value={route.limit_rate_after ?? 0} onChange={(e) => onChange({ limit_rate_after: parseInt(e.target.value) || 0 })} mono />
          </Field>
        </div>
      </Section>

      <Section title="Response body (addition / charset / gunzip)">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Field label="add_before_body" hint="URI on this vhost or full URL">
            <Input value={route.add_before_body ?? ""} onChange={(e) => onChange({ add_before_body: e.target.value || null })} placeholder="/header.html" mono className="text-xs" />
          </Field>
          <Field label="add_after_body">
            <Input value={route.add_after_body ?? ""} onChange={(e) => onChange({ add_after_body: e.target.value || null })} placeholder="/footer.html" mono className="text-xs" />
          </Field>
          <Field label="addition_types" hint="comma-separated, besides text/html">
            <Input value={(route.addition_types ?? []).join(", ")} onChange={(e) => onChange({ addition_types: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} mono className="text-xs" />
          </Field>
          <Field label="charset" hint="declared / converted to">
            <Input value={route.charset ?? ""} onChange={(e) => onChange({ charset: e.target.value || null })} placeholder="utf-8" mono className="text-xs" />
          </Field>
          <Field label="source_charset">
            <Input value={route.source_charset ?? ""} onChange={(e) => onChange({ source_charset: e.target.value || null })} placeholder="windows-1251" mono className="text-xs" />
          </Field>
          <Field label="charset_types" hint="comma-separated">
            <Input value={(route.charset_types ?? []).join(", ")} onChange={(e) => onChange({ charset_types: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} mono className="text-xs" />
          </Field>
        </div>
        <div className="flex flex-wrap gap-6">
          <Toggle checked={!!route.override_charset} onChange={(v) => onChange({ override_charset: v })} label="override_charset" />
          <Toggle checked={!!route.gunzip} onChange={(v) => onChange({ gunzip: v })} label="gunzip for clients without gzip" />
        </div>
      </Section>

      <Section
        title="Visitor id cookie (userid)"
        enabled={!!route.userid}
        onToggle={(on) => onChange({ userid: on ? { name: "uid", path: "/", expires_secs: 31536000 } : null })}
      >
        {route.userid && (
          <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
            {(["name", "domain", "path", "flags"] as const).map((k) => (
              <Field key={k} label={k}>
                <Input value={(route.userid as any)[k] ?? ""} onChange={(e) => onChange({ userid: { ...route.userid!, [k]: e.target.value || null } })} mono className="text-xs" />
              </Field>
            ))}
            <Field label="Max-Age (s)">
              <Input type="number" value={route.userid.expires_secs ?? ""} onChange={(e) => onChange({ userid: { ...route.userid!, expires_secs: num(e.target.value) } })} mono />
            </Field>
          </div>
        )}
      </Section>

      <Section
        title="CONNECT forward proxy (tunnel_pass)"
        hint="CONNECT host:port opens a TCP tunnel; keep it private with an IP list"
        enabled={!!route.tunnel}
        onToggle={(on) => onChange({ tunnel: on ? { pass: null, allowed_ports: [443], allowed_hosts: [], idle_timeout_ms: null } : null })}
      >
        {route.tunnel && (
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            <Field label="Allowed ports" hint="comma-separated">
              <Input value={route.tunnel.allowed_ports.join(", ")} onChange={(e) => onChange({ tunnel: { ...route.tunnel!, allowed_ports: e.target.value.split(",").map((s) => parseInt(s.trim())).filter((n) => !Number.isNaN(n)) } })} mono />
            </Field>
            <Field label="Allowed hosts" hint="comma-separated, *.example.com ok; empty = any">
              <Input value={route.tunnel.allowed_hosts.join(", ")} onChange={(e) => onChange({ tunnel: { ...route.tunnel!, allowed_hosts: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) } })} mono className="text-xs" />
            </Field>
            <Field label="Fixed target" hint="host:port; empty = what the client asks">
              <Input value={route.tunnel.pass ?? ""} onChange={(e) => onChange({ tunnel: { ...route.tunnel!, pass: e.target.value || null } })} mono className="text-xs" />
            </Field>
          </div>
        )}
      </Section>

      <Section title="Internal only (internal)" hint="Reachable only through error_redirects, try_files or X-Accel-Redirect" enabled={!!route.internal} onToggle={(on) => onChange({ internal: on })} />
      <Section title="Error redirects for this route (error_page)" hint="Override the host's rules for this route">
        <ErrorRedirectsEditor value={route.error_redirects ?? []} onChange={(v) => onChange({ error_redirects: v })} />
      </Section>
      {isProxy && (
        <Section title="Request to the backend (proxy_method / proxy_set_body / proxy_hide_header)">
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            <Field label="Method override">
              <Input value={route.proxy_method ?? ""} onChange={(e) => onChange({ proxy_method: e.target.value.toUpperCase() || null })} placeholder="POST" mono />
            </Field>
            <Field label="Body (variables allowed)">
              <Input value={route.proxy_set_body ?? ""} onChange={(e) => onChange({ proxy_set_body: e.target.value || null })} placeholder="user=$arg_user" mono className="text-xs" />
            </Field>
            <Field label="Hide response headers" hint="comma-separated">
              <Input value={(route.proxy_hide_headers ?? []).join(", ")} onChange={(e) => onChange({ proxy_hide_headers: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} mono className="text-xs" />
            </Field>
            <Field label="Pass hidden headers" hint="e.g. X-Accel-Redirect">
              <Input value={(route.proxy_pass_headers ?? []).join(", ")} onChange={(e) => onChange({ proxy_pass_headers: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} mono className="text-xs" />
            </Field>
          </div>
          <div className="flex flex-wrap gap-6">
            <Toggle checked={route.proxy_pass_request_headers !== false} onChange={(v) => onChange({ proxy_pass_request_headers: v })} label="Pass client headers" />
            <Toggle checked={route.proxy_pass_request_body !== false} onChange={(v) => onChange({ proxy_pass_request_body: v })} label="Pass client body" />
          </div>
        </Section>
      )}
      <Section title="1×1 transparent GIF (empty_gif)" enabled={!!route.empty_gif} onToggle={(on) => onChange({ empty_gif: on })} />
      {isStatic && (
        <Section title="Random index file (random_index)" hint="A directory request serves a random file from it" enabled={!!route.random_index} onToggle={(on) => onChange({ random_index: on })} />
      )}

      <Toggle checked={route.access_log !== false} onChange={(v) => onChange({ access_log: v })} label="Access log" description="Off = this route is not written to logs (access_log off)" />
    </div>
  );
};
