/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        // Espelham as CSS custom properties de src/design/tokens.css. Trocar o tom de uma
        // superfície muda os dois lugares de uma vez.
        surface: {
          0: "var(--surface-0)",
          1: "var(--surface-1)",
          2: "var(--surface-2)",
          3: "var(--surface-3)",
        },
        border: {
          subtle: "var(--border-subtle)",
          DEFAULT: "var(--border-default)",
          strong: "var(--border-strong)",
        },
        content: {
          primary: "var(--text-primary)",
          secondary: "var(--text-secondary)",
          muted: "var(--text-muted)",
        },
        brand: {
          50: "var(--brand-50)",
          300: "var(--brand-300)",
          400: "var(--brand-400)",
          500: "var(--brand-500)",
          600: "var(--brand-600)",
          700: "var(--brand-700)",
        },
        success: { 400: "var(--success-400)", 500: "var(--success-500)" },
        warning: { 400: "var(--warning-400)", 500: "var(--warning-500)" },
        danger: { 400: "var(--danger-400)", 500: "var(--danger-500)" },
        info: { 400: "var(--info-400)", 500: "var(--info-500)" },

        // Mantido para não quebrar classes ainda não migradas às novas telas.
        ferrix: {
          50: "#f0fdf4",
          100: "#dcfce7",
          500: "#22c55e",
          600: "#16a34a",
          900: "#14532d",
          dark: "#0f172a",
          card: "#1e293b",
          border: "#334155",
        },
      },
      fontFamily: {
        sans: ["var(--font-sans)"],
        mono: ["var(--font-mono)"],
      },
      boxShadow: {
        card: "var(--shadow-md)",
        modal: "var(--shadow-lg)",
        glow: "var(--shadow-glow)",
      },
      keyframes: {
        "toast-in": {
          "0%": { opacity: "0", transform: "translateY(-8px) scale(0.98)" },
          "100%": { opacity: "1", transform: "translateY(0) scale(1)" },
        },
      },
      animation: {
        "toast-in": "toast-in 180ms ease-out",
      },
    },
  },
  plugins: [],
};
