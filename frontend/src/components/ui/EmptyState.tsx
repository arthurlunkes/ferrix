import React from "react";
import { Button } from "./Button";

interface EmptyStateProps {
  icon: React.ReactNode;
  title: string;
  description?: string;
  actionLabel?: string;
  onAction?: () => void;
}

/** Estado vazio para listas sem itens ainda — antes uma lista sem vhosts/upstreams
 * simplesmente renderizava nada, o que parece uma tela quebrada, não uma tela nova. */
export const EmptyState: React.FC<EmptyStateProps> = ({
  icon,
  title,
  description,
  actionLabel,
  onAction,
}) => (
  <div className="flex flex-col items-center justify-center text-center py-16 px-6 border border-dashed border-border rounded-2xl bg-surface-1/40">
    <div className="p-3 rounded-xl bg-surface-2 text-content-muted mb-4">{icon}</div>
    <h3 className="text-sm font-semibold text-content-primary">{title}</h3>
    {description && (
      <p className="text-xs text-content-muted mt-1.5 max-w-sm leading-relaxed">
        {description}
      </p>
    )}
    {actionLabel && onAction && (
      <Button variant="primary" size="sm" onClick={onAction} className="mt-5">
        {actionLabel}
      </Button>
    )}
  </div>
);
