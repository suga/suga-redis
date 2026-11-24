# Menu de índice e apagar em massa — plano de implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Botão direito num prefixo da árvore abre um menu do aplicativo cuja primeira ação apaga todas as chaves sob um ou mais prefixos, com cópia de segurança opcional e volta atrás atômica.

**Architecture:** Módulo novo `core/bulk.rs` enumera por `SCAN MATCH` (coletando `TYPE` na mesma passagem) e apaga por `UNLINK` em lotes. `core/history.rs` ganha o caminho que grava N retratos numa entrada só. `commands.rs` ganha duas portas — enumerar e apagar — e é onde a trava de produção mora. Na tela, um `ContextMenu` genérico, seleção múltipla na `KeyTree` e um diálogo próprio.

**Tech Stack:** Rust (tokio, redis-rs, rusqlite, serde), Tauri 2, Svelte 5, vitest.

**Spec:** `docs/superpowers/specs/2026-11-24-menu-de-indice-design.md`

## Global Constraints

- `KEYS`, `FLUSHALL` e `FLUSHDB` são proibidos. Apagar "tudo" é sempre enumerar e apagar o enumerado.
- Nada fala com o Redis fora de `core/`. Nada fala com o `core` a partir da tela sem passar por `commands.rs`.
- A trava de produção é decidida em `commands.rs`, nunca na tela.
- Identificadores em inglês no Rust; comentários e texto de tela em português. Nos componentes Svelte, locais seguem a convenção portuguesa do arquivo.
- Os três catálogos (`src/lib/idiomas/{pt-BR,en,es}.ts`) têm a forma do `pt-BR.ts`: chave faltando não compila.
- Vermelho `--prod` e caixa alta significam ambiente, e só.
- Mensagens de erro novas do Rust ficam em português, como as 37 que já existem.
- Nada neste repositório pode citar outro projeto, cliente ou host real.
- Esta leva **não** expõe nada ao MCP.
- Testes de interface travados em `pt-BR` (`src/tests/setup.ts`).
- Testes Rust precisam do contêiner: `./scripts/test-redis.sh up`. Bancos desta leva: **13 e 14** (os demais já têm dono).
- Tetos fixados pelo desenho: `MAX_ENUMERATED = 100_000`, `MAX_BACKUP_BYTES = 64 MiB`, `UNLINK_BATCH = 500`.

## Review Focus

Classes de entrada que o desenho implica e que o caminho feliz não exercita. Cada uma tem seu teste na tarefa dona.

1. **Prefixo que também é chave.** `wallet` existe como chave E como prefixo de `wallet:1`. `SCAN MATCH wallet*` traz as duas, e a contagem do galho já conta as duas — apagar sob `wallet` apaga `wallet` também. Pinado na Tarefa 1.
2. **Nome degenerado.** Chave `:`, chave terminada em `:`, `::` no meio, chave vazia. A enumeração não pode perder nenhuma nem inventar prefixo. Pinado na Tarefa 1.
3. **Chave que expira sozinha entre enumerar e apagar.** O `UNLINK` devolve menos que o pedido; a contagem gravada é a real, não a pedida. Pinado na Tarefa 2.
4. **Valor binário no retrato.** `Bytes` carrega `base64`; uma cópia que estraga byte não-UTF8 restaura lixo. Pinado na Tarefa 4.
5. **Conexão cai no meio do apagar.** O que saiu, saiu: o erro carrega o parcial e a entrada de histórico registra o apagado de verdade. Pinado na Tarefa 2 (a falha), reforçado na Tarefa 6 (a entrada).

---

## Estrutura de arquivos

| Arquivo | Responsabilidade |
|:-|:-|
| `src-tauri/src/core/bulk.rs` (criar) | Enumerar por prefixo e apagar por `UNLINK` em lotes |
| `src-tauri/src/core/mod.rs` (modificar) | Registrar `bulk` |
| `src-tauri/src/core/value.rs` (modificar) | `WriteOp::DeletePrefix`, `BackupKind` |
| `src-tauri/src/core/history.rs` (modificar) | Entrada com N retratos, cópia em arquivo, volta atrás com relatório |
| `src-tauri/src/commands.rs` (modificar) | `enumerate_prefix`, `delete_prefix`, trava de produção, esquecer caches |
| `src/lib/api.ts` (modificar) | Tipos e chamadas novas |
| `src/lib/ContextMenu.svelte` (criar) | Menu genérico: posição, teclado, fechar |
| `src/lib/DeletePrefixDialog.svelte` (criar) | Contagem conferida, escolha da cópia, datilografia em prod, progresso |
| `src/lib/KeyTree.svelte` (modificar) | `contextmenu`, seleção múltipla |
| `src/lib/Workspace.svelte` (modificar) | Fio entre menu, diálogo e `api.ts` |
| `src/lib/HistoryPanel.svelte` (modificar) | Linha sem cópia, relatório de volta atrás parcial |
| `src/lib/idiomas/*.ts` (modificar) | ~25 chaves nos três |

---

### Task 1: Enumerar um prefixo

**Files:**
- Create: `src-tauri/src/core/bulk.rs`
- Modify: `src-tauri/src/core/mod.rs`
- Test: `src-tauri/src/core/bulk.rs` (`#[cfg(test)] mod tests`, banco 13)

**Interfaces:**
- Consumes: `core::conn::Pool`, `core::keys::{Alvo, ScanTick, escapar_glob, SCAN_COUNT}` (tornar `escapar_glob` e `SCAN_COUNT` `pub(crate)`), `core::keys::KeyKind`.
- Produces:
```rust
pub const MAX_ENUMERATED: usize = 100_000;

pub struct Enumeration {
    pub keys: Vec<String>,
    /// Quantas de cada tipo, para o diálogo avisar o que não volta atrás.
    pub kinds: Vec<(KeyKind, u64)>,
    /// Parou no teto: há mais do que isto sob o prefixo.
    pub truncated: bool,
}

pub async fn enumerate(
    pool: &Pool,
    alvo: Alvo<'_>,
    prefixes: &[String],
    progress: impl Fn(ScanTick),
) -> Result<Enumeration, ConnError>;
```

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn enumerates_only_the_prefix()        // cria a:1,a:2,b:1 → keys == [a:1,a:2]
#[tokio::test] async fn the_prefix_itself_is_included()     // Review Focus 1: cria wallet e wallet:1, prefixo "wallet" → ambas
#[tokio::test] async fn wildcard_in_prefix_does_not_leak()  // cria lit*:1 e outro:1, prefixo "lit*" → só lit*:1
#[tokio::test] async fn degenerate_names_survive()          // Review Focus 2: "", ":", "a:", "::b" sob prefixo "" → todas
#[tokio::test] async fn empty_prefix_takes_the_whole_db()   // prefixes == [] ou [""] → todas as chaves do banco
#[tokio::test] async fn several_prefixes_scan_once()        // prefixos a: e c: → chaves dos dois, nenhuma de b:
#[tokio::test] async fn kinds_are_counted()                 // um hash e duas strings → kinds soma 1 Hash, 2 String
#[tokio::test] async fn stops_at_the_ceiling()              // com MAX_ENUMERATED baixado no teste: truncated == true e keys.len() == teto
```

Use o auxiliar `com_chaves` de `core::keys::tests` como modelo (cria, roda, apaga — mesmo se a asserção falhar). O teto vira parâmetro de uma função interna `enumerate_com_teto(.., teto: usize)` para o teste não criar 100.000 chaves; `enumerate` chama com `MAX_ENUMERATED`.

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test core::bulk`
Expected: FAIL — `unresolved import crate::core::bulk`

- [ ] **Step 3: Implementar `enumerate` em `src-tauri/src/core/bulk.rs`**

Uma volta de `SCAN` com `MATCH <escapar_glob(prefixo)>*` quando há **um** prefixo; com vários, `SCAN` sem `MATCH` filtrando por `starts_with` contra todos — o `MATCH` não encurta o trabalho do servidor, então uma varredura filtrada em Rust custa o mesmo que uma filtrada no servidor e evita N varreduras. O `TYPE` de cada lote vai num `pipeline` sobre os nomes que passaram no filtro. `progress` recebe `ScanTick { scanned, total_estimate }` por lote, com `total_estimate` do `DBSIZE` lido antes do laço.

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test core::bulk`
Expected: PASS (8 testes)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/bulk.rs src-tauri/src/core/mod.rs src-tauri/src/core/keys.rs
git commit -m "feat: enumerate every key under one or more prefixes"
```

---

### Task 2: Apagar o enumerado

**Files:**
- Modify: `src-tauri/src/core/bulk.rs`
- Test: mesmo módulo, banco 13

**Interfaces:**
- Consumes: Tarefa 1.
- Produces:
```rust
pub const UNLINK_BATCH: usize = 500;

/// Erro que CARREGA o parcial: o que já saiu, saiu, e quem grava o
/// histórico precisa do número de verdade mesmo quando a conexão morre.
pub struct BulkError { pub deleted: u64, pub source: ConnError }

/// Devolve quantas chaves o servidor disse ter apagado — que pode ser
/// menos que `keys.len()` se alguma expirou sozinha no caminho.
pub async fn unlink_keys(
    pool: &Pool,
    alvo: Alvo<'_>,
    keys: &[String],
    cancel: &AtomicBool,
    progress: impl Fn(u64),
) -> Result<u64, BulkError>;

/// O laço de lotes, sem Redis: o `apagar` é a costura. `unlink_keys` passa
/// o pipeline de `UNLINK`; os testes passam um fechamento. É o que torna
/// o lote e a falha no meio testáveis sem contêiner e sem 1200 chaves.
pub(crate) async fn unlink_em_lotes<F, Fut>(
    keys: &[String], lote: usize, cancel: &AtomicBool,
    progress: impl Fn(u64), apagar: F,
) -> Result<u64, BulkError>
where F: Fn(&[String]) -> Fut, Fut: Future<Output = Result<u64, ConnError>>;
```

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn deletes_what_it_was_given()      // 3 chaves → devolve 3, DBSIZE cai 3
#[tokio::test] async fn leaves_the_rest_alone()          // apaga a:*, b:1 continua lá
#[tokio::test] async fn counts_what_actually_went()      // Review Focus 3: uma chave da lista já não existe → devolve n-1
#[tokio::test] async fn cancel_stops_and_reports()       // cancel marcado após o 1º lote → devolve o parcial, o resto continua no banco
#[tokio::test] async fn batches_are_bounded()            // 1200 nomes, lote 500 → o fechamento recebe 500, 500, 200
#[tokio::test] async fn a_failed_batch_carries_the_partial()  // Review Focus 5: fechamento acerta o 1º lote e falha o 2º → BulkError.deleted == 500
```

Os três últimos usam `unlink_em_lotes` com um fechamento, sem tocar no Redis:
é a costura que torna lote e falha-no-meio testáveis. Os dois primeiros usam
`unlink_keys` contra o contêiner.

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test core::bulk::tests::deletes_what_it_was_given`
Expected: FAIL — `cannot find function unlink_keys`

- [ ] **Step 3: Implementar `unlink_keys`**

`unlink_em_lotes` fatia, confere `cancel` **entre** lotes (nunca dentro: o lote corrente termina), soma o que o fechamento devolve, e em erro devolve `BulkError` com a soma até ali. `unlink_keys` passa um fechamento que manda `UNLINK` por `pipeline` — não `DEL`: `DEL` libera a memória na mesma linha de execução e um lote de valores grandes prende o servidor.

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test core::bulk`
Expected: PASS (13 testes)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/bulk.rs
git commit -m "feat: unlink enumerated keys in bounded batches"
```

---

### Task 3: A operação no vocabulário de escrita

**Files:**
- Modify: `src-tauri/src/core/value.rs:594-629` (`WriteOp`), e `op_summary`
- Test: `src-tauri/src/core/value.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "backup", rename_all = "snake_case")]
pub enum BackupKind {
    Inline,
    File { path: String },
    None,
}

// nova variante de WriteOp
DeletePrefix {
    prefixes: Vec<String>,
    /// Quantas saíram de verdade (cancelar faz isto ser menor).
    deleted: u64,
    /// Quantas a varredura achou quando a pessoa confirmou.
    enumerated: u64,
    backup: BackupKind,
},
```

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[test] fn delete_prefix_round_trips_through_serde()
#[test] fn old_op_json_still_deserializes()   // uma string {"op":"delete_key"} gravada antes desta variante
#[test] fn summary_names_count_and_prefix()   // op_summary → contém "1395" e "wallet:"
#[test] fn summary_says_when_there_is_no_backup()  // BackupKind::None → a frase diz "sem cópia"
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test core::value::tests::delete_prefix`
Expected: FAIL — `no variant named DeletePrefix`

- [ ] **Step 3: Implementar a variante e o braço de `op_summary`**

A frase é o registro do que foi feito e sai em português, como as demais. Com vários prefixos: o primeiro e "e mais N".

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test core::value`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/value.rs
git commit -m "feat: add the bulk prefix delete to the write vocabulary"
```

---

### Task 4: Uma entrada de histórico com N retratos

**Files:**
- Modify: `src-tauri/src/core/history.rs`
- Test: mesmo módulo, banco 14

**Interfaces:**
- Consumes: Tarefas 1–3, `History::insert_entry`, `snapshot_from_view`, `read_value`, `FULL_READ_LIMIT`.
- Produces:
```rust
pub const MAX_BACKUP_BYTES: usize = 64 * 1024 * 1024;

pub enum BackupChoice<'a> {
    Inline,
    /// Diretório onde gravar; o nome do arquivo é montado aqui.
    File(&'a Path),
    None,
}

pub struct BulkDelete<'a> {
    pub prefixes: &'a [String],
    pub keys: &'a [String],
    pub backup: BackupChoice<'a>,
    pub cancel: &'a AtomicBool,
}

pub struct BulkOutcome {
    pub entry_id: i64,
    pub deleted: u64,
}

impl History {
    pub async fn delete_prefix_with_backup(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        db: u8,
        plan: BulkDelete<'_>,
        progress: impl Fn(u64),
    ) -> Result<BulkOutcome>;
}
```
`HistoryError` ganha `BackupTooLarge { bytes: usize }` e `BackupIo(std::io::Error)`.

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn inline_backup_holds_every_snapshot()  // 3 chaves → before_json é vetor de 3
#[tokio::test] async fn binary_values_survive_the_backup()    // Review Focus 4: valor não-UTF8 → base64 preenchido, texto não estragado
#[tokio::test] async fn no_backup_leaves_before_json_null()   // BackupChoice::None → before_json None, op.backup == None
#[tokio::test] async fn ceiling_refuses_before_deleting()     // retratos acima do teto com Inline → Err(BackupTooLarge) e as chaves CONTINUAM no banco
#[tokio::test] async fn file_backup_writes_and_points()       // File → arquivo existe, op.backup == File{path}, before_json None
#[tokio::test] async fn entry_records_what_actually_went()    // cancel no meio → deleted < enumerated na op gravada
```

O nome do arquivo é `AAAA-MM-DD-hhmmss-<host>-db<N>-<prefixo>.json`, com o prefixo saneado para nome de arquivo.

Como na Tarefa 1, o teto entra por parâmetro numa função interna
(`delete_prefix_com_teto(.., teto_bytes: usize)`) e a pública chama com
`MAX_BACKUP_BYTES` — senão o teste do teto teria que criar 64 MiB de valor.

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test core::history::tests::inline_backup`
Expected: FAIL — `no method named delete_prefix_with_backup`

- [ ] **Step 3: Implementar `delete_prefix_with_backup`**

`BulkError` do `unlink` é gravado antes de subir: a linha fica com o `deleted` parcial, e só então o erro propaga. Ordem que o teste do teto exige: lê os retratos → **confere o teto** → grava a linha → `bulk::unlink_keys` → atualiza a linha com o `deleted` real. Se o `unlink` falhar inteiro, a linha recém-gravada é removida, como `write_with_backup` já faz. `BackupChoice::None` pula a leitura dos retratos — é o que torna o caminho sem cópia rápido.

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test core::history`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/history.rs
git commit -m "feat: record a bulk delete as one history entry"
```

---

### Task 5: Voltar atrás de uma apagada em massa

**Files:**
- Modify: `src-tauri/src/core/history.rs` (`rollback`), `src-tauri/src/core/value.rs` (`WriteOutcome`)
- Test: `src-tauri/src/core/history.rs`, banco 14

**Interfaces:**
- Produces:
```rust
pub struct FailedKey { pub key: String, pub reason: String }

// nova variante de WriteOutcome
Restored { restored: u64, failed: Vec<FailedKey> },
```
`History::rollback` passa a despachar pela `op` da entrada: `DeletePrefix` restaura o vetor; tudo mais segue o caminho de hoje.

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn rollback_restores_every_key()          // valor E TTL de volta
#[tokio::test] async fn rollback_restores_from_the_file()      // cópia em arquivo restaura o mesmo que a embutida
#[tokio::test] async fn rollback_without_backup_is_refused()   // BackupKind::None → Err com mensagem clara, nada escrito
#[tokio::test] async fn partial_rollback_does_not_mark_done()  // uma chave de tipo sem suporte → rolled_back continua false e failed a nomeia
#[tokio::test] async fn rollback_reports_what_it_restored()    // restored == 2, failed.len() == 1
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test core::history::tests::rollback_restores_every_key`
Expected: FAIL — `no variant named Restored`

- [ ] **Step 3: Implementar o despacho e a restauração**

Melhor esforço por chave, na ordem gravada; coleta falha em `failed` e segue. `rolled_back` só é marcado quando `failed` está vazio — um "desfeito" que desfez 900 de 1.395 é mentira.

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test core::history`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/core/history.rs src-tauri/src/core/value.rs
git commit -m "feat: roll a bulk delete back, reporting what did not return"
```

---

### Task 6: As duas portas da fachada

**Files:**
- Modify: `src-tauri/src/commands.rs` (`AppState`, funções novas, gêmeas `#[tauri::command]`, `invoke_handler`)
- Test: `src-tauri/src/commands.rs` (`mod tests`), banco 13

**Interfaces:**
- Consumes: Tarefas 1–5, `needs_confirmation`, `forget_key_in_tree`, `Index::forget_key`, `registrar`.
- Produces:
```rust
pub struct EnumerationHandle {
    pub id: String,
    pub count: u64,
    pub kinds: Vec<(KeyKind, u64)>,
    pub truncated: bool,
    /// Quantas são de tipo sem volta atrás (set, stream, RedisJSON).
    pub unrestorable: u64,
}

pub enum BackupRequest { Inline, File, None }

pub async fn enumerate_prefix(
    state: &AppState, host_id: &str, db: u8, prefixes: Vec<String>,
) -> Result<EnumerationHandle>;

/// `typed_host_name` é o nome digitado no diálogo. Em host `prod` ele tem
/// que bater EXATAMENTE com `host.name`; fora de produção é ignorado.
pub async fn delete_prefix(
    state: &AppState, enumeration_id: &str,
    backup: BackupRequest, typed_host_name: Option<&str>,
) -> Result<WriteOutcome>;
```
`AppState` ganha `pub enumerations: Mutex<HashMap<String, StoredEnumeration>>`, onde `StoredEnumeration` guarda `host_id`, `db`, `prefixes` e as chaves. Eventos novos: `bulk-scan-progress` e `bulk-delete-progress`.

- [ ] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn prod_without_the_typed_name_deletes_nothing()  // devolve NeedsConfirmation, DBSIZE intacto
#[tokio::test] async fn prod_with_the_wrong_name_deletes_nothing()     // "redis de teste" ≠ "Redis de teste"
#[tokio::test] async fn prod_with_the_exact_name_deletes()
#[tokio::test] async fn dev_does_not_need_a_typed_name()
#[tokio::test] async fn deleting_forgets_the_tree_and_the_index()
#[tokio::test] async fn a_key_created_after_enumeration_survives()     // cria c:9 entre as duas chamadas → continua lá
#[tokio::test] async fn a_dead_connection_still_records_what_went()    // Review Focus 5: erro no 2º lote → entrada existe com o parcial, erro sobe
#[tokio::test] async fn an_unknown_enumeration_id_is_an_error()
#[tokio::test] async fn rolling_a_prod_bulk_back_needs_confirmation()  // ambiente da ENTRADA, não do host na tela
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `cd src-tauri && cargo test commands::tests::prod_without_the_typed_name`
Expected: FAIL — `cannot find function delete_prefix`

- [ ] **Step 3: Implementar as duas funções, as gêmeas `cmd_` e o registro**

`enumerate_prefix` guarda o resultado sob um id e devolve o resumo — a lista de chaves **não** atravessa a fronteira do Tauri. `delete_prefix` consome o id (tira do mapa), aplica a trava e chama `History::delete_prefix_with_backup`. Cada chave apagada sai da trie (`forget_key_in_tree`) e do índice (`index.forget_key`), como `write_value` já faz com `DeleteKey`. Ambas passam por `registrar`, como todas as operações da fachada.

**Não registrar nada disto no MCP.**

- [ ] **Step 4: Rodar e ver passar**

Run: `cd src-tauri && cargo test && cargo clippy`
Expected: PASS; clippy sem avisos novos (os 3 pré-existentes continuam)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat: expose enumerate and bulk delete behind the production lock"
```

---

### Task 7: A porta da tela

**Files:**
- Modify: `src/lib/api.ts`
- Test: `src/lib/__tests__/api-bulk.test.ts` (criar)

**Interfaces:**
- Produces:
```ts
export interface EnumerationHandle {
  id: string; count: number; kinds: [KeyKind, number][];
  truncated: boolean; unrestorable: number;
}
export type BackupRequest = "inline" | "file" | "none";
export interface FailedKey { key: string; reason: string }
// WriteOutcome ganha: | { outcome: "restored"; restored: number; failed: FailedKey[] }

export function enumeratePrefix(hostId: string, db: number, prefixes: string[]): Promise<EnumerationHandle>;
export function deletePrefix(enumerationId: string, backup: BackupRequest, typedHostName: string | null): Promise<WriteOutcome>;
export function onBulkScanProgress(cb: (t: ScanTick) => void): Promise<() => void>;
export function onBulkDeleteProgress(cb: (deleted: number) => void): Promise<() => void>;
```

- [ ] **Step 1: Escrever o teste que falha**

```ts
it("enumeratePrefix chama cmd_enumerate_prefix com host, banco e prefixos")
it("deletePrefix manda o nome digitado quando há um, e null quando não há")
```
Use o mesmo mock de `invoke` dos testes existentes (ver `api-log.test.ts`).

- [ ] **Step 2: Rodar e ver falhar**

Run: `npm test -- api-bulk`
Expected: FAIL — `enumeratePrefix is not a function`

- [ ] **Step 3: Implementar as quatro funções e os tipos**

- [ ] **Step 4: Rodar e ver passar**

Run: `npm test -- api-bulk && npm run check`
Expected: PASS; svelte-check 0 erros

- [ ] **Step 5: Commit**

```bash
git add src/lib/api.ts src/lib/__tests__/api-bulk.test.ts
git commit -m "feat: add the bulk delete calls to the single door"
```

---

### Task 8: O menu

**Files:**
- Create: `src/lib/ContextMenu.svelte`
- Test: `src/lib/__tests__/context-menu.test.ts` (criar)

**Interfaces:**
- Produces:
```ts
interface Item { id: string; rotulo: string; perigoso?: boolean }
interface Props {
  x: number; y: number; itens: Item[];
  onEscolher: (id: string) => void;
  onFechar: () => void;
}
```

- [ ] **Step 1: Escrever os testes que falham**

```ts
it("mostra um botão por item")
it("Escape fecha")
it("clique fora fecha sem escolher")
it("setas andam entre os itens e Enter escolhe")
it("vira para cima quando não cabe abaixo do cursor")
it("devolve o foco ao elemento que o abriu")
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `npm test -- context-menu`
Expected: FAIL — arquivo não existe

- [ ] **Step 3: Implementar `ContextMenu.svelte`**

`role="menu"`, itens `role="menuitem"`. Posição por `style` a partir de `x`/`y`, virando quando `y + altura > innerHeight` (e espelhado no eixo x). Visual só com os tokens existentes (`--panel-dark`, `--rule-strong`, `--text-faint`); `perigoso` não usa `--prod`.

- [ ] **Step 4: Rodar e ver passar**

Run: `npm test -- context-menu`
Expected: PASS (6 testes)

- [ ] **Step 5: Commit**

```bash
git add src/lib/ContextMenu.svelte src/lib/__tests__/context-menu.test.ts
git commit -m "feat: an app context menu with keyboard access"
```

---

### Task 9: Seleção múltipla na árvore

**Files:**
- Modify: `src/lib/KeyTree.svelte`
- Test: `src/lib/__tests__/tree-selection.test.ts` (criar)

**Interfaces:**
- Consumes: Tarefa 8.
- Produces: duas props novas em `KeyTree` — `onMenuPrefixo: (prefixos: string[], x: number, y: number) => void` e `onMenuChave: (chave: string, x: number, y: number) => void`.

- [ ] **Step 1: Escrever os testes que falham**

```ts
it("contextmenu numa linha de prefixo chama preventDefault e onMenuPrefixo")
it("Shift+F10 na linha com foco abre o menu ancorado nela")
it("contextmenu numa linha de chave chama onMenuChave")
it("⌘+clique junta prefixos à seleção")
it("⌘+clique numa folha não junta nada")
it("Shift+clique pega a faixa")
it("⌘+A seleciona os prefixos de primeiro nível")
it("contextmenu numa linha JÁ selecionada manda a seleção inteira")
it("contextmenu numa linha fora da seleção limpa a seleção e manda só ela")
it("a marca de selecionado não usa a classe de produção")
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `npm test -- tree-selection`
Expected: FAIL — `onMenuPrefixo is not a function`

- [ ] **Step 3: Implementar a seleção e o `oncontextmenu`**

`SvelteSet<string>` de prefixos, mais o índice da última linha clicada para a faixa. A seleção vive junto de `abertos` e é limpa pelo mesmo efeito que troca de host/banco — e, como ele, de forma **idempotente**.

- [ ] **Step 4: Rodar e ver passar**

Run: `npm test -- tree-selection key-level`
Expected: PASS; `key-level.test.ts` segue passando

- [ ] **Step 5: Commit**

```bash
git add src/lib/KeyTree.svelte src/lib/__tests__/tree-selection.test.ts
git commit -m "feat: select several prefixes and open a menu on them"
```

---

### Task 10: O diálogo

**Files:**
- Create: `src/lib/DeletePrefixDialog.svelte`
- Test: `src/lib/__tests__/delete-prefix.test.ts` (criar)

**Interfaces:**
- Consumes: Tarefa 7 (`EnumerationHandle`, `BackupRequest`).
- Produces:
```ts
interface Props {
  host: Host; db: number; prefixos: string[];
  enumeracao: EnumerationHandle | null;   // null = ainda varrendo
  progresso: { scanned: number; total: number } | null;
  apagados: number | null;                // não-nulo = apagando
  onConfirmar: (backup: BackupRequest, nomeDigitado: string | null) => void;
  onCancelar: () => void;
}
```

- [ ] **Step 1: Escrever os testes que falham**

```ts
it("mostra a contagem da enumeração, não a da árvore")
it("a opção de cópia vem marcada")
it("desmarcar a cópia troca o texto do botão e acende o irreversível")
it("avisa quantas são de tipo sem volta atrás, mesmo com a cópia marcada")
it("em prod o botão fica travado até o nome do host bater exatamente")
it("fora de prod não pede nome nenhum")
it("acima do teto oferece gravar em arquivo e não oferece a cópia embutida")
it("mostra o carimbo de ambiente e o banco")
it("enquanto varre, mostra o progresso e nenhum botão de confirmar")
it("cancelar durante o apagar chama onCancelar")
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `npm test -- delete-prefix`
Expected: FAIL — arquivo não existe

- [ ] **Step 3: Implementar `DeletePrefixDialog.svelte`**

`role="dialog" aria-modal="true"`, foco inicial em Cancelar (a saída segura é a que já está sob o dedo), `Escape` cancela — as três coisas como o `ConfirmDialog` já faz. Lista até cinco prefixos e "e mais N". **O `ConfirmDialog.svelte` não muda:** este é um irmão dele, não uma alteração.

- [ ] **Step 4: Rodar e ver passar**

Run: `npm test -- delete-prefix`
Expected: PASS (10 testes)

- [ ] **Step 5: Commit**

```bash
git add src/lib/DeletePrefixDialog.svelte src/lib/__tests__/delete-prefix.test.ts
git commit -m "feat: the dialog that gates a bulk delete"
```

---

### Task 11: O fio da ponta a ponta

**Files:**
- Modify: `src/lib/Workspace.svelte`, `src/lib/HistoryPanel.svelte`, `src/lib/idiomas/{pt-BR,en,es}.ts`
- Test: `src/lib/__tests__/workspace-apagar.test.ts` (criar); estender `history-panel.test.ts`, `i18n.test.ts`

**Interfaces:**
- Consumes: Tarefas 7–10.

- [ ] **Step 1: Escrever os testes que falham**

```ts
// workspace-apagar.test.ts
it("escolher o item do menu chama enumeratePrefix e abre o diálogo")
it("confirmar chama deletePrefix com a escolha de cópia")
it("needs_confirmation reabre o diálogo pedindo o nome digitado")
it("uma falha ao enumerar aparece na tela, não vira banco vazio")
it("BackupTooLarge reabre o diálogo oferecendo gravar em arquivo")
it("terminado o apagar, a árvore é relida")
it("o menu de prefixo traz Apagar tudo e Copiar prefixo, e nada cinza")
it("Copiar prefixo põe o prefixo na área de transferência")
it("o menu de chave traz Copiar nome e Apagar chave")
it("Apagar chave usa o writeValue que já existe, não a porta nova")
// history-panel.test.ts
it("a entrada sem cópia não oferece voltar atrás e diz por quê")
it("uma volta atrás parcial mostra quais chaves não voltaram")
// i18n.test.ts
it("os três catálogos têm as chaves novas")
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `npm test -- workspace-apagar history-panel i18n`
Expected: FAIL

- [ ] **Step 3: Ligar os pedaços e traduzir**

A lista de itens do menu é montada aqui, não dentro do `ContextMenu` — ele é genérico. Itens de busca e estatística **não entram desabilitados**: item cinza que nunca acende ensina a ignorar o menu. `Workspace` hospeda `ContextMenu` e `DeletePrefixDialog`, assina os dois eventos de progresso e desassina ao fechar. As ~25 chaves entram em `pt-BR.ts` primeiro (é a base) e depois em `en.ts` e `es.ts` — faltar uma não compila. Erro de enumeração ganha estado próprio e visível, como `erroBancos` e `erroIndice`: **nenhum `catch` silencioso**.

- [ ] **Step 4: Rodar e ver passar**

Run: `npm test && npm run check`
Expected: PASS (179 + os novos); svelte-check 0 erros, 0 avisos

- [ ] **Step 5: Commit**

```bash
git add src/lib/Workspace.svelte src/lib/HistoryPanel.svelte src/lib/idiomas src/lib/__tests__
git commit -m "feat: wire the index menu from right click to history"
```

---

### Task 12: Registrar o que custou caro

**Files:**
- Modify: `redis-desk/docs/CONTEXTO.md`

- [ ] **Step 1: Atualizar o documento de retomada**

Quatro coisas, cada uma em uma ou duas linhas: os bancos **13 e 14** na lista de bancos de teste; `core/bulk.rs` no mapa da arquitetura; a invariante nova — *apagar em massa é sempre enumerar e apagar o enumerado, nunca `FLUSHDB`; a contagem mostrada é a contagem apagada*; e a tensão registrada no desenho sobre datilografar o nome do host em produção, com o porquê.

Risque do item 5 de *Onde está incompleto* o que esta leva entregou, e deixe escrito que **busca no galho e estatística E2 continuam por fazer**, apontando para o desenho.

- [ ] **Step 2: Conferir**

Run: `cd src-tauri && cargo test && cargo clippy` e `npm test && npm run check`
Expected: tudo passando; clippy só com os 3 avisos pré-existentes

- [ ] **Step 3: Commit**

```bash
git add docs/CONTEXTO.md
git commit -m "docs: record the bulk delete invariant and the test databases"
```
