import React, { useState } from "react";
import { Plus, Trash2, Server, HeartPulse } from "lucide-react";
import { UpstreamConfig, UpstreamTarget, newUpstream, newUpstreamTarget } from "../services/api";
import { Modal } from "./ui/Modal";
import { Button } from "./ui/Button";
import { Field } from "./ui/Field";
import { Input, Select, Toggle } from "./ui/Input";

interface UpstreamModalProps {
  upstream: UpstreamConfig | null;
  onClose: () => void;
  onSave: (upstream: UpstreamConfig) => Promise<void>;
}

export const UpstreamModal: React.FC<UpstreamModalProps> = ({ upstream, onClose, onSave }) => {
  const [formData, setFormData] = useState<UpstreamConfig>(
    () =>
      upstream ?? newUpstream({ targets: [newUpstreamTarget({ url: "http://127.0.0.1:9001" })] }),
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const patchTarget = (index: number, patch: Partial<UpstreamTarget>) => {
    setFormData((prev) => ({
      ...prev,
      targets: prev.targets.map((t, i) => (i === index ? { ...t, ...patch } : t)),
    }));
  };

  const addTarget = () => {
    setFormData((prev) => ({ ...prev, targets: [...prev.targets, newUpstreamTarget()] }));
  };

  const removeTarget = (index: number) => {
    setFormData((prev) => ({ ...prev, targets: prev.targets.filter((_, i) => i !== index) }));
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    if (formData.targets.length === 0) {
      setError("Configure at least one backend server.");
      return;
    }
    setSaving(true);
    try {
      await onSave(formData);
    } catch (err: any) {
      setError(err.message || "Failed to save upstream");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      title={upstream ? "Edit Upstream Pool" : "New Upstream Pool"}
      description="Load balancing algorithm, backend servers, and health checks"
      icon={<Server className="w-5 h-5" />}
      iconTone="teal"
      size="lg"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={handleSubmit} loading={saving}>
            Save and apply
          </Button>
        </>
      }
    >
      <form onSubmit={handleSubmit} className="space-y-6">
        {error && (
          <div className="p-3 rounded-lg bg-danger-500/10 border border-danger-500/30 text-danger-400 text-xs">
            {error}
          </div>
        )}

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <Field label="Pool name" required>
            <Input
              required
              disabled={!!upstream}
              value={formData.name}
              onChange={(e) => setFormData({ ...formData, name: e.target.value })}
              placeholder="backend_cluster_1"
              mono
            />
          </Field>
          <Field label="Load balancing algorithm">
            <Select
              value={formData.algorithm}
              onChange={(e) => setFormData({ ...formData, algorithm: e.target.value as UpstreamConfig["algorithm"] })}
            >
              <option value="round_robin">Round-Robin</option>
              <option value="weighted_round_robin">Weighted Round-Robin</option>
              <option value="least_conn">Least Connections</option>
              <option value="ip_hash">IP Hash (sticky session)</option>
              <option value="random">Random</option>
            </Select>
          </Field>
        </div>

        <Toggle
          checked={formData.verify_tls}
          onChange={(v) => setFormData({ ...formData, verify_tls: v })}
          label="Verify backend TLS certificate"
          description="Disable only for backends with self-signed certificates on trusted networks"
        />

        <div className="border-t border-border-subtle pt-5">
          <div className="flex items-center justify-between mb-3">
            <div>
              <h3 className="text-sm font-bold text-content-primary">Backend servers</h3>
              <p className="text-xs text-content-muted">Weight, fault tolerance, and pool role</p>
            </div>
            <Button variant="secondary" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={addTarget}>
              Add server
            </Button>
          </div>

          <div className="space-y-3">
            {formData.targets.map((target, idx) => (
              <div key={idx} className="bg-surface-0 border border-border-subtle rounded-xl p-3.5 space-y-3">
                <div className="flex flex-wrap items-end gap-3">
                  <Field label="Server URL" required className="flex-1 min-w-[200px]">
                    <Input
                      required
                      value={target.url}
                      onChange={(e) => patchTarget(idx, { url: e.target.value })}
                      placeholder="http://127.0.0.1:3000"
                      mono
                      className="text-xs"
                    />
                  </Field>
                  <Field label="Weight" className="w-20">
                    <Input
                      type="number"
                      min={1}
                      value={target.weight}
                      onChange={(e) => patchTarget(idx, { weight: parseInt(e.target.value) || 1 })}
                      mono
                      className="text-xs"
                    />
                  </Field>
                  <Field label="Max fails" className="w-24">
                    <Input
                      type="number"
                      min={1}
                      value={target.max_fails}
                      onChange={(e) => patchTarget(idx, { max_fails: parseInt(e.target.value) || 3 })}
                      mono
                      className="text-xs"
                    />
                  </Field>
                  <Field label="Max connections" hint="Empty = unlimited" className="w-28">
                    <Input
                      type="number"
                      min={0}
                      value={target.max_conns ?? ""}
                      onChange={(e) =>
                        patchTarget(idx, { max_conns: e.target.value ? parseInt(e.target.value) : null })
                      }
                      mono
                      className="text-xs"
                    />
                  </Field>
                  {formData.targets.length > 1 && (
                    <button
                      type="button"
                      onClick={() => removeTarget(idx)}
                      className="p-2 text-content-muted hover:text-danger-400 rounded-lg transition"
                      title="Remove server"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  )}
                </div>
                <div className="flex items-center gap-6 pt-1">
                  <Toggle
                    checked={target.backup}
                    onChange={(v) => patchTarget(idx, { backup: v })}
                    label="Backup"
                    description="only receives traffic if all others fail"
                  />
                  <Toggle
                    checked={target.down}
                    onChange={(v) => patchTarget(idx, { down: v })}
                    label="Down"
                    description="manually removed from rotation"
                  />
                </div>
              </div>
            ))}
          </div>
        </div>

        <div className="border-t border-border-subtle pt-5">
          <div className="flex items-center gap-2 mb-3">
            <HeartPulse className="w-4 h-4 text-brand-400" />
            <h3 className="text-sm font-bold text-content-primary">Active health check</h3>
          </div>

          <Toggle
            checked={!!formData.health_check}
            onChange={(checked) =>
              setFormData({
                ...formData,
                health_check: checked
                  ? { path: "/health", interval_secs: 5, timeout_secs: 2, expected_status: 200 }
                  : null,
              })
            }
            label="Periodically check backend health"
          />

          {formData.health_check && (
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3 bg-surface-0 border border-border-subtle rounded-xl p-4 mt-3">
              <Field label="Path">
                <Input
                  value={formData.health_check.path}
                  onChange={(e) =>
                    setFormData({ ...formData, health_check: { ...formData.health_check!, path: e.target.value } })
                  }
                  mono
                  className="text-xs"
                />
              </Field>
              <Field label="Interval (s)">
                <Input
                  type="number"
                  min={1}
                  value={formData.health_check.interval_secs}
                  onChange={(e) =>
                    setFormData({
                      ...formData,
                      health_check: { ...formData.health_check!, interval_secs: parseInt(e.target.value) || 5 },
                    })
                  }
                  mono
                  className="text-xs"
                />
              </Field>
              <Field label="Timeout (s)">
                <Input
                  type="number"
                  min={1}
                  value={formData.health_check.timeout_secs}
                  onChange={(e) =>
                    setFormData({
                      ...formData,
                      health_check: { ...formData.health_check!, timeout_secs: parseInt(e.target.value) || 2 },
                    })
                  }
                  mono
                  className="text-xs"
                />
              </Field>
              <Field label="Expected status">
                <Input
                  type="number"
                  value={formData.health_check.expected_status}
                  onChange={(e) =>
                    setFormData({
                      ...formData,
                      health_check: {
                        ...formData.health_check!,
                        expected_status: parseInt(e.target.value) || 200,
                      },
                    })
                  }
                  mono
                  className="text-xs"
                />
              </Field>
            </div>
          )}
        </div>
      </form>
    </Modal>
  );
};

