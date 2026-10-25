import React, { useState } from "react";
import { Check, Copy } from "lucide-react";

export const CodeBlock: React.FC<{ code: string; language?: string; maxHeight?: string }> = ({
  code,
  maxHeight = "24rem",
}) => {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {

    }
  };

  return (
    <div className="relative rounded-xl border border-border bg-surface-0 overflow-hidden">
      <button
        type="button"
        onClick={copy}
        className="absolute top-2 right-2 z-10 p-1.5 rounded-md bg-surface-2 hover:bg-surface-3 text-content-muted hover:text-content-primary transition"
        title="Copiar"
      >
        {copied ? (
          <Check className="w-3.5 h-3.5 text-success-400" />
        ) : (
          <Copy className="w-3.5 h-3.5" />
        )}
      </button>
      <pre
        className="p-4 text-xs font-mono text-content-secondary overflow-auto"
        style={{ maxHeight }}
      >
        <code>{code}</code>
      </pre>
    </div>
  );
};

