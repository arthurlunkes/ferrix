# 06. Manual do Painel Web (React Dashboard)

O **Ferrix** vem equipado com uma interface gráfica completa construída em **React 19**, **Vite** e estilizada com **Tailwind CSS** em tema dark profissional.

O frontend está totalmente embutido dentro do próprio binário executável em Rust, o que significa que você não precisa instalar o Node.js em servidores de produção para utilizar a interface web.

Acesse o painel abrindo no seu navegador:
👉 **[http://localhost:8080](http://localhost:8080)**

---

## 🧭 Barra de Navegação Superior (Navbar)

No topo de qualquer tela, a barra de navegação apresenta:

- **Logo e Versão**: Indicação visual do Ferrix e versão instalada do binário em Rust.
- **Abas de Navegação**:
  - `Dashboard`: Visão geral e telemetria.
  - `Virtual Hosts`: Gestão de sites e regras de roteamento.
  - `Live Requests`: Inspeção e stream ao vivo de tráfego.
  - `Upstreams`: Monitoramento e configuração de backends.
  - `Config Editor`: Editor de JSON com validação e hot-reload manual.
- **Indicador de Conexão SSE**:
  - Um ponto pulsante verde indica que a conexão de Server-Sent Events está aberta e recebendo telemetria ao vivo em tempo real.
- **Uptime e Portas**:
  - Mostra a porta de tráfego do proxy ativo e o tempo de atividade contínuo do processo.

---

## 📈 1. Tela de Dashboard

A tela principal foi projetada para dar visibilidade imediata da saúde da sua infraestrutura:

1. **Cards de KPIs**:
   - **Total Requests**: Quantidade total de requisições atendidas desde o início do processo e conexões ativas simultâneas.
   - **Throughput (RPS)**: Taxa atualizada de requisições por segundo.
   - **Avg Latency**: Latência média de resposta em milissegundos e percentil p95.
   - **Error Rate**: Porcentagem de erros 4xx/5xx em relação ao tráfego total.
2. **Gráfico de Área em Tempo Real**:
   - Desenha a curva contínua de tráfego (RPS em verde) e latência de resposta (em azul) segundo a segundo.
3. **Distribuição de Status HTTP**:
   - Gráfico de rosca (_donut chart_) dividindo as respostas em 2xx (Sucesso), 3xx (Redirecionamento), 4xx (Erro do Cliente) e 5xx (Erro do Servidor).
4. **Feed Recente**:
   - Tabela compacta com as últimas 6 requisições finalizadas.

---

## ⚡ 2. Tela de Live Requests (Inspeção ao Vivo)

A tela de Live Requests transforma o Ferrix em uma ferramenta poderosa de depuração de tráfego HTTP e WebSockets:

- **Stream Instantâneo**: Cada requisição feita aos seus sites aparece imediatamente na tabela sem necessidade de recarregar a página.
- **Filtros por Código de Status**:
  - Botões para filtrar apenas sucessos (`2xx`), redirecionamentos (`3xx`), erros de cliente (`4xx`) ou falhas de servidor (`5xx`).
- **Filtros por Método HTTP**:
  - Filtre por `GET`, `POST`, `PUT`, `DELETE` ou conexões `WS` (WebSocket).
- **Busca Textual em Tempo Real**:
  - Digite no campo de busca para filtrar por caminho da URL, IP de origem, host ou servidor upstream.
- **Controles de Reprodução**:
  - Botão **"Pause Stream"**: Congela a rolagem para analisar uma requisição específica com calma sem que novos eventos empurrem a tabela.
  - Botão **"Resume Stream"**: Reativa a captura de novos eventos.
  - Botão **"Clear"**: Limpa o histórico em memória.

### 🔍 Modal de Detalhes da Requisição

Ao clicar em qualquer linha da tabela, um modal é exibido com:

- Código de status e método;
- Tempo exato de resposta em milissegundos;
- Volume de dados transferido (bytes);
- Tipo de protocolo (HTTP/1.1 ou WebSocket);
- IP do cliente e cabeçalho `Host`;
- Endereço do backend upstream que atendeu a requisição;
- Cabeçalho `User-Agent` completo;
- Detalhes do erro (caso o status seja >= 400).

---

## 🌐 3. Tela de Virtual Hosts

Gerencie todos os seus domínios e regras de proxy visualmente:

- **Listagem de Sites**: Exibe todos os domínios mapeados, porta de escuta e rotas ativas.
- **Criação / Edição de Virtual Host**:
  - Defina os nomes de servidor (ex: `app.meudominio.com`, `api.meudominio.com` ou `*`).
  - Adicione múltiplas rotas com caminhos prefixados (ex: `/`, `/api`, `/assets`).
  - Escolha o destino de cada rota:
    - **Proxy Upstream**: Selecione o pool de balanceamento e ative o suporte a WebSockets.
    - **Static Files**: Aponte para uma pasta local e configure o fallback `try_files` para SPAs.
    - **Redirect**: Configure redirecionamentos permanentes (301) ou temporários (302).
  - Ao salvar, a alteração é aplicada imediatamente em memória via **Hot-Reload**.

---

## 🖥️ 4. Tela de Upstreams

Monitore a saúde e disponibilidade dos seus servidores de backend:

- **Listagem de Pools**: Visualize os clusters de backend, quantidade de nós saudáveis e algoritmo de balanceamento em uso.
- **Status em Tempo Real por Servidor**:
  - Indicador visual verde (**Healthy**) ou vermelho (**Unhealthy**).
  - Latência medida pelo health check em milissegundos.
  - Contagem de falhas consecutivas e limite tolerado.
- **Configuração de Health Checks**:
  - Defina o caminho de verificação (ex: `/health`), o intervalo em segundos e o status HTTP esperado (ex: `200`).

---

## 📝 5. Tela de Config Editor

Para desenvolvedores e administradores que preferem editar a configuração diretamente em texto:

- **Editor com Destaque Sintático**: Exibe o JSON de configuração completo do Ferrix.
- **Botão "Validate & Hot-Reload"**: Valida se a sintaxe do JSON é válida e aplica as alterações diretamente no processo em execução.
- **Exportar e Importar**:
  - Baixe um backup da configuração em um clique (`Export JSON`).
  - Carregue um arquivo local salvo anteriormente (`Import JSON`).
- **Botão "Reset"**: Restaura o editor para o estado atualmente ativo caso você cometa algum engano durante a edição.
