import React from "react";
import { Plus, Trash2 } from "lucide-react";
import { Input } from "./Input";
import { Button } from "./Button";

const RemoveButton: React.FC<{ onClick: () => void }> = ({ onClick }) => (
  <button
    type="button"
    onClick={onClick}
    className="p-2 rounded-lg text-content-muted hover:text-danger-400 hover:bg-surface-2 transition shrink-0"
    aria-label="Remove"
  >
    <Trash2 className="w-3.5 h-3.5" />
  </button>
);

/** Editable list of strings (one input per item). */
export const StringListEditor: React.FC<{
  value: string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
  addLabel?: string;
  emptyText?: string;
}> = ({ value, onChange, placeholder, addLabel = "Add", emptyText }) => (
  <div className="space-y-2">
    {value.length === 0 && emptyText && <p className="text-[11px] text-content-muted">{emptyText}</p>}
    {value.map((v, i) => (
      <div key={i} className="flex items-center gap-2">
        <Input
          value={v}
          onChange={(e) => onChange(value.map((x, j) => (j === i ? e.target.value : x)))}
          placeholder={placeholder}
          mono
          className="flex-1 text-xs"
        />
        <RemoveButton onClick={() => onChange(value.filter((_, j) => j !== i))} />
      </div>
    ))}
    <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={() => onChange([...value, ""])}>
      {addLabel}
    </Button>
  </div>
);

/** Editable ordered list of [a, b] pairs (order matters, keys may repeat). */
export const PairListEditor: React.FC<{
  value: [string, string][];
  onChange: (next: [string, string][]) => void;
  leftPlaceholder?: string;
  rightPlaceholder?: string;
  addLabel?: string;
}> = ({ value, onChange, leftPlaceholder, rightPlaceholder, addLabel = "Add" }) => {
  const set = (i: number, side: 0 | 1, v: string) =>
    onChange(value.map((p, j) => (j === i ? ((side === 0 ? [v, p[1]] : [p[0], v]) as [string, string]) : p)));
  return (
    <div className="space-y-2">
      {value.map((p, i) => (
        <div key={i} className="flex items-center gap-2">
          <Input value={p[0]} onChange={(e) => set(i, 0, e.target.value)} placeholder={leftPlaceholder} mono className="flex-1 text-xs" />
          <span className="text-content-muted text-xs">→</span>
          <Input value={p[1]} onChange={(e) => set(i, 1, e.target.value)} placeholder={rightPlaceholder} mono className="flex-1 text-xs" />
          <RemoveButton onClick={() => onChange(value.filter((_, j) => j !== i))} />
        </div>
      ))}
      <Button type="button" variant="ghost" size="sm" icon={<Plus className="w-3.5 h-3.5" />} onClick={() => onChange([...value, ["", ""]])}>
        {addLabel}
      </Button>
    </div>
  );
};

/** Collapsible-free titled block used by the module editors. */
export const Section: React.FC<{
  title: string;
  hint?: string;
  enabled?: boolean;
  onToggle?: (on: boolean) => void;
  children?: React.ReactNode;
}> = ({ title, hint, enabled, onToggle, children }) => (
  <div className="rounded-lg border border-border-subtle p-3 space-y-3">
    <div className="flex items-start justify-between gap-3">
      <div>
        <span className="block text-[11px] font-semibold text-content-secondary">{title}</span>
        {hint && <span className="block text-[11px] text-content-muted mt-0.5">{hint}</span>}
      </div>
      {onToggle && (
        <button
          type="button"
          role="switch"
          aria-checked={!!enabled}
          aria-label={title}
          onClick={() => onToggle(!enabled)}
          className={`relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors ${
            enabled ? "bg-brand-600" : "bg-surface-3 border border-border"
          }`}
        >
          <span className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${enabled ? "translate-x-[1.15rem]" : "translate-x-1"}`} />
        </button>
      )}
    </div>
    {(onToggle ? enabled : true) && children}
  </div>
);
