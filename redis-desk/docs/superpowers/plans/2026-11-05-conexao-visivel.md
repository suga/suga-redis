# Plano de implementação — Tornar a conexão visível

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Que a janela diga se está conectada, desde quando e por que não está — e que exista um lugar onde ler o motivo depois.

**Architecture:** Um anel de log em memória no núcleo (`core/log.rs`), alimentado em dois pontos honestos: o `Pool` registra os eventos de conexão (só ele sabe quando um socket nasce ou morre) e a fachada `commands.rs` registra falha e lentidão (só ela sabe o nome da operação). A tela lê o anel por um comando novo e o mostra numa aba. O estado de conexão por host é **derivado** do que as operações respondem — o pool é preguiçoso e multiplexado, não existe "sessão aberta" para consultar.

**Tech Stack:** Tauri 2, Rust (tokio, redis-rs), Svelte 5, vitest + @testing-library/svelte.

**Spec:** `docs/superpowers/specs/2026-11-05-conexao-visivel-design.md`

## Global Constraints

- **Identificadores em inglês; comentários e texto de tela em português.**
- **Nada fala com o Redis fora de `core/`.** A tela chega ao núcleo só por `commands.rs`, e só chega a `commands.rs` por `src/lib/api.ts`.
- **Vermelho e caixa alta significam produção, e nada mais.** Estado de conexão usa verde, âmbar e cinza.
- **Nunca** valor de chave nem senha no log.
- **Nenhum `catch` mudo.** Erro engolido é o defeito que este trabalho veio corrigir.
- Limiar de lentidão: **1 s**. Anel: **1000 linhas**, mais recente em cima.
- TDD: teste primeiro, falha observada, implementação, um commit por tarefa.
- Testes Rust exigem `./scripts/test-redis.sh up` (contêiner `suga-redis-test`, 127.0.0.1:6399). Bancos 0–12 e 15 estão tomados; **use o banco 13** para o que for novo.
- Comandos: `cd src-tauri && cargo test`, `npm test`, `npm run check`.

## Review Focus

1. **Senha e valor de chave no log** — a linha registra o nome da operação e o erro, nunca o conteúdo; um host com senha no chaveiro não pode vazar por `Display` de erro. *(teste na Task 1 e na Task 3)*
2. **Desconectar no meio de uma varredura** — retratos em voo continuam chegando por evento depois do `drop_host` e não podem repovoar a árvore de um host do qual já se saiu. *(teste na Task 7)*
3. **Largura guardada inválida** — `localStorage` com lixo, `NaN`, negativo ou maior que a janela não pode deixar a coluna irrecuperável. *(teste na Task 8)*
4. **Janela estreita** — abaixo de `52rem` o layout empilha; a largura guardada precisa ser ignorada aí, não aplicada a uma coluna que já não existe. *(teste na Task 8)*
5. **Anel cheio** — a linha 1001 descarta a mais velha, e a ordem continua mais-recente-primeiro. *(teste na Task 1)*

---

## Estrutura de arquivos

| Arquivo | Responsabilidade |
|:-|:-|
| `src-tauri/src/core/log.rs` *(novo)* | `LogRing`: anel de 1000 entradas, sem nada de Redis dentro |
| `src-tauri/src/core/conn.rs` *(modificar)* | `Pool` recebe o anel e registra conexão aberta, falhada e descartada |
| `src-tauri/src/commands.rs` *(modificar)* | `AppState.logs`; cronometragem de falha/lentidão; `connect`, `disconnect`, `logs` |
| `src/lib/api.ts` *(modificar)* | `connect`, `disconnect`, `logs` e os tipos deles |
| `src/lib/LogPanel.svelte` *(novo)* | A aba Logs |
| `src/lib/HostList.svelte` *(modificar)* | Linha de estado e botão Conectar ↔ Desconectar |
| `src/lib/Divisor.svelte` *(novo)* | Alça de arrastar, com piso, teto e largura guardada |
| `src/routes/+page.svelte`, `src/lib/Workspace.svelte` *(modificar)* | Aba nova, divisores, fim dos erros engolidos |

---

## Task 1: O anel de log

**Files:**
- Create: `src-tauri/src/core/log.rs`
- Modify: `src-tauri/src/core/mod.rs` (declarar `pub mod log;`)
- Test: dentro de `core/log.rs`, `mod tests`

**Interfaces:**
- Produces:
  ```rust
  pub enum LogKind { Connection, Failure, Slow }
  pub struct LogEntry {
      pub at_ms: u64,        // epoch em milissegundos
      pub kind: LogKind,
      pub host: String,      // nome do host, não o id
      pub db: Option<u8>,
      pub operation: String, // "scan_tree", "list_dbs", "connect"
      pub duration_ms: Option<u64>,
      pub detail: Option<String>, // mensagem de erro já classificada
  }
  pub struct LogRing { /* Mutex<VecDeque<LogEntry>> */ }
  impl LogRing {
      pub fn new() -> LogRing;
      pub fn push(&self, entry: LogEntry);
      pub fn snapshot(&self) -> Vec<LogEntry>; // mais recente primeiro
  }
  pub const CAPACITY: usize = 1000;
  pub const SLOW_MS: u64 = 1000;
  ```
  `LogEntry` deriva `Debug, Clone, Serialize, Deserialize`; `LogKind` serializa em minúsculas (`#[serde(rename_all = "lowercase")]`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn snapshot_puts_the_newest_first() { /* push a, push b → snapshot()[0] é b */ }

#[test]
fn the_ring_drops_the_oldest_past_capacity() {
    // CAPACITY + 1 entradas; snapshot().len() == CAPACITY,
    // a primeira empurrada não está mais lá, e a mais nova é snapshot()[0].
}

#[test]
fn an_entry_has_no_room_for_a_value_or_a_password() {
    // serde_json::to_string de uma LogEntry montada com todos os campos
    // preenchidos não contém "password" nem "senha" nem "value":
    // a struct não tem onde guardar isso.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib core::log`
Expected: FAIL — o módulo não existe.

- [ ] **Step 3: Implement `core/log.rs`**

`Mutex<VecDeque<LogEntry>>`, `push` faz `push_front` e corta a cauda em `CAPACITY`. `at_ms` vem de `SystemTime::now()` no chamador, não aqui — o anel não lê relógio, para o teste não depender de tempo.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test --lib core::log`
Expected: PASS (3 testes)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/log.rs src-tauri/src/core/mod.rs
git commit -m "feat: add an in-memory ring for connection, failure and slowness"
```

---

## Task 2: O pool registra o que acontece com o socket

**Files:**
- Modify: `src-tauri/src/core/conn.rs` (`Pool`, `connect`, `drop_host`)
- Test: `core/conn.rs`, `mod tests`

**Interfaces:**
- Consumes: `LogRing`, `LogEntry`, `LogKind` da Task 1.
- Produces:
  ```rust
  impl Pool {
      pub fn with_log(self, logs: Arc<LogRing>) -> Pool;
  }
  ```
  `Pool::new()` e `with_timeouts` seguem funcionando sem anel (anel próprio, descartável) — nenhum teste existente muda de assinatura.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn opening_a_connection_is_logged() {
    // Pool::new().with_log(anel), get() no contêiner de teste →
    // snapshot() tem uma entrada Connection com operation "connect",
    // host = nome do host e duration_ms preenchido.
}

#[tokio::test]
async fn a_refused_connection_is_logged_with_the_reason() {
    // host apontando para 127.0.0.1:1 → get() devolve Err, e o anel tem
    // uma entrada Connection cujo detail contém o texto de
    // ConnError::Unreachable. A senha NÃO aparece no detail.
}

#[tokio::test]
async fn dropping_a_host_is_logged() {
    // get() e depois drop_host() → entrada Connection, operation "disconnect".
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib core::conn`
Expected: FAIL — `with_log` não existe.

- [ ] **Step 3: Implement**

Campo `logs: Arc<LogRing>` no `Pool`. Registra dentro de `connect` (sucesso com duração, erro com `detail` = `Display` do `ConnError` já classificado — **nunca** a URL, que carrega a senha) e dentro de `drop_host` quando havia algo para descartar.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test --lib core::conn`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/conn.rs
git commit -m "feat: the pool records when a socket opens, fails or is dropped"
```

---

## Task 3: A fachada cronometra, registra e entrega o log

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Test: `commands.rs`, `mod tests`

**Interfaces:**
- Consumes: Tasks 1 e 2.
- Produces:
  ```rust
  // em AppState
  pub logs: Arc<LogRing>,

  pub fn logs(state: &AppState) -> Vec<LogEntry>;
  #[tauri::command] pub fn cmd_logs(state: tauri::State<'_, AppState>) -> Vec<LogEntry>;
  ```
  `AppState::open` monta o anel e passa o mesmo `Arc` ao `Pool` via `with_log`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_failed_command_leaves_a_line_in_the_log() {
    // list_dbs com host_id inexistente → Err, e logs(&state) tem uma
    // entrada Failure com operation "list_dbs" e o detail do erro.
}

#[tokio::test]
async fn a_command_that_works_fast_leaves_no_line() {
    // list_dbs no contêiner de teste → Ok, e nenhuma entrada Failure
    // nem Slow (a de Connection do pool pode existir).
}

#[test]
fn the_log_a_command_hands_over_is_newest_first() {
    // duas falhas seguidas → logs(&state)[0] é a segunda.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib commands`
Expected: FAIL — `state.logs` não existe.

- [ ] **Step 3: Implement**

Um auxiliar privado — `async fn registrar<T>(state, operation: &str, host: Option<&Host>, db: Option<u8>, fut) -> Result<T>` — que cronometra, empurra `Failure` quando o resultado é `Err` e `Slow` quando passa de `SLOW_MS`. Aplicar em `test_host`, `list_dbs`, `scan_tree`, `rescan`, `read_value`, `write_value`, `search`, `load_more_children`. `detail` é a mensagem já classificada; nada de valor de chave.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS — os 101 existentes mais os novos.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat: time every command, record what failed or dragged"
```

---

## Task 4: Conectar e desconectar de verdade

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs` (registrar os comandos)
- Test: `commands.rs`, `mod tests`

**Interfaces:**
- Consumes: Tarefa 3.
- Produces:
  ```rust
  pub async fn connect(state: &AppState, host_id: &str) -> Result<u64>; // ms do PING
  pub fn disconnect(state: &AppState, host_id: &str) -> Result<()>;
  #[tauri::command] pub async fn cmd_connect(..., host_id: String) -> Result<u64>;
  #[tauri::command] pub fn cmd_disconnect(..., host_id: String) -> Result<()>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn connect_answers_with_the_ping_time() { /* > = 0 e entrada no log */ }

#[tokio::test]
async fn disconnect_forgets_the_tree_of_every_database_of_that_host() {
    // scan_tree no banco 13, tree_level("") devolve Some;
    // disconnect → tree_level("") devolve None.
}

#[tokio::test]
async fn disconnecting_a_host_that_was_never_connected_is_not_an_error() {}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test --lib commands::tests::connect`
Expected: FAIL — `connect` não existe.

- [ ] **Step 3: Implement**

`connect` reusa `pool.ping`. `disconnect` chama `pool.drop_host` **e** remove de `state.trees` todas as entradas cuja chave começa pelo `host_id` — sem isso a árvore de um host desconectado continuaria respondendo do cache.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat: connect and disconnect a host for real"
```

---

## Task 5: A porta da tela

**Files:**
- Modify: `src/lib/api.ts`
- Test: `src/lib/__tests__/api-log.test.ts` *(novo)*

**Interfaces:**
- Consumes: Tasks 3 e 4.
- Produces:
  ```ts
  export type LogKind = "connection" | "failure" | "slow";
  export interface LogEntry {
    at_ms: number; kind: LogKind; host: string; db: number | null;
    operation: string; duration_ms: number | null; detail: string | null;
  }
  export function logs(): Promise<LogEntry[]>;
  export function connect(hostId: string): Promise<number>;
  export function disconnect(hostId: string): Promise<void>;
  ```

- [ ] **Step 1: Write the failing test**

```ts
it("fala com os comandos certos", async () => {
  // invoke mockado: logs() → "cmd_logs"; connect(id) → "cmd_connect" com
  // { hostId: id }; disconnect(id) → "cmd_disconnect".
});
```
Siga o estilo de mock de `src/lib/__tests__/approval.test.ts`.

- [ ] **Step 2: Run to verify it fails** — `npm test -- api-log` → FAIL

- [ ] **Step 3: Implement** as três funções, ao lado das existentes.

- [ ] **Step 4: Run to verify it passes** — `npm test -- api-log` → PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/api.ts src/lib/__tests__/api-log.test.ts
git commit -m "feat: open the door for logs, connect and disconnect"
```

---

## Task 6: A aba Logs

**Files:**
- Create: `src/lib/LogPanel.svelte`
- Modify: `src/lib/Workspace.svelte:65-70` (`type Aba`, `ABAS`) e `:294-303` (o corpo das abas)
- Test: `src/lib/__tests__/log-panel.test.ts` *(novo)*

**Interfaces:**
- Consumes: `logs()`, `LogEntry` da Task 5.
- Produces: `<LogPanel />`, sem props — o log é do aplicativo, não do host aberto.

- [ ] **Step 1: Write the failing tests**

```ts
it("mostra a mais recente em cima, com hora, host, operação e duração", ...);
it("mostra o motivo da falha", ...);           // detail aparece na tela
it("diz quando não há nada registrado", ...);  // texto de vazio, não tabela em branco
it("mostra hosts que não são o aberto", ...);  // o log é do aplicativo
it("avisa quando não conseguiu ler o log", ...); // logs() rejeita → mensagem, não branco
```

- [ ] **Step 2: Run to verify they fail** — `npm test -- log-panel` → FAIL

- [ ] **Step 3: Implement `LogPanel.svelte`**

Lista, não tabela — a coluna é estreita e vai ser arrastada. Uma linha por entrada: hora local (`HH:MM:SS`), host, `operação · duração`, e o detalhe embaixo quando houver. Cor do tipo em verde/âmbar/cinza, **nunca vermelho**. Lê no `onMount` e relê quando a aba é aberta.

- [ ] **Step 4: Run to verify they pass** — `npm test -- log-panel` → PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/LogPanel.svelte src/lib/Workspace.svelte src/lib/__tests__/log-panel.test.ts
git commit -m "feat: add a Logs tab that survives nothing but answers everything"
```

---

## Task 7: Estado da conexão na linha do host

**Files:**
- Modify: `src/lib/HostList.svelte` (props, `testes`, o bloco de ações `:86-99` e o resultado `:100-104`), `src/routes/+page.svelte` (`aberto`, `onSelect`)
- Test: `src/lib/__tests__/host-state.test.ts` *(novo)*

**Interfaces:**
- Consumes: `connect`, `disconnect` da Task 5.
- Produces: em `HostList`, `estados: Record<string, EstadoConexao>` com
  ```ts
  type EstadoConexao =
    | { tipo: "desligado" }
    | { tipo: "conectando" }
    | { tipo: "conectado"; ms: number }
    | { tipo: "falhou"; motivo: string };
  ```

- [ ] **Step 1: Write the failing tests**

```ts
it("começa desligado e diz isso", ...);                       // "nunca conectado"
it("mostra conectando enquanto a chamada está em voo", ...);
it("mostra conectado com o tempo em ms", ...);                // "conectado · 12 ms"
it("mostra o motivo quando falha, e continua oferecendo Conectar", ...);
it("abre a área de trabalho mesmo quando a conexão falha", ...);  // a aba Logs precisa estar alcançável
it("troca o botão para Desconectar quando conectado", ...);
it("desconectar devolve o host ao estado desligado", ...);
it("não usa vermelho nem caixa alta no estado", ...);         // só o selo de ambiente usa
```

- [ ] **Step 2: Run to verify they fail** — `npm test -- host-state` → FAIL

- [ ] **Step 3: Implement**

O botão "Conectar" passa a chamar `connect(host.id)` **antes** de `onSelect`. Falhando, mostra o motivo na linha do host **e abre o `Workspace` assim mesmo**, em estado de falha — sem varredura e sem árvore. É deliberado: a aba Logs precisa estar alcançável justamente quando a conexão falhou, e ela vive dentro do `Workspace`. Não abrir deixaria o log sobre a falha legível só se *outro* host estivesse aberto.

Review Focus 2 (retrato de varredura em voo depois de desconectar) fica por conta do desmonte: ao desconectar, `+page.svelte` zera `aberto`, o `{#key}` desmonta o `Workspace` e os ouvintes de evento morrem com ele.

O resultado do botão **Testar** continua existindo e aparece na mesma linha quando não há estado de conexão — ele responde outra pergunta ("dá para alcançar?") e sobrevive.

- [ ] **Step 4: Run to verify they pass** — `npm test -- host-state` → PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/HostList.svelte src/routes/+page.svelte src/lib/__tests__/host-state.test.ts
git commit -m "feat: say on the host row whether it is connected, and why not"
```

---

## Task 8: Divisores arrastáveis

**Files:**
- Create: `src/lib/Divisor.svelte`
- Modify: `src/routes/+page.svelte:140` e `src/lib/Workspace.svelte:344` (as duas `grid-template-columns`)
- Test: `src/lib/__tests__/divisor.test.ts` *(novo)*

**Interfaces:**
- Produces:
  ```svelte
  <!-- Divisor.svelte -->
  interface Props {
    chave: string;        // chave no localStorage: "largura:hosts" | "largura:chaves"
    inicial: number;      // px
    min?: number;         // padrão 192 (12rem)
    onLargura: (px: number) => void;
  }
  ```
  Teto: metade de `window.innerWidth`, recalculado no arrasto.

- [ ] **Step 1: Write the failing tests**

```ts
it("arrastar para a direita alarga, e avisa em px", ...);     // mousedown/mousemove/mouseup
it("não deixa encolher abaixo do mínimo", ...);
it("não deixa passar de metade da janela", ...);
it("guarda a largura e a devolve na montagem seguinte", ...);
it("ignora largura guardada inválida e cai no inicial", ...); // "abc", "-40", "99999"
it("não aplica largura guardada em janela estreita", ...);    // innerWidth < 52rem → usa o padrão empilhado
```

- [ ] **Step 2: Run to verify they fail** — `npm test -- divisor` → FAIL

- [ ] **Step 3: Implement**

`localStorage` e não SQLite: é preferência de janela, não dado do produto, e um `localStorage` perdido custa um arrasto. Ao montar, lê e **valida** (`Number.isFinite`, dentro de `[min, metade]`); qualquer outra coisa cai em `inicial`. A grade recebe `grid-template-columns: {px}px 1fr` em vez do `minmax` fixo; abaixo de `52rem` a regra de empilhamento existente continua vencendo.

Acessibilidade: a alça é `role="separator"` com `aria-orientation="vertical"` e responde às setas ← → (passo de 16 px), porque uma alça só de mouse deixa a largura fora do alcance de quem não usa mouse.

- [ ] **Step 4: Run to verify they pass** — `npm test -- divisor` → PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/Divisor.svelte src/routes/+page.svelte src/lib/Workspace.svelte src/lib/__tests__/divisor.test.ts
git commit -m "feat: let both column dividers be dragged, and remember the width"
```

---

## Task 9: Fechar os erros engolidos

**Files:**
- Modify: `src/lib/Workspace.svelte:111` (`scanStatus`) e `:130` (`listDbs`)
- Test: `src/lib/__tests__/workspace-erro.test.ts` *(novo)*

**Interfaces:**
- Consumes: nada novo. Esta é a tarefa que fecha o buraco que escondeu o host-serverless.

- [ ] **Step 1: Write the failing tests**

```ts
it("mostra o erro quando a lista de bancos falha, em vez de um seletor pelado", ...);
it("mostra o erro quando o relatório de índice falha", ...);
it("distingue banco vazio de falha ao perguntar", ...);
// banco vazio → "Nenhuma chave neste banco";
// falha → a mensagem do erro, e NÃO o texto de vazio.
```

- [ ] **Step 2: Run to verify they fail** — `npm test -- workspace-erro` → FAIL

- [ ] **Step 3: Implement**

Os dois `.catch(() => …)` passam a guardar a mensagem e mostrá-la. O estado de "não consegui perguntar" é distinto de "perguntei e não tem nada" — eram o mesmo pixel, e é essa confusão que custou a sessão inteira de investigação.

- [ ] **Step 4: Run to verify they pass** — `npm test -- workspace-erro` → PASS

- [ ] **Step 5: Commit**

```bash
git add src/lib/Workspace.svelte src/lib/__tests__/workspace-erro.test.ts
git commit -m "fix: an empty database and a failed question are no longer the same pixel"
```

---

## Task 10: Fechamento

- [ ] **Step 1:** `cd src-tauri && cargo test` → tudo passa
- [ ] **Step 2:** `cd src-tauri && cargo clippy` → nenhum aviso novo (3 pré-existentes são conhecidos)
- [ ] **Step 3:** `npm test` e `npm run check` → tudo passa, 0 erros, 0 avisos
- [ ] **Step 4:** `npm run dist && ditto dist/Suga-Redis.app /Applications/Suga-Redis.app`
- [ ] **Step 5:** Abrir o host-serverless no binário empacotado e **registrar no `docs/CONTEXTO.md`** qual das duas hipóteses está de pé: (a) há mensagem de erro ao lado do seletor de bancos — e então o anel tem a linha de falha de `list_dbs` com o motivo; ou (b) não há erro nenhum e o seletor mostra `0` chaves — e então o servidor respondeu vazio de verdade. **O log não guarda a resposta crua de um comando que deu certo**: ele registra conexão, falha e lentidão, e um comando saudável não deixa rastro (ver *Não-objetivos* do desenho). Uma aba Logs calada com a tela sem erro É a evidência da hipótese (b), não sinal de instrumento quebrado.
- [ ] **Step 6:** Atualizar `docs/CONTEXTO.md` (estado, contagem de testes, o que ficou incompleto) e commitar.
