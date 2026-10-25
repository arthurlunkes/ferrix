<p align="center">
  <img src="assets/ferrix-banner.png" alt="Ferrix Banner" width="100%">
</p>

# Documentação Oficial do Ferrix

Bem-vindo à documentação completa do **Ferrix** — um servidor web e reverse proxy de alto desempenho inspirado no Nginx, desenvolvido em **Rust** e equipado nativamente com um **Dashboard Web em React** e uma **API REST** para gerenciamento dinâmico em tempo real (_zero-downtime hot-reload_).

---

## 📚 Índice da Documentação

1. [**01. Visão Geral e Comparativo**](01-visao-geral.md)
   - O que é o Ferrix e motivação do projeto.
   - Comparação direta com Nginx e Caddy.
   - Recursos e vantagens de segurança e concorrência do Rust.

2. [**02. Instalação e Execução**](02-instalacao-e-execucao.md)
   - Pré-requisitos de sistema.
   - Compilação do backend (Rust) e frontend (React).
   - Como rodar via CLI, flags disponíveis e execução em segundo plano.
   - Exemplo de containerização com Docker.

3. [**03. Arquitetura Interna**](03-arquitetura.md)
   - Runtime assíncrono com Tokio, Hyper e Axum.
   - Hot-Reload Zero Downtime via ponteiros atômicos (`ArcSwap`).
   - Coletor de métricas, buffer circular de logs e Server-Sent Events (SSE).
   - Estrutura de módulos do código fonte.

4. [**04. Guia de Recursos e Configuração**](04-guia-de-configuracao.md)
   - Formato do arquivo `ferrix.json`.
   - Virtual Hosts e roteamento por domínio/prefixo.
   - Algoritmos de Load Balancing (Round-Robin, Weighted, Least Connections, IP Hash, Random).
   - Health Checks ativos e failover automático (_proxy_next_upstream_).
   - Túnel transparente para WebSockets.
   - Arquivos estáticos e diretiva `try_files` para Single Page Applications (SPAs).
   - Rate Limiting (Token Bucket), IP ACL (Allow/Deny) e Basic Auth.

5. [**05. Referência da API REST**](05-api-rest.md)
   - Especificação detalhada de todos os endpoints HTTP e canal SSE.
   - Exemplos práticos de chamadas com `curl` e payloads JSON.
   - CRUD de Virtual Hosts e Upstreams via API.

6. [**06. Manual do Painel Web (React Dashboard)**](06-dashboard-web.md)
   - Visão geral das telas (Dashboard, Virtual Hosts, Live Requests, Upstreams, Config Editor).
   - Inspeção de requisições ao vivo em tempo real.
   - Gestão visual sem necessidade de tocar em arquivos de configuração.
