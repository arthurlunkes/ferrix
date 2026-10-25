import React, { createContext, useCallback, useContext, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { Button } from "./Button";
import { Modal } from "./Modal";

type ToastTone = "success" | "error" | "info";

interface ToastItem {
  id: number;
  tone: ToastTone;
  message: string;
}

interface ToastApi {
  success: (message: string) => void;
  error: (message: string) => void;
  info: (message: string) => void;
}

interface ConfirmOptions {
  title: string;
  description?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: "danger" | "brand";
}

type ConfirmApi = (options: ConfirmOptions) => Promise<boolean>;

const ToastContext = createContext<ToastApi | null>(null);
const ConfirmContext = createContext<ConfirmApi | null>(null);

const TOAST_ICON: Record<ToastTone, React.ReactNode> = {
  success: <CheckCircle2 className="w-4 h-4 text-success-400" />,
  error: <XCircle className="w-4 h-4 text-danger-400" />,
  info: <Info className="w-4 h-4 text-info-400" />,
};

const TOAST_BORDER: Record<ToastTone, string> = {
  success: "border-success-500/30",
  error: "border-danger-500/30",
  info: "border-info-500/30",
};

export const OverlayProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const push = useCallback(
    (tone: ToastTone, message: string) => {
      const id = nextId.current++;
      setToasts((prev) => [...prev, { id, tone, message }]);
      window.setTimeout(() => dismiss(id), tone === "error" ? 6000 : 3500);
    },
    [dismiss],
  );

  const toastApi: ToastApi = {
    success: (m) => push("success", m),
    error: (m) => push("error", m),
    info: (m) => push("info", m),
  };

  const [confirmState, setConfirmState] = useState<
    (ConfirmOptions & { resolve: (v: boolean) => void }) | null
  >(null);

  const confirmApi: ConfirmApi = useCallback(
    (options) =>
      new Promise<boolean>((resolve) => {
        setConfirmState({ ...options, resolve });
      }),
    [],
  );

  const closeConfirm = (result: boolean) => {
    confirmState?.resolve(result);
    setConfirmState(null);
  };

  return (
    <ToastContext.Provider value={toastApi}>
      <ConfirmContext.Provider value={confirmApi}>
        {children}

        {}
        <div className="fixed top-4 right-4 z-[100] flex flex-col gap-2 w-80 pointer-events-none">
          {toasts.map((t) => (
            <div
              key={t.id}
              className={`pointer-events-auto flex items-start gap-2.5 p-3 rounded-xl bg-surface-1 border ${TOAST_BORDER[t.tone]} shadow-modal animate-toast-in`}
            >
              <div className="mt-0.5 shrink-0">{TOAST_ICON[t.tone]}</div>
              <p className="text-sm text-content-primary flex-1 leading-snug">{t.message}</p>
              <button
                onClick={() => dismiss(t.id)}
                className="text-content-muted hover:text-content-primary transition"
                aria-label="Dismiss"
              >
                <X className="w-3.5 h-3.5" />
              </button>
            </div>
          ))}
        </div>

        <Modal
          open={confirmState !== null}
          onClose={() => closeConfirm(false)}
          title={confirmState?.title ?? ""}
          icon={<AlertTriangle className="w-5 h-5" />}
          iconTone={confirmState?.tone === "danger" ? "amber" : "brand"}
          size="md"
          footer={
            <>
              <Button variant="ghost" onClick={() => closeConfirm(false)}>
                {confirmState?.cancelLabel ?? "Cancel"}
              </Button>
              <Button
                variant={confirmState?.tone === "danger" ? "danger" : "primary"}
                onClick={() => closeConfirm(true)}
              >
                {confirmState?.confirmLabel ?? "Confirm"}
              </Button>
            </>
          }
        >
          <p className="text-sm text-content-secondary leading-relaxed">
            {confirmState?.description}
          </p>
        </Modal>
      </ConfirmContext.Provider>
    </ToastContext.Provider>
  );
};

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error("useToast precisa estar dentro de <OverlayProvider>");
  return ctx;
}

export function useConfirm(): ConfirmApi {
  const ctx = useContext(ConfirmContext);
  if (!ctx) throw new Error("useConfirm precisa estar dentro de <OverlayProvider>");
  return ctx;
}

