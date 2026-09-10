import React, { useState } from "react";
import { Server, Plus, Edit2, Trash2, CheckCircle2, XCircle, HeartPulse, ShieldOff } from "lucide-react";
import { UpstreamConfig, UpstreamWithHealth } from "../services/api";
import { UpstreamModal } from "../components/UpstreamModal";
import { Button } from "../components/ui/Button";
import { Badge } from "../components/ui/Badge";
import { EmptyState } from "../components/ui/EmptyState";
import { SkeletonCard } from "../components/ui/Skeleton";
import { useConfirm, useToast } from "../components/ui/Overlays";

interface UpstreamsProps {
  upstreams: UpstreamWithHealth[];
  loading?: boolean;
  onSaveUpstream: (upstream: UpstreamConfig) => Promise<void>;
  onDeleteUpstream: (name: string) => Promise<void>;
}

const ALGORITHM_LABEL: Record<string, string> = {
  round_robin: "Round-Robin",
  weighted_round_robin: "Weighted Round-Robin",
  least_conn: "Least Connections",
  ip_hash: "IP Hash (Sticky)",
  random: "Random",
};

export const Upstreams: React.FC<UpstreamsProps> = ({
  upstreams,
  loading,
  onSaveUpstream,
  onDeleteUpstream,
}) => {
  const toast = useToast();
  const confirm = useConfirm();

  const [editingUpstream, setEditingUpstream] = useState<UpstreamConfig | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);

  const handleEdit = (u: UpstreamWithHealth) => {
    setEditingUpstream(u.config);
    setIsModalOpen(true);
  };

  const handleCreate = () => {
    setEditingUpstream(null);
    setIsModalOpen(true);
  };

  const handleDelete = async (name: string) => {
    const ok = await confirm({
      title: "Remover pool de upstream?",
      description: `Qualquer rota que aponte para "${name}" passará a responder 502 até ser reconfigurada.`,
      confirmLabel: "Remover",
      tone: "danger",
    });
    if (!ok) return;
    try {
      await onDeleteUpstream(name);
      toast.success(`Upstream "${name}" removido.`);
    } catch (err: any) {
      toast.error(err.message || "Falha ao remover o upstream");
    }
  };

  const handleSave = async (upstream: UpstreamConfig) => {
    try {
      await onSaveUpstream(upstream);
      toast.success(`Upstream "${upstream.name}" salvo e recarregado a quente.`);
      setIsModalOpen(false);
    } catch (err: any) {
      const report = err.body?.report;
      throw new Error(report?.summary || err.message || "Falha ao salvar o upstream");
    }
  };

  if (loading) {
    return (
      <div className="space-y-6">
        {[0, 1].map((i) => (
          <SkeletonCard key={i} />
        ))}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-xl font-bold text-content-primary tracking-tight">
            Pools de Upstream
          </h2>
          <p className="text-xs text-content-muted">
            Servidores de backend, algoritmo de balanceamento e health checks ativos
          </p>
        </div>
        <Button variant="primary" icon={<Plus className="w-4 h-4" />} onClick={handleCreate}>
          Novo Pool
        </Button>
      </div>

      {upstreams.length === 0 ? (
        <EmptyState
          icon={<Server className="w-6 h-6" />}
          title="Nenhum upstream ainda"
          description="Um upstream é um conjunto de servidores de backend que dividem a carga — crie o primeiro para uma rota de proxy poder apontar para ele."
          actionLabel="Criar o primeiro upstream"
          onAction={handleCreate}
        />
      ) : (
        <div className="grid grid-cols-1 gap-6">
          {upstreams.map((u) => {
            const totalTargets = u.config.targets.length;
            const healthyTargets = u.statuses.filter((s) => s.healthy).length;

            return (
              <div
                key={u.name}
                className="bg-surface-1/90 border border-border-subtle rounded-2xl p-6 shadow-card hover:border-border transition"
              >
                <div className="flex flex-wrap items-center justify-between gap-4 pb-4 border-b border-border-subtle">
                  <div className="flex items-center gap-3">
                    <div className="p-2.5 rounded-xl bg-teal-500/10 text-teal-400 border border-teal-500/20">
                      <Server className="w-5 h-5" />
                    </div>
                    <div>
                      <div className="flex items-center gap-2 flex-wrap">
                        <h3 className="text-base font-bold text-content-primary font-mono">{u.name}</h3>
                        <Badge tone="info">{ALGORITHM_LABEL[u.config.algorithm]}</Badge>
                        {!u.config.verify_tls && (
                          <Badge tone="warning">
                            <ShieldOff className="w-3 h-3" /> TLS não verificado
                          </Badge>
                        )}
                      </div>
                      <div className="flex items-center gap-3 mt-1 text-xs text-content-muted">
                        <span>
                          Saúde:{" "}
                          <span className="text-brand-400 font-bold">
                            {healthyTargets}/{totalTargets} online
                          </span>
                        </span>
                        {u.config.health_check && (
                          <span className="flex items-center gap-1">
                            <HeartPulse className="w-3.5 h-3.5 text-brand-400" />
                            {u.config.health_check.path} ({u.config.health_check.interval_secs}s)
                          </span>
                        )}
                      </div>
                    </div>
                  </div>

                  <div className="flex items-center gap-2">
                    <Button variant="ghost" size="sm" onClick={() => handleEdit(u)} title="Editar">
                      <Edit2 className="w-4 h-4" />
                    </Button>
                    {upstreams.length > 1 && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleDelete(u.name)}
                        title="Remover"
                        className="hover:!text-danger-400"
                      >
                        <Trash2 className="w-4 h-4" />
                      </Button>
                    )}
                  </div>
                </div>

                <div className="mt-5 space-y-3">
                  <span className="text-xs font-semibold text-content-muted uppercase tracking-wider block">
                    Servidores ({totalTargets})
                  </span>

                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    {u.config.targets.map((target, idx) => {
                      const status = u.statuses.find((s) => s.target_url === target.url);
                      const isHealthy = target.down ? false : status ? status.healthy : true;

                      return (
                        <div
                          key={idx}
                          className={`border rounded-xl p-4 font-mono text-xs transition ${
                            target.down
                              ? "bg-surface-2/40 border-border-subtle opacity-60"
                              : isHealthy
                                ? "bg-surface-0 border-border-subtle"
                                : "bg-danger-500/5 border-danger-500/30"
                          }`}
                        >
                          <div className="flex items-center justify-between mb-2 gap-2">
                            <div className="flex items-center gap-2 min-w-0">
                              {target.down ? (
                                <ShieldOff className="w-4 h-4 text-content-muted shrink-0" />
                              ) : isHealthy ? (
                                <CheckCircle2 className="w-4 h-4 text-brand-400 shrink-0" />
                              ) : (
                                <XCircle className="w-4 h-4 text-danger-400 shrink-0" />
                              )}
                              <span className="font-bold text-content-primary text-sm truncate">
                                {target.url}
                              </span>
                            </div>
                            <div className="flex items-center gap-1 shrink-0">
                              {target.backup && <Badge tone="neutral">Backup</Badge>}
                              <Badge tone={target.down ? "neutral" : isHealthy ? "success" : "danger"}>
                                {target.down ? "Down" : isHealthy ? "Healthy" : "Unhealthy"}
                              </Badge>
                            </div>
                          </div>

                          <div className="grid grid-cols-3 gap-2 pt-2 border-t border-border-subtle text-[11px] text-content-muted">
                            <div>
                              <span className="block">Peso</span>
                              <span className="text-content-secondary">{target.weight}</span>
                            </div>
                            <div>
                              <span className="block">Latência</span>
                              <span className="text-brand-400 font-bold">
                                {status?.latency_ms !== undefined ? `${status.latency_ms} ms` : "—"}
                              </span>
                            </div>
                            <div>
                              <span className="block">Falhas / Máx</span>
                              <span
                                className={
                                  status && status.consecutive_fails > 0
                                    ? "text-danger-400"
                                    : "text-content-secondary"
                                }
                              >
                                {status?.consecutive_fails || 0} / {target.max_fails}
                              </span>
                            </div>
                            {target.max_conns != null && (
                              <div className="col-span-3 pt-1">
                                <span>Máx. conexões simultâneas: </span>
                                <span className="text-content-secondary">{target.max_conns}</span>
                              </div>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {isModalOpen && (
        <UpstreamModal
          upstream={editingUpstream}
          onClose={() => setIsModalOpen(false)}
          onSave={handleSave}
        />
      )}
    </div>
  );
};
