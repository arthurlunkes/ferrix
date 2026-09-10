import React, { useState, useEffect } from "react";
import {
  Activity,
  Zap,
  Clock,
  AlertTriangle,
  Server,
  Globe,
  Radio,
  ArrowUpRight,
} from "lucide-react";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  PieChart,
  Pie,
  Cell,
} from "recharts";
import { MetricCard } from "../components/MetricCard";
import { RequestDetailModal } from "../components/RequestDetailModal";
import { SkeletonCard } from "../components/ui/Skeleton";
import { MetricsSnapshot, RequestLog, ServerStatus } from "../services/api";

interface DashboardProps {
  metrics: MetricsSnapshot | null;
  status: ServerStatus | null;
  liveLogs: RequestLog[];
  loading?: boolean;
}

export const Dashboard: React.FC<DashboardProps> = ({
  metrics,
  status,
  liveLogs,
  loading,
}) => {
  const [selectedLog, setSelectedLog] = useState<RequestLog | null>(null);
  const [chartData, setChartData] = useState<
    { time: string; rps: number; latency: number }[]
  >([]);

  // Update chart data series
  useEffect(() => {
    if (!metrics) return;
    const now = new Date();
    const timeStr = `${now.getHours().toString().padStart(2, "0")}:${now
      .getMinutes()
      .toString()
      .padStart(2, "0")}:${now.getSeconds().toString().padStart(2, "0")}`;

    setChartData((prev) => {
      const next = [
        ...prev,
        {
          time: timeStr,
          rps: metrics.requests_per_second,
          latency: metrics.avg_latency_ms,
        },
      ];
      if (next.length > 25) next.shift();
      return next;
    });
  }, [metrics]);

  const totalRequests = metrics?.total_requests || 0;
  const errors = (metrics?.status_4xx || 0) + (metrics?.status_5xx || 0);
  const errorRate =
    totalRequests > 0 ? ((errors / totalRequests) * 100).toFixed(2) : "0.00";

  const statusPieData = [
    { name: "2xx Success", value: metrics?.status_2xx || 0, color: "#10b981" },
    { name: "3xx Redirect", value: metrics?.status_3xx || 0, color: "#3b82f6" },
    {
      name: "4xx Client Err",
      value: metrics?.status_4xx || 0,
      color: "#f59e0b",
    },
    {
      name: "5xx Server Err",
      value: metrics?.status_5xx || 0,
      color: "#ef4444",
    },
  ].filter((d) => d.value > 0);

  const getStatusBadge = (code: number) => {
    if (code >= 200 && code < 300)
      return "text-emerald-400 bg-emerald-500/10 border-emerald-500/20";
    if (code >= 300 && code < 400)
      return "text-blue-400 bg-blue-500/10 border-blue-500/20";
    if (code >= 400 && code < 500)
      return "text-amber-400 bg-amber-500/10 border-amber-500/20";
    return "text-rose-400 bg-rose-500/10 border-rose-500/20";
  };

  if (loading) {
    return (
      <div className="space-y-6">
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
          {[0, 1, 2, 3].map((i) => (
            <SkeletonCard key={i} />
          ))}
        </div>
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
          <div className="lg:col-span-2">
            <SkeletonCard />
          </div>
          <SkeletonCard />
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Top Metrics Row */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <MetricCard
          title="Total Requests"
          value={metrics?.total_requests.toLocaleString() || "0"}
          subtitle={`${metrics?.active_connections || 0} active connections`}
          icon={Activity}
          color="emerald"
        />
        <MetricCard
          title="Throughput (RPS)"
          value={`${metrics?.requests_per_second.toFixed(1) || "0.0"} req/s`}
          subtitle="Real-time request rate"
          icon={Zap}
          color="blue"
        />
        <MetricCard
          title="Avg Latency"
          value={`${metrics?.avg_latency_ms.toFixed(1) || "0.0"} ms`}
          subtitle={`p95: ${metrics?.p95_latency_ms.toFixed(1) || "0.0"} ms`}
          icon={Clock}
          color="purple"
        />
        <MetricCard
          title="Error Rate"
          value={`${errorRate}%`}
          subtitle={`${errors} total 4xx/5xx errors`}
          icon={AlertTriangle}
          color={Number(errorRate) > 5 ? "rose" : "emerald"}
        />
      </div>

      {/* Charts Row */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Real-time Traffic Area Chart */}
        <div className="lg:col-span-2 bg-[#111827]/90 border border-gray-800 rounded-2xl p-6 shadow-sm">
          <div className="flex items-center justify-between mb-4">
            <div>
              <h3 className="text-base font-bold text-white">
                Live Traffic & Latency
              </h3>
              <p className="text-xs text-gray-400">
                Requests per second and response latency trend
              </p>
            </div>
            <div className="flex items-center gap-4 text-xs font-mono">
              <span className="flex items-center gap-1.5 text-emerald-400">
                <span className="w-2.5 h-2.5 rounded-full bg-emerald-500"></span>
                RPS
              </span>
              <span className="flex items-center gap-1.5 text-blue-400">
                <span className="w-2.5 h-2.5 rounded-full bg-blue-500"></span>
                Latency (ms)
              </span>
            </div>
          </div>

          <div className="h-64 w-full">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={chartData}>
                <defs>
                  <linearGradient id="rpsGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%" stopColor="#10b981" stopOpacity={0.4} />
                    <stop offset="95%" stopColor="#10b981" stopOpacity={0.0} />
                  </linearGradient>
                  <linearGradient id="latGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%" stopColor="#3b82f6" stopOpacity={0.3} />
                    <stop offset="95%" stopColor="#3b82f6" stopOpacity={0.0} />
                  </linearGradient>
                </defs>
                <XAxis
                  dataKey="time"
                  stroke="#4b5563"
                  fontSize={11}
                  tickLine={false}
                />
                <YAxis stroke="#4b5563" fontSize={11} tickLine={false} />
                <Tooltip
                  contentStyle={{
                    backgroundColor: "#0f172a",
                    borderColor: "#374151",
                    borderRadius: "8px",
                    fontSize: "12px",
                  }}
                />
                <Area
                  type="monotone"
                  dataKey="rps"
                  stroke="#10b981"
                  strokeWidth={2}
                  fillOpacity={1}
                  fill="url(#rpsGrad)"
                />
                <Area
                  type="monotone"
                  dataKey="latency"
                  stroke="#3b82f6"
                  strokeWidth={2}
                  fillOpacity={1}
                  fill="url(#latGrad)"
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </div>

        {/* Status Codes Distribution */}
        <div className="bg-[#111827]/90 border border-gray-800 rounded-2xl p-6 shadow-sm flex flex-col justify-between">
          <div>
            <h3 className="text-base font-bold text-white">
              HTTP Status Breakdown
            </h3>
            <p className="text-xs text-gray-400">
              Distribution of response status codes
            </p>
          </div>

          <div className="h-44 w-full my-2 flex items-center justify-center">
            {statusPieData.length > 0 ? (
              <ResponsiveContainer width="100%" height="100%">
                <PieChart>
                  <Pie
                    data={statusPieData}
                    cx="50%"
                    cy="50%"
                    innerRadius={45}
                    outerRadius={70}
                    paddingAngle={3}
                    dataKey="value"
                  >
                    {statusPieData.map((entry, index) => (
                      <Cell key={`cell-${index}`} fill={entry.color} />
                    ))}
                  </Pie>
                  <Tooltip
                    contentStyle={{
                      backgroundColor: "#0f172a",
                      borderColor: "#374151",
                      borderRadius: "8px",
                      fontSize: "12px",
                    }}
                  />
                </PieChart>
              </ResponsiveContainer>
            ) : (
              <div className="text-gray-500 text-xs">
                Waiting for incoming traffic...
              </div>
            )}
          </div>

          <div className="grid grid-cols-2 gap-2 text-xs font-mono pt-2 border-t border-gray-800">
            <div className="flex items-center justify-between p-2 rounded bg-[#0b0f19]">
              <span className="text-emerald-400">2xx:</span>
              <span className="font-bold text-white">
                {metrics?.status_2xx || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2 rounded bg-[#0b0f19]">
              <span className="text-blue-400">3xx:</span>
              <span className="font-bold text-white">
                {metrics?.status_3xx || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2 rounded bg-[#0b0f19]">
              <span className="text-amber-400">4xx:</span>
              <span className="font-bold text-white">
                {metrics?.status_4xx || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2 rounded bg-[#0b0f19]">
              <span className="text-rose-400">5xx:</span>
              <span className="font-bold text-white">
                {metrics?.status_5xx || 0}
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Live Recent Requests Stream Preview */}
      <div className="bg-[#111827]/90 border border-gray-800 rounded-2xl p-6 shadow-sm">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-emerald-500/10 text-emerald-400">
              <Radio className="w-4 h-4 animate-pulse" />
            </div>
            <div>
              <h3 className="text-base font-bold text-white">
                Live Requests Stream
              </h3>
              <p className="text-xs text-gray-400">
                Real-time telemetry of incoming requests through the proxy
              </p>
            </div>
          </div>
          <span className="text-xs font-mono text-gray-400">
            Showing latest {Math.min(liveLogs.length, 6)} requests
          </span>
        </div>

        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs font-mono">
            <thead className="text-gray-400 bg-[#0b0f19] border-b border-gray-800">
              <tr>
                <th className="py-2.5 px-3">Status</th>
                <th className="py-2.5 px-3">Method</th>
                <th className="py-2.5 px-3">Host & URI</th>
                <th className="py-2.5 px-3">Upstream Target</th>
                <th className="py-2.5 px-3">Latency</th>
                <th className="py-2.5 px-3">Client IP</th>
                <th className="py-2.5 px-3 text-right">Time</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800/60">
              {liveLogs.slice(0, 6).map((log) => (
                <tr
                  key={log.id}
                  onClick={() => setSelectedLog(log)}
                  className="hover:bg-gray-800/40 cursor-pointer transition"
                >
                  <td className="py-2.5 px-3">
                    <span
                      className={`px-2 py-0.5 rounded border text-[11px] font-bold ${getStatusBadge(log.status)}`}
                    >
                      {log.status}
                    </span>
                  </td>
                  <td className="py-2.5 px-3 font-bold text-gray-200">
                    {log.method}
                  </td>
                  <td className="py-2.5 px-3 text-gray-300 max-w-xs truncate">
                    <span className="text-gray-500 mr-1">{log.host}</span>
                    {log.uri}
                  </td>
                  <td className="py-2.5 px-3 text-teal-400 truncate max-w-[180px]">
                    {log.upstream || "Local Static / Direct"}
                  </td>
                  <td className="py-2.5 px-3 text-emerald-400">
                    {log.latency_ms} ms
                  </td>
                  <td className="py-2.5 px-3 text-gray-400">{log.client_ip}</td>
                  <td className="py-2.5 px-3 text-right text-gray-500">
                    {new Date(log.timestamp).toLocaleTimeString()}
                  </td>
                </tr>
              ))}
              {liveLogs.length === 0 && (
                <tr>
                  <td colSpan={7} className="py-8 text-center text-gray-500">
                    No requests recorded yet. Send traffic to http://localhost:
                    {status?.proxy_listen.split(":")[1] || "8000"}
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
