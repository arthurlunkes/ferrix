import React from "react";
import { LucideIcon } from "lucide-react";

interface MetricCardProps {
  title: string;
  value: string | number;
  subtitle?: string;
  icon: LucideIcon;
  trend?: string;
  trendUp?: boolean;
  color?: "emerald" | "blue" | "purple" | "amber" | "rose";
}

export const MetricCard: React.FC<MetricCardProps> = ({
  title,
  value,
  subtitle,
  icon: Icon,
  trend,
  trendUp,
  color = "emerald",
}) => {
  const colorMap = {
    emerald: {
      bg: "bg-emerald-500/10",
      text: "text-emerald-400",
      border: "border-emerald-500/20",
      glow: "group-hover:border-emerald-500/40",
    },
    blue: {
      bg: "bg-blue-500/10",
      text: "text-blue-400",
      border: "border-blue-500/20",
      glow: "group-hover:border-blue-500/40",
    },
    purple: {
      bg: "bg-purple-500/10",
      text: "text-purple-400",
      border: "border-purple-500/20",
      glow: "group-hover:border-purple-500/40",
    },
    amber: {
      bg: "bg-amber-500/10",
      text: "text-amber-400",
      border: "border-amber-500/20",
      glow: "group-hover:border-amber-500/40",
    },
    rose: {
      bg: "bg-rose-500/10",
      text: "text-rose-400",
      border: "border-rose-500/20",
      glow: "group-hover:border-rose-500/40",
    },
  };

  const c = colorMap[color];

  return (
    <div
      className={`group bg-[#111827]/80 rounded-xl p-5 border border-gray-800/80 hover:bg-[#111827] transition-all shadow-sm ${c.glow}`}
    >
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-gray-400 uppercase tracking-wider">
          {title}
        </span>
        <div className={`p-2 rounded-lg ${c.bg} ${c.text}`}>
          <Icon className="w-5 h-5" />
        </div>
      </div>

      <div className="mt-3">
        <div className="text-2xl font-bold font-mono text-white tracking-tight">
          {value}
        </div>
        {(subtitle || trend) && (
          <div className="mt-1 flex items-center gap-2 text-xs">
            {trend && (
              <span
                className={`font-medium ${trendUp ? "text-emerald-400" : "text-rose-400"}`}
              >
                {trend}
              </span>
            )}
            {subtitle && <span className="text-gray-400">{subtitle}</span>}
          </div>
        )}
      </div>
    </div>
  );
};

