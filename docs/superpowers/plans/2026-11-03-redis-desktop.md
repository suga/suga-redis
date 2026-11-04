# Cliente de mesa para Redis — plano de implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** App de mesa em Tauri que cadastra hosts Redis marcados por ambiente, mostra as chaves em árvore, busca texto dentro dos valores em menos de um segundo, edita/cria/apaga com cópia de segurança e volta atrás, e expõe tudo a um servidor MCP para o Claude local.

**Architecture:** Um binário só. Sem argumento, sobe a janela Tauri; com `--mcp`, roda como servidor MCP stdio. Os dois modos compartilham o mesmo módulo `core` (hosts, conexão, valores, índice, histórico). Leitura o modo MCP faz sozinho, conectando no Redis direto. Escrita o modo MCP não faz: ele pede ao app aberto por um socket unix, e o app só libera depois de o dono digitar o nome do host na tela.

**Tech Stack:** Rust + Tauri 2 (`tauri = "2"`, núcleo 2.12.x), `redis = "1"` (features `tokio-comp`, `tls-rustls-webpki-roots`), `rusqlite` com feature `bundled` (traz FTS5), `keyring = "3"`, `rmcp = "3"` (SDK oficial de MCP em Rust), `tokio`, `serde`. Interface em Vite + TypeScript + Svelte 5.

**Spec:** `docs/superpowers/specs/2026-11-03-redis-desktop-design.md`

## Global Constraints

- Mac primeiro, linux do mesmo código. O código não pode depender de API só de mac fora do empacotamento.
- `KEYS`, `FLUSHALL`, `FLUSHDB` e qualquer comando bloqueante são proibidos. Varredura só com `SCAN` / `HSCAN` / `SSCAN` / `ZSCAN`.
- Tipos editáveis: `string`, `hash`, `list`, `zset`. `set`, `stream` e RedisJSON são só leitura.
- Escrita pela tela em host de ambiente `prod`: um diálogo de confirmar, mostrando host, chave e o que muda. Um clique. Nunca digitar frase.
- Escrita pedida pela LLM, em qualquer ambiente: diálogo na janela do app onde o dono **digita o nome do host** para liberar. Pedido sem resposta expira em 120 segundos.
- O índice guarda o valor em claro, sem cifrar. A LLM recebe valor em claro, sem máscara. Decisão do dono — não adicionar cifragem nem mascaramento.
- Busca atua em um host por vez: o que estiver aberto.
- Histórico guarda 30 dias ou 500 entradas por host, o que vier primeiro, configurável.
- Senha de host no chaveiro do sistema (`keyring`), nunca em arquivo.
- Alvo de desempenho da busca: menos de 1 segundo para 5.000 chaves indexadas.
- Identificadores em inglês; comentários e todo texto que aparece na tela em português do Brasil.
- Tokens visuais fixos: fundo `#22262B`, painel `#262B31`, painel escuro `#1C2024`, texto `#DDE3E8`, ação âmbar `#F2A33C`, prod `#8E2B24`, homolog `#C98A20`, dev `#4E7A4A`. Tipografia: Barlow Semi Condensed para interface, Azeret Mono para chave e valor.

## Review Focus

Cinco situações que a spec implica, que nenhuma tarefa exercitaria por conta própria, e onde o app quebrar machuca o dono. Cada uma tem o teste colocado na tarefa que manda no código:

1. **Host fora do ar ou VPN caída no meio da operação** — toda chamada devolve erro tratado com o nome do host, a janela não trava e o que estava sendo editado não some. *(teste na Task 2)*
2. **Valor enorme, de megabytes** — o painel mostra um pedaço e oferece o resto sob pedido; o índice guarda no máximo 64 KB por valor e marca que truncou. *(testes nas Tasks 4 e 7)*
3. **Chave morre entre a varredura e o clique** — resultado de busca e abertura de chave que não existe mais dizem "essa chave não existe mais" e tiram a chave da árvore, em vez de erro cru. *(teste na Task 8)*
4. **Valor que não é UTF-8** — exibir, buscar e salvar não corrompem o conteúdo nem derrubam o app. *(testes nas Tasks 4 e 7)*
5. **Nome de chave degenerado** — chave vazia de prefixo, terminada em `:`, com `::` no meio ou com `:` sozinho: a árvore não perde chave nem cria galho fantasma. *(teste na Task 3)*

---

## File Structure

```
redis-desk/
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ tauri.conf.json
│  └─ src/
│     ├─ main.rs            # escolhe modo GUI ou --mcp
│     ├─ lib.rs             # monta o app Tauri, registra comandos
│     ├─ commands.rs        # #[tauri::command] — fachada para a interface
│     ├─ core/
│     │  ├─ mod.rs
│     │  ├─ config.rs       # Host, HostStore, senha no chaveiro
│     │  ├─ conn.rs         # pool de conexão por host, erros
│     │  ├─ keys.rs         # SCAN, metadados, árvore por prefixo
│     │  ├─ value.rs        # ler e escrever valor por tipo
│     │  ├─ index.rs        # SQLite FTS5, varredura incremental
│     │  ├─ search.rs       # consulta, conferência, trechos
│     │  └─ history.rs      # cópia de segurança, histórico, volta atrás
│     ├─ approval/
│     │  ├─ mod.rs          # protocolo do pedido de escrita
│     │  ├─ server.rs       # socket unix dentro da GUI
│     │  └─ client.rs       # lado do processo MCP
│     └─ mcp/
│        ├─ mod.rs          # servidor stdio
│        └─ tools.rs        # ferramentas expostas ao Claude
├─ src/
│  ├─ main.ts
│  ├─ App.svelte
│  ├─ app.css               # tokens visuais
│  └─ lib/
│     ├─ api.ts             # tipos + invoke
│     ├─ HostList.svelte
│     ├─ KeyTree.svelte
│     ├─ ValuePanel.svelte
│     ├─ editors/{StringEditor,HashEditor,ListEditor,ZSetEditor,ReadOnlyView}.svelte
│     ├─ SearchPanel.svelte
│     ├─ HistoryPanel.svelte
│     ├─ ConfirmDialog.svelte
│     └─ ApprovalDialog.svelte
└─ scripts/test-redis.sh    # sobe/derruba Redis de teste em docker
```

---

### Task 1: Esqueleto do projeto e cadastro de hosts

**Files:**
- Create: `redis-desk/` (scaffold `create-tauri-app`, template Svelte + TypeScript)
- Create: `redis-desk/src-tauri/src/core/config.rs`
- Create: `redis-desk/scripts/test-redis.sh`
- Create: `.gitignore`
- Test: `redis-desk/src-tauri/src/core/config.rs` (módulo `#[cfg(test)]`)

**Interfaces:**
- Consumes: nada.
- Produces:
  - `enum Env { Prod, Homolog, Dev }` (serde: `"prod" | "homolog" | "dev"`)
  - `struct Host { id: String, name: String, addr: String, port: u16, db: u8, username: Option<String>, tls: bool, env: Env }`
  - `struct HostStore` com `HostStore::open(dir: &Path) -> Result<HostStore>`, `list(&self) -> Vec<Host>`, `add(&mut self, host: Host, password: Option<&str>) -> Result<String>`, `update(&mut self, host: Host, password: Option<&str>) -> Result<()>`, `remove(&mut self, id: &str) -> Result<()>`, `password(&self, id: &str) -> Result<Option<String>>`

Hosts em `hosts.json` no diretório de dados do app; senha no chaveiro sob serviço `redis-desk` e conta `<host id>`. `id` é um UUID v4 gerado no `add`.

- [x] **Step 1: Instalar o toolchain e criar o scaffold**

Rust não está instalado nesta máquina. Rodar:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
cd ~/programs/redis
npm create tauri-app@latest redis-desk -- --template svelte-ts --manager npm --yes
cd redis-desk && npm install && git init
```

Acrescentar ao `.gitignore` da raiz: `.superpowers/`, `node_modules/`, `target/`, `dist/`.

- [x] **Step 2: Escrever os testes que falham**

Em `core/config.rs`, com `keyring::set_default_credential_builder(keyring::mock::default_credential_builder())` no começo de cada teste e um `tempfile::TempDir` como diretório:

```rust
#[test] fn add_then_list_returns_the_host()           // add devolve id não vazio; list tem 1 host com os campos iguais
#[test] fn password_is_stored_in_the_keyring_not_the_file() // password(id) == Some("segredo"); o texto de hosts.json não contém "segredo"
#[test] fn update_changes_fields_and_keeps_the_id()   // env vira Dev, id igual, list continua com 1
#[test] fn remove_deletes_host_and_password()         // list vazio; password(id) == None
#[test] fn reopening_the_store_keeps_the_hosts()      // nova HostStore::open no mesmo dir devolve o host
```

- [x] **Step 3: Rodar os testes e confirmar que falham**

Run: `cd redis-desk/src-tauri && cargo test config::`
Expected: FAIL — `HostStore` não existe.

- [x] **Step 4: Implementar `core/config.rs`**

Dependências novas no `Cargo.toml`: `serde`/`serde_json`, `keyring = "3"`, `uuid` (feature `v4`), `thiserror`, e `tempfile` em `[dev-dependencies]`. Gravação do `hosts.json` por arquivo temporário + rename, para não corromper se o app morrer no meio.

- [x] **Step 5: Rodar os testes e confirmar que passam**

Run: `cargo test config::`
Expected: PASS, 5 testes.

- [x] **Step 6: Escrever `scripts/test-redis.sh`**

`up` sobe `redis:7-alpine` como container `redis-desk-test` em `127.0.0.1:6399`; `down` remove; `reset` roda `FLUSHALL` nesse container de teste. É o único lugar do repositório onde `FLUSHALL` aparece, e só contra a porta 6399.

- [x] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: project scaffold and host registry with keychain-backed passwords"
```

---

### Task 2: Conexão com o host e metadados

**Files:**
- Create: `redis-desk/src-tauri/src/core/conn.rs`
- Test: módulo `#[cfg(test)]` em `conn.rs` (precisa de `scripts/test-redis.sh up`)

**Interfaces:**
- Consumes: `Host`, `HostStore::password` da Task 1.
- Produces:
  - `enum ConnError { Unreachable { host: String, detail: String }, Auth { host: String }, Protocol(String), KeyGone(String) }` — `Display` em português, citando o nome do host.
  - `struct Pool` com `Pool::new() -> Pool`, `async fn get(&self, host: &Host, password: Option<&str>) -> Result<MultiplexedConnection, ConnError>`, `fn drop_host(&self, id: &str)`
  - `async fn ping(&self, host: &Host, pw: Option<&str>) -> Result<Duration, ConnError>`
  - `async fn db_sizes(&self, host: &Host, pw: Option<&str>) -> Result<Vec<(u8, u64)>, ConnError>` — lê `INFO keyspace`, devolve `(índice do db, nº de chaves)`

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn ping_returns_a_duration()                     // contra 127.0.0.1:6399
#[tokio::test] async fn db_sizes_counts_keys_in_db0()                 // grava 3 chaves, espera (0, 3)
#[tokio::test] async fn unreachable_host_names_itself_in_the_error()  // porta 6555 fechada → ConnError::Unreachable, mensagem contém o nome do host
#[tokio::test] async fn pool_reuses_the_same_connection()             // dois get() seguidos não abrem conexão nova (checar via CLIENT LIST)
```

O terceiro teste é a situação 1 do Review Focus: erro tratado, com nome do host, sem travar.

- [x] **Step 2: Rodar e confirmar que falham**

Run: `./scripts/test-redis.sh up && cargo test conn::`
Expected: FAIL — `Pool` não existe.

- [x] **Step 3: Implementar `core/conn.rs`**

`redis = { version = "1", features = ["tokio-comp", "tls-rustls-webpki-roots"] }`. A URL sai do `Host`: `rediss://` quando `tls`, senão `redis://`. Timeout de conexão e de resposta de 5 segundos — sem isso a VPN caindo congela a interface.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test conn::`
Expected: PASS, 4 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: redis connection pool with typed, host-named errors"
```

---

### Task 3: Varredura de chaves e árvore por prefixo

**Files:**
- Create: `redis-desk/src-tauri/src/core/keys.rs`
- Test: módulo `#[cfg(test)]` em `keys.rs`

**Interfaces:**
- Consumes: `Pool` da Task 2.
- Produces:
  - `struct KeyMeta { key: String, kind: KeyKind, ttl_ms: Option<i64>, size_bytes: u64 }`
  - `enum KeyKind { String, Hash, List, ZSet, Set, Stream, Json, Unknown(String) }` com `fn editable(&self) -> bool` (verdadeiro só para String, Hash, List, ZSet)
  - `async fn scan_all(pool: &Pool, host: &Host, pw: Option<&str>, db: u8) -> Result<Vec<KeyMeta>, ConnError>` — `SCAN` com `COUNT 500`, metadados em pipeline de `TYPE` + `PTTL` + `MEMORY USAGE` em lotes de 200
  - `struct TreeNode { label: String, full_prefix: String, count: u64, children: Vec<TreeNode>, leaf_key: Option<String> }`
  - `fn build_tree(keys: &[KeyMeta]) -> Vec<TreeNode>` — separador `:`

- [x] **Step 1: Escrever os testes que falham**

`build_tree` é função pura, então os testes de árvore não precisam de Redis:

```rust
#[test] fn groups_by_colon_prefix()        // session:user:1, session:user:2, cache:a → 2 raízes; session.count == 2
#[test] fn leaf_carries_the_full_key()     // o nó folha de session:user:1 tem leaf_key == Some("session:user:1")
#[test] fn degenerate_key_names_survive()  // ["", ":", "a:", "::b", "plain"] → toda chave de entrada aparece como exatamente uma folha, nenhuma se perde, nenhum galho vazio sem folha
#[test] fn counts_roll_up_through_levels() // a contagem de um galho é a soma das folhas abaixo dele
#[tokio::test] async fn scan_all_finds_every_type_with_ttl_and_size()
```

O terceiro é a situação 5 do Review Focus.

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test keys::`
Expected: FAIL — `build_tree` não existe.

- [x] **Step 3: Implementar `core/keys.rs`**

`MEMORY USAGE` pode não existir em servidor gerenciado antigo: quando o comando falhar, `size_bytes` vira 0 em vez de derrubar a varredura.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test keys::`
Expected: PASS, 5 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: key scan with metadata and prefix tree builder"
```

---

### Task 4: Ler valor de cada tipo

**Files:**
- Create: `redis-desk/src-tauri/src/core/value.rs`
- Test: módulo `#[cfg(test)]` em `value.rs`

**Interfaces:**
- Consumes: `Pool`, `KeyKind`.
- Produces:
  - `enum Value { Str(Bytes), Hash(Vec<(String, Bytes)>), List(Vec<Bytes>), ZSet(Vec<(Bytes, f64)>), Set(Vec<Bytes>), Stream(Vec<StreamEntry>), ReadOnlyText(String) }` onde `Bytes` serializa como `{ text: Option<String>, base64: Option<String>, truncated: bool }` — `text` quando o conteúdo é UTF-8 válido, `base64` quando não é
  - `struct ValueView { key: String, kind: KeyKind, ttl_ms: Option<i64>, size_bytes: u64, value: Value, total_elements: u64, truncated: bool }`
  - `async fn read_value(pool: &Pool, host: &Host, pw: Option<&str>, db: u8, key: &str, limit: ValueLimit) -> Result<ValueView, ConnError>`
  - `struct ValueLimit { offset: usize, max_bytes: usize, max_elements: usize }` com `Default` = offset 0, 256 KB e 500 elementos. `offset` é o número de elementos já mostrados, para o botão "carregar mais" da Task 11 pedir o pedaço seguinte sem reler o começo.

Chave inexistente devolve `ConnError::KeyGone(key)`.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn reads_each_editable_type()         // string, hash, list, zset com os valores gravados
#[tokio::test] async fn reads_set_and_stream_as_read_only()
#[tokio::test] async fn missing_key_is_key_gone()
#[tokio::test] async fn huge_string_is_truncated_and_flagged()  // 2 MB → value.truncated, len(text) <= max_bytes, size_bytes real preservado
#[tokio::test] async fn non_utf8_value_comes_back_as_base64()   // grava b"\xff\xfe", espera base64 preenchido e text None
#[tokio::test] async fn offset_returns_the_next_page_of_elements() // lista de 1200 itens, offset 500 começa no item 501
```

Os dois últimos são as situações 2 e 4 do Review Focus.

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test value::read`
Expected: FAIL — `read_value` não existe.

- [x] **Step 3: Implementar a leitura em `core/value.rs`**

Coleções longas vêm por `HSCAN`/`LRANGE`/`ZRANGE` paginados até `max_elements`; `total_elements` vem de `HLEN`/`LLEN`/`ZCARD`, para a tela poder dizer "mostrando 500 de 12.430".

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test value::read`
Expected: PASS, 6 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: typed value reader with truncation and binary-safe payloads"
```

---

### Task 5: Escrever, criar, apagar e mexer no TTL

**Files:**
- Modify: `redis-desk/src-tauri/src/core/value.rs`
- Test: módulo `#[cfg(test)]` em `value.rs`

**Interfaces:**
- Consumes: tudo da Task 4.
- Produces:
  - `enum WriteOp { SetString { value: Bytes }, SetHashField { field: String, value: Bytes }, DeleteHashField { field: String }, SetListItem { index: i64, value: Bytes }, PushListItem { value: Bytes, front: bool }, RemoveListItem { index: i64 }, SetZSetMember { member: Bytes, score: f64 }, RemoveZSetMember { member: Bytes }, SetTtl { ttl_ms: Option<i64> }, DeleteKey }`
  - `async fn apply_write(pool, host, pw, db, key: &str, op: &WriteOp) -> Result<(), ConnError>`
  - `fn op_summary(key: &str, op: &WriteOp) -> String` — frase em português para o diálogo de confirmar, ex. `Apagar o campo "email" de session:user:4471`

Regra: `apply_write` preserva o TTL que a chave já tinha, exceto quando a operação é `SetTtl` ou `DeleteKey`. Tipo não editável devolve erro.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn set_string_replaces_the_value()
#[tokio::test] async fn editing_a_key_preserves_its_ttl()        // TTL de 100s, SetHashField, PTTL continua ~100s
#[tokio::test] async fn set_ttl_none_makes_the_key_persistent()
#[tokio::test] async fn delete_key_removes_it()
#[tokio::test] async fn remove_list_item_by_index_keeps_order()  // [a,b,c] remove 1 → [a,c]
#[tokio::test] async fn writing_a_set_is_refused()               // tipo não editável → Err
#[test]        fn op_summary_is_written_in_portuguese()          // contém a chave e o verbo da ação
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test value::write`
Expected: FAIL — `apply_write` não existe.

- [x] **Step 3: Implementar a escrita**

`RemoveListItem` por índice não existe no Redis: marcar com um sentinela por `LSET` e depois `LREM` desse sentinela, dentro de um `MULTI`/`EXEC`. É o único ponto do módulo onde o algoritmo não sai da assinatura.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test value::write`
Expected: PASS, 7 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: typed write operations preserving ttl"
```

---

### Task 6: Cópia de segurança, histórico e volta atrás

**Files:**
- Create: `redis-desk/src-tauri/src/core/history.rs`
- Test: módulo `#[cfg(test)]` em `history.rs`

**Interfaces:**
- Consumes: `read_value`, `apply_write`, `WriteOp`, `op_summary`.
- Produces:
  - `struct HistoryEntry { id: i64, host_id: String, host_name: String, env: Env, db: u8, key: String, summary: String, before_json: Option<String>, after_json: Option<String>, actor: Actor, at: i64, rolled_back: bool }`
  - `enum Actor { Human, Llm }`
  - `struct History` com `History::open(db_path: &Path) -> Result<History>`, `async fn write_with_backup(&self, pool, host, pw, db, key, op: &WriteOp, actor: Actor) -> Result<i64>`, `fn list(&self, host_id: &str, limit: usize) -> Result<Vec<HistoryEntry>>`, `fn get(&self, id: i64) -> Result<HistoryEntry>`, `async fn rollback(&self, pool, host, pw, entry_id: i64) -> Result<()>`, `fn prune(&self, host_id: &str, keep_days: i64, keep_entries: usize) -> Result<usize>`

`write_with_backup` lê o valor inteiro antes (sem truncar), grava a linha, só então aplica a escrita. Se a escrita falhar, a linha é removida. `rollback` regrava o `before_json` e marca `rolled_back`.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn backup_is_taken_before_the_write()
#[tokio::test] async fn rollback_restores_the_previous_value()
#[tokio::test] async fn rollback_of_a_delete_recreates_the_key_with_its_ttl()
#[tokio::test] async fn failed_write_leaves_no_history_row()
#[test]        fn prune_keeps_30_days_or_500_entries_whichever_comes_first()
#[test]        fn entry_records_whether_the_llm_or_the_human_did_it()
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test history::`
Expected: FAIL — `History` não existe.

- [x] **Step 3: Implementar `core/history.rs`**

`rusqlite = { version = "0.3x", features = ["bundled"] }` — a versão mais recente que compile com o resto. Uma tabela `history`, índice em `(host_id, at)`. Banco em `<dir de dados do app>/redis-desk.sqlite`, o mesmo arquivo que a Task 7 usa para o índice.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test history::`
Expected: PASS, 6 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: value backup, change history and rollback"
```

---

### Task 7: Índice local e varredura incremental

**Files:**
- Create: `redis-desk/src-tauri/src/core/index.rs`
- Test: módulo `#[cfg(test)]` em `index.rs`

**Interfaces:**
- Consumes: `scan_all`, `read_value`, `Pool`.
- Produces:
  - `struct Index` com `Index::open(db_path: &Path) -> Result<Index>`
  - `async fn rescan(&self, pool, host, pw, db: u8, progress: impl Fn(ScanProgress)) -> Result<ScanReport>`
  - `struct ScanProgress { scanned: u64, total_estimate: u64 }`
  - `struct ScanReport { host_id: String, db: u8, keys: u64, finished_at: i64, duration_ms: u64 }`
  - `fn last_scan(&self, host_id: &str, db: u8) -> Result<Option<ScanReport>>`
  - `fn forget_key(&self, host_id: &str, db: u8, key: &str) -> Result<()>`

Tabela FTS5 `key_index(host_id, db, key, kind, ttl_ms, body)` onde `body` é o texto achatado do valor: string é ela mesma; hash vira `campo valor` por linha; lista e sorted set viram um elemento por linha. Teto de **64 KB por chave** no `body`, com coluna `truncated`. Valor não-UTF-8 entra no índice como string vazia e `binary = 1` — não quebra a varredura e não polui a busca.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn rescan_indexes_every_type_into_body()
#[tokio::test] async fn body_is_capped_at_64kb_and_flagged()      // Review Focus 2
#[tokio::test] async fn binary_value_is_indexed_as_binary_not_text() // Review Focus 4
#[tokio::test] async fn rescan_replaces_the_previous_snapshot()   // chave apagada no servidor some do índice
#[tokio::test] async fn last_scan_reports_time_and_count()
#[tokio::test] async fn progress_callback_is_called_at_least_once()
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test index::`
Expected: FAIL — `Index` não existe.

- [x] **Step 3: Implementar `core/index.rs`**

A varredura roda por `(host, db)` dentro de uma transação, trocando o retrato inteiro no fim. Só varre o host aberto; os outros ficam para quando forem abertos.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test index::`
Expected: PASS, 6 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: local fts index with incremental rescan"
```

---

### Task 8: Busca

**Files:**
- Create: `redis-desk/src-tauri/src/core/search.rs`
- Test: módulo `#[cfg(test)]` em `search.rs`

**Interfaces:**
- Consumes: `Index`, `Pool`, `read_value`.
- Produces:
  - `struct SearchHit { key: String, kind: KeyKind, ttl_ms: Option<i64>, where_found: Where, snippet: String, match_start: usize, match_len: usize }`
  - `enum Where { KeyName, Value }`
  - `struct SearchOutcome { hits: Vec<SearchHit>, scanned: u64, elapsed_ms: u64, stale_keys_removed: u64, index_age_secs: i64 }`
  - `async fn search(index: &Index, pool: &Pool, host: &Host, pw: Option<&str>, db: u8, needle: &str, limit: usize) -> Result<SearchOutcome, ConnError>`

Busca é por **substring literal**, não por palavra: procurar `marco@` tem que achar dentro de `"email":"marco@empresa.com"`. Cada acerto é conferido com `EXISTS` antes de entrar no resultado; o que não existe mais sai do índice e entra em `stale_keys_removed`.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn finds_text_inside_a_hash_field()
#[tokio::test] async fn finds_text_inside_a_list_item_and_a_zset_member()
#[tokio::test] async fn finds_text_inside_json_stored_as_a_string()
#[tokio::test] async fn finds_by_key_name_and_marks_where_found()
#[tokio::test] async fn substring_in_the_middle_of_a_word_is_found()   // "arco@emp" acha
#[tokio::test] async fn key_expired_since_the_scan_is_dropped_not_errored() // Review Focus 3
#[tokio::test] async fn five_thousand_keys_answer_under_one_second()   // anti-meta 1: elapsed_ms < 1000
#[tokio::test] async fn snippet_points_at_the_match()
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test search::`
Expected: FAIL — `search` não existe.

- [x] **Step 3: Implementar `core/search.rs`**

FTS5 sozinho casa por token e não acha substring no meio de palavra. A combinação: FTS5 com tokenizador `trigram` (vem no SQLite embutido) para pré-filtrar, e confirmação por `contains` em Rust sobre os candidatos. É o algoritmo que a assinatura não determina — o resto sai dela.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test search::`
Expected: PASS, 8 testes. Se o teste de um segundo falhar, o problema é o pré-filtro, não o `contains`.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: substring search across key names and values"
```

---

### Task 9: Fachada de comandos para a interface

**Files:**
- Create: `redis-desk/src-tauri/src/commands.rs`
- Modify: `redis-desk/src-tauri/src/lib.rs`, `redis-desk/src-tauri/src/main.rs`
- Create: `redis-desk/src/lib/api.ts`
- Test: `redis-desk/src-tauri/src/commands.rs` (módulo `#[cfg(test)]`)

**Interfaces:**
- Consumes: todo o `core`.
- Produces, cada um um `#[tauri::command]` e a função gêmea em `api.ts` com o mesmo nome em camelCase:
  - `list_hosts`, `add_host`, `update_host`, `remove_host`, `test_host`
  - `list_dbs(host_id)`, `load_tree(host_id, db)`, `rescan(host_id, db)`, `scan_status(host_id, db)`
  - `read_value(host_id, db, key)`, `read_value_more(host_id, db, key, offset)`
  - `write_value(host_id, db, key, op, confirmed: bool)` — com host de ambiente `prod` e `confirmed == false`, **não escreve**: devolve `WriteOutcome::NeedsConfirmation { summary }`
  - `list_history(host_id, limit)`, `rollback(entry_id, confirmed: bool)` — mesma regra de confirmação
  - `search(host_id, db, needle)`
- `struct AppState { hosts: Mutex<HostStore>, pool: Pool, index: Index, history: History }` em `tauri::Manager`. A Task 16 acrescenta o campo `approvals` e mais dois comandos; esta tarefa não os cita.

`main.rs` olha `std::env::args()`: com `--mcp`, chama `mcp::run_stdio()` e nunca sobe janela.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn prod_write_without_confirmation_is_refused()  // WriteOutcome::NeedsConfirmation, e o valor no Redis não mudou
#[tokio::test] async fn prod_write_with_confirmation_goes_through()
#[tokio::test] async fn dev_write_needs_no_confirmation()
#[tokio::test] async fn prod_rollback_follows_the_same_rule()
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test commands::`
Expected: FAIL — `write_value` não existe.

- [x] **Step 3: Implementar `commands.rs`, registrar no `lib.rs`, ramificar o `main.rs`**

- [x] **Step 4: Escrever `src/lib/api.ts`**

Tipos TypeScript espelhando os structs de serde, e um wrapper por comando sobre `invoke`. É o único arquivo da interface que fala com o Rust.

- [x] **Step 5: Rodar e confirmar que passam**

Run: `cargo test commands:: && npm run check`
Expected: PASS nos 4 testes; `svelte-check` sem erro.

- [x] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: tauri command surface with prod write confirmation gate"
```

---

### Task 10: Casca da janela, tokens visuais e lista de hosts

**Files:**
- Modify: `redis-desk/src/App.svelte`, `redis-desk/src/app.css`
- Create: `redis-desk/src/lib/HostList.svelte`, `redis-desk/src/lib/HostForm.svelte`
- Test: `redis-desk/src/lib/__tests__/host-chrome.test.ts` (Vitest + `@testing-library/svelte`)

**Interfaces:**
- Consumes: `api.ts` (`list_hosts`, `add_host`, `update_host`, `remove_host`, `test_host`).
- Produces: `export function envChrome(env: Env): { band: string; label: string; border: string }` em `src/lib/env.ts`.

Barra do topo com a cor do ambiente e o rótulo carimbado: `PRODUÇÃO`, `HOMOLOGAÇÃO`, `DESENVOLVIMENTO`. Fontes Barlow Semi Condensed e Azeret Mono empacotadas localmente em `src/assets/fonts/` — o app abre sem internet.

- [x] **Step 1: Escrever os testes que falham**

```ts
test('host de prod pinta a barra de vermelho e carimba PRODUÇÃO')
test('host de dev usa a cor calma e não carimba PRODUÇÃO')
test('trocar de host troca a cor da janela')
test('cadastrar host sem endereço mostra erro e não chama add_host')
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `npm run test -- host-chrome`
Expected: FAIL — `envChrome` não existe.

- [x] **Step 3: Implementar os tokens em `app.css`, `env.ts`, `HostList.svelte` e `HostForm.svelte`**

Instalar `vitest`, `@testing-library/svelte`, `jsdom`. Valores exatos de cor na seção Global Constraints.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `npm run test -- host-chrome`
Expected: PASS, 4 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: operations-room shell with environment-colored chrome"
```

---

### Task 11: Árvore de chaves e painel de valor (leitura)

**Files:**
- Create: `redis-desk/src/lib/KeyTree.svelte`, `redis-desk/src/lib/ValuePanel.svelte`, `redis-desk/src/lib/editors/ReadOnlyView.svelte`
- Test: `redis-desk/src/lib/__tests__/key-tree.test.ts`

**Interfaces:**
- Consumes: `load_tree`, `read_value`, `read_value_more`, `scan_status`.
- Produces: evento `select` do `KeyTree` carregando `{ key: string }`.

Galho abre e fecha mostrando a contagem. Lista virtualizada — milhares de folhas não podem pesar. O painel mostra chave, tipo, tamanho, TTL em contagem regressiva, e o conteúdo. Valor truncado mostra "mostrando X de Y" com botão de carregar mais. Topo da coluna tem o filtro de nome, instantâneo, e a idade da última varredura com botão de varrer de novo.

- [x] **Step 1: Escrever os testes que falham**

```ts
test('clicar num galho abre os filhos e mostra a contagem')
test('clicar numa folha chama read_value com a chave inteira')
test('filtro de nome esconde os galhos sem acerto e mantém o caminho dos que têm')
test('valor truncado mostra "mostrando 500 de 12.430" e o botão de carregar mais')
test('tipo só leitura não mostra botão de salvar')
test('ttl aparece em contagem regressiva e some quando a chave não expira')
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `npm run test -- key-tree`
Expected: FAIL — componentes não existem.

- [x] **Step 3: Implementar os três componentes**

- [x] **Step 4: Rodar e confirmar que passam**

Run: `npm run test -- key-tree`
Expected: PASS, 6 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: virtualized key tree and value panel"
```

---

### Task 12: Editores por tipo e confirmar em produção

**Files:**
- Create: `redis-desk/src/lib/editors/{StringEditor,HashEditor,ListEditor,ZSetEditor}.svelte`, `redis-desk/src/lib/ConfirmDialog.svelte`, `redis-desk/src/lib/NewKeyDialog.svelte`
- Test: `redis-desk/src/lib/__tests__/editors.test.ts`

**Interfaces:**
- Consumes: `write_value`, `WriteOp`, `WriteOutcome`.

Cada tipo tem sua tela: string é área de texto com botão de formatar JSON quando o conteúdo for JSON; hash é tabela campo/valor com adicionar e remover linha; lista é tabela com índice, mover e remover; sorted set é tabela membro/pontuação ordenada. Em prod, salvar e excluir abrem o `ConfirmDialog` com o resumo vindo do `WriteOutcome::NeedsConfirmation`; um clique em "Confirmar" reenvia com `confirmed: true`.

- [x] **Step 1: Escrever os testes que falham**

```ts
test('salvar string em prod abre o confirmar com o resumo da mudança')
test('confirmar reenvia write_value com confirmed true')
test('cancelar não chama write_value de novo e mantém o texto editado na tela')
test('salvar em dev não abre confirmar')
test('editar campo de hash manda SetHashField só do campo mexido')
test('remover item de lista manda RemoveListItem com o índice certo')
test('criar chave nova pede tipo e nome e recusa nome vazio')
test('erro de conexão ao salvar mostra o nome do host e não limpa a edição')  // Review Focus 1
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `npm run test -- editors`
Expected: FAIL.

- [x] **Step 3: Implementar os editores, o `ConfirmDialog` e o `NewKeyDialog`**

- [x] **Step 4: Rodar e confirmar que passam**

Run: `npm run test -- editors`
Expected: PASS, 8 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: per-type editors with prod confirmation dialog"
```

---

### Task 13: Busca na interface

**Files:**
- Create: `redis-desk/src/lib/SearchPanel.svelte`
- Modify: `redis-desk/src/App.svelte`
- Test: `redis-desk/src/lib/__tests__/search-panel.test.ts`

**Interfaces:**
- Consumes: `search`, `SearchOutcome`, `rescan`.

Campo de busca no topo da janela. O resultado mostra, por acerto: a chave, o tipo, o TTL, onde achou (nome ou valor) e o trecho com o pedaço encontrado destacado. Clicar no acerto abre a chave na árvore. Abaixo, uma linha de aviso: quantas chaves varridas, quanto tempo levou, e a idade do índice.

- [x] **Step 1: Escrever os testes que falham**

```ts
test('buscar mostra os acertos com o trecho destacado')
test('clicar num acerto seleciona a chave na árvore')
test('busca sem acerto mostra o que fazer, não uma tela vazia')
test('índice velho mostra aviso com botão de varrer de novo')
test('acerto cujo índice estava velho some da lista e o aviso conta quantos sumiram')
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `npm run test -- search-panel`
Expected: FAIL.

- [x] **Step 3: Implementar o `SearchPanel`**

- [x] **Step 4: Rodar e confirmar que passam**

Run: `npm run test -- search-panel`
Expected: PASS, 5 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: search panel with highlighted snippets and index freshness"
```

---

### Task 14: Aba de histórico e volta atrás

**Files:**
- Create: `redis-desk/src/lib/HistoryPanel.svelte`
- Test: `redis-desk/src/lib/__tests__/history-panel.test.ts`

**Interfaces:**
- Consumes: `list_history`, `rollback`, `HistoryEntry`.

Lista por host, mais recente primeiro: quando, quem fez (você ou o Claude), a chave, o resumo, e os valores antes e depois lado a lado. Botão de voltar atrás por linha. Linha já revertida fica marcada e sem botão.

- [x] **Step 1: Escrever os testes que falham**

```ts
test('mostra quem fez cada mudança, você ou o Claude')
test('mostra antes e depois lado a lado')
test('voltar atrás em prod abre o confirmar')
test('linha já revertida não oferece voltar atrás de novo')
test('histórico vazio convida a fazer a primeira mudança')
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `npm run test -- history-panel`
Expected: FAIL.

- [x] **Step 3: Implementar o `HistoryPanel`**

- [x] **Step 4: Rodar e confirmar que passam**

Run: `npm run test -- history-panel`
Expected: PASS, 5 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: history panel with before/after diff and rollback"
```

---

### Task 15: Servidor MCP — as ferramentas de leitura

**Files:**
- Create: `redis-desk/src-tauri/src/mcp/mod.rs`, `redis-desk/src-tauri/src/mcp/tools.rs`
- Test: módulo `#[cfg(test)]` em `tools.rs`

**Interfaces:**
- Consumes: `HostStore`, `Pool`, `Index`, `search`, `read_value`, `scan_all`.
- Produces: `async fn run_stdio() -> Result<()>` e as ferramentas, com estes nomes exatos:
  - `list_hosts` → id, nome, ambiente, endereço (sem senha)
  - `list_databases(host_id)` → db e contagem de chaves
  - `search_keys(host_id, db, needle, limit)` → os mesmos `SearchHit`
  - `read_key(host_id, db, key)` → `ValueView` completo, sem máscara
  - `count_keys(host_id, db, prefix)` → contagem sob um prefixo
  - `key_ttls(host_id, db, prefix)` → mínimo, máximo, mediana e quantas não expiram
  - `summarize_prefixes(host_id, db, depth)` → contagem e bytes por prefixo

Ferramenta de leitura que precisa de índice e não tem varredura recente varre antes, sozinha, e diz no resultado que varreu.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn list_hosts_never_returns_a_password()
#[tokio::test] async fn search_keys_finds_value_text()
#[tokio::test] async fn read_key_returns_the_whole_value_unmasked()
#[tokio::test] async fn count_keys_counts_by_prefix()
#[tokio::test] async fn key_ttls_reports_min_max_median_and_persistent_count()
#[tokio::test] async fn reading_without_a_recent_scan_scans_first_and_says_so()
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test mcp::`
Expected: FAIL — `run_stdio` não existe.

- [x] **Step 3: Implementar com `rmcp = "3"`**

Descrição de cada ferramenta em português, porque é o que o Claude vai ler para escolher.

- [x] **Step 4: Verificar de verdade, por fora**

Run: `./target/debug/redis-desk --mcp` e mandar um `initialize` seguido de `tools/list` por stdin.
Expected: as 7 ferramentas na resposta.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: mcp stdio server with read-only tools"
```

---

### Task 16: A trava de escrita da LLM

**Files:**
- Create: `redis-desk/src-tauri/src/approval/{mod.rs,server.rs,client.rs}`
- Modify: `redis-desk/src-tauri/src/mcp/tools.rs`, `redis-desk/src-tauri/src/commands.rs`
- Create: `redis-desk/src/lib/ApprovalDialog.svelte`
- Test: módulo `#[cfg(test)]` em `approval/mod.rs`; `redis-desk/src/lib/__tests__/approval.test.ts`

**Interfaces:**
- Produces:
  - `struct ApprovalRequest { id: String, host_id: String, host_name: String, env: Env, db: u8, key: String, summary: String, op: WriteOp, asked_at: i64 }`
  - `enum ApprovalVerdict { Allowed, Denied, TimedOut }`
  - `struct ApprovalQueue` com `fn pending(&self) -> Vec<ApprovalRequest>`, `fn resolve(&self, id: &str, typed: &str) -> Result<ApprovalVerdict>`
  - `async fn request_approval(socket: &Path, req: ApprovalRequest, timeout: Duration) -> Result<ApprovalVerdict>` (lado MCP)
  - `async fn serve(queue: ApprovalQueue, socket: &Path) -> Result<()>` (lado GUI)
- Comandos Tauri novos: `pending_approvals()`, `resolve_approval(request_id, typed_host_name)`; e o campo `approvals: ApprovalQueue` entra no `AppState` da Task 9.
- Ferramentas MCP novas: `write_key`, `delete_key`, `set_ttl`. Cada uma monta o pedido, espera o veredito e só então chama `History::write_with_backup(..., Actor::Llm)`.

Socket unix em `<dir de runtime do app>/redis-desk.sock`, permissão 0600. Sem app aberto, as ferramentas de escrita devolvem "abra o aplicativo para liberar esta escrita". Veredito `Allowed` só sai quando o texto digitado for **exatamente** o nome do host. Tempo de espera: 120 segundos.

- [x] **Step 1: Escrever os testes que falham**

```rust
#[tokio::test] async fn llm_write_waits_for_the_human()
#[test]        fn wrong_host_name_does_not_unlock()
#[test]        fn exact_host_name_unlocks()
#[tokio::test] async fn no_answer_in_120s_times_out_and_does_not_write()
#[tokio::test] async fn denied_request_leaves_redis_untouched()
#[tokio::test] async fn approved_write_is_recorded_with_actor_llm()
#[tokio::test] async fn dev_host_is_gated_too()
#[tokio::test] async fn write_tool_without_a_running_app_explains_how_to_fix_it()
```

```ts
test('pedido do Claude aparece com host, chave e o que vai mudar')
test('botão liberar fica desligado até o nome do host estar digitado certo')
test('nome digitado errado mostra o que se espera e não libera')
```

- [x] **Step 2: Rodar e confirmar que falham**

Run: `cargo test approval:: && npm run test -- approval`
Expected: FAIL.

- [x] **Step 3: Implementar o socket, a fila, as três ferramentas e o diálogo**

A janela ganha foco e toca o alerta do sistema quando um pedido chega — pedido que ninguém vê é pedido que expira.

- [x] **Step 4: Rodar e confirmar que passam**

Run: `cargo test approval:: && npm run test -- approval`
Expected: PASS, 8 + 3 testes.

- [x] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: human-in-the-loop gate for llm writes"
```

---

### Task 17: Empacotar para mac e linux

**Files:**
- Modify: `redis-desk/src-tauri/tauri.conf.json`
- Create: `redis-desk/README.md`, `redis-desk/docs/mcp-setup.md`

- [ ] **Step 1: Configurar os alvos**

`tauri.conf.json`: identificador `com.sugamele.redisdesk`, janela de 1280x800 com mínimo de 980x600, alvos `dmg` e `app` no mac, `appimage` e `deb` no linux.

- [ ] **Step 2: Gerar o build do mac e abrir o app**

Run: `npm run tauri build`
Expected: `.dmg` em `src-tauri/target/release/bundle/dmg/`; abrir o app, cadastrar o Redis de teste em `127.0.0.1:6399` marcado como dev, ver a árvore, buscar um valor, editar e desfazer.

- [ ] **Step 3: Escrever o `README.md` e o `docs/mcp-setup.md`**

O de MCP traz a linha pronta para o `claude mcp add`, apontando para o binário com `--mcp`, e explica que escrita pede liberação na janela digitando o nome do host.

- [ ] **Step 4: Gerar o build do linux em docker e confirmar que sai o AppImage**

Run: `docker run --rm -v "$PWD":/app -w /app rust:1-bookworm bash -lc "apt-get update && apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev nodejs npm && npm ci && npm run tauri build"`
Expected: AppImage gerado. Falha aqui não bloqueia o mac; registrar o que faltou.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: mac and linux packaging, readme and mcp setup guide"
```
