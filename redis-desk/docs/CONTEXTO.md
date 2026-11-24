# Contexto do Suga-Redis

Documento de retomada. Leia antes de mexer no código — especialmente a
seção **Fatos que custaram caro**, que registra coisas que não se deduzem
lendo os arquivos e que já quebraram este projeto uma vez cada.

Estado em 2026-11-24. Branch `feat/menu-de-indice`, 135 commits, árvore limpa.
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

Um terceiro desenho (`docs/superpowers/specs/2026-11-24-menu-de-indice-design.md`)
trocou o menu nativo do sistema no botão direito da árvore por um menu do
aplicativo e, dentro dele, entregou **apagar todas as chaves sob um prefixo**:
seleção múltipla de prefixos, enumeração conferida na hora, cópia de segurança
embutida ou em arquivo, uma entrada de histórico por operação e volta atrás.
Do mesmo desenho, **busca no galho** e **estatística do prefixo** ficaram por
fazer (ver *Onde está incompleto*).

## Estado

| | |
|:-|-:|
| Testes Rust | **196**, todos passando |
| Testes de interface | **262**, todos passando |
| Linhas de código | ~32.000 (~17.000 Rust, ~15.000 interface) |
| Idiomas da interface | **3** (pt-BR, en, es), ~260 chaves por catálogo |
| `cargo clippy` | 3 avisos pré-existentes (`config.rs:246`, `history.rs:697`, `history.rs:776`) |
| `svelte-check` | 0 erros, 0 avisos |

Empacotado e instalado em `/Applications/Suga-Redis.app`, registrado no
Claude Code como servidor MCP (`claude mcp list` mostra `✔ Connected`). **Esse
empacotamento é de 2026-11-16 e não tem o menu de contexto nem o apagar em
massa** — nada desta leva foi empacotado ainda.

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

A suíte Rust roda **em paralelo contra o mesmo contêiner**, então cada módulo
de teste tem o seu banco. Quem está lá hoje, conferido no código, para não
colidir ao escrever novos:

| Banco | De quem é |
|:-|:-|
| 0 | `core::conn` (`db_sizes` conta as chaves do db0) |
| 1 | `core::keys` |
| 2 | `core::value`, leitura |
| 3 | `core::value`, escrita |
| 4 | `core::history` e o teste de índice frio de `core::search` |
| 5 | `core::index` |
| 6–9 | `core::search` |
| 10 | `commands.rs` |
| 11 | `mcp` |
| 12 | `approval` |
| 13 | `core::bulk`, os testes de exclusão em massa de `commands.rs`, e os de conectar/desconectar de `core::conn` e `commands.rs` |
| 14 | os testes de exclusão em massa de `core::history` |
| 15 | **de ninguém** — reservado por convenção para medições descartáveis |

**O 13 é o banco mais povoado, e isso impõe uma disciplina:** quem escrever
teste nele dá a cada caso um prefixo de chave só dele, não enumera o banco
inteiro e **não confere `DBSIZE`** — a contagem do banco é de todo mundo. O que
prova que uma exclusão não aconteceu é a própria chave continuar lá.

**O 15 é a única linha da tabela que não sai do código de um teste.** Nenhum
teste o toma; a única menção é um comentário em `commands.rs` que o dá por
tomado, e nada o corrobora. Está reservado por costume, não por uso — quem
precisar de um banco confira esta linha no código antes de confiar nela.

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
    mod.rs       só declara os módulos; `sqlite` é privado ao núcleo
    config.rs    hosts; senha no chaveiro do SO, nunca em arquivo
    conn.rs      pool por (host, banco); classificação de erro
    keys.rs      varredura, trie de prefixos, paginação de galho
    bulk.rs      enumeração por prefixo; exclusão por UNLINK em lotes
    value.rs     ler/escrever por tipo, paginação de string
    index.rs     índice FTS5 (trigram) para busca em VALORES
    search.rs    busca; nome ao vivo + valor pelo índice
    history.rs   cópia de segurança, histórico, volta atrás
    log.rs       anel de log em memória; nunca toca disco
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
- **Apagar em massa é sempre enumerar e apagar o enumerado**, nunca `FLUSHDB`
  — inclusive quando o alvo são todos os prefixos de primeiro nível, que no
  efeito esvazia o banco. **Apaga-se a lista que o diálogo exibiu**, não o que o
  prefixo tiver no instante de apagar: chave criada no meio não é apagada,
  chave que sumiu sozinha já estava fora. O número que a linha de histórico
  guarda é **o que de fato saiu** (`deleted`), ao lado do que foi enumerado
  (`enumerated`) — os dois diferem quando uma chave expira ou some entre a
  contagem e a exclusão, e quando o "desistir" interrompe a exclusão no meio do
  caminho. A linha não pode prometer de volta uma chave que já não existia
  (`core/history.rs`, `delete_prefix_with_backup`). A diferença entre os dois
  caminhos não é cosmética — o enumerado tem contagem conferida, cópia de
  segurança, linha de histórico e volta atrás, e o `FLUSHDB` não tem nenhum dos
  quatro.
- **A cópia de segurança descreve o conjunto TENTADO, nunca o enumerado.** Ela
  é *tirada* do enumerado — tem de ser, ela nasce antes de a primeira chave
  sair —, mas a linha que a oferece de volta só pode conter os retratos das
  chaves que o `UNLINK` chegou a tentar. Uma linha que ofereça mais do que isso
  escreve, a um clique, valores velhos por cima de valores vivos; e `rollback`
  **não tira retrato do que sobrescreve**, então não há segunda volta atrás.
  Quem garante isso é `close_bulk_row` (`core/history.rs`), apoiado na
  invariante de `bulk::BulkDeleted`: os lotes correm na ORDEM da lista e
  `cancel` só é conferido entre eles, então o conjunto tentado é sempre um
  **prefixo** dela e um `attempted: u64` o descreve por completo. Quando nada
  foi tentado, **a linha é removida** — ela registraria uma mudança que não
  aconteceu, e isso é pior do que não ter linha (a mesma decisão de
  `write_with_backup`).
- **`deleted` é um limite INFERIOR; `attempted` é o número que descreve um
  conjunto.** `Pool::with_retry` reenvia o `UNLINK` quando a conexão morre, e
  o comando é idempotente no estado final mas **não no valor de retorno**: um
  reenvio de algo já aplicado devolve 0 para chaves que saíram. Por isso quem
  recorta a cópia, a trie e o índice usa `attempted`, e a frase "o resto
  continua no banco" é dita sobre ele. Usar `deleted` para recortar caches
  deixa na árvore chaves que já não existem; afirmar sobre `deleted` mente
  sobre o banco.
- **Escrita em host `prod` exige confirmação**, decidida em `commands.rs`
  (não na tela — um botão pode esquecer de perguntar).
- **Escrita pedida pela LLM exige o dono digitar o nome do host**,
  exatamente, em qualquer ambiente. Sem app aberto não há socket e a escrita
  não acontece. Isso vale por construção.
- **Exclusão em massa em host `prod` também exige o nome do host digitado**,
  byte a byte — sem aparar espaço e sem ignorar caixa. Quem **decide** é
  `commands.rs` (`delete_prefix` compara `typed_host_name` com `host.name` e
  passa o resultado ao mesmo `needs_confirmation` do `write_value`), pela razão
  que a invariante do `prod` acima já dá: um botão pode esquecer de perguntar.
  Na tela, `DeletePrefixDialog.svelte` só **espelha** a regra em `liberado`,
  para o botão não acender antes da hora. É uma **divergência deliberada**, não
  uma inconsistência a corrigir: o `ConfirmDialog.svelte` diz em comentário
  que a decisão do produto é "que a pessoa seja avisada, não que seja obrigada
  a datilografar", e essa decisão continua valendo para o que ela governa —
  **uma** escrita, em **uma** chave, com cópia automática e volta atrás a um
  clique. Aqui o raio é de N chaves e o caminho "sem cópia" não tem volta: é a
  mesma régua que o projeto já aplica à escrita da LLM, pelo mesmo motivo — o
  custo de errar não cabe num clique. O `ConfirmDialog` não mudou; ganhou um
  irmão.
- **Número que vive dos dois lados da fronteira do Tauri tem teste que compara
  os dois.** São dois hoje: `MAX_BACKUP_BYTES` (`core/history.rs`) e
  `MAX_ENUMERATED` (`core/bulk.rs`), os dois espelhados em `api.ts`, e
  `delete-prefix.test.ts` lê as duas constantes do fonte Rust com expressão
  regular e exige igualdade. Sem isso os dois andam separados em silêncio, e
  erram para os dois lados: tela menor que o Rust manda para arquivo uma cópia
  que cabia embutida; tela maior oferece a embutida e colhe o erro do Rust
  depois de a pessoa ter escolhido. O teto de chaves passou um tempo do lado
  errado desta regra — digitado como prosa em três catálogos — e foi
  consertado junto do recorte da cópia.
- **Toda operação longa sobre o banco pode ser interrompida, e cada fase dela
  mostra andamento.** São três fases, e cada uma tem bandeira e número
  próprios: a enumeração (`cancel_enumeration`, por (host, banco), porque o id
  da contagem só nasce no fim da varredura), a leitura dos retratos
  (`BulkTick::Lendo`) e a exclusão em lotes (`BulkTick::Apagando`). A fase dos
  retratos é a mais LONGA — uma ida e volta por chave — e foi a última a
  ganhar as duas coisas: sem elas, "desistir" não tinha efeito observável e a
  janela ficava parada em "0 chaves apagadas", indistinguível de travada. Os
  números de fases diferentes **não** dividem canal: "12.000 retratos lidos" e
  "12.000 chaves apagadas" são frases diferentes.
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
- **Um `$derived` se recalcula na hora em que é lido.** Mexer no que ele lê,
  no meio de um laço que indexa o resultado dele, move o chão: a lista se
  refaz menor debaixo de índices contados na lista maior. Em `KeyTree.svelte`
  a lista achatada `linhas` depende da seleção **quando há filtro digitado** —
  só aí ela lê `guardamMarca`, para segurar na tela a marca que o filtro não
  casa; sem filtro não depende, e é por isso que o defeito era intermitente e
  não se reproduz sem digitar no filtro. Limpar a seleção antes de
  percorrer custou duas coisas — um `TypeError` saindo do manipulador de
  clique no `Shift+clique` (`atuais[i]` virava `undefined`) e, pior porque
  ninguém reporta, um estreitamento calado no `⌘+A`, que marcava menos linhas
  do que a tela mostrava. A correção é **fotografar a lista antes de qualquer
  mutação e indexar a foto** (`const atuais = linhas;` em `marcarFaixa` e
  `marcarPrimeiroNivel`).
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

### 2. As mensagens de erro do Rust continuam em português

E continuam **de propósito**. São 37 (`#[error(...)]` nos módulos de
`core/`), e elas atravessam a fronteira do Tauri como texto pronto. A tela
não as traduz: ela põe acima delas, no idioma de quem lê, a frase que diz
QUE operação falhou (`ErroTecnico.svelte`), e mostra a mensagem crua
embaixo, em mono. Traduzir a mensagem técnica a esconderia de quem a cola
num chamado ou a procura no código.

Virá-las em código + parâmetros continua possível — de dois a três dias,
mexendo em sete módulos — e continua não valendo enquanto ninguém reclamar.

O `--help` **já segue** o idioma: ele lê `LC_ALL`/`LANG`, a convenção do
Unix, porque a linha de comando não alcança a escolha guardada na janela.

As 10 descrições das ferramentas MCP ficam num idioma só, também de
propósito: quem as lê é o modelo, não gente.

### 3. Nenhuma release foi cortada

Versão `0.1.0` nos quatro lugares, `git tag` vazio. O `release.yml` nunca
rodou, então ninguém sabe se ele funciona. Depende de
`TAURI_SIGNING_PRIVATE_KEY` e `_PASSWORD` estarem nos Secrets do
repositório. Enquanto não houver tag, o badge de versão do README aponta
para uma release que não existe.

### 4. Nível com milhões de nomes distintos

Um nível visto em uso tinha ~3,4 milhões de filhos distintos. O teto por nível é
50.000; além dele, o botão **Carregar mais** revarre só aquele prefixo e
estende a lista. São ~68 levas de ~45 s. Alcançável, **não fluido**.

Minha leitura: rolar 3,4 milhões de irmãos não é interface para ninguém. O
que serve é **filtrar dentro do galho** — uma varredura direcionada em vez de
68. A aba **Busca** já faz isso pelo nome (ao vivo, sem índice); o que falta é
o filtro da árvore chamar essa busca quando o galho está cortado, em vez de
filtrar só o que está em memória.

**Continua por fazer, e agora tem onde morar.** A leva de 2026-11-24 construiu
o menu de botão direito que hospeda esses itens, e dele entregou só o apagar em
massa — que resolve *zerar* o galho, não *achar* dentro dele. Ficaram de fora,
as duas decididas e desenhadas em
`docs/superpowers/specs/2026-11-24-menu-de-indice-design.md`:

- **Busca no galho** — o item do menu que chama a busca ao vivo com o prefixo
  do galho por âncora, em vez de filtrar o que está em memória. É esta pergunta
  aberta, vista pelo outro lado.
- **Estatística do prefixo (abordagem E2)** — uma varredura dirigida colhendo
  `TYPE`, `PTTL` e `MEMORY USAGE` por chave, produzindo quebra por tipo,
  memória total, quantas chaves sem TTL e as maiores. Só o `TYPE` existe hoje,
  colhido de graça pela enumeração que precede o apagar (`core/bulk.rs`). O
  resto custa os pipelines de metadado que foram **tirados** da varredura de
  árvore por preço — eles levavam 11 idas e voltas a 350 —, e por isso o
  desenho os põe sob pedido explícito, com progresso, nunca por conta própria.

E o apagar em massa **não resolve este nível**: a enumeração para no teto de
`bulk::MAX_ENUMERATED` (100.000 chaves) e marca o resultado como cortado, então
um galho de 3,4 milhões não sai por aqui de uma vez. O teto existe pela mesma
razão que o da trie — juntar nomes num `Vec` custa o tamanho do prefixo. Com o
resultado cortado, o diálogo e o desfecho **dizem** que vão sobrar chaves e que
a operação precisa ser repetida: o item do menu promete "Apagar tudo sob este
prefixo…", e sobrar chave em silêncio é pior que falhar alto.

Nenhuma das duas tem item no menu hoje, e **não é para ganhar item cinza**
enquanto não existir: item que nunca acende ensina a pessoa a ignorar o menu. O
menu tem quatro itens, todos vivos — copiar prefixo e apagar tudo sob o
prefixo na linha de galho, copiar nome e apagar chave na folha.

### 5. Não implementado, de propósito

- **Reordenar itens de lista.** Não há troca atômica no conjunto de
  operações; em duas escritas, uma falha deixa a lista errada.
- **Som de alerta** quando chega pedido da LLM (a janela ganha foco). Exigiria
  o plugin de notificação.
- **Verificação interativa do empacotamento** (cadastrar host, navegar,
  editar e desfazer clicando). Os caminhos têm teste automatizado e foram
  conferidos pelo binário empacotado via MCP, mas o clique humano continua
  devendo. O **menu de contexto e o apagar em massa** devem mais que isso:
  nunca rodaram em binário empacotado, e o que os prova hoje é só a suíte. São
  os únicos caminhos que apagam N chaves de uma vez — o clique humano neles
  vale mais do que valia nos outros.

### 6. Avisos conhecidos

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
