# Contexto do Suga-Redis

Documento de retomada. Leia antes de mexer no código — especialmente a
seção **Fatos que custaram caro**, que registra coisas que não se deduzem
lendo os arquivos e que já quebraram este projeto uma vez cada.

Estado em 2026-11-16. Branch `main`, 97 commits, árvore limpa.
O repositório é **público** (`suga/suga-redis`) e é projeto pessoal.

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
desempenho: o aplicativo não dizia se estava conectado. Abrir um host
**Valkey serverless** de produção, por TLS, mostrou uma árvore
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
| Testes Rust | **128**, todos passando |
| Testes de interface | **165**, todos passando |
| Linhas de código | ~23.000 |
| Idiomas da interface | **3** (pt-BR, en, es), 176 chaves por catálogo |
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

## O que mudou depois de 2026-11-05

Três levas, nesta ordem:

**Distribuição.** `release.yml` empacota mac (universal, `.dmg`) e linux
(`.AppImage` + `.deb`) a partir de uma tag `v*`; o app pergunta ao GitHub
por versão nova ao abrir e se atualiza sozinho, conferindo a assinatura
minisign antes de trocar qualquer coisa. `npm run versao 0.2.0` sobe a
versão nos quatro lugares onde ela mora e cria a tag. **Nenhuma release foi
cortada ainda** — o `release.yml` nunca rodou.

**README com imagens, em cinco idiomas.** `README.md` (inglês) na raiz é a
fonte; `README.pt-BR.md`, `.es.md`, `.zh-CN.md` e `.ru.md` ao lado. As nove
imagens em `docs/img/` saíram do binário empacotado rodando contra um
cadastro de hosts fictício (`SUGA_REDIS_DATA_DIR` apontado para um
diretório descartável e um contêiner Redis só da demonstração) — **nunca
contra host real**, e é assim que elas devem ser refeitas.

**Interface em três idiomas** (ver *Idiomas*, abaixo).

## Idiomas

`core/i18n.svelte.ts` guarda o idioma em uso e `t(chave, valores)` devolve o
texto, trocando `{marcas}` por valores. Os catálogos vivem em
`src/lib/idiomas/` — `pt-BR.ts` é a base e os outros dois **têm a forma
dele**, então um idioma que esquece uma chave não compila.

- A escolha fica no `localStorage` e vence o idioma do sistema; sistema que
  não falamos cai no **inglês**, não no português.
- O **carimbo de ambiente é traduzido junto** (`ambiente.prod` e irmãos). Um
  aviso que não acompanha o idioma é o aviso perdido.
- **Os testes estão travados em `pt-BR`** (`src/tests/setup.ts`). Eles acham
  botão pelo nome visível; sem a trava passariam a depender do idioma de
  quem roda.
- Número e data saem por `Intl` com o idioma em uso — inclusive o aviso de
  valor binário, que foi o último a escapar.

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
- **Nada neste repositório pode citar outro projeto, cliente ou host real.**
  Ele é público e é só sobre si mesmo. Já vazou duas vezes, e as duas em
  comentário e doc, não em código — ninguém faz grep na própria prosa.
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

### O Node 22 já tem um `localStorage`, e ele é inerte

`globalThis.localStorage` passou a existir no Node 22 como um `getter`
nativo que não funciona sem `--localstorage-file` — e, por existir antes,
impede o jsdom de instalar o dele. Qualquer `localStorage.getItem` num
teste explodia com "Cannot read properties of undefined", e nada no
código diz por quê. `src/tests/setup.ts` instala um `Storage` em memória
por cima. Mesma família da armadilha do `JSON.parse` logo abaixo: as duas
nasceram da subida do Node 20 para o 22.

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

### 1. O host serverless: a causa raiz segue aberta

Um host **Valkey serverless** de produção, por TLS, respondeu
árvore vazia a uma pergunta cujo banco o dono confirma que **tem dado**.
`PING` respondia; `SCAN` voltava cursor `0` com lote vazio, sem erro; o
seletor de banco aparecia pelado, sem contagem. Duas hipóteses sobreviveram
à investigação e nenhuma foi descartada:

- `db_sizes` estourava e o `catch` silencioso de `Workspace.svelte`
  engolia o erro, mostrando "banco vazio" onde havia falha; ou
- o servidor responde vazio de verdade a `DBSIZE`/`SCAN` nesse host —
  proxy do serverless, ACL ou shard entre o cliente e o dado.

Esta leva de tarefas fechou o `catch` silencioso que confundia as duas
hipóteses e deu à tela o que falta para decidi-las. **Atenção ao que o log
NÃO é:** ele registra conexão, falha e lentidão — um comando saudável não
deixa rastro, por decisão de desenho (ver *Não-objetivos*). Se o `DBSIZE`
daquele host responder `0` de verdade, a aba **Logs** fica calada, e
calada é a resposta certa.

Quem decide entre as duas hipóteses é a tela do `Workspace`:

- `db_sizes` estourou → `erroBancos` mostra a mensagem ao lado do seletor
  de bancos (e o anel ganha uma linha `failure` de `list_dbs`, com o
  motivo, para ler depois);
- nenhum erro na tela e o seletor com `0` chaves → o servidor **respondeu
  vazio de verdade**, e a hipótese que sobra é a do proxy/ACL/shard.

**Ninguém ainda abriu esse host no binário empacotado para olhar
essas duas coisas** — essa leitura é o próximo passo, não um resultado
desta sessão. Até lá, a causa raiz continua em aberto.

### 2. O binário ainda só fala português

A tela fala três idiomas; o que vem do Rust, não. São **37 mensagens de
erro** (`#[error(...)]` nos módulos de `core/`) que atravessam a fronteira
do Tauri como texto pronto, mais o `--help` em `lib.rs` e as 10 descrições
das ferramentas MCP.

O caminho decidido para os erros é o barato: frase traduzida na tela com a
mensagem técnica do Rust embaixo, em fonte mono, como detalhe. Virá-los em
código + parâmetros é de dois a três dias e mexe em sete módulos — só vale
se alguém reclamar. As descrições do MCP ficam num idioma só **de
propósito**: quem as lê é o modelo, não gente.

### 3. As imagens do README mostram a janela em português

As nove capturas saíram antes do i18n. O README em inglês afirma que o
carimbo sai `PRODUCTION` e argumenta justamente que uma janela gritando
`PRODUÇÃO` para quem lê inglês perde o aviso — com as imagens logo abaixo
mostrando essa janela. Refazer exige empacotar o binário novo e repetir a
montagem descrita em *O que mudou*.

### 4. Nenhuma release foi cortada

Versão `0.1.0` nos quatro lugares, `git tag` vazio. O `release.yml` nunca
rodou, então ninguém sabe se ele funciona. Depende de
`TAURI_SIGNING_PRIVATE_KEY` e `_PASSWORD` estarem nos Secrets do
repositório. Enquanto não houver tag, o badge de versão do README aponta
para uma release que não existe.

### 5. Nível com milhões de nomes distintos

Um nível visto em uso tinha ~3,4 milhões de filhos distintos. O teto por nível é
50.000; além dele, o botão **Carregar mais** revarre só aquele prefixo e
estende a lista. São ~68 levas de ~45 s. Alcançável, **não fluido**.

Minha leitura: rolar 3,4 milhões de irmãos não é interface para ninguém. O
que serve é **filtrar dentro do galho** — uma varredura direcionada em vez de
68. A aba **Busca** já faz isso pelo nome (ao vivo, sem índice); o que falta é
o filtro da árvore chamar essa busca quando o galho está cortado, em vez de
filtrar só o que está em memória.

**Era a pergunta aberta quando esta sessão terminou.**

### 6. Não implementado, de propósito

- **Reordenar itens de lista.** Não há troca atômica no conjunto de
  operações; em duas escritas, uma falha deixa a lista errada.
- **Som de alerta** quando chega pedido da LLM (a janela ganha foco). Exigiria
  o plugin de notificação.
- **Verificação interativa do empacotamento** (cadastrar host, navegar,
  editar e desfazer clicando). Os caminhos têm teste automatizado e foram
  conferidos pelo binário empacotado via MCP, mas o clique humano continua
  devendo.

### 7. Avisos conhecidos

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
