import React, { useEffect, useState } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import { Navbar } from "./components/Navbar";
import { Dashboard } from "./pages/Dashboard";
import { VirtualHosts } from "./pages/VirtualHosts";
import { LiveRequests } from "./pages/LiveRequests";
import { Upstreams } from "./pages/Upstreams";
import { ConfigEditor } from "./pages/ConfigEditor";
import { ImportNginx } from "./pages/ImportNginx";
import { AuthModal } from "./components/AuthModal";
import { useToast } from "./components/ui/Overlays";
import {
  api,
  getAdminToken,
  ServerStatus,
  MetricsSnapshot,
  RequestLog,
  VirtualHost,
  UpstreamWithHealth,
  FerrixConfig,
  UpstreamConfig,
} from "./services/api";

export function App() {
  const toast = useToast();

  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [metrics, setMetrics] = useState<MetricsSnapshot | null>(null);
  const [vhosts, setVHosts] = useState<VirtualHost[]>([]);
  const [upstreams, setUpstreams] = useState<UpstreamWithHealth[]>([]);
  const [config, setConfig] = useState<FerrixConfig | null>(null);
  const [liveLogs, setLiveLogs] = useState<RequestLog[]>([]);
  const [sseConnected, setSseConnected] = useState<boolean>(false);
  const [isStreamingPaused, setIsStreamingPaused] = useState<boolean>(false);
  const [initialLoading, setInitialLoading] = useState<boolean>(true);
  const [isAuthenticated, setIsAuthenticated] = useState<boolean>(!!getAdminToken());
  const [showAuthModal, setShowAuthModal] = useState<boolean>(!getAdminToken());

  useEffect(() => {
    const handleUnauthorized = () => {
      setIsAuthenticated(false);
      setShowAuthModal(true);
    };
    window.addEventListener("ferrix:unauthorized", handleUnauthorized);
    return () => window.removeEventListener("ferrix:unauthorized", handleUnauthorized);
  }, []);

  const loadAllData = async () => {
    try {
      const [st, mt, vh, up, cfg, historicalLogs] = await Promise.all([
        api.getStatus(),
        api.getMetrics(),
        api.getVHosts(),
        api.getUpstreams(),
        api.getConfig(),
        api.getLogs({ limit: 50 }),
      ]);
      setStatus(st);
      setMetrics(mt);
      setVHosts(vh);
      setUpstreams(up);
      setConfig(cfg);
      setLiveLogs(historicalLogs);
    } catch (err: any) {
      if (err?.status !== 401) {
        toast.error(`Failed to load Ferrix data: ${err.message || err}`);
      }
    } finally {
      setInitialLoading(false);
    }
  };

  useEffect(() => {
    loadAllData();

    const interval = setInterval(async () => {
      try {
        const [mt, up, st] = await Promise.all([
          api.getMetrics(),
          api.getUpstreams(),
          api.getStatus(),
        ]);
        setMetrics(mt);
        setUpstreams(up);
        setStatus(st);
      } catch {

      }
    }, 2500);

    return () => clearInterval(interval);

  }, []);

  useEffect(() => {

    if (!isAuthenticated) {
      setSseConnected(false);
      return;
    }

    const unsubscribe = api.subscribeLiveLogs(
      (log) => {
        setSseConnected(true);
        if (!isStreamingPaused) {
          setLiveLogs((prev) => [log, ...prev.slice(0, 499)]);
        }
      },
      () => setSseConnected(false),
    );
    return () => unsubscribe();
  }, [isStreamingPaused, isAuthenticated]);

  const handleSaveVHost = async (vhost: VirtualHost) => {
    await api.saveVHost(vhost);
    const [updated, cfg] = await Promise.all([api.getVHosts(), api.getConfig()]);
    setVHosts(updated);
    setConfig(cfg);
  };

  const handleDeleteVHost = async (id: string) => {
    await api.deleteVHost(id);
    const [updated, cfg] = await Promise.all([api.getVHosts(), api.getConfig()]);
    setVHosts(updated);
    setConfig(cfg);
  };

  const handleSaveUpstream = async (upstream: UpstreamConfig) => {
    await api.saveUpstream(upstream);
    const [updated, cfg] = await Promise.all([api.getUpstreams(), api.getConfig()]);
    setUpstreams(updated);
    setConfig(cfg);
  };

  const handleDeleteUpstream = async (name: string) => {
    await api.deleteUpstream(name);
    const [updated, cfg] = await Promise.all([api.getUpstreams(), api.getConfig()]);
    setUpstreams(updated);
    setConfig(cfg);
  };

  const handleSaveConfig = async (newCfg: FerrixConfig) => {
    await api.updateConfig(newCfg);
    await loadAllData();
  };

  const handleAuthSuccess = () => {
    setIsAuthenticated(true);
    setShowAuthModal(false);
    loadAllData();
  };

  return (
    <div className="min-h-screen bg-surface-0 flex flex-col text-content-primary">
      <Navbar
        status={status}
        sseConnected={sseConnected}
        isAuthenticated={isAuthenticated}
        onOpenAuth={() => setShowAuthModal(true)}
      />

      <AuthModal
        isOpen={showAuthModal}
        onSuccess={handleAuthSuccess}
        onClose={() => setShowAuthModal(false)}
        canClose={isAuthenticated}
      />

      <main className="flex-1 max-w-7xl w-full mx-auto px-4 sm:px-6 lg:px-8 py-8">
        <Routes>
          <Route path="/" element={<Navigate to="/dashboard" replace />} />
          <Route
            path="/dashboard"
            element={
              <Dashboard metrics={metrics} status={status} liveLogs={liveLogs} loading={initialLoading} />
            }
          />
          <Route
            path="/vhosts"
            element={
              <VirtualHosts
                vhosts={vhosts}
                upstreams={upstreams}
                loading={initialLoading}
                onSaveVHost={handleSaveVHost}
                onDeleteVHost={handleDeleteVHost}
              />
            }
          />
          <Route
            path="/live"
            element={
              <LiveRequests
                logs={liveLogs}
                onClearLogs={() => setLiveLogs([])}
                isStreamingPaused={isStreamingPaused}
                onTogglePause={() => setIsStreamingPaused(!isStreamingPaused)}
                metricsDisabled={metrics?.enabled === false}
              />
            }
          />
          <Route
            path="/upstreams"
            element={
              <Upstreams
                upstreams={upstreams}
                loading={initialLoading}
                onSaveUpstream={handleSaveUpstream}
                onDeleteUpstream={handleDeleteUpstream}
              />
            }
          />
          <Route path="/import" element={<ImportNginx onApplied={loadAllData} />} />
          <Route
            path="/config"
            element={
              <ConfigEditor config={config} onSaveConfig={handleSaveConfig} onRefreshConfig={loadAllData} />
            }
          />
          <Route path="*" element={<Navigate to="/dashboard" replace />} />
        </Routes>
      </main>

      <footer className="border-t border-border-subtle py-4 bg-surface-1/60 text-center text-xs text-content-muted">
        Ferrix — High-Performance Nginx Clone written in Rust & Tokio • Management Dashboard
        & Telemetry
      </footer>
    </div>
  );
}

export default App;

