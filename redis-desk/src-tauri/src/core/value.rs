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

    /// Bytes crus prontos para mandar ao Redis — o inverso de `from_raw`,
    /// usado pela escrita (Task 5). Prefere `text` (o caminho comum: a UI
    /// edita um campo como texto) e só decodifica `base64` quando não há
    /// texto, para reabrir um valor binário sem passar por uma conversão de
    /// texto que corromperia bytes que não são UTF-8 válido. Um `Bytes`
    /// nem `text` nem `base64` preenchidos (não deveria acontecer vindo da
    /// UI, que sempre preenche um dos dois) vira bytes vazios em vez de
    /// entrar em pânico — o pior caso é gravar um valor vazio, não derrubar
    /// o processo por causa de um dado malformado.
    fn to_raw(&self) -> Vec<u8> {
        if let Some(text) = &self.text {
            text.as_bytes().to_vec()
        } else if let Some(encoded) = &self.base64 {
            BASE64.decode(encoded).unwrap_or_default()
        } else {
            Vec::new()
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

/// Uma mudança tipada no conteúdo ou no TTL de uma chave editável. Todo
/// caminho que grava dado neste produto — o editor da UI, o desfazer da
/// Task 6 e as escritas controladas da LLM na Task 16 — passa por aqui, nunca
/// por um comando Redis solto montado em outro lugar.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOp {
    SetString { value: Bytes },
    SetHashField { field: String, value: Bytes },
    DeleteHashField { field: String },
    SetListItem { index: i64, value: Bytes },
    PushListItem { value: Bytes, front: bool },
    RemoveListItem { index: i64 },
    SetZSetMember { member: Bytes, score: f64 },
    RemoveZSetMember { member: Bytes },
    SetTtl { ttl_ms: Option<i64> },
    DeleteKey,
}

/// Sentinela usado só durante `RemoveListItem`, para marcar com `LSET` o
/// item no índice alvo e removê-lo em seguida com `LREM` (ver
/// `remove_list_item`). Fixo (não gerado por request) porque o algoritmo não
/// precisa de aleatoriedade de verdade — só precisa de um valor que nenhum
/// dado real escrito por um humano ou por este produto jamais vai igualar
/// por acidente. O prefixo identifica a ferramenta e o UUID embutido é
/// estático (não é gerado a cada chamada): mesmo assim, a chance de uma
/// string de usuário colidir byte a byte com um UUID v4 de 128 bits
/// prefixado é desprezível (2^-122), e o próprio texto deixa claro, a quem
/// inspecionar a lista no meio da operação, que aquele item é um marcador
/// interno transitório, não dado de produção.
const REMOVE_LIST_SENTINEL: &str =
    "\u{0}__redis-desk:tombstone:f47ac10b-58cc-4372-a567-0e02b2c3d479__";

/// Aplica `op` sobre `key`. A chave mantém o TTL que já tinha, exceto quando
/// a própria operação é `SetTtl` ou `DeleteKey`, que decidem o TTL
/// explicitamente — mas isso não exige nenhuma lógica extra de preservação
/// neste módulo: de todo comando Redis que as variantes de `WriteOp` usam
/// (`HSET`, `HDEL`, `LSET`, `LPUSH`/`RPUSH`, `LREM`, `ZADD`, `ZREM`), só
/// `SET` (usado por `SetString`) apaga o TTL existente por padrão, e
/// `execute_op` já manda esse `SET` com `KEEPTTL`. Os demais operam sobre a
/// estrutura de dados já existente, sem recriar a chave, e por isso nunca
/// tocam no TTL dela — confirmado contra o Redis de teste, comando a
/// comando, antes desta escolha (ver o relatório da task). Chegou-se antes
/// a fazer essa preservação "por fora" (ler o TTL, escrever, reaplicar com
/// `PEXPIRE`), mas isso abre uma janela real de dano: se a escrita for
/// aplicada e só o `PEXPIRE` seguinte falhar, `apply_write` devolve `Err`
/// com a chave já mudada e já sem TTL — um erro que diz "nada aconteceu"
/// exatamente quando algo aconteceu e piorou. Depender do próprio comando
/// Redis preservar (ou não) o TTL elimina essa janela por construção, às
/// custas de precisar desta verificação manual uma única vez.
///
/// Tipo não editável (`Set`, `Stream`, `RedisJSON` ou um tipo desconhecido)
/// é recusado antes de qualquer tentativa de escrita — `KeyKind::editable`
/// é quem decide. Isso vale para toda operação, incluindo `SetTtl` e
/// `DeleteKey`: este produto trata esses tipos como somente leitura de
/// ponta a ponta, não só o conteúdo deles.
///
/// `RemoveListItem` e `PushListItem` não passam pelo retry automático de
/// `Pool::with_retry` — ver `Pool::run_once` para o motivo (reenviar um
/// `LSET`/`LPUSH`/`RPUSH` depois de um erro de conexão pode acertar o
/// elemento errado ou duplicar o item, se a primeira tentativa na verdade
/// já tiver sido aplicada no servidor).
pub async fn apply_write(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
    op: &WriteOp,
) -> Result<(), ConnError> {
    let type_name: String = pool
        .with_retry(host, db, pw, |mut conn| async move {
            let type_name: String = redis::cmd("TYPE").arg(key).query_async(&mut conn).await?;
            Ok(type_name)
        })
        .await?;

    if type_name != "none" {
        let kind = KeyKind::from_type_name(&type_name);
        if !kind.editable() {
            return Err(ConnError::NotEditable {
                key: key.to_string(),
                kind: type_name,
            });
        }
    }

    let is_retry_unsafe = matches!(
        op,
        WriteOp::RemoveListItem { .. } | WriteOp::PushListItem { .. }
    );

    if is_retry_unsafe {
        pool.run_once(host, db, pw, key, |mut conn| {
            let op = op.clone();
            async move { execute_op(&mut conn, key, &op).await }
        })
        .await
    } else {
        pool.with_retry(host, db, pw, |mut conn| {
            let op = op.clone();
            async move { execute_op(&mut conn, key, &op).await }
        })
        .await
    }
}

/// Executa o comando Redis de `op`, assumindo que o tipo da chave já foi
/// validado como editável por quem chama (`apply_write`).
async fn execute_op(
    conn: &mut MultiplexedConnection,
    key: &str,
    op: &WriteOp,
) -> redis::RedisResult<()> {
    match op {
        WriteOp::SetString { value } => {
            // `KEEPTTL`: sem essa opção, `SET` apaga o TTL existente da
            // chave por padrão — é o único comando entre as variantes de
            // `WriteOp` que faz isso (ver o comentário de `apply_write`).
            // Sobre uma chave nova (sem TTL ainda), `KEEPTTL` não tem
            // efeito — não há TTL para manter.
            redis::cmd("SET")
                .arg(key)
                .arg(value.to_raw())
                .arg("KEEPTTL")
                .query_async::<()>(conn)
                .await
        }
        WriteOp::SetHashField { field, value } => {
            redis::cmd("HSET")
                .arg(key)
                .arg(field)
                .arg(value.to_raw())
                .query_async::<()>(conn)
                .await
        }
        WriteOp::DeleteHashField { field } => {
            redis::cmd("HDEL")
                .arg(key)
                .arg(field)
                .query_async::<()>(conn)
                .await
        }
        WriteOp::SetListItem { index, value } => {
            redis::cmd("LSET")
                .arg(key)
                .arg(*index)
                .arg(value.to_raw())
                .query_async::<()>(conn)
                .await
        }
        WriteOp::PushListItem { value, front } => {
            let cmd_name = if *front { "LPUSH" } else { "RPUSH" };
            redis::cmd(cmd_name)
                .arg(key)
                .arg(value.to_raw())
                .query_async::<()>(conn)
                .await
        }
        WriteOp::RemoveListItem { index } => remove_list_item(conn, key, *index).await,
        WriteOp::SetZSetMember { member, score } => {
            redis::cmd("ZADD")
                .arg(key)
                .arg(score)
                .arg(member.to_raw())
                .query_async::<()>(conn)
                .await
        }
        WriteOp::RemoveZSetMember { member } => {
            redis::cmd("ZREM")
                .arg(key)
                .arg(member.to_raw())
                .query_async::<()>(conn)
                .await
        }
        WriteOp::SetTtl { ttl_ms } => set_ttl(conn, key, *ttl_ms).await,
        WriteOp::DeleteKey => redis::cmd("DEL").arg(key).query_async::<()>(conn).await,
    }
}

/// `PEXPIRE` quando há um TTL novo, `PERSIST` quando `ttl_ms` é `None` — os
/// dois comandos que, juntos, cobrem toda mudança de TTL que a UI oferece.
async fn set_ttl(
    conn: &mut MultiplexedConnection,
    key: &str,
    ttl_ms: Option<i64>,
) -> redis::RedisResult<()> {
    match ttl_ms {
        Some(ms) => {
            redis::cmd("PEXPIRE")
                .arg(key)
                .arg(ms)
                .query_async::<()>(conn)
                .await
        }
        None => redis::cmd("PERSIST").arg(key).query_async::<()>(conn).await,
    }
}

/// Remove o item de `key` no índice `index`. O Redis não tem um comando de
/// "apagar elemento de lista por índice" — `LREM` remove por VALOR, `LSET`
/// substitui por índice — então a técnica é: marcar o item alvo (por
/// índice, via `LSET`) com `REMOVE_LIST_SENTINEL`, depois remover esse
/// sentinela (por valor, via `LREM count=0`, que apaga todas as ocorrências,
/// mas só deve haver a que acabamos de marcar). As duas idas ao Redis
/// acontecem dentro de um `MULTI`/`EXEC` (`Pipeline::atomic`) para que
/// nenhuma outra escrita concorrente consiga enxergar ou reagir ao estado
/// intermediário em que o sentinela está visível na lista.
async fn remove_list_item(
    conn: &mut MultiplexedConnection,
    key: &str,
    index: i64,
) -> redis::RedisResult<()> {
    let mut pipe = redis::pipe();
    pipe.atomic();
    pipe.cmd("LSET")
        .arg(key)
        .arg(index)
        .arg(REMOVE_LIST_SENTINEL);
    pipe.cmd("LREM").arg(key).arg(0).arg(REMOVE_LIST_SENTINEL);
    pipe.query_async::<()>(conn).await
}

/// Representação curta de um `Bytes` para aparecer numa frase de resumo:
/// o texto quando há, ou um aviso quando o valor é binário (sem texto
/// decodificável) — nunca o base64 cru, que não ajudaria quem está lendo o
/// diálogo de confirmação.
fn bytes_label(bytes: &Bytes) -> String {
    bytes
        .text
        .clone()
        .unwrap_or_else(|| "(valor binário)".to_string())
}

/// Frase em português para o diálogo de confirmação de escrita — é o texto
/// que a pessoa lê antes de autorizar a mudança em produção, então a
/// redação aqui é copy de produto, não uma string de debug.
pub fn op_summary(key: &str, op: &WriteOp) -> String {
    match op {
        WriteOp::SetString { .. } => format!("Definir o valor de {key}"),
        WriteOp::SetHashField { field, .. } => {
            format!("Definir o campo \"{field}\" de {key}")
        }
        WriteOp::DeleteHashField { field } => {
            format!("Apagar o campo \"{field}\" de {key}")
        }
        WriteOp::SetListItem { index, .. } => {
            format!("Definir o item de índice {index} de {key}")
        }
        WriteOp::PushListItem { front, .. } => {
            let posicao = if *front { "no início" } else { "no final" };
            format!("Adicionar um item {posicao} de {key}")
        }
        WriteOp::RemoveListItem { index } => {
            format!("Remover o item de índice {index} de {key}")
        }
        WriteOp::SetZSetMember { member, score } => {
            format!(
                "Definir o membro \"{}\" de {key} com score {score}",
                bytes_label(member)
            )
        }
        WriteOp::RemoveZSetMember { member } => {
            format!("Remover o membro \"{}\" de {key}", bytes_label(member))
        }
        WriteOp::SetTtl { ttl_ms: Some(ms) } => {
            format!("Definir o TTL de {key} para {ms} ms")
        }
        WriteOp::SetTtl { ttl_ms: None } => {
            format!("Tornar {key} persistente (remover o TTL)")
        }
        WriteOp::DeleteKey => format!("Apagar a chave {key}"),
    }
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
    async fn four_byte_cutoff_requires_the_full_utf8_slack() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:four_byte_cutoff";
        // "😀" ocupa 4 bytes em UTF-8 — o pior caso que `UTF8_SLACK` precisa
        // cobrir (um emoji num JSON cacheado é um valor comum, não exótico,
        // neste produto). 9 "a"s, depois o emoji, depois mais "a"s: o emoji
        // começa bem no byte 9 (`max_bytes - 1`, com `max_bytes = 10`) e
        // precisa dos 3 bytes seguintes (9, 10, 11... o emoji ocupa
        // [9, 13)) para completar — exatamente os 3 bytes de
        // `UTF8_SLACK`. Com `UTF8_SLACK = 2` (ou menos), o `GETRANGE` para
        // no meio do emoji, `from_utf8` falha antes de alcançar
        // `max_bytes`, e o valor seria classificado (errado) como binário.
        let value = format!("{}{}{}", "a".repeat(9), "😀", "a".repeat(20));

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let limit = ValueLimit {
            max_bytes: 10,
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
                    "corte no meio de um caractere de 4 bytes não pode classificar o valor como binário",
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
                // O emoji não cabe inteiro em `max_bytes` (começa no último
                // byte do orçamento) — o corte correto é excluí-lo por
                // completo, não mostrar metade dele.
                assert_eq!(
                    text,
                    "a".repeat(9),
                    "o emoji que começa em max_bytes-1 não pode aparecer pela metade"
                );
                assert!(bytes.truncated);
            }
            other => panic!("esperava Value::Str, veio {other:?}"),
        }
    }

    #[tokio::test]
    async fn four_byte_char_cut_by_the_fetch_window_recovers_as_text() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "value_rs_test:four_byte_fetch_cut";
        // Diferença crucial para o teste anterior (`four_byte_cutoff_...`):
        // lá o emoji começava em `max_bytes - 1` e cabia inteiro dentro de
        // `fetch_len = max_bytes + UTF8_SLACK`, então `from_utf8` tinha
        // sucesso direto (`Ok`) e quem fazia o trabalho era
        // `char_boundary_prefix` recuando no limite de exibição — cobertura
        // real, mas não a do branch de recuperação.
        //
        // Aqui o emoji começa exatamente em `max_bytes` (byte 10): a janela
        // de BUSCA (`fetch_len = 10 + 3 = 13`) só alcança os 3 primeiros dos
        // 4 bytes do emoji (bytes 10, 11, 12), faltando o byte 13. Isso faz
        // `from_utf8(&prefix)` falhar com `valid_up_to() == 10`, batendo
        // exatamente em `max_bytes.min(prefix.len()) == 10` — o caso em que
        // o branch `Err(err) if err.valid_up_to() >= max_bytes.min(...)`
        // (value.rs, dentro de `read_string`) precisa reconhecer que o
        // valor É válido em UTF-8 até onde importa, e não classificá-lo como
        // binário só porque a busca (não a exibição) cortou um caractere.
        let value = format!("{}{}{}", "a".repeat(10), "😀", "a".repeat(20));

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let limit = ValueLimit {
            max_bytes: 10,
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
                    "corte na janela de busca não pode virar base64 — o valor é válido em UTF-8"
                );
                let text = bytes.text.expect(
                    "corte do GETRANGE no meio do emoji não pode classificar o valor como binário",
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
                    text,
                    "a".repeat(10),
                    "deveria mostrar só os 10 \"a\"s antes do emoji, sem fragmento dele"
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

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::core::config::Env;

    fn text(value: &str) -> Bytes {
        Bytes {
            text: Some(value.to_string()),
            base64: None,
            truncated: false,
        }
    }

    fn test_host() -> Host {
        Host {
            id: "value-write-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            // db3: db0 é do teste de `conn::db_sizes_counts_keys_in_db0`,
            // db1 é do teste de varredura de `keys.rs` e db2 é dos testes de
            // leitura logo acima neste arquivo; a suíte roda em paralelo
            // contra o mesmo container, então a escrita precisa do seu
            // próprio banco.
            db: 3,
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
    async fn set_string_replaces_the_value() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:set_string";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-antigo")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::SetString {
                value: text("valor-novo"),
            },
        )
        .await;

        let stored: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert_eq!(stored, "valor-novo");
    }

    #[tokio::test]
    async fn editing_a_key_preserves_its_ttl() {
        // Nome e forma vêm do brief da task ao pé da letra, mas vale
        // registrar o que este teste PROVA de fato: que o caminho de
        // escrita de `SetHashField` funciona de ponta a ponta sobre uma
        // chave com TTL (continua lá depois). Não é a prova de que o
        // módulo preserva TTL em geral — `HSET` nunca derruba TTL por
        // conta própria no Redis, então este teste continuaria verde
        // mesmo sem nenhuma lógica de preservação (confirmado na prática
        // durante o self-review desta task). A prova real de que o risco
        // descrito no brief está coberto é
        // `set_string_preserves_ttl_even_though_set_drops_it_natively`,
        // logo abaixo — `SET` é o único comando usado por este módulo que
        // de fato apaga o TTL por padrão.
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:preserve_ttl";

        let _: () = redis::cmd("HSET")
            .arg(key)
            .arg("campo1")
            .arg("valor1")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(100)
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::SetHashField {
                field: "campo2".to_string(),
                value: text("valor2"),
            },
        )
        .await;

        let pttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert!(
            pttl > 90_000 && pttl <= 100_000,
            "esperava PTTL perto de 100s (100000ms) preservado, veio {pttl}ms"
        );
    }

    #[tokio::test]
    async fn set_string_preserves_ttl_even_though_set_drops_it_natively() {
        // `editing_a_key_preserves_its_ttl` acima usa `SetHashField`, mas
        // `HSET` nunca derruba o TTL por conta própria — esse teste passaria
        // mesmo sem nenhuma lógica de preservação. O risco real que o
        // módulo existe para evitar é outro: `SET` (`WriteOp::SetString`)
        // substitui a chave inteira e, sem `KEEPTTL`, apaga o TTL existente
        // por padrão no próprio Redis. Este teste exercita exatamente esse
        // caminho.
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:set_string_preserves_ttl";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-antigo")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(100)
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::SetString {
                value: text("valor-novo"),
            },
        )
        .await;

        let pttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert!(
            pttl > 90_000 && pttl <= 100_000,
            "esperava PTTL perto de 100s (100000ms) preservado mesmo após SET, veio {pttl}ms"
        );
    }

    #[tokio::test]
    async fn set_ttl_none_makes_the_key_persistent() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:set_ttl_none";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(100)
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::SetTtl { ttl_ms: None },
        )
        .await;

        let pttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert_eq!(
            pttl, -1,
            "esperava chave persistente (PTTL == -1), veio {pttl}"
        );
    }

    #[tokio::test]
    async fn delete_key_removes_it() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:delete_key";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(&pool, &host, None, host.db, key, &WriteOp::DeleteKey).await;

        let exists: bool = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert!(!exists, "a chave deveria ter sido apagada");
    }

    #[tokio::test]
    async fn remove_list_item_by_index_keeps_order() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:remove_list_item";

        let _: () = redis::cmd("RPUSH")
            .arg(key)
            .arg("a")
            .arg("b")
            .arg("c")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::RemoveListItem { index: 1 },
        )
        .await;

        let items: Vec<String> = redis::cmd("LRANGE")
            .arg(key)
            .arg(0)
            .arg(-1)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        result.unwrap();
        assert_eq!(items, vec!["a".to_string(), "c".to_string()]);
    }

    // O comportamento de "não reenviar RemoveListItem/PushListItem depois
    // de um erro de classe conexão" (achado "Important 1" da revisão) é
    // pinado em `core::conn::tests::run_once_reports_ambiguous_without_retrying_a_dead_connection`,
    // não aqui. Tentei primeiro simular isso a partir de `apply_write`
    // (matando a conexão cacheada antes de chamar `apply_write`), mas
    // `apply_write` sempre começa com um `TYPE key` via `with_retry` — que
    // tem o próprio retry automático e já reconecta sozinho antes de
    // chegar em `RemoveListItem`/`PushListItem`. Por isso, matar a conexão
    // antes de uma chamada a `apply_write` nunca alcança o caminho sem
    // retry: a cura acontece silenciosamente no passo do `TYPE`, e
    // `run_once` só veria uma conexão morta se ela caísse bem na janela
    // entre o `TYPE` e a escrita — uma corrida não determinística,
    // impossível de forçar de fora. `Pool::run_once` é testado isolado,
    // pelo mesmo padrão que `conn::tests::ping_evicts_a_dead_cached_connection_and_retries`
    // já usa para testar o retry de `with_retry`.

    #[tokio::test]
    async fn writing_a_set_is_refused() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "value_rs_write_test:refuse_set";

        let _: () = redis::cmd("SADD")
            .arg(key)
            .arg("membro")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = apply_write(
            &pool,
            &host,
            None,
            host.db,
            key,
            &WriteOp::SetString { value: text("x") },
        )
        .await;

        let unchanged: Vec<String> = redis::cmd("SMEMBERS")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        match result {
            Err(ConnError::NotEditable { key: k, kind }) => {
                assert_eq!(k, key);
                assert_eq!(kind, "set");
            }
            other => panic!("esperava ConnError::NotEditable, veio {other:?}"),
        }
        assert_eq!(
            unchanged,
            vec!["membro".to_string()],
            "o Set não pode ter sido tocado pela escrita recusada"
        );
    }

    #[test]
    fn op_summary_is_written_in_portuguese() {
        let key = "session:user:4471";
        let summary = op_summary(
            key,
            &WriteOp::DeleteHashField {
                field: "email".to_string(),
            },
        );

        assert!(summary.contains(key), "resumo deveria citar a chave");
        assert!(
            summary.contains("Apagar"),
            "resumo deveria conter o verbo da ação"
        );
        assert_eq!(summary, "Apagar o campo \"email\" de session:user:4471");
    }
}
