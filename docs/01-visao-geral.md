# 01. Visão Geral e Comparativo

## 📌 O que é o Ferrix?

O **Ferrix** é um servidor web e proxy reverso assíncrono projetado para combinar a eficiência e robustez do **Nginx** com a segurança de memória do **Rust** e a facilidade de uso de um **Dashboard Web moderno em React**.

No ecossistema tradicional, ferramentas como o Nginx exigem edição manual de arquivos de configuração em texto puro (`nginx.conf`), recarga por sinais de processo (`nginx -s reload`) e dependem de módulos de terceiros ou ferramentas externas para monitoramento de tráfego em tempo real.

O Ferrix resolve essa complexidade integrando em um único executável leve:

1. **Engine de Rede de Alta Concorrência**: Construída sobre Tokio, Hyper e Axum, processando dezenas de milhares de requisições por segundo de forma não-bloqueante.
2. **Painel Web React Embutido**: Uma interface gráfica sofisticada com dark mode, gráficos de tráfego, telemetria de latência e inspeção de conexões.
3. **API REST Administrativa**: Controle total programático para ferramentas de CI/CD, scripts e orquestradores.
4. **Hot-Reload em Memória (Zero Downtime)**: Atualizações de rota e balanceamento aplicadas atomicamente via ponteiros em memória (`ArcSwap`), sem que nenhuma conexão ativa seja derrubada.

---

## ⚖️ Comparativo: Ferrix vs. Nginx vs. Caddy

| Recurso                              | Nginx                                        | Caddy                  | Ferrix                                             |
| ------------------------------------ | -------------------------------------------- | ---------------------- | -------------------------------------------------- |
| **Linguagem Base**                   | C                                            | Go                     | **Rust** (Tokio / Hyper)                           |
| **Segurança de Memória**             | Vulnerável a memory leaks/overflows          | Segura (com GC pauses) | **Zero-Cost Abstractions / Sem Garbage Collector** |
| **Interface Web Nativa**             | ❌ Não (requer Nginx Proxy Manager separado) | ❌ Não                 | ✅ **Sim (React SPA embutida no binário)**         |
| **API REST para Configuração**       | ⚠️ Apenas na versão paga Nginx Plus          | ✅ Sim                 | ✅ **Sim (Nativa e aberta)**                       |
| **Monitoramento de Tráfego ao Vivo** | ⚠️ Requer exporter + Prometheus/Grafana      | ⚠️ Requer exporters    | ✅ **Sim (Stream em tempo real via SSE)**          |
| **Proxy de WebSockets**              | ✅ Sim (`proxy_set_header Upgrade`)          | ✅ Sim                 | ✅ **Sim (Túnel full-duplex transparente)**        |
| **Load Balancing & Failover**        | ✅ Sim                                       | ✅ Sim                 | ✅ **Sim (5 algoritmos + failover automático)**    |
| **Health Checks Ativos**             | ⚠️ Apenas no Nginx Plus                      | ✅ Sim                 | ✅ **Sim (Background worker nativo)**              |
| **SPA Fallback (`try_files`)**       | ✅ Sim                                       | ✅ Sim                 | ✅ **Sim**                                         |
| **Formato de Distribuição**          | Binário + módulos dinâmicos                  | Binário único          | **Binário único estático (com UI embutida)**       |

---

## 🛡️ Vantagens de ser Construído em Rust

1. **Memory Safety sem Garbage Collector (GC)**:
   - Diferente do Go (usado no Caddy e Traefik), o Rust não possui pausas de garbage collection (_stop-the-world_). Isso garante latências estáveis no p99 e previsibilidade sob alta carga.
2. **Prevenção Concorrente em Tempo de Compilação**:
   - O compilador do Rust impede _data races_ através do sistema de _ownership_ e _borrowing_.
3. **Zero-Copy e I/O Assíncrono com Tokio**:
   - Streams de dados e transferência de arquivos acontecem sem duplicação desnecessária de buffers na memória RAM.
