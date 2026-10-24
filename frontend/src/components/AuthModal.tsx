import React, { useState } from "react";
import { KeyRound, ShieldCheck, AlertCircle, Loader2, X } from "lucide-react";
import { api, getAdminToken, setAdminToken } from "../services/api";

interface AuthModalProps {
  isOpen: boolean;
  onSuccess: () => void;
  onClose?: () => void;
  canClose?: boolean;
}

export const AuthModal: React.FC<AuthModalProps> = ({
  isOpen,
  onSuccess,
  onClose,
  canClose = false,
}) => {
  const [tokenInput, setTokenInput] = useState<string>(getAdminToken() || "");
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  if (!isOpen) return null;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = tokenInput.trim();
    if (!trimmed) {
      setError("Por favor, insira o token de administração.");
      return;
    }

    setLoading(true);
    setError(null);

    const isValid = await api.testToken(trimmed);
    setLoading(false);

    if (isValid) {
      setAdminToken(trimmed);
      onSuccess();
    } else {
      setError(
        "Token inválido ou rejeitado pelo Ferrix. Verifique o arquivo ferrix.json.",
      );
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/80 backdrop-blur-sm animate-fade-in">
      <div className="bg-[#111827] border border-gray-800 rounded-2xl max-w-md w-full p-6 shadow-2xl relative">
        {canClose && onClose && (
          <button
            onClick={onClose}
            className="absolute top-4 right-4 text-gray-400 hover:text-gray-200 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        )}

        <div className="flex items-center gap-3 mb-4">
          <div className="w-10 h-10 rounded-xl bg-emerald-500/10 border border-emerald-500/20 flex items-center justify-center text-emerald-400">
            <KeyRound className="w-5 h-5" />
          </div>
          <div>
            <h2 className="text-lg font-bold text-white tracking-tight">
              Autenticação do Ferrix
            </h2>
            <p className="text-xs text-gray-400">
              API Administrativa Protegida
            </p>
          </div>
        </div>

        <p className="text-sm text-gray-300 mb-4 leading-relaxed">
          O backend do Ferrix exige um token para exibir telemetria, rotas e
          logs ao vivo. Insira o token configurado no seu{" "}
          <code className="px-1.5 py-0.5 rounded bg-gray-800 text-emerald-400 font-mono text-xs">
            ferrix.json
          </code>
          :
        </p>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label className="block text-xs font-semibold text-gray-300 uppercase tracking-wider mb-1.5">
              Admin Bearer Token
            </label>
            <input
              type="text"
              value={tokenInput}
              onChange={(e) => {
                setTokenInput(e.target.value);
                setError(null);
              }}
              placeholder="token do campo token no ferrix.json"
              className="w-full px-3.5 py-2.5 bg-gray-900 border border-gray-700 rounded-lg text-sm text-white font-mono placeholder-gray-500 focus:outline-none focus:border-emerald-500 focus:ring-1 focus:ring-emerald-500 transition-all"
              autoFocus
            />
          </div>

          {error && (
            <div className="flex items-center gap-2 p-3 rounded-lg bg-rose-950/40 border border-rose-800/60 text-rose-300 text-xs">
              <AlertCircle className="w-4 h-4 shrink-0 text-rose-400" />
              <span>{error}</span>
            </div>
          )}

          <div className="flex items-center justify-end gap-3 pt-2">
            {canClose && onClose && (
              <button
                type="button"
                onClick={onClose}
                className="px-4 py-2 rounded-lg text-xs font-medium text-gray-400 hover:text-gray-200 hover:bg-gray-800 transition-colors"
              >
                Cancelar
              </button>
            )}
            <button
              type="submit"
              disabled={loading}
              className="flex items-center gap-2 px-5 py-2.5 rounded-lg text-xs font-semibold text-white bg-emerald-600 hover:bg-emerald-500 active:bg-emerald-700 disabled:opacity-50 transition-all shadow-lg shadow-emerald-600/20"
            >
              {loading ? (
                <>
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                  Validando...
                </>
              ) : (
                <>
                  <ShieldCheck className="w-3.5 h-3.5" />
                  Salvar e Conectar
                </>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
