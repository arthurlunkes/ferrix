import React from "react";

const BASE =
  "w-full px-3 py-2 rounded-lg bg-surface-2 border border-border text-content-primary text-sm " +
  "placeholder:text-content-muted focus:outline-none focus:border-brand-500 focus:ring-1 focus:ring-brand-500/30 " +
  "disabled:opacity-50 disabled:cursor-not-allowed transition-colors";

export const Input: React.FC<React.InputHTMLAttributes<HTMLInputElement> & { mono?: boolean }> = ({
  mono,
  className = "",
  ...rest
}) => <input className={`${BASE} ${mono ? "font-mono" : ""} ${className}`} {...rest} />;

export const Textarea: React.FC<
  React.TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean }
> = ({ mono, className = "", ...rest }) => (
  <textarea className={`${BASE} ${mono ? "font-mono" : ""} resize-y ${className}`} {...rest} />
);

export const Select: React.FC<React.SelectHTMLAttributes<HTMLSelectElement>> = ({
  className = "",
  children,
  ...rest
}) => (
  <select className={`${BASE} cursor-pointer ${className}`} {...rest}>
    {children}
  </select>
);

interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label?: string;
  description?: string;
  disabled?: boolean;
}

/** Interruptor sim/não com rótulo opcional. Usado no lugar de `<input type="checkbox">`
 * cru para as flags dos formulários (websocket, strip_prefix, backup, down...). */
export const Toggle: React.FC<ToggleProps> = ({
  checked,
  onChange,
  label,
  description,
  disabled,
}) => (
  <label
    className={`flex items-start gap-3 cursor-pointer select-none ${disabled ? "opacity-50 cursor-not-allowed" : ""}`}
  >
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors
        focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-500/50
        ${checked ? "bg-brand-600" : "bg-surface-3 border border-border"}`}
    >
      <span
        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform
          ${checked ? "translate-x-[1.15rem]" : "translate-x-1"}`}
      />
    </button>
    {(label || description) && (
      <span className="text-xs">
        {label && <span className="block text-content-primary font-medium">{label}</span>}
        {description && <span className="block text-content-muted mt-0.5">{description}</span>}
      </span>
    )}
  </label>
);
