import React, { useState } from "react";
import {
  Zap,
  Search,
  Pause,
  Play,
  Trash2,
  Filter,
  ArrowUpDown,
  Radio,
} from "lucide-react";
import { MetricsDisabledNotice } from "../components/MetricsDisabledNotice";
import { RequestLog } from "../services/api";
import { RequestDetailModal } from "../components/RequestDetailModal";

interface LiveRequestsProps {
  logs: RequestLog[];
  onClearLogs: () => void;
  isStreamingPaused: boolean;
  onTogglePause: () => void;
  metricsDisabled?: boolean;
}

export const LiveRequests: React.FC<LiveRequestsProps> = ({
  logs,
  onClearLogs,
  isStreamingPaused,
  onTogglePause,
  metricsDisabled,
}) => {
  const [selectedLog, setSelectedLog] = useState<RequestLog | null>(null);
  const [searchTerm, setSearchTerm] = useState("");
  const [statusFilter, setStatusFilter] = useState<string>("all");
  const [methodFilter, setMethodFilter] = useState<string>("all");

  const filteredLogs = logs.filter((log) => {

    if (searchTerm) {
      const q = searchTerm.toLowerCase();
      const matches =
        log.uri.toLowerCase().includes(q) ||
        log.client_ip.toLowerCase().includes(q) ||
        log.host.toLowerCase().includes(q) ||
        (log.upstream && log.upstream.toLowerCase().includes(q));
      if (!matches) return false;
    }

    if (statusFilter !== "all") {
      const prefix = parseInt(statusFilter[0]);
      if (Math.floor(log.status / 100) !== prefix) return false;
    }

    if (methodFilter !== "all") {
      if (methodFilter === "WS") {
        if (!log.is_websocket) return false;
      } else if (log.method !== methodFilter) {
        return false;
      }
    }

    return true;
  });

  const getStatusBadge = (code: number) => {
    if (code >= 200 && code < 300)
      return "text-emerald-400 bg-emerald-500/10 border-emerald-500/20";
    if (code >= 300 && code < 400)
      return "text-blue-400 bg-blue-500/10 border-blue-500/20";
    if (code >= 400 && code < 500)
      return "text-amber-400 bg-amber-500/10 border-amber-500/20";
    return "text-rose-400 bg-rose-500/10 border-rose-500/20";
  };

  const getMethodBadge = (method: string, isWs: boolean) => {
    if (isWs)
      return "bg-purple-950/60 text-purple-400 border border-purple-800/60";
    switch (method) {
      case "GET":
        return "bg-emerald-950/60 text-emerald-400 border border-emerald-800/60";
      case "POST":
        return "bg-blue-950/60 text-blue-400 border border-blue-800/60";
      case "PUT":
      case "PATCH":
        return "bg-amber-950/60 text-amber-400 border border-amber-800/60";
      case "DELETE":
        return "bg-rose-950/60 text-rose-400 border border-rose-800/60";
      default:
        return "bg-gray-800 text-gray-300";
    }
  };

  return (
    <div className="space-y-6">
      {metricsDisabled && <MetricsDisabledNotice />}
      <div className="flex flex-wrap items-center justify-between gap-4 bg-[#111827]/90 border border-gray-800 rounded-2xl p-4 shadow-sm">
        <div className="flex items-center gap-3">
          <div className="p-2 rounded-lg bg-emerald-500/10 text-emerald-400">
            <Radio
              className={`w-5 h-5 ${isStreamingPaused ? "" : "animate-pulse"}`}
            />
          </div>
          <div>
            <h2 className="text-base font-bold text-white">
              Live Request Inspector
            </h2>
            <p className="text-xs text-gray-400">
              {isStreamingPaused
                ? "Stream Paused"
                : "Streaming live traffic in real-time"}{" "}
              ({logs.length} in buffer)
            </p>
          </div>
        </div>

        {}
        <div className="flex items-center gap-2">
          <button
            onClick={onTogglePause}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-xl text-xs font-bold transition ${
              isStreamingPaused
                ? "bg-emerald-600 hover:bg-emerald-500 text-white shadow-lg shadow-emerald-500/20"
                : "bg-amber-600/20 hover:bg-amber-600/30 text-amber-400 border border-amber-500/30"
            }`}
          >
            {isStreamingPaused ? (
              <Play className="w-3.5 h-3.5" />
            ) : (
              <Pause className="w-3.5 h-3.5" />
            )}
            {isStreamingPaused ? "Resume Stream" : "Pause Stream"}
          </button>

          <button
            onClick={onClearLogs}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-xl bg-gray-800 hover:bg-gray-700 text-gray-300 text-xs font-medium transition"
          >
            <Trash2 className="w-3.5 h-3.5" />
            Clear
          </button>
        </div>
      </div>

      {}
      <div className="flex flex-wrap items-center gap-3 bg-[#111827]/90 border border-gray-800 rounded-2xl p-4">
        {}
        <div className="flex-1 min-w-[220px] relative">
          <Search className="w-4 h-4 text-gray-500 absolute left-3 top-2.5" />
          <input
            type="text"
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            placeholder="Search by URL, Client IP, Host, or Upstream..."
            className="w-full pl-9 pr-3 py-1.5 rounded-xl bg-[#0b0f19] border border-gray-800 text-gray-100 text-xs focus:outline-none focus:border-emerald-500 font-mono"
          />
        </div>

        {}
        <div className="flex items-center gap-1.5">
          <span className="text-xs text-gray-500 font-medium">Status:</span>
          {["all", "2xx", "3xx", "4xx", "5xx"].map((s) => (
            <button
              key={s}
              onClick={() => setStatusFilter(s)}
              className={`px-2.5 py-1 rounded-lg text-xs font-mono font-medium transition ${
                statusFilter === s
                  ? "bg-emerald-500/10 text-emerald-400 border border-emerald-500/30"
                  : "bg-gray-800/60 text-gray-400 hover:text-gray-200"
              }`}
            >
              {s.toUpperCase()}
            </button>
          ))}
        </div>

        {}
        <div className="flex items-center gap-1.5">
          <span className="text-xs text-gray-500 font-medium">Method:</span>
          {["all", "GET", "POST", "PUT", "DELETE", "WS"].map((m) => (
            <button
              key={m}
              onClick={() => setMethodFilter(m)}
              className={`px-2.5 py-1 rounded-lg text-xs font-mono font-medium transition ${
                methodFilter === m
                  ? "bg-blue-500/10 text-blue-400 border border-blue-500/30"
                  : "bg-gray-800/60 text-gray-400 hover:text-gray-200"
              }`}
            >
              {m}
            </button>
          ))}
        </div>
      </div>

      {}
      <div className="bg-[#111827]/90 border border-gray-800 rounded-2xl shadow-sm overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs font-mono">
            <thead className="text-gray-400 bg-[#0b0f19] border-b border-gray-800">
              <tr>
                <th className="py-3 px-4">Status</th>
                <th className="py-3 px-4">Method</th>
                <th className="py-3 px-4">Host & URI</th>
                <th className="py-3 px-4">Upstream Target</th>
                <th className="py-3 px-4">Latency</th>
                <th className="py-3 px-4">Client IP</th>
                <th className="py-3 px-4 text-right">Timestamp</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800/60">
              {filteredLogs.map((log) => (
                <tr
                  key={log.id}
                  onClick={() => setSelectedLog(log)}
                  className="hover:bg-gray-800/50 cursor-pointer transition"
                >
                  <td className="py-2.5 px-4">
                    <span
                      className={`px-2.5 py-0.5 rounded border text-[11px] font-bold ${getStatusBadge(log.status)}`}
                    >
                      {log.status}
                    </span>
                  </td>
                  <td className="py-2.5 px-4">
                    <span
                      className={`px-2 py-0.5 rounded text-[11px] font-bold ${getMethodBadge(log.method, log.is_websocket)}`}
                    >
                      {log.is_websocket ? "WS" : log.method}
                    </span>
                  </td>
                  <td className="py-2.5 px-4 text-gray-200 max-w-sm truncate">
                    <span className="text-gray-500 mr-1.5">{log.host}</span>
                    <span className="font-semibold">{log.uri}</span>
                  </td>
                  <td className="py-2.5 px-4 text-teal-400 truncate max-w-[200px]">
                    {log.upstream || "Local Static / Direct"}
                  </td>
                  <td className="py-2.5 px-4 text-emerald-400 font-bold">
                    {log.latency_ms} ms
                  </td>
                  <td className="py-2.5 px-4 text-gray-400">{log.client_ip}</td>
                  <td className="py-2.5 px-4 text-right text-gray-500">
                    {new Date(log.timestamp).toLocaleTimeString()}
                  </td>
                </tr>
              ))}
              {filteredLogs.length === 0 && (
                <tr>
                  <td colSpan={7} className="py-14 text-center">
                    <div className="flex flex-col items-center gap-2 text-gray-500">
                      <Zap className="w-6 h-6 opacity-60" />
                      <span className="text-xs max-w-sm">
                        {searchTerm || statusFilter !== "all" || methodFilter !== "all"
                          ? "No requests match the current filter."
                          : "No traffic yet. Send an HTTP request through the proxy and it will appear here in real time."}
                      </span>
                    </div>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      <RequestDetailModal
        log={selectedLog}
        onClose={() => setSelectedLog(null)}
      />
    </div>
  );
};

