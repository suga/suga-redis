//! Busca por substring sobre o índice local (Task 7).
//!
//! Esta é a consulta que a Task 7 existe para servir: achar um texto em
//! qualquer lugar do banco aberto — dentro de campo de hash, item de lista,
//! membro de sorted set, JSON guardado como string, ou no próprio nome da
//! chave — em menos de um segundo com 5.000 chaves indexadas.
//!
//! # Como a busca é feita em dois tempos
//!
//! O algoritmo é **pré-filtro no SQLite, confirmação em Rust**:
//!
//! 1. O FTS5 com tokenizador `trigram` devolve as chaves CANDIDATAS. Ele é
//!    rápido (usa o índice invertido) mas não é exato: casa trigramas, e um
//!    texto pode conter todos os trigramas da agulha sem conter a agulha.
//! 2. Cada candidata é conferida com uma busca de substring de verdade sobre
//!    o texto guardado. Essa segunda passada é que decide — e é barata,
//!    porque roda sobre um punhado de candidatas, não sobre o banco inteiro.
//!
//! O que NÃO funcionaria: `body LIKE '%agulha%'` sozinho é exato mas não usa
//! índice nenhum (varre todas as linhas); o FTS5 padrão (`unicode61`)
//! sozinho usa índice mas casa só PALAVRAS inteiras, e procurar `arco@emp`
//! precisa achar `marco@empresa.com`.
//!
//! # Caixa alta e baixa
//!
//! A busca ignora a caixa das letras, em todo o Unicode (`JOÃO` acha `joão`)
//! — é o que a pessoa espera de um campo de busca, e também é o que o
//! tokenizador `trigram` já faz por conta própria no pré-filtro. Fazer a
//! confirmação sensível à caixa deixaria o pré-filtro e a confirmação em
//! desacordo: o FTS5 traria a linha e o Rust a descartaria.
//!
//! # Agulha curta
//!
//! Um trigrama tem, por definição, três caracteres: o pré-filtro do FTS5 não
//! enxerga agulha de um ou dois caracteres. Nesse caso a busca cai para
//! `LIKE` sobre `body` — varredura mesmo, mas só das linhas deste
//! (host, banco), e sem ela uma busca por `R$` devolveria vazio em silêncio,
//! que é o pior desfecho possível.

use std::collections::BTreeMap;
use std::time::Instant;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::config::Host;
use super::conn::{ConnError, Pool};
use super::index::{Index, Result, kind_from_label, now_unix_ms};
use super::keys::KeyKind;

/// Mínimo de caracteres para o tokenizador `trigram` conseguir indexar (e
/// portanto casar) a agulha. Abaixo disso, ver "Agulha curta" no topo.
const TRIGRAM_MIN_CHARS: usize = 3;

/// Quantos bytes de contexto o trecho mostra de cada lado do acerto. É um
/// teto em bytes, não em caracteres, mas sempre cortado em fronteira de
/// caractere — o acerto nunca vem sem o texto em volta que lhe dá sentido,
/// e um valor de 64 KB nunca vem inteiro para a tela.
const SNIPPET_WINDOW_BYTES: usize = 60;

/// Quantas chaves por lote na conferência de existência contra o Redis. Uma
/// ida e volta por chave transformaria o teto de um segundo em impossível.
const EXISTS_BATCH: usize = 256;

/// `index_age_secs` quando nenhuma varredura rodou ainda para este
/// (host, banco) — não existe idade a informar, e zero mentiria dizendo
/// "acabou de ser varrido".
pub const NEVER_SCANNED: i64 = -1;

/// Caractere que marca, nas pontas do trecho, que o valor continua.
const ELLIPSIS: char = '…';

/// Onde a agulha foi encontrada. Uma chave cujo NOME casa é sempre
/// reportada como `KeyName`, mesmo que o valor também case: é a informação
/// mais específica das duas, e o nome é o que a pessoa reconhece na lista.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Where {
    KeyName,
    Value,
}

/// Uma chave encontrada. `match_start` e `match_len` são deslocamentos em
/// BYTES dentro de `snippet` (não dentro do valor inteiro), prontos para a
/// interface grifar o pedaço: `&snippet[match_start..match_start + match_len]`
/// é o texto que casou, COMO ESTÁ guardado — que pode diferir da agulha
/// digitada na caixa das letras.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchHit {
    pub key: String,
    pub kind: KeyKind,
    pub ttl_ms: Option<i64>,
    pub where_found: Where,
    pub snippet: String,
    pub match_start: usize,
    pub match_len: usize,
}

/// O resultado de uma busca, com o que a interface precisa para explicar à
/// pessoa o que acabou de acontecer: quantas chaves o pré-filtro trouxe para
/// conferir (`scanned`), quanto tempo levou, quantas chaves mortas foram
/// descartadas pelo caminho, e de quando é o retrato que respondeu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub scanned: u64,
    pub elapsed_ms: u64,
    pub stale_keys_removed: u64,
    pub index_age_secs: i64,
}

/// Procura `needle` como substring nos nomes de chave e nos valores
/// indexados do (host, banco) aberto, devolvendo até `limit` acertos.
///
/// Toda chave que entra no resultado é conferida com `EXISTS` contra o
/// Redis antes de ser devolvida: o índice é um retrato de um instante
/// passado, e uma chave que expirou ou foi apagada desde a varredura não
/// pode aparecer como se existisse (Review Focus 3). Quem sumiu é removido
/// do índice na hora e contado em `stale_keys_removed`, para a busca
/// seguinte nem considerá-la.
///
/// A conferência acontece em lotes e para assim que `limit` acertos vivos
/// são reunidos — candidatas além desse ponto não são conferidas, e por
/// isso `stale_keys_removed` conta as chaves mortas ENCONTRADAS, não todas
/// as que existem no índice.
pub async fn search(
    index: &Index,
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    needle: &str,
    limit: usize,
) -> Result<SearchOutcome> {
    let started = Instant::now();

    let index_age_secs = match index.last_scan(&host.id, db)? {
        Some(report) => ((now_unix_ms() - report.finished_at) / 1000).max(0),
        None => NEVER_SCANNED,
    };

    // Agulha vazia casaria com tudo, e `limit` zero não tem resposta a dar:
    // os dois são pedidos legítimos da interface (campo de busca ainda em
    // branco), não erros.
    if needle.is_empty() || limit == 0 {
        return Ok(SearchOutcome {
            hits: Vec::new(),
            scanned: 0,
            elapsed_ms: started.elapsed().as_millis() as u64,
            stale_keys_removed: 0,
            index_age_secs,
        });
    }

    let candidates = collect_candidates(index, &host.id, db, needle)?;
    let scanned = candidates.len() as u64;
    let confirmed = confirm_text(candidates, needle);

    let mut hits = Vec::new();
    let mut stale_keys_removed = 0u64;
    let mut pending = confirmed.into_iter();
    while hits.len() < limit {
        let batch: Vec<SearchHit> = pending.by_ref().take(EXISTS_BATCH).collect();
        if batch.is_empty() {
            break;
        }

        let keys: Vec<String> = batch.iter().map(|hit| hit.key.clone()).collect();
        let alive = exists_many(pool, host, pw, db, &keys).await?;

        for (hit, alive) in batch.into_iter().zip(alive) {
            if alive {
                if hits.len() < limit {
                    hits.push(hit);
                }
            } else {
                index.forget_key(&host.id, db, &hit.key)?;
                stale_keys_removed += 1;
            }
        }
    }

    Ok(SearchOutcome {
        hits,
        scanned,
        elapsed_ms: started.elapsed().as_millis() as u64,
        stale_keys_removed,
        index_age_secs,
    })
}

/// Uma linha do índice trazida pelo pré-filtro, antes de ser confirmada.
struct Candidate {
    key: String,
    kind: KeyKind,
    ttl_ms: Option<i64>,
    body: String,
}

/// As chaves que PODEM conter a agulha, pelo nome ou pelo valor, sem
/// repetição e em ordem de nome de chave (ordem estável: a mesma busca
/// devolve a mesma lista, e `limit` corta sempre no mesmo lugar).
fn collect_candidates(
    index: &Index,
    host_id: &str,
    db: u8,
    needle: &str,
) -> Result<Vec<Candidate>> {
    const COLUMNS: &str = "SELECT key, kind, ttl_ms, body FROM key_index";
    let like = like_pattern(needle);

    index.with_conn(|conn| {
        let mut found: BTreeMap<String, Candidate> = BTreeMap::new();

        // Pelo NOME da chave. `key` é UNINDEXED na tabela FTS5 (o índice de
        // trigramas cobre só `body`), então isto percorre as linhas deste
        // (host, banco) de fato — aceitável porque são as mesmas linhas que
        // o índice já tem na mão, e nomes de chave são curtos.
        let mut by_name = conn.prepare(&format!(
            "{COLUMNS} WHERE host_id = ?1 AND db = ?2 AND key LIKE ?3 ESCAPE '\\'"
        ))?;
        for row in by_name.query_map(params![host_id, db as i64, &like], read_candidate)? {
            let candidate = row?;
            found.insert(candidate.key.clone(), candidate);
        }

        // Pelo VALOR.
        if needle.chars().count() >= TRIGRAM_MIN_CHARS {
            let mut by_body = conn.prepare(&format!(
                "{COLUMNS} WHERE key_index MATCH ?3 AND host_id = ?1 AND db = ?2"
            ))?;
            let phrase = fts_phrase(needle);
            for row in by_body.query_map(params![host_id, db as i64, &phrase], read_candidate)? {
                let candidate = row?;
                found.insert(candidate.key.clone(), candidate);
            }
        } else {
            let mut by_body = conn.prepare(&format!(
                "{COLUMNS} WHERE host_id = ?1 AND db = ?2 AND body LIKE ?3 ESCAPE '\\'"
            ))?;
            for row in by_body.query_map(params![host_id, db as i64, &like], read_candidate)? {
                let candidate = row?;
                found.insert(candidate.key.clone(), candidate);
            }
        }

        Ok(found.into_values().collect())
    })
}

fn read_candidate(row: &rusqlite::Row) -> rusqlite::Result<Candidate> {
    Ok(Candidate {
        key: row.get(0)?,
        kind: kind_from_label(&row.get::<_, String>(1)?),
        ttl_ms: row.get(2)?,
        body: row.get(3)?,
    })
}

/// A segunda passada: confirma a agulha de verdade dentro de cada candidata
/// e monta o trecho. Uma candidata em que a agulha não aparece é um falso
/// positivo do pré-filtro e some sem alarde — é por isso que esta passada
/// existe.
fn confirm_text(candidates: Vec<Candidate>, needle: &str) -> Vec<SearchHit> {
    let needle_folded = fold_case(needle).0;
    let mut hits = Vec::new();

    for candidate in candidates {
        let found = find_ignoring_case(&candidate.key, &needle_folded)
            .map(|at| (Where::KeyName, at, &candidate.key))
            .or_else(|| {
                find_ignoring_case(&candidate.body, &needle_folded)
                    .map(|at| (Where::Value, at, &candidate.body))
            });

        let Some((where_found, (start, len), text)) = found else {
            continue;
        };
        let (snippet, match_start, match_len) = snippet_around(text, start, len);

        hits.push(SearchHit {
            key: candidate.key.clone(),
            kind: candidate.kind.clone(),
            ttl_ms: candidate.ttl_ms,
            where_found,
            snippet,
            match_start,
            match_len,
        });
    }

    hits
}

/// Monta a expressão de busca do FTS5: a agulha inteira como uma FRASE, para
/// o FTS5 exigir os trigramas adjacentes e na ordem — sem as aspas ele
/// trataria a agulha como vários termos soltos, e qualquer caractere de
/// pontuação viraria um operador da linguagem de consulta. Aspas dentro da
/// agulha são dobradas, que é como o FTS5 escapa aspas (procurar
/// `email":"ana` tem que funcionar).
fn fts_phrase(needle: &str) -> String {
    format!("\"{}\"", needle.replace('"', "\"\""))
}

/// Monta o padrão do `LIKE`: a agulha cercada de `%`, com os curingas do
/// próprio `LIKE` (`%`, `_`) e a barra de escape neutralizados — sem isso,
/// procurar `100%` casaria com qualquer coisa começando em `100`.
fn like_pattern(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// Devolve `text` em caixa baixa e, junto, o mapa que leva cada byte do
/// texto dobrado de volta ao byte correspondente no ORIGINAL.
///
/// O mapa é necessário porque dobrar a caixa pode mudar o tamanho em bytes —
/// e até o número de caracteres: `İ` em caixa baixa vira dois caracteres.
/// Um deslocamento achado no texto dobrado, usado direto no original,
/// apontaria para o lugar errado (ou para o meio de um caractere, e aí o
/// corte entra em pânico). O mapa tem um byte a mais que o texto dobrado: a
/// sentinela do fim, para traduzir também o deslocamento final de um acerto.
fn fold_case(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len() + 1);

    for (at, ch) in text.char_indices() {
        for lowered in ch.to_lowercase() {
            folded.push(lowered);
            for _ in 0..lowered.len_utf8() {
                origin.push(at);
            }
        }
    }
    origin.push(text.len());

    (folded, origin)
}

/// Acha `needle_folded` (já em caixa baixa, por `fold_case`) dentro de
/// `haystack`, ignorando a caixa. Devolve o deslocamento e o comprimento em
/// bytes NO ORIGINAL — o comprimento pode diferir do da agulha, porque o
/// texto guardado pode ocupar mais bytes em caixa alta do que em baixa.
fn find_ignoring_case(haystack: &str, needle_folded: &str) -> Option<(usize, usize)> {
    if needle_folded.is_empty() {
        return None;
    }

    let (folded, origin) = fold_case(haystack);
    let at = folded.find(needle_folded)?;

    let start = origin[at];
    let end = origin[at + needle_folded.len()];
    Some((start, end - start))
}

/// Recorta uma janela de `text` em volta do acerto, devolvendo o trecho e
/// onde o acerto caiu DENTRO dele. As pontas ganham reticências quando há
/// texto escondido, e o deslocamento devolvido já as desconta.
fn snippet_around(text: &str, start: usize, len: usize) -> (String, usize, usize) {
    let mut from = start.saturating_sub(SNIPPET_WINDOW_BYTES);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (start + len + SNIPPET_WINDOW_BYTES).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }

    let mut snippet = String::new();
    if from > 0 {
        snippet.push(ELLIPSIS);
    }
    let prefix_len = snippet.len();
    snippet.push_str(&text[from..to]);
    if to < text.len() {
        snippet.push(ELLIPSIS);
    }

    (snippet, prefix_len + (start - from), len)
}

/// `EXISTS` de várias chaves numa ida e volta só. Não usa o `EXISTS` de
/// múltiplos argumentos do Redis porque aquele devolve uma CONTAGEM e aqui
/// é preciso saber QUAIS sumiram, uma a uma.
async fn exists_many(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    keys: &[String],
) -> std::result::Result<Vec<bool>, ConnError> {
    pool.with_retry(host, db, pw, |mut conn| {
        let keys = keys.to_vec();
        async move {
            let mut pipe = redis::pipe();
            for key in &keys {
                pipe.cmd("EXISTS").arg(key);
            }
            pipe.query_async::<Vec<bool>>(&mut conn).await
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::config::{Env, Host};
    use super::super::conn::Pool;
    use super::super::index::Index;
    use super::super::keys::KeyKind;
    use super::*;
    use redis::aio::MultiplexedConnection;

    /// Host de teste apontando para o container de `scripts/test-redis.sh`.
    /// O banco é parâmetro porque alguns testes precisam de um banco só
    /// seu: a suíte roda em paralelo contra o mesmo container, e um teste
    /// que conte chaves obsoletas ou use uma agulha curtíssima não pode ver
    /// as chaves de outro teste.
    fn test_host(db: u8) -> Host {
        Host {
            id: "search-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            db,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    fn open_index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::TempDir::new().unwrap();
        let index = Index::open(&dir.path().join("suga-redis.sqlite")).unwrap();
        (dir, index)
    }

    async fn del(conn: &mut MultiplexedConnection, keys: &[&str]) {
        for key in keys {
            let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        }
    }

    /// O acerto de `key` dentro do resultado, ou um panic explicando o que
    /// voltou. Os testes que rodam no banco compartilhado (6) procuram o SEU
    /// acerto em vez de exigir `hits.len() == 1`: outro teste em paralelo
    /// pode ter chaves no mesmo banco (nunca com a mesma agulha, mas o
    /// índice daquele teste varre o banco inteiro).
    fn hit_for<'a>(outcome: &'a SearchOutcome, key: &str) -> &'a SearchHit {
        outcome
            .hits
            .iter()
            .find(|hit| hit.key == key)
            .unwrap_or_else(|| {
                panic!(
                    "esperava um acerto para a chave {key:?}, vieram {:?}",
                    outcome.hits.iter().map(|h| &h.key).collect::<Vec<_>>()
                )
            })
    }

    #[tokio::test]
    async fn finds_text_inside_a_hash_field() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:hash_field";
        let needle = "alvo-hash-7f3a";

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("HSET")
            .arg(key)
            .arg("email")
            .arg(format!("{needle}@empresa.com"))
            .arg("nome")
            .arg("Ana")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        let hit = hit_for(&outcome, key);
        assert_eq!(hit.kind, KeyKind::Hash);
        assert_eq!(hit.where_found, Where::Value);
        assert!(
            hit.snippet.contains(needle),
            "o trecho precisava mostrar a agulha, veio {:?}",
            hit.snippet
        );
        assert!(
            outcome.index_age_secs >= 0 && outcome.index_age_secs < 60,
            "a idade do índice recém-varrido veio {:?}",
            outcome.index_age_secs
        );
    }

    #[tokio::test]
    async fn finds_text_inside_a_list_item_and_a_zset_member() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let list_key = "search_rs_test:list_item";
        let zset_key = "search_rs_test:zset_member";
        let needle = "alvo-colecao-9c2b";

        del(&mut conn, &[list_key, zset_key]).await;
        let _: () = redis::cmd("RPUSH")
            .arg(list_key)
            .arg("item-um")
            .arg(format!("pedido {needle}"))
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("ZADD")
            .arg(zset_key)
            .arg(1)
            .arg(format!("membro {needle}"))
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        del(&mut conn, &[list_key, zset_key]).await;

        assert_eq!(hit_for(&outcome, list_key).kind, KeyKind::List);
        assert_eq!(hit_for(&outcome, list_key).where_found, Where::Value);
        assert_eq!(hit_for(&outcome, zset_key).kind, KeyKind::ZSet);
        assert_eq!(hit_for(&outcome, zset_key).where_found, Where::Value);
    }

    #[tokio::test]
    async fn finds_text_inside_json_stored_as_a_string() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:json_string";

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(r#"{"email":"alvo-json-6b8e@empresa.com","plano":"ouro"}"#)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        // A agulha atravessa a pontuação do JSON (`":"`), que o tokenizador
        // por PALAVRAS do FTS5 nunca casaria — só o trigrama casa.
        let needle = r#"email":"alvo-json-6b8e"#;
        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        let hit = hit_for(&outcome, key);
        assert_eq!(hit.kind, KeyKind::String);
        assert_eq!(hit.where_found, Where::Value);
    }

    #[tokio::test]
    async fn finds_by_key_name_and_marks_where_found() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let needle = "alvo-nome-3d9f";
        let key = format!("search_rs_test:{needle}:perfil");

        del(&mut conn, &[&key]).await;
        // O valor NÃO contém a agulha: o único caminho até esta chave é o
        // nome dela.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg("conteudo qualquer sem a agulha")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        del(&mut conn, &[&key]).await;

        let hit = hit_for(&outcome, &key);
        assert_eq!(hit.where_found, Where::KeyName);
        assert_eq!(
            &hit.snippet[hit.match_start..hit.match_start + hit.match_len],
            needle,
            "num acerto por nome, o trecho e o recorte precisam apontar para o nome da chave"
        );
    }

    #[tokio::test]
    async fn substring_in_the_middle_of_a_word_is_found() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:middle_of_word";

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("contato marco-mid-8a4c@empresa.com cadastrado")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        // Começa e termina no MEIO de palavras: nenhuma busca por palavra
        // inteira acharia isto.
        let outcome = search(&index, &pool, &host, None, host.db, "co-mid-8a4", 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert_eq!(hit_for(&outcome, key).where_found, Where::Value);
    }

    #[tokio::test]
    async fn key_expired_since_the_scan_is_dropped_not_errored() {
        // Banco só deste teste: ele exige a contagem EXATA de chaves
        // obsoletas, que outro teste apagando as suas em paralelo estragaria.
        let pool = Pool::new();
        let host = test_host(9);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let vivo = "search_rs_test:stale:vivo";
        let morto = "search_rs_test:stale:morto";
        let needle = "alvo-stale-2f6e";

        del(&mut conn, &[vivo, morto]).await;
        for key in [vivo, morto] {
            let _: () = redis::cmd("SET")
                .arg(key)
                .arg(format!("registro {needle}"))
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        // A chave morre DEPOIS da varredura: o índice ainda a conhece.
        del(&mut conn, &[morto]).await;

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();

        assert_eq!(
            outcome.hits.iter().map(|h| &h.key).collect::<Vec<_>>(),
            vec![vivo],
            "a chave que morreu entre a varredura e a busca não podia aparecer"
        );
        assert_eq!(outcome.stale_keys_removed, 1);

        // E some do índice de vez: a próxima busca nem a considera. A chave
        // viva segue viva aqui de propósito — apagá-la antes desta segunda
        // busca criaria uma SEGUNDA chave obsoleta e a contagem não diria
        // mais nada sobre a primeira.
        let depois = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        assert_eq!(depois.stale_keys_removed, 0);
        assert_eq!(
            depois.hits.iter().map(|h| &h.key).collect::<Vec<_>>(),
            vec![vivo]
        );

        del(&mut conn, &[vivo]).await;
    }

    #[tokio::test]
    async fn five_thousand_keys_answer_under_one_second() {
        // Banco só deste teste: 5.000 chaves atrapalhariam a varredura de
        // qualquer outro teste que dividisse o banco.
        let pool = Pool::new();
        let host = test_host(8);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let needle = "alvo-agulha-no-palheiro-1b7d";

        // Escreve em lotes por pipeline: 5.000 idas e voltas separadas
        // custariam mais que a varredura inteira.
        const TOTAL: usize = 5_000;
        for chunk_start in (0..TOTAL).step_by(500) {
            let mut pipe = redis::pipe();
            for i in chunk_start..(chunk_start + 500).min(TOTAL) {
                let body = if i == TOTAL / 2 {
                    format!("usuario {i} com {needle} no cadastro")
                } else {
                    format!("usuario {i} cadastro comum sem nada de especial")
                };
                pipe.cmd("SET")
                    .arg(format!("search_rs_test:perf:{i}"))
                    .arg(body)
                    .ignore();
            }
            let _: () = pipe.query_async(&mut conn).await.unwrap();
        }

        let (_dir, index) = open_index();
        let report = index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();
        assert_eq!(report.keys, TOTAL as u64);

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();

        for chunk_start in (0..TOTAL).step_by(500) {
            let mut pipe = redis::pipe();
            for i in chunk_start..(chunk_start + 500).min(TOTAL) {
                pipe.cmd("DEL")
                    .arg(format!("search_rs_test:perf:{i}"))
                    .ignore();
            }
            let _: () = pipe.query_async(&mut conn).await.unwrap();
        }

        assert_eq!(
            outcome.hits.iter().map(|h| &h.key).collect::<Vec<_>>(),
            vec![&format!("search_rs_test:perf:{}", TOTAL / 2)]
        );
        assert!(
            outcome.elapsed_ms < 1000,
            "5.000 chaves indexadas precisavam responder em menos de 1s, levou {}ms",
            outcome.elapsed_ms
        );
    }

    #[tokio::test]
    async fn snippet_points_at_the_match() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:snippet";
        let needle = "alvo-trecho-55aa";

        // Enchimento longo dos dois lados, para que o trecho precise ser
        // uma JANELA em volta do acerto, e não o valor inteiro.
        let filler = "0123456789".repeat(400);
        let body = format!("{filler} {needle} {filler}");

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&body)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        let outcome = search(&index, &pool, &host, None, host.db, needle, 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        let hit = hit_for(&outcome, key);
        assert_eq!(
            &hit.snippet[hit.match_start..hit.match_start + hit.match_len],
            needle,
            "o recorte precisa apontar exatamente para a agulha dentro do trecho"
        );
        assert!(
            hit.snippet.len() < body.len(),
            "o trecho precisava ser uma janela, veio com {} bytes de um valor de {}",
            hit.snippet.len(),
            body.len()
        );
    }

    #[tokio::test]
    async fn search_ignores_letter_case() {
        let pool = Pool::new();
        let host = test_host(6);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:letter_case";

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("Contato: Alvo-Caixa-4E1D@Empresa.com")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        let outcome = search(&index, &pool, &host, None, host.db, "alvo-caixa-4e1d", 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        let hit = hit_for(&outcome, key);
        assert_eq!(
            &hit.snippet[hit.match_start..hit.match_start + hit.match_len],
            "Alvo-Caixa-4E1D",
            "o recorte precisa apontar para o texto COMO ESTÁ no valor, não para a agulha digitada"
        );
    }

    #[tokio::test]
    async fn needle_shorter_than_a_trigram_still_finds_the_key() {
        // Banco só deste teste: uma agulha de dois caracteres casaria com
        // as chaves de qualquer outro teste que dividisse o banco.
        let pool = Pool::new();
        let host = test_host(7);
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "search_rs_test:short_needle";

        del(&mut conn, &[key]).await;
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("saldo em R$ devido")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        // Duas letras: curto demais para virar um trigrama, então o
        // pré-filtro do FTS5 não acharia nada sozinho.
        let outcome = search(&index, &pool, &host, None, host.db, "R$", 50)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert_eq!(hit_for(&outcome, key).where_found, Where::Value);
    }
}
