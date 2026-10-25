import React, { useState } from "react";
import { NavLink } from "react-router-dom";
import {
  Activity,
  Globe,
  Server,
  Zap,
  FileCode,
  KeyRound,
  Compass,
  UploadCloud,
  ShieldAlert,
} from "lucide-react";
import { ServerStatus } from "../services/api";
import { RouteTesterModal } from "./RouteTesterModal";

interface NavbarProps {
  status: ServerStatus | null;
  sseConnected: boolean;
  isAuthenticated: boolean;
  onOpenAuth: () => void;
}

const TABS = [
  { to: "/dashboard", label: "Dashboard", icon: Activity },
  { to: "/vhosts", label: "Virtual Hosts", icon: Globe },
  { to: "/live", label: "Live Requests", icon: Zap },
  { to: "/upstreams", label: "Upstreams", icon: Server },
  { to: "/import", label: "Import Nginx", icon: UploadCloud },
  { to: "/config", label: "Config Editor", icon: FileCode },
];

function formatUptime(seconds: number) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export const Navbar: React.FC<NavbarProps> = ({
  status,
  sseConnected,
  isAuthenticated,
  onOpenAuth,
}) => {
  const [routeTesterOpen, setRouteTesterOpen] = useState(false);

  return (
    <header className="bg-surface-1/95 backdrop-blur border-b border-border-subtle sticky top-0 z-40">
      <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
        {}
        <div className="flex items-center justify-between h-14 gap-4">
          <div className="flex items-center gap-3 shrink-0">
            <div className="w-8 h-8 rounded-lg bg-gradient-to-tr from-brand-600 to-teal-400 flex items-center justify-center shadow-glow">
              <span className="font-mono font-black text-lg text-white">F</span>
            </div>
            <div className="hidden sm:block">
              <div className="flex items-center gap-2">
                <span className="font-bold text-content-primary tracking-tight">Ferrix</span>
                <span className="text-[10px] uppercase font-mono px-1.5 py-0.5 rounded bg-brand-500/10 text-brand-400 border border-brand-500/30">
                  Rust v{status?.version || "0.1.0"}
                </span>
              </div>
            </div>
          </div>

          <div className="flex items-center gap-2 flex-wrap justify-end">
            {status && !status.admin_auth_enabled && (
              <span
                title="Admin API has no authentication configured"
                className="hidden md:flex items-center gap-1.5 px-2.5 py-1 rounded-full bg-warning-500/10 text-warning-400 border border-warning-500/30 text-[11px] font-medium"
              >
                <ShieldAlert className="w-3.5 h-3.5" />
                Unauthenticated
              </span>
            )}

            <button
              onClick={() => setRouteTesterOpen(true)}
              title="Test which route handles a request"
              className="p-2 rounded-lg text-content-secondary hover:text-content-primary hover:bg-surface-2 transition"
            >
              <Compass className="w-4 h-4" />
            </button>

            <div className="hidden sm:flex items-center gap-2 px-2.5 py-1 rounded-full bg-surface-2 border border-border-subtle text-xs">
              <span className="relative flex h-2 w-2">
                {sseConnected && (
                  <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-brand-400 opacity-75" />
                )}
                <span
                  className={`relative inline-flex rounded-full h-2 w-2 ${
                    sseConnected ? "bg-brand-500" : "bg-warning-500"
                  }`}
                />
              </span>
              <span className="text-content-secondary font-mono text-[11px]">
                {sseConnected ? "Live SSE" : "Reconnecting..."}
              </span>
            </div>

            <button
              onClick={onOpenAuth}
              title={isAuthenticated ? "Active token — click to change" : "Click to authenticate"}
              className={`flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-mono transition-all border ${
                isAuthenticated
                  ? "bg-brand-500/10 text-brand-400 border-brand-500/30 hover:bg-brand-500/20"
                  : "bg-warning-500/10 text-warning-400 border-warning-500/30 hover:bg-warning-500/20 animate-pulse"
              }`}
            >
              <KeyRound className="w-3.5 h-3.5" />
              <span className="hidden sm:inline">{isAuthenticated ? "Authenticated" : "Authenticate"}</span>
            </button>

            <div className="hidden xl:flex flex-col text-right text-xs leading-tight">
              <span className="text-content-secondary">
                Proxy:{" "}
                <span className="text-brand-400 font-mono font-medium">
                  {status?.proxy_listen || "0.0.0.0:8000"}
                </span>
              </span>
              <span className="text-content-muted text-[10px]">
                Uptime: {status ? formatUptime(status.uptime_seconds) : "0s"}
              </span>
            </div>
          </div>
        </div>

        {}
        <nav className="flex flex-wrap items-center gap-1 pb-2">
          {TABS.map((tab) => {
            const Icon = tab.icon;
            return (
              <NavLink
                key={tab.to}
                to={tab.to}
                className={({ isActive }) =>
                  `flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium whitespace-nowrap transition-all ${
                    isActive
                      ? "bg-brand-500/10 text-brand-400 border border-brand-500/20"
                      : "text-content-secondary hover:text-content-primary hover:bg-surface-2 border border-transparent"
                  }`
                }
              >
                <Icon className="w-3.5 h-3.5" />
                {tab.label}
              </NavLink>
            );
          })}
        </nav>
      </div>

      <RouteTesterModal open={routeTesterOpen} onClose={() => setRouteTesterOpen(false)} />
    </header>
  );
};

