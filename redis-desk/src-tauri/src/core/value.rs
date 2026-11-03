//! Leitura tipada do valor de uma chave: dado o tipo (`KeyKind`), devolve o
//! conteúdo pronto para a UI mostrar — já paginado para coleções grandes e
//! com valores binários/gigantes tratados de forma segura (nunca lendo tudo
//! de uma vez nem corrompendo bytes não-UTF-8). A escrita (Task 5) mora
//! neste mesmo arquivo, reaproveitando `Bytes` e `ValueLimit`.

use redis::aio::MultiplexedConnection;

use super::config::Host;
use super::conn::{ConnError, Pool};
use super::keys::KeyKind;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// `COUNT` pedido a cada chamada de `HSCAN`/`SSCAN` ao paginar coleções que
/// não têm um comando de range por índice (ao contrário de `LRANGE`/`ZRANGE`,
/// que endereçam por posição diretamente).
const SCAN_COUNT: usize = 500;

/// Bytes extras pedidos além de `max_bytes` ao ler o prefixo de uma
/// `string` via `GETRANGE`. O maior caractere UTF-8 ocupa 4 bytes, então 3
/// bytes de folga bastam para sempre completar um caractere que comece
/// antes do corte em `max_bytes` — sem essa folga, um valor inteiramente
/// válido em UTF-8 poderia ser confundido com binário só porque o corte caiu
/// no meio de um caractere.
const UTF8_SLACK: usize = 3;

/// Quanto pedir de cada vez e a partir de onde, ao ler o conteúdo de uma
/// chave. `offset` é o número de elementos já mostrados pela UI — o botão
/// "carregar mais" da Task 11 pede a página seguinte passando o total já
/// visto, em vez de reler o começo da coleção.
#[derive(Debug, Clone, Copy)]
pub struct ValueLimit {
    pub offset: usize,
    pub max_bytes: usize,
    pub max_elements: usize,
}

impl Default for ValueLimit {
    fn default() -> ValueLimit {
        ValueLimit {
            offset: 0,
            max_bytes: 256 * 1024,
            max_elements: 500,
        }
    }
}

/// Um valor binário do Redis, pronto para a UI: texto quando os bytes são
/// UTF-8 válido, base64 quando não são — nunca uma conversão "lossy" que
/// corromperia o conteúdo original. `truncated` indica que o que está aqui é
/// só um pedaço do valor real (ver `max_bytes` em `ValueLimit`).
#[derive(Debug, Clone, PartialEq)]
pub struct Bytes {
    pub text: Option<String>,
    pub base64: Option<String>,
    pub truncated: bool,
}

impl Bytes {
    /// Decide a representação pelo valor INTEIRO (`raw`), não pelo pedaço já
    /// cortado: assim um valor grande mas legitimamente UTF-8 continua
    /// virando texto (só que truncado), e um valor binário de verdade nunca
    /// aparenta ser texto só porque o corte caiu num prefixo válido por
    /// acaso. Usado pelos elementos de uma coleção (campo de hash, item de
    /// lista, membro de set/zset, campo de stream): ao contrário de uma
    /// `string` solta, não há um comando Redis para ler só um pedaço de um
    /// elemento dentro de um hash/lista/set, então aqui o valor inteiro já
    /// chegou via `HSCAN`/`LRANGE`/`ZRANGE`/`SSCAN`/`XRANGE` antes de passar
    /// por aqui.
    fn from_raw(raw: &[u8], max_bytes: usize) -> Bytes {
        let truncated = raw.len() > max_bytes;
        match std::str::from_utf8(raw) {
            Ok(full_text) => Bytes {
                text: Some(char_boundary_prefix(full_text, max_bytes).to_string()),
                base64: None,
                truncated,
            },
            Err(_) => {
                let limited = if truncated { &raw[..max_bytes] } else { raw };
                Bytes {
                    text: None,
                    base64: Some(BASE64.encode(limited)),
                    truncated,
                }
            }
        }
    }
}

/// Maior prefixo de `text` (em caracteres) cujo tamanho em bytes cabe em
/// `max_bytes`, sem nunca partir um caractere multibyte ao meio. Compartilhado
/// por `Bytes::from_raw` e `read_string`.
fn char_boundary_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Uma entrada de stream: id (`"<ms>-<seq>"`) e seus campos, na mesma forma
/// binary-safe usada pelo resto do arquivo.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, Bytes)>,
}

/// O conteúdo tipado de uma chave. `Set`, `Stream` e `ReadOnlyText` (usado
/// para RedisJSON e qualquer tipo não reconhecido) são somente leitura neste
/// produto — ver `KeyKind::editable`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(Bytes),
    Hash(Vec<(String, Bytes)>),
    List(Vec<Bytes>),
    ZSet(Vec<(Bytes, f64)>),
    Set(Vec<Bytes>),
    Stream(Vec<StreamEntry>),
    ReadOnlyText(String),
}

/// Tudo que a UI precisa para mostrar uma chave: o valor tipado, o TTL, o
/// tamanho real (`size_bytes`, via `MEMORY USAGE` — não encolhe quando o
/// valor é truncado para exibição) e `truncated`, que avisa quando o que
/// está em `value` é só parte do conteúdo real (elementos a mais além da
/// página pedida, ou um valor individual cortado por `max_bytes`).
#[derive(Debug, Clone, PartialEq)]
pub struct ValueView {
    pub key: String,
    pub kind: KeyKind,
    pub ttl_ms: Option<i64>,
    pub size_bytes: u64,
    pub value: Value,
    pub total_elements: u64,
    pub truncated: bool,
}

/// Lê o valor de `key`, tipado conforme `TYPE`, respeitando `limit`. Chave
/// inexistente devolve `ConnError::KeyGone`. Passa por `Pool::with_retry`
/// como qualquer outra operação deste núcleo — se a conexão cacheada tiver
/// morrido, o pool descarta e tenta de novo uma vez sozinho.
///
/// Duas idas ao Redis, não uma só: a primeira determina `TYPE` (e portanto
/// se a chave existe), porque `ConnError::KeyGone` não é algo que
/// `with_retry` consiga produzir sozinho — ele só classifica
/// `redis::RedisError`, e "chave não existe" não é um erro do driver. A
/// segunda busca TTL, tamanho e o conteúdo propriamente dito, já sabendo o
/// tipo.
pub async fn read_value(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
    limit: ValueLimit,
) -> Result<ValueView, ConnError> {
    let type_name: String = pool
        .with_retry(host, db, pw, |mut conn| async move {
            let type_name: String = redis::cmd("TYPE").arg(key).query_async(&mut conn).await?;
            Ok(type_name)
        })
        .await?;

    if type_name == "none" {
        return Err(ConnError::KeyGone(key.to_string()));
    }

    let kind = KeyKind::from_type_name(&type_name);

    pool.with_retry(host, db, pw, |mut conn| {
        let kind = kind.clone();
        async move { read_details(&mut conn, key, kind, &limit).await }
    })
    .await
}

/// Busca TTL, tamanho e o conteúdo tipado de `key`, assumindo que ela existe
/// (checado por quem chama, via `TYPE`, antes deste ponto).
async fn read_details(
    conn: &mut MultiplexedConnection,
    key: &str,
    kind: KeyKind,
    limit: &ValueLimit,
) -> redis::RedisResult<ValueView> {
    let pttl: i64 = redis::cmd("PTTL").arg(key).query_async(conn).await?;
    let ttl_ms = if pttl >= 0 { Some(pttl) } else { None };

    let size_bytes = memory_usage(conn, key).await;

    let (value, total_elements, shown, element_truncated) = match &kind {
        KeyKind::String => {
            let bytes = read_string(conn, key, limit.max_bytes).await?;
            let truncated = bytes.truncated;
            (Value::Str(bytes), 1, 1, truncated)
        }
        KeyKind::Hash => {
            let (page, total) = read_hash(conn, key, limit).await?;
            let shown = page.len();
            let truncated = page.iter().any(|(_, bytes)| bytes.truncated);
            (Value::Hash(page), total, shown, truncated)
        }
        KeyKind::List => {
            let (page, total) = read_list(conn, key, limit).await?;
            let shown = page.len();
            let truncated = page.iter().any(|bytes| bytes.truncated);
            (Value::List(page), total, shown, truncated)
        }
        KeyKind::ZSet => {
            let (page, total) = read_zset(conn, key, limit).await?;
            let shown = page.len();
            let truncated = page.iter().any(|(bytes, _)| bytes.truncated);
            (Value::ZSet(page), total, shown, truncated)
        }
        KeyKind::Set => {
            let (page, total) = read_set(conn, key, limit).await?;
            let shown = page.len();
            let truncated = page.iter().any(|bytes| bytes.truncated);
            (Value::Set(page), total, shown, truncated)
        }
        KeyKind::Stream => {
            let (page, total) = read_stream(conn, key, limit).await?;
            let shown = page.len();
            let truncated = page
                .iter()
                .any(|entry| entry.fields.iter().any(|(_, bytes)| bytes.truncated));
            (Value::Stream(page), total, shown, truncated)
        }
        KeyKind::Json => {
            let raw: String = redis::cmd("JSON.GET").arg(key).query_async(conn).await?;
            let (text, truncated) = truncate_text(&raw, limit.max_bytes);
            (Value::ReadOnlyText(text), 1, 1, truncated)
        }
        KeyKind::Unknown(name) => {
            // Nenhum comando genérico de leitura existe para um tipo que
            // nem reconhecemos (ex.: um módulo novo) — avisa a UI em vez de
            // tentar adivinhar um comando que pode nem existir.
            let text = format!("(tipo \"{name}\" não suportado para leitura)");
            (Value::ReadOnlyText(text), 1, 1, false)
        }
    };

    let has_more = (limit.offset as u64 + shown as u64) < total_elements;
    let truncated = element_truncated || has_more;

    Ok(ValueView {
        key: key.to_string(),
        kind,
        ttl_ms,
        size_bytes,
        value,
        total_elements,
        truncated,
    })
}

/// `MEMORY USAGE` do tamanho real de `key`, degradando para `0` quando o
/// comando falha (ex.: servidor gerenciado antigo sem suporte) — mesmo
/// comportamento de `core::keys::scan_all`, para que o painel de detalhes
/// nunca trave por causa de uma métrica auxiliar.
async fn memory_usage(conn: &mut MultiplexedConnection, key: &str) -> u64 {
    let usage: redis::RedisResult<i64> = redis::cmd("MEMORY")
        .arg("USAGE")
        .arg(key)
        .query_async(conn)
        .await;

    usage
        .ok()
        .filter(|&bytes| bytes >= 0)
        .map(|bytes| bytes as u64)
        .unwrap_or(0)
}

/// Corta `raw` (um texto que já sabemos ser válido, ex.: `JSON.GET`) no
/// maior limite de caractere que caiba em `max_bytes`, sem partir um
/// caractere multibyte ao meio.
fn truncate_text(raw: &str, max_bytes: usize) -> (String, bool) {
    if raw.len() <= max_bytes {
        return (raw.to_string(), false);
    }
    let mut end = max_bytes;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    (raw[..end].to_string(), true)
}

/// Lê só o começo do valor de uma chave `string`, via `GETRANGE`, em vez do
/// valor inteiro — este é exatamente o caso da "string de megabytes": o
/// painel nunca precisa (nem deveria) puxar o valor inteiro pela rede só
/// para mostrar os primeiros `max_bytes`. `truncated` vem de quantos bytes
/// vieram (`prefix.len()`), não do tamanho do texto decodificado: os dois só
/// coincidem quando o valor inteiro já veio (ver o comentário no branch de
/// `Err` guardado abaixo).
async fn read_string(
    conn: &mut MultiplexedConnection,
    key: &str,
    max_bytes: usize,
) -> redis::RedisResult<Bytes> {
    let fetch_len = max_bytes + UTF8_SLACK;
    let prefix: Vec<u8> = redis::cmd("GETRANGE")
        .arg(key)
        .arg(0)
        .arg(fetch_len as isize - 1)
        .query_async(conn)
        .await?;

    // Se vieram exatamente `fetch_len` bytes, o valor real pode ser maior
    // (GETRANGE corta no que pedimos); se vieram menos, é porque o valor
    // real acabou ali — ou seja, o que veio é o valor inteiro, e sabemos o
    // tamanho real com certeza.
    let truncated = prefix.len() > max_bytes;

    match std::str::from_utf8(&prefix) {
        Ok(full_text) => Ok(Bytes {
            text: Some(char_boundary_prefix(full_text, max_bytes).to_string()),
            base64: None,
            truncated,
        }),
        Err(err) if err.valid_up_to() >= max_bytes.min(prefix.len()) => {
            // O byte inválido está só na folga de `UTF8_SLACK` além de
            // `max_bytes` (ou depois) — o valor é, na verdade, UTF-8 válido
            // até onde precisamos mostrar; o corte do `GETRANGE` é que caiu
            // no meio de um caractere mais adiante, irrelevante para o que
            // será exibido.
            let valid = std::str::from_utf8(&prefix[..err.valid_up_to()])
                .expect("prefixo já validado por from_utf8 acima");
            Ok(Bytes {
                text: Some(char_boundary_prefix(valid, max_bytes).to_string()),
                base64: None,
                truncated,
            })
        }
        Err(_) => {
            // Byte inválido dentro da região que de fato mostraríamos —
            // valor binário de verdade, não um artefato do corte.
            let limited = &prefix[..max_bytes.min(prefix.len())];
            Ok(Bytes {
                text: None,
                base64: Some(BASE64.encode(limited)),
                truncated,
            })
        }
    }
}

/// Pagina um hash via `HSCAN` (não há comando de range por índice para
/// hashes): varre em lotes de `SCAN_COUNT` até reunir pelo menos
/// `offset + max_elements` campos ou o cursor voltar a `0`, depois pula
/// `offset` e corta em `max_elements`. Devolve a página e o total real
/// (`HLEN`), para a UI poder dizer "mostrando N de total".
async fn read_hash(
    conn: &mut MultiplexedConnection,
    key: &str,
    limit: &ValueLimit,
) -> redis::RedisResult<(Vec<(String, Bytes)>, u64)> {
    let total: u64 = redis::cmd("HLEN").arg(key).query_async(conn).await?;

    let needed = limit.offset + limit.max_elements;
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut cursor: u64 = 0;
    loop {
        let (next_cursor, batch): (u64, Vec<Vec<u8>>) = redis::cmd("HSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(SCAN_COUNT)
            .query_async(conn)
            .await?;

        for chunk in batch.chunks(2) {
            if let [field, value] = chunk {
                pairs.push((field.clone(), value.clone()));
            }
        }

        if next_cursor == 0 || pairs.len() >= needed {
            break;
        }
        cursor = next_cursor;
    }

    let page = pairs
        .into_iter()
        .skip(limit.offset)
        .take(limit.max_elements)
        .map(|(field, value)| {
            (
                String::from_utf8_lossy(&field).into_owned(),
                Bytes::from_raw(&value, limit.max_bytes),
            )
        })
        .collect();

    Ok((page, total))
}

/// Pagina um set via `SSCAN`, pelo mesmo motivo e com a mesma estratégia de
/// `read_hash`: não existe comando de range por índice para sets.
async fn read_set(
    conn: &mut MultiplexedConnection,
    key: &str,
    limit: &ValueLimit,
) -> redis::RedisResult<(Vec<Bytes>, u64)> {
    let total: u64 = redis::cmd("SCARD").arg(key).query_async(conn).await?;

    let needed = limit.offset + limit.max_elements;
    let mut members: Vec<Vec<u8>> = Vec::new();
    let mut cursor: u64 = 0;
    loop {
        let (next_cursor, batch): (u64, Vec<Vec<u8>>) = redis::cmd("SSCAN")
            .arg(key)
            .arg(cursor)
            .arg("COUNT")
            .arg(SCAN_COUNT)
            .query_async(conn)
            .await?;

        members.extend(batch);

        if next_cursor == 0 || members.len() >= needed {
            break;
        }
        cursor = next_cursor;
    }

    let page = members
        .into_iter()
        .skip(limit.offset)
        .take(limit.max_elements)
        .map(|member| Bytes::from_raw(&member, limit.max_bytes))
        .collect();

    Ok((page, total))
}

/// Pagina uma lista via `LRANGE`, que endereça por índice diretamente — por
/// isso `offset` aqui é exato (não uma aproximação por varredura, como em
/// `read_hash`/`read_set`).
async fn read_list(
    conn: &mut MultiplexedConnection,
    key: &str,
    limit: &ValueLimit,
) -> redis::RedisResult<(Vec<Bytes>, u64)> {
    let total: u64 = redis::cmd("LLEN").arg(key).query_async(conn).await?;

    let start = limit.offset as isize;
    let stop = start + limit.max_elements as isize - 1;
    let items: Vec<Vec<u8>> = redis::cmd("LRANGE")
        .arg(key)
        .arg(start)
        .arg(stop)
        .query_async(conn)
        .await?;

    let page = items
        .into_iter()
        .map(|item| Bytes::from_raw(&item, limit.max_bytes))
        .collect();

    Ok((page, total))
}

/// Pagina um sorted set via `ZRANGE ... WITHSCORES`, que também endereça
/// por índice (rank) diretamente, com o mesmo `offset` exato de `read_list`.
async fn read_zset(
    conn: &mut MultiplexedConnection,
    key: &str,
    limit: &ValueLimit,
) -> redis::RedisResult<(Vec<(Bytes, f64)>, u64)> {
    let total: u64 = redis::cmd("ZCARD").arg(key).query_async(conn).await?;

    let start = limit.offset as isize;
    let stop = start + limit.max_elements as isize - 1;
    let items: Vec<(Vec<u8>, f64)> = redis::cmd("ZRANGE")
        .arg(key)
        .arg(start)
        .arg(stop)
        .arg("WITHSCORES")
        .query_async(conn)
        .await?;

    let page = items
        .into_iter()
        .map(|(member, score)| (Bytes::from_raw(&member, limit.max_bytes), score))
        .collect();

    Ok((page, total))
}

/// Pagina uma stream via `XRANGE - + COUNT`: não há um `SKIP`/`OFFSET` nativo
/// para streams (a "posição" de uma entrada é o id, não um índice), então
/// pedimos `offset + max_elements` entradas desde o início e cortamos as
/// primeiras `offset` no lado da aplicação — mesma estratégia de
/// `read_hash`/`read_set`, adaptada a um comando de range em vez de scan.
async fn read_stream(
    conn: &mut MultiplexedConnection,
    key: &str,
    limit: &ValueLimit,
) -> redis::RedisResult<(Vec<StreamEntry>, u64)> {
    let total: u64 = redis::cmd("XLEN").arg(key).query_async(conn).await?;

    let needed = limit.offset + limit.max_elements;
    let raw: Vec<(String, Vec<Vec<u8>>)> = redis::cmd("XRANGE")
        .arg(key)
        .arg("-")
        .arg("+")
        .arg("COUNT")
        .arg(needed)
        .query_async(conn)
        .await?;

    let page = raw
        .into_iter()
        .skip(limit.offset)
        .take(limit.max_elements)
        .map(|(id, fields)| {
            let fields = fields
                .chunks(2)
                .filter_map(|chunk| match chunk {
                    [name, value] => Some((
                        String::from_utf8_lossy(name).into_owned(),
                        Bytes::from_raw(value, limit.max_bytes),
                    )),
                    _ => None,
                })
                .collect();
            StreamEntry { id, fields }
        })
        .collect();

    Ok((page, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Env;

    fn test_host() -> Host {
        Host {
            id: "value-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            // db2: db0 é do teste de `conn::db_sizes_counts_keys_in_db0` e
            // db1 é do teste de varredura de `keys.rs`; a suíte roda em
            // paralelo contra o mesmo container, então cada módulo de teste
            // precisa do seu próprio banco.
            db: 2,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    async fn del(conn: &mut MultiplexedConnection, keys: &[&str]) {
        for key in keys {
            let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        }
    }

    #[tokio::test]
    async fn reads_each_editable_type() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "value_rs_test:editable";
        let string_key = format!("{prefix}:string");
        let hash_key = format!("{prefix}:hash");
        let list_key = format!("{prefix}:list");
        let zset_key = format!("{prefix}:zset");

        let _: () = redis::cmd("SET")
            .arg(&string_key)
            .arg("valor-string")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(&string_key)
            .arg(60)
            .query_async(&mut conn)
            .await
            .unwrap();

        let _: () = redis::cmd("HSET")
            .arg(&hash_key)
            .arg("campo1")
            .arg("valor1")
            .arg("campo2")
            .arg("valor2")
            .query_async(&mut conn)
            .await
            .unwrap();

        let _: () = redis::cmd("RPUSH")
            .arg(&list_key)
            .arg("a")
            .arg("b")
            .arg("c")
            .query_async(&mut conn)
            .await
            .unwrap();

        let _: () = redis::cmd("ZADD")
            .arg(&zset_key)
            .arg(1)
            .arg("um")
            .arg(2)
            .arg("dois")
            .query_async(&mut conn)
            .await
            .unwrap();

        let string_view = read_value(
            &pool,
            &host,
            None,
            host.db,
            &string_key,
            ValueLimit::default(),
        )
        .await
        .unwrap();
        let hash_view = read_value(
            &pool,
            &host,
            None,
            host.db,
            &hash_key,
            ValueLimit::default(),
        )
        .await
        .unwrap();
        let list_view = read_value(
            &pool,
            &host,
            None,
            host.db,
            &list_key,
            ValueLimit::default(),
        )
        .await
        .unwrap();
        let zset_view = read_value(
            &pool,
            &host,
            None,
            host.db,
            &zset_key,
            ValueLimit::default(),
        )
        .await
        .unwrap();

        del(&mut conn, &[&string_key, &hash_key, &list_key, &zset_key]).await;

        assert_eq!(string_view.kind, KeyKind::String);
        assert!(string_view.kind.editable());
        match string_view.value {
            Value::Str(bytes) => assert_eq!(bytes.text.as_deref(), Some("valor-string")),
            other => panic!("esperava Value::Str, veio {other:?}"),
        }
        assert!(
            matches!(string_view.ttl_ms, Some(ms) if ms > 0),
            "esperava TTL positivo, veio {:?}",
            string_view.ttl_ms
        );
        assert!(!string_view.truncated);

        assert_eq!(hash_view.kind, KeyKind::Hash);
        assert!(hash_view.kind.editable());
        assert_eq!(hash_view.ttl_ms, None);
        assert_eq!(hash_view.total_elements, 2);
        match hash_view.value {
            Value::Hash(pairs) => {
                assert_eq!(pairs.len(), 2);
                let find = |field: &str| {
                    pairs
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, bytes)| bytes.text.clone())
                };
                assert_eq!(find("campo1"), Some(Some("valor1".to_string())));
                assert_eq!(find("campo2"), Some(Some("valor2".to_string())));
            }
            other => panic!("esperava Value::Hash, veio {other:?}"),
        }

        assert_eq!(list_view.kind, KeyKind::List);
        assert!(list_view.kind.editable());
        assert_eq!(list_view.total_elements, 3);
        match list_view.value {
            Value::List(items) => {
                let texts: Vec<Option<String>> = items.into_iter().map(|b| b.text).collect();
                assert_eq!(
                    texts,
                    vec![
                        Some("a".to_string()),
                        Some("b".to_string()),
                        Some("c".to_string())
                    ]
                );
            }
            other => panic!("esperava Value::List, veio {other:?}"),
        }

        assert_eq!(zset_view.kind, KeyKind::ZSet);
        assert!(zset_view.kind.editable());
        assert_eq!(zset_view.total_elements, 2);
        match zset_view.value {
            Value::ZSet(items) => {
                let pairs: Vec<(Option<String>, f64)> = items
                    .into_iter()
                    .map(|(bytes, score)| (bytes.text, score))
                    .collect();
                assert_eq!(
                    pairs,
                    vec![
                        (Some("um".to_string()), 1.0),
                        (Some("dois".to_string()), 2.0)
                    ]
                );
            }
            other => panic!("esperava Value::ZSet, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn reads_set_and_stream_as_read_only() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "value_rs_test:read_only";
        let set_key = format!("{prefix}:set");
        let stream_key = format!("{prefix}:stream");

        let _: () = redis::cmd("SADD")
            .arg(&set_key)
            .arg("membro1")
            .arg("membro2")
            .query_async(&mut conn)
            .await
            .unwrap();

        let _: String = redis::cmd("XADD")
            .arg(&stream_key)
            .arg("*")
            .arg("campo")
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();

        let set_view = read_value(&pool, &host, None, host.db, &set_key, ValueLimit::default())
            .await
            .unwrap();
        let stream_view = read_value(
            &pool,
            &host,
            None,
            host.db,
            &stream_key,
            ValueLimit::default(),
        )
        .await
        .unwrap();

        del(&mut conn, &[&set_key, &stream_key]).await;

        assert_eq!(set_view.kind, KeyKind::Set);
        assert!(!set_view.kind.editable());
        assert_eq!(set_view.total_elements, 2);
        match set_view.value {
            Value::Set(members) => {
                let texts: Vec<Option<String>> = members.into_iter().map(|b| b.text).collect();
                assert!(texts.contains(&Some("membro1".to_string())));
                assert!(texts.contains(&Some("membro2".to_string())));
            }
            other => panic!("esperava Value::Set, veio {other:?}"),
        }

        assert_eq!(stream_view.kind, KeyKind::Stream);
        assert!(!stream_view.kind.editable());
        assert_eq!(stream_view.total_elements, 1);
        match stream_view.value {
            Value::Stream(entries) => {
                assert_eq!(entries.len(), 1);
                let entry = &entries[0];
                assert!(!entry.id.is_empty());
                let field = entry
                    .fields
                    .iter()
                    .find(|(name, _)| name == "campo")
                    .expect("campo ausente na entrada da stream");
                assert_eq!(field.1.text.as_deref(), Some("valor"));
            }
            other => panic!("esperava Value::Stream, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_key_is_key_gone() {
        let pool = Pool::new();
        let host = test_host();
        let key = "value_rs_test:missing:does_not_exist";

        let err = read_value(&pool, &host, None, host.db, key, ValueLimit::default())
            .await
            .unwrap_err();

        match err {
            ConnError::KeyGone(ref name) => assert_eq!(name, key),
            other => panic!("esperava ConnError::KeyGone, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn huge_string_is_truncated_and_flagged() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:huge_string";
        let huge_value = "a".repeat(2_000_000);

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&huge_value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let view = read_value(&pool, &host, None, host.db, key, ValueLimit::default()).await;

        del(&mut conn, &[key]).await;

        let view = view.unwrap();

        assert!(view.truncated, "ValueView.truncated deveria ser true");
        assert!(
            view.size_bytes >= 2_000_000,
            "size_bytes deveria preservar o tamanho real (~2MB), veio {}",
            view.size_bytes
        );
        match view.value {
            Value::Str(bytes) => {
                assert!(bytes.truncated, "Bytes.truncated deveria ser true");
                let text = bytes.text.expect("esperava texto, já que o valor é ASCII");
                assert!(
                    text.len() <= ValueLimit::default().max_bytes,
                    "texto devolvido deveria caber em max_bytes, veio {} bytes",
                    text.len()
                );
            }
            other => panic!("esperava Value::Str, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_byte_cutoff_is_truncated_at_a_clean_char_boundary() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:multibyte_cutoff";
        // "café" ocupa 5 bytes em UTF-8 (c, a, f e um "é" de 2 bytes) — toda
        // string com acentuação em português é feita desses caracteres de
        // 2 bytes, então o corte do painel cair bem no meio de um deles não
        // é um caso exótico, é rotina. "café" repetido 6 vezes dá 30 bytes;
        // `max_bytes = 19` cai exatamente no primeiro byte do "é" da quarta
        // repetição (bytes 18 e 19 do valor) — esse "é" começa em
        // `max_bytes - 1`, o pior caso que `UTF8_SLACK` existe para cobrir.
        //
        // Sem folga nenhuma (`UTF8_SLACK = 0`), o `GETRANGE` pararia bem
        // nesse primeiro byte do "é", `from_utf8` falharia no meio de um
        // caractere que é válido no valor inteiro, e o branch de
        // `read_string` cairia no `Err(_)` final — tratando como binário um
        // valor que é só texto em português cortado no lugar errado. Esse é
        // o teste que fica vermelho se alguém reduzir `UTF8_SLACK`.
        let value = "café".repeat(6);

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let limit = ValueLimit {
            max_bytes: 19,
            ..ValueLimit::default()
        };
        let view = read_value(&pool, &host, None, host.db, key, limit).await;

        del(&mut conn, &[key]).await;

        let view = view.unwrap();

        assert!(view.truncated);
        match view.value {
            Value::Str(bytes) => {
                assert!(
                    bytes.base64.is_none(),
                    "não pode virar base64 por causa do corte"
                );
                let text = bytes.text.expect(
                    "corte no meio de um caractere multibyte não pode classificar o valor como binário",
                );
                assert!(
                    text.len() <= limit.max_bytes,
                    "texto devolvido deveria caber em max_bytes, veio {} bytes",
                    text.len()
                );
                assert!(
                    !text.contains('\u{FFFD}'),
                    "texto não pode conter caractere de substituição (U+FFFD)"
                );
                assert_eq!(
                    text, "cafécafécafécaf",
                    "deveria incluir as 3 repetições completas de \"café\" mais o \"caf\" \
                     da quarta, sem o \"é\" cortado ao meio"
                );
                assert!(bytes.truncated);
            }
            other => panic!("esperava Value::Str, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_utf8_value_comes_back_as_base64() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:non_utf8";
        let raw: &[u8] = &[0xff, 0xfe];

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(raw)
            .query_async(&mut conn)
            .await
            .unwrap();

        let view = read_value(&pool, &host, None, host.db, key, ValueLimit::default()).await;

        del(&mut conn, &[key]).await;

        let view = view.unwrap();

        match view.value {
            Value::Str(bytes) => {
                assert!(
                    bytes.text.is_none(),
                    "text deveria ser None para bytes inválidos em UTF-8"
                );
                let encoded = bytes.base64.expect("base64 deveria estar preenchido");
                let decoded = BASE64.decode(&encoded).unwrap();
                assert_eq!(
                    decoded, raw,
                    "os bytes originais precisam sobreviver intactos"
                );
            }
            other => panic!("esperava Value::Str, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn offset_returns_the_next_page_of_elements() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:offset_list";
        let total_items = 1200;
        let mut cmd = redis::cmd("RPUSH");
        cmd.arg(key);
        for i in 0..total_items {
            cmd.arg(format!("item_{i}"));
        }
        let _: () = cmd.query_async(&mut conn).await.unwrap();

        let limit = ValueLimit {
            offset: 500,
            ..ValueLimit::default()
        };
        let view = read_value(&pool, &host, None, host.db, key, limit).await;

        del(&mut conn, &[key]).await;

        let view = view.unwrap();

        assert_eq!(view.total_elements, total_items as u64);
        match view.value {
            Value::List(items) => {
                assert_eq!(items.len(), limit.max_elements);
                assert_eq!(items[0].text.as_deref(), Some("item_500"));
                assert_eq!(items[1].text.as_deref(), Some("item_501"));
            }
            other => panic!("esperava Value::List, veio {other:?}"),
        }
    }
}
