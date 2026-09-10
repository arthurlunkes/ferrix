import React from "react";

export interface TabItem {
  id: string;
  label: string;
  icon?: React.ReactNode;
  /** Um ponto de aviso no canto da aba — usado para sinalizar campo com erro sem abrir a aba. */
  badge?: React.ReactNode;
}

interface TabsProps {
  tabs: TabItem[];
  active: string;
  onChange: (id: string) => void;
}

/** Navegação por abas dentro de um modal — usada para separar Geral/Rotas/Headers/
 * Segurança/Limites no formulário de Virtual Host, que crescido demais para uma coluna só. */
export const Tabs: React.FC<TabsProps> = ({ tabs, active, onChange }) => (
  <div className="flex items-center gap-1 border-b border-border-subtle overflow-x-auto -mx-1 px-1">
    {tabs.map((tab) => {
      const isActive = tab.id === active;
      return (
        <button
          key={tab.id}
          type="button"
          onClick={() => onChange(tab.id)}
          className={`relative flex items-center gap-1.5 px-3 py-2 text-xs font-semibold whitespace-nowrap transition
            ${isActive ? "text-brand-400" : "text-content-muted hover:text-content-secondary"}`}
        >
          {tab.icon}
          {tab.label}
          {tab.badge}
          {isActive && (
            <span className="absolute left-0 right-0 -bottom-px h-0.5 bg-brand-500 rounded-full" />
          )}
        </button>
      );
    })}
  </div>
);
