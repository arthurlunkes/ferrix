import React, { useEffect } from "react";
import { X } from "lucide-react";

interface ModalProps {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: string;
  icon?: React.ReactNode;
  iconTone?: "brand" | "teal" | "amber";
  size?: "md" | "lg" | "xl";
  children: React.ReactNode;
  footer?: React.ReactNode;
}

const SIZE_CLASSES: Record<NonNullable<ModalProps["size"]>, string> = {
  md: "max-w-lg",
  lg: "max-w-2xl",
  xl: "max-w-3xl",
};

const ICON_TONE_CLASSES: Record<NonNullable<ModalProps["iconTone"]>, string> = {
  brand: "bg-brand-500/10 text-brand-400",
  teal: "bg-teal-500/10 text-teal-400",
  amber: "bg-amber-500/10 text-amber-400",
};

/** Casco de modal comum: header com ícone/título/fechar, corpo rolável, rodapé fixo.
 * Todos os modais do painel (VHost, Upstream, confirmação) montam sobre isto. */
export const Modal: React.FC<ModalProps> = ({
  open,
  onClose,
  title,
  description,
  icon,
  iconTone = "brand",
  size = "md",
  children,
  footer,
}) => {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/75 backdrop-blur-sm animate-fade-in"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className={`bg-surface-1 border border-border rounded-2xl w-full ${SIZE_CLASSES[size]} max-h-[92vh] overflow-hidden shadow-modal flex flex-col`}
      >
        <div className="flex items-center justify-between p-6 border-b border-border-subtle shrink-0">
          <div className="flex items-center gap-3">
            {icon && (
              <div className={`p-2 rounded-lg ${ICON_TONE_CLASSES[iconTone]}`}>{icon}</div>
            )}
            <div>
              <h2 className="text-lg font-bold text-content-primary">{title}</h2>
              {description && (
                <p className="text-xs text-content-muted">{description}</p>
              )}
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded-lg text-content-muted hover:text-content-primary hover:bg-surface-2 transition"
            aria-label="Fechar"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-6">{children}</div>

        {footer && (
          <div className="p-4 border-t border-border-subtle shrink-0 flex items-center justify-end gap-3">
            {footer}
          </div>
        )}
      </div>
    </div>
  );
};
