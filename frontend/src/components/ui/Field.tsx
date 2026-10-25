import React from "react";

interface FieldProps {
  label: string;
  hint?: string;
  error?: string;
  required?: boolean;
  children: React.ReactNode;
  className?: string;
}

export const Field: React.FC<FieldProps> = ({
  label,
  hint,
  error,
  required,
  children,
  className = "",
}) => (
  <div className={className}>
    <label className="block text-xs font-medium text-content-secondary mb-1">
      {label}
      {required && <span className="text-danger-400 ml-0.5">*</span>}
    </label>
    {children}
    {error ? (
      <p className="mt-1 text-[11px] text-danger-400">{error}</p>
    ) : hint ? (
      <p className="mt-1 text-[11px] text-content-muted">{hint}</p>
    ) : null}
  </div>
);

