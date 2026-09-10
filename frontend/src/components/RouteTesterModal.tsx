import React, { useState } from "react";
import { Compass, CheckCircle2, XCircle } from "lucide-react";
import { Modal } from "./ui/Modal";
import { Button } from "./ui/Button";
import { Field } from "./ui/Field";
import { Input } from "./ui/Input";
import { Badge } from "./ui/Badge";
import { api, RouteTestResult } from "../services/api";

interface RouteTesterModalProps {
  open: boolean;
  onClose: () => void;
}

/** Responde "qual host e qual rota atenderiam esta requisição, e por quê" — o antídoto
 * para a classe de erro mais cara em nginx: uma regra que nunca é alcançada e só se
 * descobre em produção. Consome `POST /api/route-test`. */
export const RouteTesterModal: React.FC<RouteTesterModalProps> = ({ open, onClose }) => {
  const [host, setHost] = useState("localhost");
  const [path, setPath] = useState("/");
  const [port, setPort] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<RouteTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const run = async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await api.routeTest({
        host,
        path,
        port: port ? parseInt(port, 10) : undefined,
      });
      setResult(r);
    } catch (err: any) {
      setError(err.message || "Falha ao testar a rota");
      setResult(null);
    } finally {
      setLoading(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Testar Rota"
      description="Simula qual virtual host e qual rota atenderiam uma requisição, e por qual regra de precedência"
      icon={<Compass className="w-5 h-5" />}
      size="lg"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Fechar
          </Button>
          <Button variant="primary" onClick={run} loading={loading}>
            Testar
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid grid-cols-3 gap-3">
          <Field label="Host" className="col-span-1">
            <Input value={host} onChange={(e) => setHost(e.target.value)} mono placeholder="localhost" />
          </Field>
          <Field label="Caminho" className="col-span-1">
            <Input value={path} onChange={(e) => setPath(e.target.value)} mono placeholder="/api/users" />
          </Field>
          <Field label="Porta (opcional)" className="col-span-1">
            <Input
              value={port}
              onChange={(e) => setPort(e.target.value)}
              mono
              placeholder="8000"
              inputMode="numeric"
            />
          </Field>
        </div>

        {error && (
          <div className="p-3 rounded-lg bg-danger-500/10 border border-danger-500/30 text-danger-400 text-xs">
            {error}
          </div>
        )}

        {result && (
          <div className="rounded-xl border border-border bg-surface-0 p-4 space-y-3">
            <div className="flex items-center gap-2">
              {result.matched ? (
                <CheckCircle2 className="w-4 h-4 text-success-400" />
              ) : (
                <XCircle className="w-4 h-4 text-danger-400" />
              )}
              <span className="text-sm font-semibold text-content-primary">
                {result.matched ? "Casou com uma rota" : "Nenhuma rota casou"}
              </span>
            </div>

            <p className="text-xs text-content-secondary leading-relaxed">{result.explanation}</p>

            {result.matched && (
              <div className="grid grid-cols-2 gap-3 pt-2 border-t border-border-subtle text-xs">
                <div>
                  <span className="text-content-muted block mb-1">Virtual Host</span>
                  <span className="text-content-primary font-mono">{result.vhost_name}</span>
                  {result.vhost_reason && (
                    <Badge tone="info" className="mt-1.5">
                      {result.vhost_reason}
                    </Badge>
                  )}
                </div>
                <div>
                  <span className="text-content-muted block mb-1">Rota</span>
                  <span className="text-content-primary font-mono">{result.route_pattern}</span>
                  {result.route_reason && (
                    <Badge tone="brand" className="mt-1.5">
                      {result.route_reason}
                    </Badge>
                  )}
                </div>
                {result.target && (
                  <div className="col-span-2">
                    <span className="text-content-muted block mb-1">Destino</span>
                    <span className="text-content-primary font-mono">{result.target}</span>
                  </div>
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
};
