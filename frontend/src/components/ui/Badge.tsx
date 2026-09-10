import React from "react";

type Tone = "brand" | "success" | "warning" | "danger" | "info" | "neutral";

const TONE_CLASSES: Record<Tone, string> = {
  brand: "bg-brand-500/10 text-brand-400 border-brand-500/30",
  success: "bg-success-500/10 text-success-400 border-success-500/30",
  warning: "bg-warning-500/10 text-warning-400 border-warning-500/30",
  danger: "bg-danger-500/10 text-danger-400 border-danger-500/30",
  info: "bg-info-500/10 text-info-400 border-info-500/30",
  neutral: "bg-surface-2 text-content-secondary border-border",
};

export const Badge: React.FC<{
  tone?: Tone;
  children: React.ReactNode;
  className?: string;
}> = ({ tone = "neutral", children, className = "" }) => (
  <span
    className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full border text-[11px] font-medium ${TONE_CLASSES[tone]} ${className}`}
  >
    {children}
  </span>
);
