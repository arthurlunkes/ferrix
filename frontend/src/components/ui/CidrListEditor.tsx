import React from "react";
import { Plus, Trash2 } from "lucide-react";
import { Input } from "./Input";
import { Button } from "./Button";

/** Verificação leve, só para dar feedback imediato — a validação de verdade (IPv6, forma
 * mapeada, limites de máscara) é a mesma que o servidor roda em `net::cidr`. Isto aqui só
 * evita mandar "abc" para o servidor e descobrir o erro só depois de salvar. */
function looksLikeIpOrCidr(value: string): boolean {
  const v = value.trim();
  if (v === "" || v === "*" || v.toLowerCase() === "all") return true;
  const ipv4 = /^(\d{1,3}\.){3}\d{1,3}(\/\d{1,2})?$/;
  const ipv6 = /^[0-9a-fA-F:]+(\/\d{1,3})?$/;
  return ipv4.test(v) || ipv6.test(v);
}

interface CidrListEditorProps {
  value: string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
  addLabel?: string;
}

/** Lista de IPs/blocos CIDR para `allowed_ips`/`denied_ips` — equivalente a `allow`/`deny`
 * do nginx. Antes só existia como array cru no JSON do Config Editor. */
export const CidrListEditor: React.FC<CidrListEditorProps> = ({
  value,
  onChange,
  placeholder = "10.0.0.0/8, 192.168.1.1, *",
  addLabel = "Adicionar",
}) => {
  const update = (index: number, next: string) => {
    onChange(value.map((v, i) => (i === index ? next : v)));
  };
  const remove = (index: number) => onChange(value.filter((_, i) => i !== index));
  const add = () => onChange([...value, ""]);

  return (
    <div className="space-y-2">
      {value.length === 0 && (
        <p className="text-[11px] text-content-muted">Nenhuma entrada — todos os IPs se aplicam.</p>
      )}
      {value.map((v, i) => {
        const valid = looksLikeIpOrCidr(v);
        return (
          <div key={i} className="flex items-center gap-2">
            <Input
              value={v}
              onChange={(e) => update(i, e.target.value)}
              placeholder={placeholder}
              mono
              className={`flex-1 text-xs ${!valid ? "border-danger-500/60" : ""}`}
            />
            <button
              type="button"
              onClick={() => remove(i)}
              className="p-2 rounded-lg text-content-muted hover:text-danger-400 hover:bg-surface-2 transition shrink-0"
              aria-label="Remover"
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          </div>
        );
      })}
      <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={add}>
        {addLabel}
      </Button>
    </div>
  );
};
