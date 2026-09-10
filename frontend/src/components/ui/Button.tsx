import React from "react";
import { Loader2 } from "lucide-react";

type Variant = "primary" | "secondary" | "danger" | "ghost";
type Size = "sm" | "md";

interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  loading?: boolean;
  icon?: React.ReactNode;
}

const VARIANT_CLASSES: Record<Variant, string> = {
  primary:
    "bg-brand-600 hover:bg-brand-500 text-white shadow-glow disabled:hover:bg-brand-600",
  secondary:
    "bg-surface-2 hover:bg-surface-3 text-content-primary border border-border",
  danger:
    "bg-surface-2 hover:bg-danger-500/10 hover:text-danger-400 text-content-secondary border border-border hover:border-danger-500/40",
  ghost: "bg-transparent hover:bg-surface-2 text-content-secondary hover:text-content-primary",
};

const SIZE_CLASSES: Record<Size, string> = {
  sm: "px-3 py-1.5 text-xs gap-1.5",
  md: "px-4 py-2 text-sm gap-2",
};

/** Botão padrão do painel. Substitui os `<button className="...">` repetidos em cada
 * tela — a cor de foco, o disabled e o spinner de loading só existem aqui agora. */
export const Button: React.FC<ButtonProps> = ({
  variant = "secondary",
  size = "md",
  loading = false,
  icon,
  disabled,
  children,
  className = "",
  ...rest
}) => {
  return (
    <button
      disabled={disabled || loading}
      className={`inline-flex items-center justify-center font-semibold rounded-lg transition
        disabled:opacity-50 disabled:cursor-not-allowed
        focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-500/50
        ${VARIANT_CLASSES[variant]} ${SIZE_CLASSES[size]} ${className}`}
      {...rest}
    >
      {loading ? (
        <Loader2 className="w-4 h-4 animate-spin" />
      ) : (
        icon
      )}
      {children}
    </button>
  );
};
