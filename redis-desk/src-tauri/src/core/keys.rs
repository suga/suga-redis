//! Varredura de chaves com metadados (`scan_all`) e a árvore por prefixo que
//! a coluna do meio da UI renderiza (`build_tree`). `scan_all` nunca usa
//! `KEYS`: a enumeração do banco é sempre via `SCAN` em lotes, para não
//! travar um Redis com muitas chaves numa única chamada.

use redis::aio::MultiplexedConnection;

use super::config::Host;
use super::conn::{ConnError, Pool};

/// `COUNT` pedido a cada chamada de `SCAN`: alto o bastante para que um
/// banco com muitas chaves não precise de milhares de idas e vindas, sem
/// pedir uma passada tão grande que pese no servidor de uma vez só.
const SCAN_COUNT: usize = 500;

/// Tamanho do lote de chaves por pipeline de metadados (`TYPE` + `PTTL` +
/// `MEMORY USAGE`). Lotes menores que o total de chaves evitam um pipeline
/// gigante numa varredura de banco grande.
const METADATA_BATCH: usize = 200;

/// Separador usado para agrupar chaves em árvore por prefixo.
const SEPARATOR: char = ':';

/// Tipo de uma chave Redis, conforme a resposta de `TYPE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyKind {
    String,
    Hash,
    List,
    ZSet,
    Set,
    Stream,
    Json,
    /// Qualquer tipo que `TYPE` devolva e que não reconhecemos (ex.: um
    /// módulo novo registrando seu próprio tipo).
    Unknown(String),
}

impl KeyKind {
    /// Só os tipos que o editor de valor sabe escrever de volta no Redis.
    /// `Set`, `Stream` e `RedisJSON` são somente leitura neste produto —
    /// tasks de edição consultam este predicado antes de oferecer a UI de
    /// edição.
    pub fn editable(&self) -> bool {
        matches!(
            self,
            KeyKind::String | KeyKind::Hash | KeyKind::List | KeyKind::ZSet
        )
    }

    /// Mapeia o nome de tipo devolvido por `TYPE` para `KeyKind`.
    fn from_type_name(name: &str) -> KeyKind {
        match name {
            "string" => KeyKind::String,
            "hash" => KeyKind::Hash,
            "list" => KeyKind::List,
            "zset" => KeyKind::ZSet,
            "set" => KeyKind::Set,
            "stream" => KeyKind::Stream,
            "ReJSON-RL" => KeyKind::Json,
            other => KeyKind::Unknown(other.to_string()),
        }
    }
}

/// Metadados de uma chave coletados durante a varredura.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyMeta {
    pub key: String,
    pub kind: KeyKind,
    /// `None` quando a chave não expira; caso contrário, milissegundos
    /// restantes de vida (vindo de `PTTL`).
    pub ttl_ms: Option<i64>,
    /// Tamanho estimado em bytes (`MEMORY USAGE`). Fica em `0` quando o
    /// comando não está disponível no servidor (ex.: um gerenciado antigo)
    /// — isso nunca derruba a varredura.
    pub size_bytes: u64,
}

/// Varre todas as chaves de `db` no host informado, usando `SCAN` (nunca
/// `KEYS`) e coletando `TYPE`, `PTTL` e `MEMORY USAGE` em pipelines por
/// lote. Passa pelo mesmo caminho de resiliência de `ping`/`db_sizes`: se a
/// conexão cacheada estiver morta, o pool descarta e tenta de novo uma vez.
pub async fn scan_all(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
) -> Result<Vec<KeyMeta>, ConnError> {
    pool.with_retry(host, db, pw, |mut conn| async move {
        scan_once(&mut conn).await
    })
    .await
}

/// Faz uma tentativa completa de varredura sobre uma conexão já aberta no
/// banco certo (a seleção de banco é responsabilidade de `Pool::get` —
/// acontece uma vez, embutida na URL de conexão, não aqui): percorre o
/// cursor do `SCAN` até voltar a `0` e depois busca os metadados de cada
/// chave encontrada em lotes.
async fn scan_once(conn: &mut MultiplexedConnection) -> redis::RedisResult<Vec<KeyMeta>> {
    let mut names: Vec<String> = Vec::new();
    let mut cursor: u64 = 0;
    loop {
        let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("COUNT")
            .arg(SCAN_COUNT)
            .query_async(conn)
            .await?;

        names.extend(batch);

        if next_cursor == 0 {
            break;
        }
        cursor = next_cursor;
    }

    let mut metas = Vec::with_capacity(names.len());
    for chunk in names.chunks(METADATA_BATCH) {
        let mut pipe = redis::pipe();
        // Com `ignore_errors`, uma `MEMORY USAGE` que falhe (comando
        // ausente num servidor gerenciado antigo) não derruba o lote
        // inteiro — só aquela resposta individual vira `Err` abaixo.
        pipe.ignore_errors();
        for name in chunk {
            pipe.cmd("TYPE").arg(name);
            pipe.cmd("PTTL").arg(name);
            pipe.cmd("MEMORY").arg("USAGE").arg(name);
        }

        let replies: Vec<redis::RedisResult<redis::Value>> = pipe.query_async(conn).await?;

        for (name, triple) in chunk.iter().zip(replies.chunks(3)) {
            let type_name = triple[0]
                .as_ref()
                .ok()
                .and_then(|value| redis::from_redis_value_ref::<String>(value).ok())
                .unwrap_or_else(|| "none".to_string());

            if type_name == "none" {
                // A chave sumiu entre o SCAN e o TYPE (expirou ou foi
                // apagada por outra conexão nesse meio-tempo) — não entra
                // no resultado em vez de aparecer com metadados inválidos.
                continue;
            }

            let pttl = triple[1]
                .as_ref()
                .ok()
                .and_then(|value| redis::from_redis_value_ref::<i64>(value).ok())
                .unwrap_or(-2);
            let ttl_ms = if pttl >= 0 { Some(pttl) } else { None };

            let size_bytes = triple[2]
                .as_ref()
                .ok()
                .and_then(|value| redis::from_redis_value_ref::<i64>(value).ok())
                .filter(|&bytes| bytes >= 0)
                .map(|bytes| bytes as u64)
                .unwrap_or(0);

            metas.push(KeyMeta {
                key: name.clone(),
                kind: KeyKind::from_type_name(&type_name),
                ttl_ms,
                size_bytes,
            });
        }
    }

    Ok(metas)
}

/// Um galho (ou folha) da árvore de chaves por prefixo. Um nó pode ser as
/// duas coisas ao mesmo tempo: por exemplo, se as chaves `cache` e
/// `cache:a` existem, o nó `cache` tem `leaf_key == Some("cache")` e também
/// um filho `a`.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    pub label: String,
    pub full_prefix: String,
    /// Número de chaves (folhas) dentro desta subárvore, incluindo a
    /// própria folha deste nó, se houver.
    pub count: u64,
    pub children: Vec<TreeNode>,
    /// `Some(chave completa)` quando este nó é (também) uma folha.
    pub leaf_key: Option<String>,
}

/// Constrói a árvore de prefixos (separador `:`) a partir da lista de
/// chaves. Função pura — não depende de Redis nem de I/O.
///
/// Cada chave de entrada vira exatamente uma folha na árvore: dividir por
/// `:` e depois juntar de volta reconstrói a chave original sem perdas,
/// inclusive para nomes degenerados (`""`, `":"`, `"a:"`, `"::b"`), então
/// duas chaves diferentes nunca colidem na mesma folha.
pub fn build_tree(keys: &[KeyMeta]) -> Vec<TreeNode> {
    let entries: Vec<Entry> = keys
        .iter()
        .map(|meta| (meta.key.split(SEPARATOR).collect(), meta.key.as_str()))
        .collect();

    build_level(entries, None)
}

/// Uma chave ainda não totalmente consumida pela recursão: os segmentos que
/// faltam agrupar e a chave original completa (para virar `leaf_key` quando
/// não sobrar mais segmento nenhum).
type Entry<'a> = (Vec<&'a str>, &'a str);

/// Agrupa `entries` (segmentos restantes + chave original) pelo primeiro
/// segmento, construindo um `TreeNode` por grupo e recursando nos segmentos
/// restantes para os filhos. `parent_prefix` é `None` só no nível raiz —
/// nos demais níveis, mesmo um prefixo vazio (`Some("")`) precisa do
/// separador na hora de montar `full_prefix`, para não reescrever `"::b"`
/// como `"b"`.
fn build_level<'a>(entries: Vec<Entry<'a>>, parent_prefix: Option<&str>) -> Vec<TreeNode> {
    // Preserva a ordem de primeira aparição do rótulo para uma saída
    // determinística, usando um vetor de pares em vez de `HashMap` — o
    // número de rótulos por nível é pequeno, então a busca linear não pesa.
    let mut groups: Vec<(&'a str, Vec<Entry<'a>>)> = Vec::new();

    for (segments, full_key) in entries {
        let head = segments[0];
        match groups.iter_mut().find(|(label, _)| *label == head) {
            Some((_, group)) => group.push((segments, full_key)),
            None => groups.push((head, vec![(segments, full_key)])),
        }
    }

    groups
        .into_iter()
        .map(|(label, group)| {
            let full_prefix = match parent_prefix {
                None => label.to_string(),
                Some(parent) => format!("{parent}{SEPARATOR}{label}"),
            };

            let mut leaf_key = None;
            let mut rest_entries = Vec::new();
            for (segments, full_key) in group {
                if segments.len() == 1 {
                    leaf_key = Some(full_key.to_string());
                } else {
                    rest_entries.push((segments[1..].to_vec(), full_key));
                }
            }

            let children = build_level(rest_entries, Some(&full_prefix));
            let count = children.iter().map(|child| child.count).sum::<u64>()
                + u64::from(leaf_key.is_some());

            TreeNode {
                label: label.to_string(),
                full_prefix,
                count,
                children,
                leaf_key,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Env;

    fn meta(key: &str) -> KeyMeta {
        KeyMeta {
            key: key.to_string(),
            kind: KeyKind::String,
            ttl_ms: None,
            size_bytes: 0,
        }
    }

    #[test]
    fn groups_by_colon_prefix() {
        let keys = vec![
            meta("session:user:1"),
            meta("session:user:2"),
            meta("cache:a"),
        ];

        let tree = build_tree(&keys);

        assert_eq!(
            tree.len(),
            2,
            "esperava 2 raízes (session, cache), veio {tree:?}"
        );
        let session = tree
            .iter()
            .find(|node| node.label == "session")
            .expect("raiz \"session\" ausente");
        assert_eq!(session.count, 2);
    }

    #[test]
    fn leaf_carries_the_full_key() {
        let keys = vec![meta("session:user:1"), meta("session:user:2")];

        let tree = build_tree(&keys);

        let session = tree.iter().find(|node| node.label == "session").unwrap();
        let user = session
            .children
            .iter()
            .find(|node| node.label == "user")
            .unwrap();
        let leaf = user.children.iter().find(|node| node.label == "1").unwrap();

        assert_eq!(leaf.leaf_key, Some("session:user:1".to_string()));
        assert_eq!(leaf.full_prefix, "session:user:1");
    }

    #[test]
    fn degenerate_key_names_survive() {
        // Situação 5 do Review Focus: prefixo vazio, chave terminada em
        // `:`, `::` no meio e `:` sozinho. A árvore não pode perder nenhuma
        // delas nem criar um galho vazio sem folha.
        let inputs = ["", ":", "a:", "::b", "plain"];
        let keys: Vec<KeyMeta> = inputs.iter().map(|key| meta(key)).collect();

        let tree = build_tree(&keys);

        fn collect_leaves(nodes: &[TreeNode], out: &mut Vec<String>) {
            for node in nodes {
                if let Some(leaf_key) = &node.leaf_key {
                    out.push(leaf_key.clone());
                }
                assert!(
                    node.leaf_key.is_some() || !node.children.is_empty(),
                    "galho \"{}\" não é folha e não tem filhos — galho fantasma",
                    node.full_prefix
                );
                collect_leaves(&node.children, out);
            }
        }

        let mut leaves = Vec::new();
        collect_leaves(&tree, &mut leaves);
        leaves.sort();

        let mut expected: Vec<String> = inputs.iter().map(|key| key.to_string()).collect();
        expected.sort();

        assert_eq!(
            leaves, expected,
            "cada chave de entrada precisa aparecer como exatamente uma folha, \
             sem ser perdida nem fundida com outra"
        );
    }

    #[test]
    fn counts_roll_up_through_levels() {
        let keys = vec![meta("a:b:c"), meta("a:b:d"), meta("a:x")];

        let tree = build_tree(&keys);

        let a = tree.iter().find(|node| node.label == "a").unwrap();
        assert_eq!(a.count, 3);

        let b = a.children.iter().find(|node| node.label == "b").unwrap();
        assert_eq!(b.count, 2);

        let x = a.children.iter().find(|node| node.label == "x").unwrap();
        assert_eq!(x.count, 1);
    }

    fn test_host() -> Host {
        Host {
            id: "keys-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            // db1, não db0: outros testes (ex.: `conn::db_sizes_counts_keys_in_db0`)
            // assumem db0 vazio fora das chaves que eles mesmos criam, e os
            // testes rodam em paralelo contra o mesmo container Redis real.
            db: 1,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    #[tokio::test]
    async fn scan_all_finds_every_type_with_ttl_and_size() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "keys_rs_test:scan_all";
        let string_key = format!("{prefix}:string");
        let hash_key = format!("{prefix}:hash");
        let list_key = format!("{prefix}:list");
        let zset_key = format!("{prefix}:zset");
        let set_key = format!("{prefix}:set");

        redis::cmd("SET")
            .arg(&string_key)
            .arg("valor")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("EXPIRE")
            .arg(&string_key)
            .arg(60)
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("HSET")
            .arg(&hash_key)
            .arg("campo")
            .arg("valor")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("RPUSH")
            .arg(&list_key)
            .arg("a")
            .arg("b")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("ZADD")
            .arg(&zset_key)
            .arg(1)
            .arg("um")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("SADD")
            .arg(&set_key)
            .arg("membro")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();

        let result = scan_all(&pool, &host, None, 1).await;

        // Limpa as chaves criadas antes de verificar o resultado, para não
        // deixar o db0 de teste sujo se a asserção falhar.
        for key in [&string_key, &hash_key, &list_key, &zset_key, &set_key] {
            let _: () = redis::cmd("DEL")
                .arg(key)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let metas = result.unwrap();
        let find = |name: &str| metas.iter().find(|meta| meta.key == name);

        let string_meta = find(&string_key).expect("chave string não encontrada pela varredura");
        assert_eq!(string_meta.kind, KeyKind::String);
        assert!(string_meta.kind.editable());
        assert!(
            matches!(string_meta.ttl_ms, Some(ms) if ms > 0),
            "esperava TTL positivo, veio {:?}",
            string_meta.ttl_ms
        );
        assert!(
            string_meta.size_bytes > 0,
            "MEMORY USAGE deveria reportar um tamanho > 0 no Redis de teste"
        );

        let hash_meta = find(&hash_key).expect("chave hash não encontrada pela varredura");
        assert_eq!(hash_meta.kind, KeyKind::Hash);
        assert!(hash_meta.kind.editable());
        assert_eq!(hash_meta.ttl_ms, None);

        let list_meta = find(&list_key).expect("chave list não encontrada pela varredura");
        assert_eq!(list_meta.kind, KeyKind::List);
        assert!(list_meta.kind.editable());

        let zset_meta = find(&zset_key).expect("chave zset não encontrada pela varredura");
        assert_eq!(zset_meta.kind, KeyKind::ZSet);
        assert!(zset_meta.kind.editable());

        let set_meta = find(&set_key).expect("chave set não encontrada pela varredura");
        assert_eq!(set_meta.kind, KeyKind::Set);
        assert!(
            !set_meta.kind.editable(),
            "Set é somente leitura neste produto"
        );
    }
}
