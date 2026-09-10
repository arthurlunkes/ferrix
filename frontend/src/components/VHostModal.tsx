import React, { useMemo, useState } from "react";
import {
  Globe,
  Plus,
  Trash2,
  ArrowUp,
  ArrowDown,
  ChevronDown,
  ChevronRight,
  AlertTriangle,
} from "lucide-react";
import {
  RouteRule,
  VirtualHost,
  UpstreamWithHealth,
  RateLimitConfig,
  LimitConnConfig,
  BasicAuthConfig,
  Satisfy,
  RouteMatchType,
  NextUpstreamCondition,
  newRoute,
} from "../services/api";
import { Modal } from "./ui/Modal";
import { Button } from "./ui/Button";
import { Field } from "./ui/Field";
import { Input, Select, Textarea, Toggle } from "./ui/Input";
import { Tabs, TabItem } from "./ui/Tabs";
import { Badge } from "./ui/Badge";
import { KeyValueEditor } from "./ui/KeyValueEditor";
import { CidrListEditor } from "./ui/CidrListEditor";

interface VHostModalProps {
  vhost: VirtualHost | null;
  upstreams: UpstreamWithHealth[];
  onClose: () => void;
  onSave: (vhost: VirtualHost) => Promise<void>;
}

const MATCH_TYPE_OPTIONS: { value: RouteMatchType; label: string; hint: string }[] = [
  { value: "prefix", label: "Prefixo", hint: "/api casa /api e /api/x, não /apifoo" },
  { value: "prefix_literal", label: "Prefixo (literal)", hint: "igual ao nginx: /api casa /apifoo também" },
  { value: "exact", label: "Exato", hint: "casa só o caminho inteiro" },
  { value: "prefix_priority", label: "Prefixo prioritário (^~)", hint: "vence antes de qualquer regex" },
  { value: "regex", label: "Expressão regular", hint: "sensível a maiúsculas" },
  { value: "regex_case_insensitive", label: "Regex (i)", hint: "insensível a maiúsculas" },
];

const NEXT_UPSTREAM_CONDITIONS: { value: NextUpstreamCondition; label: string }[] = [
  { value: "error", label: "Erro de conexão" },
  { value: "timeout", label: "Timeout" },
  { value: "http_500", label: "HTTP 500" },
  { value: "http_502", label: "HTTP 502" },
  { value: "http_503", label: "HTTP 503" },
  { value: "http_504", label: "HTTP 504" },
  { value: "http_403", label: "HTTP 403" },
  { value: "http_404", label: "HTTP 404" },
];

function isRouteShadowed(routes: RouteRule[], index: number): boolean {
  const r = routes[index];
  return routes
    .slice(0, index)
    .some((other) => other.match_type === r.match_type && other.path_prefix === r.path_prefix);
}

// ---------------------------------------------------------------------------
// Editores reutilizados no nível do host e no nível da rota
// ---------------------------------------------------------------------------

const RateLimitEditor: React.FC<{
  value: RateLimitConfig | null | undefined;
  onChange: (next: RateLimitConfig | null) => void;
}> = ({ value, onChange }) => (
  <div className="space-y-3">
    <Toggle
      checked={!!value}
      onChange={(checked) =>
        onChange(
          checked
            ? { requests_per_second: 10, burst: 20, mode: "no_delay", delay: null, status: 429, retry_after: null }
            : null,
        )
      }
      label="Limitar taxa de requisições"
      description="Balde furado (token bucket), como o limit_req do nginx"
    />
    {value && (
      <div className="grid grid-cols-2 gap-3 pl-1 border-l-2 border-border-subtle ml-1.5 pl-4">
        <Field label="Requisições / segundo">
          <Input
            type="number"
            min={1}
            value={value.requests_per_second}
            onChange={(e) => onChange({ ...value, requests_per_second: parseInt(e.target.value) || 1 })}
            mono
          />
        </Field>
        <Field label="Burst">
          <Input
            type="number"
            min={0}
            value={value.burst}
            onChange={(e) => onChange({ ...value, burst: parseInt(e.target.value) || 0 })}
            mono
          />
        </Field>
        <Field label="Modo do excedente" className="col-span-2">
          <Select
            value={value.mode}
            onChange={(e) => onChange({ ...value, mode: e.target.value as RateLimitConfig["mode"] })}
          >
            <option value="no_delay">Sem espera (passa direto até o burst)</option>
            <option value="queue">Fila (espera, nunca recusa dentro do burst)</option>
            <option value="delay">Espera parcial (as N primeiras passam livres)</option>
          </Select>
        </Field>
        {value.mode === "delay" && (
          <Field label="Livres antes de esperar">
            <Input
              type="number"
              min={0}
              value={value.delay ?? 0}
              onChange={(e) => onChange({ ...value, delay: parseInt(e.target.value) || 0 })}
              mono
            />
          </Field>
        )}
        <Field label="Código de recusa">
          <Input
            type="number"
            value={value.status}
            onChange={(e) => onChange({ ...value, status: parseInt(e.target.value) || 429 })}
            mono
          />
        </Field>
      </div>
    )}
  </div>
);

const LimitConnEditor: React.FC<{
  value: LimitConnConfig | null | undefined;
  onChange: (next: LimitConnConfig | null) => void;
}> = ({ value, onChange }) => (
  <div className="space-y-3">
    <Toggle
      checked={!!value}
      onChange={(checked) => onChange(checked ? { max_connections: 10, status: 429 } : null)}
      label="Limitar conexões simultâneas"
      description="Equivalente a limit_conn — por IP de cliente"
    />
    {value && (
      <div className="grid grid-cols-2 gap-3 border-l-2 border-border-subtle ml-1.5 pl-4">
        <Field label="Máximo simultâneo">
          <Input
            type="number"
            min={1}
            value={value.max_connections}
            onChange={(e) => onChange({ ...value, max_connections: parseInt(e.target.value) || 1 })}
            mono
          />
        </Field>
        <Field label="Código de recusa">
          <Input
            type="number"
            value={value.status}
            onChange={(e) => onChange({ ...value, status: parseInt(e.target.value) || 429 })}
            mono
          />
        </Field>
      </div>
    )}
  </div>
);

const BasicAuthEditor: React.FC<{
  value: BasicAuthConfig | null | undefined;
  onChange: (next: BasicAuthConfig | null) => void;
}> = ({ value, onChange }) => (
  <div className="space-y-3">
    <Toggle
      checked={!!value}
      onChange={(checked) => onChange(checked ? { username: "admin", password: "" } : null)}
      label="Exigir HTTP Basic Auth"
    />
    {value && (
      <div className="grid grid-cols-2 gap-3 border-l-2 border-border-subtle ml-1.5 pl-4">
        <Field label="Usuário">
          <Input value={value.username} onChange={(e) => onChange({ ...value, username: e.target.value })} />
        </Field>
        <Field label="Senha">
          <Input
            type="text"
            value={value.password}
            onChange={(e) => onChange({ ...value, password: e.target.value })}
            mono
            placeholder="visível em texto — trate como um segredo"
          />
        </Field>
      </div>
    )}
  </div>
);

const SatisfySelector: React.FC<{ value: Satisfy; onChange: (v: Satisfy) => void }> = ({
  value,
  onChange,
}) => (
  <Field
    label="Combinação entre ACL de IP e autenticação"
    hint="'Ambos' exige passar nos dois; 'Qualquer um' libera com qualquer um dos dois — útil para 'rede interna OU senha'"
  >
    <Select value={value} onChange={(e) => onChange(e.target.value as Satisfy)}>
      <option value="all">Ambos (satisfy all)</option>
      <option value="any">Qualquer um (satisfy any)</option>
    </Select>
  </Field>
);

// ---------------------------------------------------------------------------
// Cartão de rota
// ---------------------------------------------------------------------------

const RouteCard: React.FC<{
  route: RouteRule;
  index: number;
  total: number;
  shadowed: boolean;
  upstreams: UpstreamWithHealth[];
  onChange: (patch: Partial<RouteRule>) => void;
  onMove: (dir: -1 | 1) => void;
  onRemove: () => void;
}> = ({ route, index, total, shadowed, upstreams, onChange, onMove, onRemove }) => {
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const toggleCondition = (cond: NextUpstreamCondition) => {
    const current = route.proxy_next_upstream?.conditions ?? [];
    const next = current.includes(cond) ? current.filter((c) => c !== cond) : [...current, cond];
    onChange({
      proxy_next_upstream: { ...(route.proxy_next_upstream ?? { conditions: [], tries: null, timeout_ms: null }), conditions: next },
    });
  };

  return (
    <div
      className={`bg-surface-0 border rounded-xl p-4 space-y-4 ${
        shadowed ? "border-warning-500/50" : "border-border-subtle"
      }`}
    >
      <div className="flex items-center justify-between">
        <span className="font-mono text-xs font-bold text-brand-400">Rota #{index + 1}</span>
        <div className="flex items-center gap-1">
          <button
            type="button"
            disabled={index === 0}
            onClick={() => onMove(-1)}
            className="p-1 rounded text-content-muted hover:text-content-primary disabled:opacity-30 transition"
            title="Mover para cima"
          >
            <ArrowUp className="w-3.5 h-3.5" />
          </button>
          <button
            type="button"
            disabled={index === total - 1}
            onClick={() => onMove(1)}
            className="p-1 rounded text-content-muted hover:text-content-primary disabled:opacity-30 transition"
            title="Mover para baixo"
          >
            <ArrowDown className="w-3.5 h-3.5" />
          </button>
          {total > 1 && (
            <button
              type="button"
              onClick={onRemove}
              className="p-1 rounded text-content-muted hover:text-danger-400 transition"
              title="Remover rota"
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          )}
        </div>
      </div>

      {shadowed && (
        <div className="flex items-center gap-2 text-[11px] text-warning-400 bg-warning-500/10 border border-warning-500/30 rounded-lg px-2.5 py-1.5">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
          Uma rota acima já usa o mesmo padrão — esta nunca será alcançada.
        </div>
      )}

      <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
        <Field label="Padrão de caminho">
          <Input
            value={route.path_prefix}
            onChange={(e) => onChange({ path_prefix: e.target.value })}
            placeholder="/api"
            mono
            className="text-xs"
          />
        </Field>
        <Field label="Tipo de match">
          <Select
            value={route.match_type}
            onChange={(e) => onChange({ match_type: e.target.value as RouteMatchType })}
          >
            {MATCH_TYPE_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Destino">
          <Select
            value={route.target_type}
            onChange={(e) => onChange({ target_type: e.target.value as RouteRule["target_type"] })}
          >
            <option value="proxy_upstream">Proxy para upstream</option>
            <option value="static_files">Arquivos estáticos</option>
            <option value="redirect">Redirecionamento HTTP</option>
          </Select>
        </Field>
      </div>
      <p className="text-[11px] text-content-muted -mt-2">
        {MATCH_TYPE_OPTIONS.find((o) => o.value === route.match_type)?.hint}
      </p>

      {route.target_type === "proxy_upstream" && (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          <Field label="Upstream">
            <Select
              value={route.upstream_name ?? ""}
              onChange={(e) => onChange({ upstream_name: e.target.value })}
            >
              <option value="" disabled>
                Selecione...
              </option>
              {upstreams.map((u) => (
                <option key={u.name} value={u.name}>
                  {u.name} ({u.config.targets.length} servidores)
                </option>
              ))}
            </Select>
          </Field>
          <div className="flex items-center gap-6 pt-5">
            <Toggle
              checked={route.websocket_enabled}
              onChange={(v) => onChange({ websocket_enabled: v })}
              label="WebSocket"
            />
            <Toggle
              checked={route.strip_prefix}
              onChange={(v) => onChange({ strip_prefix: v })}
              label="Strip prefix"
            />
          </div>
        </div>
      )}

      {route.target_type === "static_files" && (
        <div className="space-y-3">
          <Field label="Diretório raiz">
            <Input
              value={route.static_root ?? ""}
              onChange={(e) => onChange({ static_root: e.target.value })}
              placeholder="./public"
              mono
              className="text-xs"
            />
          </Field>
          <Field label="try_files" hint="Ex.: $uri $uri/ /index.html — essencial para SPAs">
            <Input
              value={route.try_files ?? ""}
              onChange={(e) => onChange({ try_files: e.target.value })}
              mono
              className="text-xs"
            />
          </Field>
        </div>
      )}

      {route.target_type === "redirect" && (
        <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
          <Field label="URL de destino" className="col-span-2">
            <Input
              value={route.redirect_url ?? ""}
              onChange={(e) => onChange({ redirect_url: e.target.value })}
              placeholder="https://exemplo.com"
              mono
              className="text-xs"
            />
          </Field>
          <Field label="Código">
            <Select
              value={String(route.redirect_code ?? 302)}
              onChange={(e) => onChange({ redirect_code: parseInt(e.target.value) })}
            >
              <option value="301">301 Permanente</option>
              <option value="302">302 Temporário</option>
              <option value="307">307 Temporário (preserva método)</option>
              <option value="308">308 Permanente (preserva método)</option>
            </Select>
          </Field>
        </div>
      )}

      {route.target_type === "proxy_upstream" && (
        <div>
          <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
            Cabeçalhos enviados ao backend
          </span>
          <KeyValueEditor
            value={route.proxy_set_headers}
            onChange={(v) => onChange({ proxy_set_headers: v })}
            keyPlaceholder="X-Real-IP"
            valuePlaceholder="$remote_addr"
            addLabel="Adicionar cabeçalho"
          />
        </div>
      )}

      <button
        type="button"
        onClick={() => setAdvancedOpen(!advancedOpen)}
        className="flex items-center gap-1.5 text-[11px] font-semibold text-content-muted hover:text-content-secondary transition"
      >
        {advancedOpen ? <ChevronDown className="w-3.5 h-3.5" /> : <ChevronRight className="w-3.5 h-3.5" />}
        Avançado: timeouts, failover, limites, controle de acesso
      </button>

      {advancedOpen && (
        <div className="space-y-5 pt-3 border-t border-border-subtle">
          {route.target_type === "proxy_upstream" && (
            <>
              <div className="grid grid-cols-3 gap-3">
                <Field label="Timeout de conexão (ms)" hint="Herda o global se vazio">
                  <Input
                    type="number"
                    value={route.proxy_connect_timeout_ms ?? ""}
                    onChange={(e) =>
                      onChange({ proxy_connect_timeout_ms: e.target.value ? parseInt(e.target.value) : null })
                    }
                    mono
                  />
                </Field>
                <Field label="Timeout de leitura (ms)">
                  <Input
                    type="number"
                    value={route.proxy_read_timeout_ms ?? ""}
                    onChange={(e) =>
                      onChange({ proxy_read_timeout_ms: e.target.value ? parseInt(e.target.value) : null })
                    }
                    mono
                  />
                </Field>
                <Field label="Timeout de envio (ms)">
                  <Input
                    type="number"
                    value={route.proxy_send_timeout_ms ?? ""}
                    onChange={(e) =>
                      onChange({ proxy_send_timeout_ms: e.target.value ? parseInt(e.target.value) : null })
                    }
                    mono
                  />
                </Field>
              </div>

              <div>
                <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
                  Tentar o próximo servidor quando (proxy_next_upstream)
                </span>
                <div className="flex flex-wrap gap-1.5">
                  {NEXT_UPSTREAM_CONDITIONS.map((c) => {
                    const active = route.proxy_next_upstream?.conditions.includes(c.value);
                    return (
                      <button
                        key={c.value}
                        type="button"
                        onClick={() => toggleCondition(c.value)}
                        className={`px-2 py-1 rounded-md text-[11px] font-medium border transition ${
                          active
                            ? "bg-brand-500/10 text-brand-400 border-brand-500/40"
                            : "bg-surface-2 text-content-muted border-border hover:text-content-secondary"
                        }`}
                      >
                        {c.label}
                      </button>
                    );
                  })}
                </div>
                <p className="text-[11px] text-content-muted mt-1">
                  Nenhuma condição marcada = failover desligado para esta rota.
                </p>
              </div>
            </>
          )}

          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
              Limite de taxa desta rota
            </span>
            <RateLimitEditor value={route.rate_limit} onChange={(v) => onChange({ rate_limit: v })} />
          </div>
          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
              Limite de conexões desta rota
            </span>
            <LimitConnEditor value={route.limit_conn} onChange={(v) => onChange({ limit_conn: v })} />
          </div>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div>
              <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
                IPs permitidos nesta rota
              </span>
              <CidrListEditor value={route.allowed_ips} onChange={(v) => onChange({ allowed_ips: v })} />
            </div>
            <div>
              <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
                IPs negados nesta rota
              </span>
              <CidrListEditor value={route.denied_ips} onChange={(v) => onChange({ denied_ips: v })} />
            </div>
          </div>

          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
              Basic Auth desta rota
            </span>
            <BasicAuthEditor value={route.basic_auth} onChange={(v) => onChange({ basic_auth: v })} />
          </div>

          {(route.allowed_ips.length > 0 || route.denied_ips.length > 0) && route.basic_auth && (
            <SatisfySelector value={route.satisfy} onChange={(v) => onChange({ satisfy: v })} />
          )}

          <div>
            <span className="block text-[11px] font-medium text-content-secondary mb-1.5">
              Cabeçalhos adicionados à resposta desta rota
            </span>
            <KeyValueEditor
              value={route.add_headers}
              onChange={(v) => onChange({ add_headers: v })}
              keyPlaceholder="Access-Control-Allow-Origin"
              valuePlaceholder="*"
            />
          </div>
        </div>
      )}
    </div>
  );
};

// ---------------------------------------------------------------------------
// Modal principal
// ---------------------------------------------------------------------------

export const VHostModal: React.FC<VHostModalProps> = ({ vhost, upstreams, onClose, onSave }) => {
  const [formData, setFormData] = useState<VirtualHost>(() => {
    if (vhost) return vhost;
    return {
      id: `vhost-${Date.now()}`,
      name: "",
      server_names: ["localhost"],
      listen_port: 8000,
      headers: { "X-Powered-By": "Ferrix" },
      rate_limit: null,
      limit_conn: null,
      allowed_ips: [],
      denied_ips: [],
      basic_auth: null,
      satisfy: "all",
      routes: [newRoute({ upstream_name: upstreams[0]?.name ?? null })],
    };
  });

  const [serverNamesInput, setServerNamesInput] = useState(formData.server_names.join(", "));
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [tab, setTab] = useState("general");

  const patchRoute = (index: number, patch: Partial<RouteRule>) => {
    setFormData((prev) => ({
      ...prev,
      routes: prev.routes.map((r, i) => (i === index ? { ...r, ...patch } : r)),
    }));
  };

  const moveRoute = (index: number, dir: -1 | 1) => {
    setFormData((prev) => {
      const routes = [...prev.routes];
      const target = index + dir;
      if (target < 0 || target >= routes.length) return prev;
      [routes[index], routes[target]] = [routes[target], routes[index]];
      return { ...prev, routes };
    });
  };

  const removeRoute = (index: number) => {
    setFormData((prev) => ({ ...prev, routes: prev.routes.filter((_, i) => i !== index) }));
  };

  const addRoute = () => {
    setFormData((prev) => ({
      ...prev,
      routes: [...prev.routes, newRoute({ path_prefix: "/nova-rota", upstream_name: upstreams[0]?.name ?? null })],
    }));
  };

  const shadowedFlags = useMemo(
    () => formData.routes.map((_, i) => isRouteShadowed(formData.routes, i)),
    [formData.routes],
  );

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setSaveError(null);
    try {
      const updated: VirtualHost = {
        ...formData,
        server_names: serverNamesInput.split(",").map((s) => s.trim()).filter(Boolean),
      };
      await onSave(updated);
    } catch (err: any) {
      setSaveError(err.message || "Falha ao salvar o virtual host");
    } finally {
      setSaving(false);
    }
  };

  const tabs: TabItem[] = [
    { id: "general", label: "Geral" },
    {
      id: "routes",
      label: "Rotas",
      badge: shadowedFlags.some(Boolean) ? (
        <AlertTriangle className="w-3 h-3 text-warning-400" />
      ) : undefined,
    },
    { id: "headers", label: "Headers" },
    { id: "security", label: "Segurança" },
    { id: "limits", label: "Limites" },
  ];

  return (
    <Modal
      open
      onClose={onClose}
      title={vhost ? "Editar Virtual Host" : "Novo Virtual Host"}
      description="Domínio, porta de escuta e roteamento de proxy reverso"
      icon={<Globe className="w-5 h-5" />}
      size="xl"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancelar
          </Button>
          <Button variant="primary" onClick={handleSubmit} loading={saving}>
            Salvar e recarregar
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-5">
        <Tabs tabs={tabs} active={tab} onChange={setTab} />

        {saveError && (
          <div className="p-3 rounded-lg bg-danger-500/10 border border-danger-500/30 text-danger-400 text-xs">
            {saveError}
          </div>
        )}

        {tab === "general" && (
          <div className="space-y-4">
            <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
              <Field label="Nome / descrição" required className="md:col-span-2">
                <Input
                  required
                  value={formData.name}
                  onChange={(e) => setFormData({ ...formData, name: e.target.value })}
                  placeholder="Ex.: Aplicação Principal"
                />
              </Field>
              <Field label="Porta de escuta" required>
                <Input
                  type="number"
                  required
                  value={formData.listen_port}
                  onChange={(e) =>
                    setFormData({ ...formData, listen_port: parseInt(e.target.value) || 8000 })
                  }
                  mono
                />
              </Field>
            </div>
            <Field
              label="Server names (domínios, separados por vírgula)"
              hint="Aceita nome exato, *.dominio.com, dominio.* ou * para qualquer domínio"
              required
            >
              <Input
                required
                value={serverNamesInput}
                onChange={(e) => setServerNamesInput(e.target.value)}
                placeholder="localhost, api.meudominio.com, *"
                mono
              />
            </Field>
          </div>
        )}

        {tab === "routes" && (
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <p className="text-xs text-content-muted">
                A ordem importa: em caso de prefixos empatados, a primeira rota declarada vence.
              </p>
              <Button variant="secondary" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={addRoute}>
                Adicionar rota
              </Button>
            </div>
            <div className="space-y-4">
              {formData.routes.map((route, idx) => (
                <RouteCard
                  key={route.id}
                  route={route}
                  index={idx}
                  total={formData.routes.length}
                  shadowed={shadowedFlags[idx]}
                  upstreams={upstreams}
                  onChange={(patch) => patchRoute(idx, patch)}
                  onMove={(dir) => moveRoute(idx, dir)}
                  onRemove={() => removeRoute(idx)}
                />
              ))}
            </div>
          </div>
        )}

        {tab === "headers" && (
          <div>
            <p className="text-xs text-content-muted mb-3">
              Aplicados a toda resposta servida por este host, antes dos cabeçalhos específicos de cada rota.
            </p>
            <KeyValueEditor
              value={formData.headers}
              onChange={(v) => setFormData({ ...formData, headers: v })}
              keyPlaceholder="X-Frame-Options"
              valuePlaceholder="SAMEORIGIN"
            />
          </div>
        )}

        {tab === "security" && (
          <div className="space-y-5">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div>
                <span className="block text-xs font-medium text-content-secondary mb-1.5">
                  IPs permitidos
                </span>
                <CidrListEditor
                  value={formData.allowed_ips}
                  onChange={(v) => setFormData({ ...formData, allowed_ips: v })}
                />
              </div>
              <div>
                <span className="block text-xs font-medium text-content-secondary mb-1.5">
                  IPs negados
                </span>
                <CidrListEditor
                  value={formData.denied_ips}
                  onChange={(v) => setFormData({ ...formData, denied_ips: v })}
                />
              </div>
            </div>
            <div>
              <span className="block text-xs font-medium text-content-secondary mb-1.5">Basic Auth</span>
              <BasicAuthEditor
                value={formData.basic_auth}
                onChange={(v) => setFormData({ ...formData, basic_auth: v })}
              />
            </div>
            {(formData.allowed_ips.length > 0 || formData.denied_ips.length > 0) && formData.basic_auth && (
              <SatisfySelector
                value={formData.satisfy}
                onChange={(v) => setFormData({ ...formData, satisfy: v })}
              />
            )}
          </div>
        )}

        {tab === "limits" && (
          <div className="space-y-6">
            <div>
              <span className="block text-xs font-medium text-content-secondary mb-1.5">
                Limite de taxa do host
              </span>
              <RateLimitEditor
                value={formData.rate_limit}
                onChange={(v) => setFormData({ ...formData, rate_limit: v })}
              />
            </div>
            <div>
              <span className="block text-xs font-medium text-content-secondary mb-1.5">
                Limite de conexões do host
              </span>
              <LimitConnEditor
                value={formData.limit_conn}
                onChange={(v) => setFormData({ ...formData, limit_conn: v })}
              />
            </div>
          </div>
        )}
      </form>
    </Modal>
  );
};
