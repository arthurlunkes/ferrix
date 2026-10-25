import React from "react";
import { X, Clock, Globe, ArrowRight, ShieldCheck, Cpu } from "lucide-react";
import { RequestLog } from "../services/api";

interface RequestDetailModalProps {
  log: RequestLog | null;
  onClose: () => void;
}

export const RequestDetailModal: React.FC<RequestDetailModalProps> = ({
  log,
  onClose,
}) => {
  if (!log) return null;

  const getStatusBadge = (status: number) => {
    if (status >= 200 && status < 300) {
      return "bg-emerald-500/10 text-emerald-400 border-emerald-500/30";
    } else if (status >= 300 && status < 400) {
      return "bg-blue-500/10 text-blue-400 border-blue-500/30";
    } else if (status >= 400 && status < 500) {
      return "bg-amber-500/10 text-amber-400 border-amber-500/30";
    } else {
      return "bg-rose-500/10 text-rose-400 border-rose-500/30";
    }
  };

  const formatBytes = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/70 backdrop-blur-sm animate-fade-in">
      <div className="bg-[#111827] border border-gray-800 rounded-2xl max-w-2xl w-full max-h-[90vh] overflow-y-auto shadow-2xl">
        {}
        <div className="flex items-center justify-between p-6 border-b border-gray-800">
          <div className="flex items-center gap-3">
            <span
              className={`px-3 py-1 rounded-lg text-sm font-mono font-bold border ${getStatusBadge(
                log.status,
              )}`}
            >
              {log.status}
            </span>
            <span className="font-mono font-bold text-gray-200">
              {log.method}
            </span>
            <span className="text-gray-400 font-mono text-sm truncate max-w-md">
              {log.uri}
            </span>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded-lg text-gray-400 hover:text-gray-100 hover:bg-gray-800 transition"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {}
        <div className="p-6 space-y-6 text-sm">
          {}
          <div className="grid grid-cols-3 gap-4">
            <div className="bg-[#0b0f19] p-3 rounded-xl border border-gray-800/80">
              <span className="text-xs text-gray-500 block mb-1">
                Response Time
              </span>
              <div className="flex items-center gap-1.5 font-mono font-bold text-emerald-400 text-base">
                <Clock className="w-4 h-4" />
                {log.latency_ms} ms
              </div>
            </div>
            <div className="bg-[#0b0f19] p-3 rounded-xl border border-gray-800/80">
              <span className="text-xs text-gray-500 block mb-1">
                Body Transferred
              </span>
              <div className="font-mono font-bold text-gray-200 text-base">
                {formatBytes(log.bytes_sent)}
              </div>
            </div>
            <div className="bg-[#0b0f19] p-3 rounded-xl border border-gray-800/80">
              <span className="text-xs text-gray-500 block mb-1">Protocol</span>
              <div className="font-mono font-bold text-blue-400 text-base">
                {log.is_websocket ? "WebSocket (WS)" : "HTTP/1.1"}
              </div>
            </div>
          </div>

          {}
          <div className="bg-[#0b0f19] rounded-xl border border-gray-800/80 divide-y divide-gray-800/60 font-mono text-xs">
            <div className="p-3 flex justify-between">
              <span className="text-gray-400">Request ID:</span>
              <span className="text-gray-200">{log.id}</span>
            </div>
            <div className="p-3 flex justify-between">
              <span className="text-gray-400">Timestamp:</span>
              <span className="text-gray-200">
                {new Date(log.timestamp).toLocaleString()}
              </span>
            </div>
            <div className="p-3 flex justify-between">
              <span className="text-gray-400">Client IP:</span>
              <span className="text-emerald-400 font-semibold">
                {log.client_ip}
              </span>
            </div>
            <div className="p-3 flex justify-between">
              <span className="text-gray-400">Host Header:</span>
              <span className="text-gray-200">{log.host || "—"}</span>
            </div>
            <div className="p-3 flex justify-between">
              <span className="text-gray-400">Upstream Target:</span>
              <span className="text-teal-400 font-semibold">
                {log.upstream || "Local Static / Internal"}
              </span>
            </div>
            {log.user_agent && (
              <div className="p-3 flex flex-col gap-1">
                <span className="text-gray-400">User-Agent:</span>
                <span className="text-gray-300 break-all text-[11px]">
                  {log.user_agent}
                </span>
              </div>
            )}
            {log.error && (
              <div className="p-3 flex flex-col gap-1 bg-rose-950/30">
                <span className="text-rose-400 font-bold">Error Details:</span>
                <span className="text-rose-200">{log.error}</span>
              </div>
            )}
          </div>
        </div>

        {}
        <div className="p-4 border-t border-gray-800 flex justify-end bg-gray-900/40">
          <button
            onClick={onClose}
            className="px-4 py-2 rounded-lg bg-gray-800 hover:bg-gray-700 text-gray-200 text-sm font-medium transition"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
};

