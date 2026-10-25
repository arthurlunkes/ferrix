import React, { useState } from "react";
import { Globe, Plus, Edit2, Trash2, ArrowRight, Zap, Lock } from "lucide-react";
import { VirtualHost, UpstreamWithHealth } from "../services/api";
import { VHostModal } from "../components/VHostModal";
import { Button } from "../components/ui/Button";
import { Badge } from "../components/ui/Badge";
import { EmptyState } from "../components/ui/EmptyState";
import { SkeletonCard } from "../components/ui/Skeleton";
import { useConfirm, useToast } from "../components/ui/Overlays";

interface VirtualHostsProps {
  vhosts: VirtualHost[];
  upstreams: UpstreamWithHealth[];
  loading?: boolean;
  onSaveVHost: (vhost: VirtualHost) => Promise<void>;
  onDeleteVHost: (id: string) => Promise<void>;
}

const MATCH_TYPE_LABEL: Record<string, string> = {
  prefix: "prefix",
  prefix_literal: "prefix (literal)",
  exact: "exact",
  prefix_priority: "priority prefix",
  regex: "regex",
  regex_case_insensitive: "regex (i)",
};

export const VirtualHosts: React.FC<VirtualHostsProps> = ({
  vhosts,
  upstreams,
  loading,
  onSaveVHost,
  onDeleteVHost,
}) => {
  const toast = useToast();
  const confirm = useConfirm();

  const [editingVHost, setEditingVHost] = useState<VirtualHost | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);

  const handleEdit = (vhost: VirtualHost) => {
    setEditingVHost(vhost);
    setIsModalOpen(true);
  };

  const handleCreate = () => {
    setEditingVHost(null);
    setIsModalOpen(true);
  };

  const handleDelete = async (id: string, name: string) => {
    const ok = await confirm({
      title: "Delete virtual host?",
      description: `"${name}" and all of its routes will immediately stop responding once confirmed.`,
      confirmLabel: "Delete",
      tone: "danger",
    });
    if (!ok) return;
    try {
      await onDeleteVHost(id);
      toast.success(`Virtual host "${name}" deleted.`);
    } catch (err: any) {
      toast.error(err.message || "Failed to delete virtual host");
    }
  };

  const handleSave = async (vhost: VirtualHost) => {
    try {
      await onSaveVHost(vhost);
      toast.success(`Virtual host "${vhost.name}" saved and hot-reloaded.`);
      setIsModalOpen(false);
    } catch (err: any) {
      const report = err.body?.report;
      throw new Error(report?.summary || err.message || "Failed to save virtual host");
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
            Virtual Hosts (Sites)
          </h2>
          <p className="text-xs text-content-muted">
            Manage domains, proxy routing, and static file directories
          </p>
        </div>
        <Button variant="primary" icon={<Plus className="w-4 h-4" />} onClick={handleCreate}>
          New Virtual Host
        </Button>
      </div>

      {vhosts.length === 0 ? (
        <EmptyState
          icon={<Globe className="w-6 h-6" />}
          title="No virtual hosts yet"
          description="A virtual host defines which domains Ferrix serves on a port and where each path is routed."
          actionLabel="Create first virtual host"
          onAction={handleCreate}
        />
      ) : (
        <div className="grid grid-cols-1 gap-6">
          {vhosts.map((vhost) => (
            <div
              key={vhost.id}
              className="bg-surface-1/90 border border-border-subtle rounded-2xl p-6 shadow-card hover:border-border transition"
            >
              <div className="flex flex-wrap items-center justify-between gap-4 pb-4 border-b border-border-subtle">
                <div className="flex items-center gap-3">
                  <div className="p-2.5 rounded-xl bg-brand-500/10 text-brand-400 border border-brand-500/20">
                    <Globe className="w-5 h-5" />
                  </div>
                  <div>
                    <div className="flex items-center gap-2 flex-wrap">
                      <h3 className="text-base font-bold text-content-primary">{vhost.name}</h3>
                      <Badge tone="neutral">Port {vhost.listen_port}</Badge>
                      {vhost.basic_auth && (
                        <Badge tone="warning">
                          <Lock className="w-3 h-3" /> Basic Auth
                        </Badge>
                      )}
                      {(vhost.allowed_ips.length > 0 || vhost.denied_ips.length > 0) && (
                        <Badge tone="info">IP ACL</Badge>
                      )}
                    </div>
                    <div className="flex items-center gap-2 mt-1.5 flex-wrap">
                      <span className="text-xs text-content-muted">Domains:</span>
                      {vhost.server_names.map((name, i) => (
                        <span
                          key={i}
                          className="px-2 py-0.5 rounded bg-brand-500/10 text-brand-400 border border-brand-500/20 font-mono text-[11px]"
                        >
                          {name}
                        </span>
                      ))}
                    </div>
                  </div>
                </div>

                <div className="flex items-center gap-2">
                  <Button variant="ghost" size="sm" onClick={() => handleEdit(vhost)} title="Edit">
                    <Edit2 className="w-4 h-4" />
                  </Button>
                  {vhosts.length > 1 && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => handleDelete(vhost.id, vhost.name)}
                      title="Delete"
                      className="hover:!text-danger-400"
                    >
                      <Trash2 className="w-4 h-4" />
                    </Button>
                  )}
                </div>
              </div>

              <div className="mt-5 space-y-3">
                <span className="text-xs font-semibold text-content-muted uppercase tracking-wider block">
                  Routes ({vhost.routes.length})
                </span>

                {vhost.routes.length === 0 ? (
                  <p className="text-xs text-content-muted italic">
                    No routes — this host responds with 404 to all requests.
                  </p>
                ) : (
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    {vhost.routes.map((route) => (
                      <div
                        key={route.id}
                        className="bg-surface-0 border border-border-subtle rounded-xl p-3.5 text-xs font-mono space-y-2"
                      >
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-brand-400 font-bold text-sm truncate">
                            {route.path_prefix}
                          </span>
                          <div className="flex items-center gap-1 shrink-0">
                            <Badge tone="neutral">{MATCH_TYPE_LABEL[route.match_type]}</Badge>
                          </div>
                        </div>

                        <div className="text-content-secondary flex items-center gap-2">
                          <ArrowRight className="w-3.5 h-3.5 text-content-muted shrink-0" />
                          {route.target_type === "proxy_upstream" && (
                            <span className="text-teal-400 truncate">
                              Upstream: <span className="font-bold">{route.upstream_name}</span>
                            </span>
                          )}
                          {route.target_type === "static_files" && (
                            <span className="text-amber-400 truncate">
                              Root: <span className="font-bold">{route.static_root}</span>
                            </span>
                          )}
                          {route.target_type === "redirect" && (
                            <span className="text-info-400 truncate">
                              {route.redirect_code || 302} &rarr; {route.redirect_url}
                            </span>
                          )}
                        </div>

                        <div className="flex items-center gap-3 pt-1 border-t border-border-subtle text-[11px] flex-wrap">
                          {route.websocket_enabled && route.target_type === "proxy_upstream" && (
                            <span className="flex items-center gap-1 text-brand-400">
                              <Zap className="w-3 h-3" /> WebSocket
                            </span>
                          )}
                          {route.strip_prefix && (
                            <span className="text-content-muted">Strip prefix</span>
                          )}
                          {route.basic_auth && (
                            <span className="flex items-center gap-1 text-warning-400">
                              <Lock className="w-3 h-3" /> Auth
                            </span>
                          )}
                          {route.rate_limit && (
                            <span className="text-content-muted">
                              {route.rate_limit.requests_per_second} r/s
                            </span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {isModalOpen && (
        <VHostModal
          vhost={editingVHost}
          upstreams={upstreams}
          onClose={() => setIsModalOpen(false)}
          onSave={handleSave}
        />
      )}
    </div>
  );
};

