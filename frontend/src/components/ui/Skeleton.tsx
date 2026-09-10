import React from "react";

/** Placeholder pulsante para o carregamento inicial — hoje a tela ou mostra dados prontos
 * ou não mostra nada, o que pisca como se tivesse falhado. */
export const Skeleton: React.FC<{ className?: string }> = ({ className = "" }) => (
  <div className={`animate-pulse rounded-lg bg-surface-2 ${className}`} />
);

export const SkeletonCard: React.FC = () => (
  <div className="bg-surface-1/90 border border-border-subtle rounded-2xl p-6 space-y-4">
    <div className="flex items-center gap-3">
      <Skeleton className="w-10 h-10 rounded-xl" />
      <div className="flex-1 space-y-2">
        <Skeleton className="h-4 w-1/3" />
        <Skeleton className="h-3 w-1/2" />
      </div>
    </div>
    <Skeleton className="h-20 w-full" />
  </div>
);
