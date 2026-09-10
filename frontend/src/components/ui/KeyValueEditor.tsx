import React from "react";
import { Plus, Trash2 } from "lucide-react";
import { Input } from "./Input";
import { Button } from "./Button";

interface KeyValueEditorProps {
  value: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
  addLabel?: string;
}

/** Editor de pares chave/valor para `headers`, `add_headers` e `proxy_set_headers` — hoje
 * a única forma de tocar esses campos era editando o JSON cru no Config Editor. */
export const KeyValueEditor: React.FC<KeyValueEditorProps> = ({
  value,
  onChange,
  keyPlaceholder = "Nome do header",
  valuePlaceholder = "Valor",
  addLabel = "Adicionar header",
}) => {
  const entries = Object.entries(value);

  const updateEntry = (index: number, newKey: string, newValue: string) => {
    const next = entries.map(([k, v], i) => (i === index ? [newKey, newValue] : [k, v]));
    onChange(Object.fromEntries(next));
  };

  const removeEntry = (index: number) => {
    onChange(Object.fromEntries(entries.filter((_, i) => i !== index)));
  };

  const addEntry = () => {
    // Uma chave vazia é permitida temporariamente; duas vazias colapsariam no mesmo
    // objeto, então cada nova entrada recebe um espaço reservado numerado.
    let key = "";
    let n = entries.length;
    while (Object.prototype.hasOwnProperty.call(value, key)) {
      n += 1;
      key = `header-${n}`;
    }
    onChange({ ...value, [key]: "" });
  };

  return (
    <div className="space-y-2">
      {entries.length === 0 && (
        <p className="text-[11px] text-content-muted">Nenhum header definido.</p>
      )}
      {entries.map(([k, v], i) => (
        <div key={i} className="flex items-center gap-2">
          <Input
            value={k}
            onChange={(e) => updateEntry(i, e.target.value, v)}
            placeholder={keyPlaceholder}
            mono
            className="flex-1 text-xs"
          />
          <Input
            value={v}
            onChange={(e) => updateEntry(i, k, e.target.value)}
            placeholder={valuePlaceholder}
            mono
            className="flex-1 text-xs"
          />
          <button
            type="button"
            onClick={() => removeEntry(i)}
            className="p-2 rounded-lg text-content-muted hover:text-danger-400 hover:bg-surface-2 transition shrink-0"
            aria-label="Remover"
          >
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        </div>
      ))}
      <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={addEntry}>
        {addLabel}
      </Button>
    </div>
  );
};
