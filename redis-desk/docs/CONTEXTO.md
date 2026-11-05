# Contexto do Suga-Redis

Documento de retomada. Leia antes de mexer no código — especialmente a
seção **Fatos que custaram caro**, que registra coisas que não se deduzem
lendo os arquivos e que já quebraram este projeto uma vez cada.

Estado em 2026-11-05. Branch `feat/redis-desk`, 61 commits, árvore limpa.

---

## O que é

Cliente de mesa para Redis (Tauri 2 + Rust + Svelte 5), feito para quem opera
banco de produção. Um binário, dois modos: sem argumento abre a janela, com
`--mcp` vira servidor MCP em stdio para o Claude local.

O plano original (`docs/superpowers/plans/2026-11-03-redis-desktop.md`) tem
**88 de 88 checkboxes marcados** — as 17 tarefas foram concluídas. O trabalho
posterior foi correção e desempenho em cima de uso real.

Um segundo desenho, fechado nesta sessão (`docs/superpowers/specs/2026-11-05-conexao-visivel-design.md`
e o plano que o segue), resolveu um problema mais estrutural do que
desempenho: o aplicativo não dizia se estava conectado. Abrir o host
**host-serverless** (Valkey serverless, produção, TLS) mostrou uma árvore
vazia, e não havia como saber, olhando a tela, se aquilo era um banco sem
chaves ou uma pergunta que falhou. A causa desse incidente **segue aberta**
(ver **Onde está incompleto**); o que esta leva de dez tarefas construiu foi
o instrumento para resolvê-la, e de quebra fechou o buraco que a escondia:

- Um **anel de log em memória** (`core/log.rs`) guarda as últimas entradas
  do aplicativo sem nunca tocar disco — fechou o app, perdeu o log, por
  decisão, não por limitação.
- O **pool registra** quando um socket abre, falha ou é derrubado.
- A **fachada em `commands.rs` cronometra** cada operação e registra o que
  falhou e o que passou de 1 s.
- **Conectar e desconectar** viraram comandos de verdade, não um efeito
  colateral de abrir a árvore.
- Uma **aba Logs**, legível depois do susto: hora, host, banco, operação,
  duração e erro.
- **Quatro estados de conexão** na linha de cada host (conectado,
  conectando, falhou, desconectado) — vermelho e caixa alta continuam
  significando só produção, nunca estado de conexão.
- Os **dois divisores de coluna** ficaram arrastáveis, com a largura
  lembrada.
- E os **dois `catch` silenciosos** que transformavam uma pergunta falha em
  resposta vazia — `listDbs` e `scanStatus`, em `Workspace.svelte` —
  pararam de existir: cada falha agora tem seu próprio estado (`erroBancos`,
  `erroIndice`), visível, em vez de se disfarçar de banco vazio.

## Estado

| | |
|:-|-:|
| Testes Rust | **114**, todos passando |
| Testes de interface | **91**, todos passando |
| Linhas de código | ~17.300 |
| `cargo clippy` | 3 avisos pré-existentes (`config.rs:246`, `history.rs:381`, `history.rs:443`) |
| `svelte-check` | 0 erros, 0 avisos |

Empacotado e instalado em `/Applications/Suga-Redis.app`, registrado no
Claude Code como servidor MCP (`claude mcp list` mostra `✔ Connected`).

## Como rodar

```bash
cd redis-desk
npm install
./scripts/test-redis.sh up        # Redis de teste em 127.0.0.1:6399 (docker)
npm run tauri dev
```

**Sem o contêiner de teste no ar, a maioria dos testes Rust falha com
`Connection refused`.** Ele se chama `suga-redis-test`.

```bash
npm test                          # interface
cd src-tauri && cargo test        # núcleo (precisa do contêiner)
npm run check                     # svelte-check
npm run dist                      # empacota em dist/ (gitignorado)
ditto dist/Suga-Redis.app /Applications/Suga-Redis.app
```

Bancos usados pelos testes, para não colidir ao escrever novos: 0–2
(`conn`/`value`), 1 (`keys`), 4 (`search` índice frio), 5 (`index`), 6–9
(`search`), 10 (`commands`), 11 (`mcp`), 12 (`approval`), 15 (medições
descartáveis).

---

## Arquitetura

```
src-tauri/src/
  core/          núcleo, independente do Tauri
    config.rs    hosts; senha no chaveiro do SO, nunca em arquivo
    conn.rs      pool por (host, banco); classificação de erro
    keys.rs      varredura, trie de prefixos, paginação de galho
    value.rs     ler/escrever por tipo, paginação de string
    index.rs     índice FTS5 (trigram) para busca em VALORES
    search.rs    busca; nome ao vivo + valor pelo índice
    history.rs   cópia de segurança, histórico, volta atrás
    sqlite.rs    abertura compartilhada (WAL + busy_timeout)
  commands.rs    fachada Tauri; AQUI mora a trava de produção
  approval/      trava de escrita da LLM (socket unix)
  mcp/           servidor MCP (rmcp): 7 leituras + 3 escritas

src/lib/
  api.ts         ÚNICA porta entre tela e Rust
  *.svelte       componentes
```

Duas regras seguidas de ponta a ponta:

- **Identificadores em inglês no Rust; comentários e texto de tela em
  português.** Nos componentes Svelte os locais seguem a convenção
  portuguesa do próprio arquivo (`bancos`, `relatorio`, `filtro`,
  `varrendo`, `erroIndice`) — a regra de inglês vale para o núcleo em Rust,
  não para a tela.
- **Nada fala com o Redis fora de `core/`,** e nada fala com o `core` a
  partir da tela sem passar por `commands.rs`.

## Invariantes que não podem ser quebradas

- `KEYS`, `FLUSHALL` e `FLUSHDB` são proibidos. O único lugar com `FLUSHALL`
  é `scripts/test-redis.sh`, contra o contêiner de teste.
- **Escrita em host `prod` exige confirmação**, decidida em `commands.rs`
  (não na tela — um botão pode esquecer de perguntar).
- **Escrita pedida pela LLM exige o dono digitar o nome do host**,
  exatamente, em qualquer ambiente. Sem app aberto não há socket e a escrita
  não acontece. Isso vale por construção.
- **Valor truncado não pode ser salvo** — gravaria o pedaço sobre o inteiro.
- Tipos editáveis: `string`, `hash`, `list`, `zset`. `set`, `stream` e
  RedisJSON são somente leitura.
- Caixa alta na interface significa **uma** coisa: o ambiente.
- O vermelho `--prod` significa **uma** coisa: produção. Erro de formulário
  não usa.

---

## Fatos que custaram caro

Cada um destes já quebrou o projeto. Estão aqui porque não se deduzem
lendo o código.

### `SCAN MATCH` não encurta o trabalho do servidor

Ele filtra o que volta pela rede; o cursor percorre o espaço de chaves
inteiro de todo jeito. Carregar a árvore por galho fez **cada clique pagar o
banco inteiro** — abrir um galho de 2 chaves num banco de 3,7 milhões
custava a varredura completa.

Por isso existe a trie: **uma** varredura, em memória, e todo galho abre
depois em microssegundos.

### `rustls` 0.23 entra em pânico sem provedor de criptografia

Não escolhe um sozinho, e o `redis` só habilita um nas dependências de teste
*dele*. Sem instalar um, **todo host com TLS era impossível de conectar** — e
nenhum teste pegava, porque o Redis de teste fala em texto puro.
`core/conn.rs` instala `ring` na primeira conexão TLS. Há teste de regressão.

### AWS ElastiCache/Valkey serverless

- TLS é **obrigatório** (`rediss://`).
- É **modo cluster**: só existe o banco 0. `SELECT` em qualquer outro falha,
  e `ConnError::ClusterSingleDb` traduz isso em instrução.
- `INFO keyspace` volta **vazio**; `db_sizes` recua para `DBSIZE`.
- O DNS resolve com prefixo `default.` — é a marca desse produto.

### A mensagem do `JSON.parse` não é contrato

Até o Node 20 trazia `at position N`; do 22 em diante traz um trecho do
texto. O validador em `src/lib/json.ts` usa o `JSON.parse` só para o
**veredito** e um varredor próprio para achar a **posição**.

### Svelte 5

- **`void x;` não registra dependência** — o compilador descarta a
  expressão. Um efeito que dependia de `void root` rodou uma vez só. Use uma
  leitura de verdade (ver `version` em `KeyTree.svelte`).
- **Não confie que um efeito roda uma vez.** O efeito que limpava estado ao
  trocar de banco re-disparava e apagava os galhos abertos. A correção foi
  torná-lo **idempotente**, não fazê-lo disparar menos.
- `$state` inicializado a partir de prop captura só o valor inicial; use
  `untrack` + `{#key}` para remontar (ver `HostForm`, `Workspace`).
- `bind:value` em `<input type="number">` entrega número, não texto.

### Build do linux em contêiner

- Use `bash -c`, **não** `bash -lc` — o shell de login reescreve o `PATH` e
  o `cargo` some.
- O Node do Debian bookworm é o 18; o Vite 8 precisa do 20+. Instale pelo
  NodeSource.
- **Depois do build, rode `rm -rf node_modules && npm install`** — o
  contêiner compartilha o volume e troca os binários nativos do rollup pelos
  de linux, quebrando o `npm test` no mac.

### Desempenho medido (500 mil chaves, local)

| | |
|:-|-:|
| Primeira árvore na tela | 26 ms |
| Varredura completa (segundo plano) | 1,67 s |
| Abrir um galho | 14–630 µs |
| Busca com 5.000 indexadas | 3 ms |

A varredura de nomes faz **11 idas e voltas** para 50 mil chaves (eram 350,
quando metadados inúteis eram coletados junto).

---

## Onde está incompleto

### 1. O host-serverless: a causa raiz segue aberta

O host **host-serverless** (Valkey serverless, produção, TLS) respondeu
árvore vazia a uma pergunta cujo banco o dono confirma que **tem dado**.
`PING` respondia; `SCAN` voltava cursor `0` com lote vazio, sem erro; o
seletor de banco aparecia pelado, sem contagem. Duas hipóteses sobreviveram
à investigação e nenhuma foi descartada:

- `db_sizes` estourava e o `catch` silencioso de `Workspace.svelte`
  engolia o erro, mostrando "banco vazio" onde havia falha; ou
- o servidor responde vazio de verdade a `DBSIZE`/`SCAN` nesse host —
  proxy do serverless, ACL ou shard entre o cliente e o dado.

Esta leva de tarefas fechou o `catch` silencioso que confundia as duas
hipóteses (ver acima) e deu à aba **Logs** o que faltava para decidi-las: a
resposta crua do servidor, por host, por operação, com o erro quando
houver. **Ninguém ainda abriu o host-serverless no binário empacotado para
ler essa aba** — essa leitura é o próximo passo, não um resultado desta
sessão. Até lá, a causa raiz continua em aberto.

### 2. Nível com milhões de nomes distintos

`sessao:token:` tem ~3,4 milhões de filhos distintos. O teto por nível é
50.000; além dele, o botão **Carregar mais** revarre só aquele prefixo e
estende a lista. São ~68 levas de ~45 s. Alcançável, **não fluido**.

Minha leitura: rolar 3,4 milhões de irmãos não é interface para ninguém. O
que serve é **filtrar dentro do galho** — uma varredura direcionada em vez de
68. A aba **Busca** já faz isso pelo nome (ao vivo, sem índice); o que falta é
o filtro da árvore chamar essa busca quando o galho está cortado, em vez de
filtrar só o que está em memória.

**Era a pergunta aberta quando esta sessão terminou.**

### 3. Não implementado, de propósito

- **Reordenar itens de lista.** Não há troca atômica no conjunto de
  operações; em duas escritas, uma falha deixa a lista errada.
- **Som de alerta** quando chega pedido da LLM (a janela ganha foco). Exigiria
  o plugin de notificação.
- **Verificação interativa do empacotamento** (cadastrar host, navegar,
  editar e desfazer clicando). Os caminhos têm teste automatizado e foram
  conferidos pelo binário empacotado via MCP, mas o clique humano continua
  devendo.

### 4. Avisos conhecidos

- 3 avisos de clippy pré-existentes, todos de "função com muitos argumentos"
  ou `assert_eq!` com booleano literal. Nenhum é defeito.
- O hook do `impeccable` reclama de `side-tab` na borda colorida à esquerda
  das linhas de host. **É intencional**: a marca de ambiente é o dispositivo
  de segurança central do produto, não enfeite de cartão.

---

## Como foi trabalhado

TDD de verdade, tarefa a tarefa: teste escrito primeiro, falha observada,
implementação depois, commit por tarefa. Vale manter — vários bugs desta
sessão só apareceram porque o teste falhou antes de existir conserto.

Medir antes de otimizar também se pagou: a lentidão da árvore parecia pedir
barra de progresso e na verdade pedia parar de buscar metadados que ninguém
lia.
