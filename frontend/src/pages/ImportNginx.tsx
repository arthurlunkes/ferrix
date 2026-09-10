import React, { useState } from "react";
import { UploadCloud, FileWarning, CheckCircle2, AlertTriangle } from "lucide-react";
import { Button } from "../components/ui/Button";
import { Textarea } from "../components/ui/Input";
import { Badge } from "../components/ui/Badge";
import { CodeBlock } from "../components/ui/CodeBlock";
import { useToast, useConfirm } from "../components/ui/Overlays";
import { api, FerrixConfig, ImportNginxResult, MigrationNote } from "../services/api";

const PLACEHOLDER = `# Cole aqui o conteúdo do seu nginx.conf, ou de um arquivo de site (server { ... }).
#
# "include" para outros arquivos não é seguido por aqui — cole o conteúdo já expandido,
# ou use "ferrix import --from nginx.conf --out ferrix.json --report report.md" na
# máquina onde os arquivos estão, que resolve os includes automaticamente.

server {
    listen 80;
    server_name meusite.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
    }
}`;

const LEVEL_META: Record<
  MigrationNote["level"],
  { label: string; tone: "danger" | "warning" | "info" }
> = {
  unsupported: { label: "Não migrado", tone: "danger" },
  partial: { label: "Migrado com ressalva", tone: "warning" },
  converted: { label: "Convertido", tone: "info" },
};

interface ImportNginxProps {
  onApplied: () => void;
}

/** Cola um nginx.conf, mostra o JSON convertido lado a lado com o relatório de migração
 * — o que foi convertido, o que ficou com ressalva, e o que não tem equivalente ainda —
 * e só aplica quando o usuário decide explicitamente. Nada é salvo sem essa confirmação. */
export const ImportNginx: React.FC<ImportNginxProps> = ({ onApplied }) => {
  const toast = useToast();
  const confirm = useConfirm();

  const [source, setSource] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<ImportNginxResult | null>(null);
  const [applying, setApplying] = useState(false);

  const runImport = async () => {
    if (!source.trim()) {
      toast.error("Cole o conteúdo do nginx.conf antes de converter.");
      return;
    }
    setLoading(true);
    setResult(null);
    try {
      const r = await api.importNginx(source, "nginx.conf");
      setResult(r);
      const unsupported = r.report.notes.filter((n) => n.level === "unsupported").length;
      if (unsupported > 0) {
        toast.info(`Convertido com ${unsupported} diretiva(s) sem equivalente — confira o relatório abaixo.`);
      } else {
        toast.success("Configuração convertida sem pendências.");
      }
    } catch (err: any) {
      toast.error(err.message || "Falha ao converter o nginx.conf");
    } finally {
      setLoading(false);
    }
  };

  const applyConfig = async (imported: FerrixConfig) => {
    const ok = await confirm({
      title: "Adicionar esta configuração à configuração ativa?",
      description:
        "Os virtual hosts e upstreams convertidos são somados ao que já está rodando, com hot-reload " +
        "imediato — a autenticação, os timeouts e o resto da configuração atual do Ferrix não são tocados. " +
        "Revise o relatório antes de confirmar.",
      confirmLabel: "Adicionar agora",
      tone: "danger",
    });
    if (!ok) return;

    setApplying(true);
    try {
      // Busca a configuração viva na hora de aplicar, não a que foi carregada quando a
      // tela abriu: outra aba pode ter mudado algo nesse meio-tempo, e mesclar em cima de
      // um estado velho arriscaria desfazer essa mudança sem querer.
      const live = await api.getConfig();
      const merged = mergeImportedConfig(live, imported);
      await api.updateConfig(merged);
      toast.success("Virtual hosts e upstreams importados foram adicionados com sucesso.");
      onApplied();
    } catch (err: any) {
      const report = err.body?.report;
      toast.error(
        report?.summary
          ? `Não foi possível aplicar: ${report.summary}`
          : err.message || "Falha ao aplicar a configuração",
      );
    } finally {
      setApplying(false);
    }
  };

  const notesByLevel = result
    ? {
        unsupported: result.report.notes.filter((n) => n.level === "unsupported"),
        partial: result.report.notes.filter((n) => n.level === "partial"),
      }
    : null;

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-xl font-bold text-content-primary tracking-tight">
          Importar configuração do nginx
        </h2>
        <p className="text-xs text-content-muted mt-1">
          Cole um <code className="text-brand-400">nginx.conf</code> (ou um bloco{" "}
          <code className="text-brand-400">server {"{ }"}</code>) e veja o que o Ferrix
          converte, o que precisa de ajuste e o que não migra ainda — antes de aplicar
          qualquer coisa.
        </p>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <div className="space-y-3">
          <Textarea
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder={PLACEHOLDER}
            mono
            rows={20}
            className="text-xs leading-relaxed"
          />
          <Button variant="primary" icon={<UploadCloud className="w-4 h-4" />} loading={loading} onClick={runImport}>
            Converter
          </Button>
        </div>

        <div className="space-y-4">
          {!result && (
            <div className="h-full flex flex-col items-center justify-center text-center py-16 border border-dashed border-border rounded-2xl text-content-muted">
              <FileWarning className="w-8 h-8 mb-3" />
              <p className="text-xs max-w-xs">
                O resultado da conversão e o relatório de migração aparecem aqui, lado a
                lado com o JSON gerado.
              </p>
            </div>
          )}

          {result && (
            <>
              <div className="grid grid-cols-3 gap-2 text-center">
                <StatCard label="Virtual hosts" value={result.report.vhosts_created} />
                <StatCard label="Rotas" value={result.report.routes_created} />
                <StatCard label="Upstreams" value={result.report.upstreams_created} />
              </div>

              {/* A validação aqui é do resultado isolado da conversão — antes de somar à
                  configuração ativa. Problemas de admin/http/real_ip não valem: esses
                  campos nunca vêm do importador, vêm sempre do servidor em execução ao
                  aplicar (ver mergeImportedConfig). Só o que sobrar aqui é relevante. */}
              {result.validation.issues.filter((i) => !i.path.startsWith("admin") && !i.path.startsWith("real_ip")).length > 0 && (
                <div className="flex items-start gap-2 p-3 rounded-lg bg-warning-500/10 border border-warning-500/30 text-warning-400 text-xs">
                  <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
                  <span>
                    A conversão tem pendências fora do que o Ferrix consegue resolver
                    sozinho (ex.: referência a um upstream inexistente) — dá para ver os
                    detalhes no Config Editor depois de aplicar.
                  </span>
                </div>
              )}

              {notesByLevel && (notesByLevel.unsupported.length > 0 || notesByLevel.partial.length > 0) && (
                <div className="rounded-xl border border-border overflow-hidden max-h-64 overflow-y-auto">
                  <table className="w-full text-xs">
                    <thead className="bg-surface-2 text-content-muted sticky top-0">
                      <tr>
                        <th className="text-left font-medium px-3 py-2">Diretiva</th>
                        <th className="text-left font-medium px-3 py-2">Linha</th>
                        <th className="text-left font-medium px-3 py-2">Situação</th>
                        <th className="text-left font-medium px-3 py-2">Motivo</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-border-subtle">
                      {[...notesByLevel.unsupported, ...notesByLevel.partial].map((n, i) => {
                        const meta = LEVEL_META[n.level];
                        return (
                          <tr key={i} className="align-top">
                            <td className="px-3 py-2 font-mono text-content-primary whitespace-nowrap">
                              {n.directive}
                            </td>
                            <td className="px-3 py-2 text-content-muted">{n.line}</td>
                            <td className="px-3 py-2">
                              <Badge tone={meta.tone}>{meta.label}</Badge>
                            </td>
                            <td className="px-3 py-2 text-content-secondary">{n.reason}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}

              <CodeBlock code={JSON.stringify(result.config, null, 2)} maxHeight="16rem" />

              <Button
                variant="primary"
                icon={<CheckCircle2 className="w-4 h-4" />}
                loading={applying}
                onClick={() => applyConfig(result.config)}
                className="w-full"
              >
                Aplicar esta configuração
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );
};

/// Um sufixo que não colide com nada em `taken`, preservando `base` quando ele já é único.
function uniqueId(base: string, taken: Set<string>): string {
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/**
 * Soma os virtual hosts e upstreams importados à configuração viva, em vez de substituir
 * o objeto inteiro.
 *
 * O importador monta um `FerrixConfig` do zero — ele não tem como saber qual token de
 * administração, CORS ou timeout o servidor em execução já usa. Aplicar esse objeto
 * inteiro por cima da configuração ativa apagaria tudo isso (foi exatamente o que
 * aconteceu num teste: a autenticação do painel foi zerada por um "Aplicar" de
 * importação). Por isso `admin`, `http` e `real_ip` vêm sempre do que já está rodando.
 *
 * Os IDs de vhost e de rota que o importador gera reiniciam a contagem em cada conversão
 * (`vhost-1`, `route-1`, ...), então duas importações — ou uma importação depois que o
 * host já tem vhosts com esses mesmos IDs — colidiriam sem o remapeamento abaixo.
 */
function mergeImportedConfig(live: FerrixConfig, imported: FerrixConfig): FerrixConfig {
  const takenVhostIds = new Set(live.vhosts.map((v) => v.id));

  const mergedVhosts = imported.vhosts.map((vhost) => {
    const newId = uniqueId(vhost.id, takenVhostIds);
    takenVhostIds.add(newId);

    const takenRouteIds = new Set<string>();
    const routes = vhost.routes.map((route) => {
      const routeId = uniqueId(route.id, takenRouteIds);
      takenRouteIds.add(routeId);
      return { ...route, id: routeId };
    });

    return { ...vhost, id: newId, routes };
  });

  return {
    ...live,
    // Upstream de mesmo nome é sobrescrito de propósito: reimportar o mesmo site deve
    // atualizar o pool, não duplicá-lo com um nome colidido.
    upstreams: { ...live.upstreams, ...imported.upstreams },
    vhosts: [...live.vhosts, ...mergedVhosts],
  };
}

const StatCard: React.FC<{ label: string; value: number }> = ({ label, value }) => (
  <div className="rounded-xl border border-border-subtle bg-surface-1 p-3">
    <div className="text-lg font-bold text-brand-400">{value}</div>
    <div className="text-[11px] text-content-muted">{label}</div>
  </div>
);
