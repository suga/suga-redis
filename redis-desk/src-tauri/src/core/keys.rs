//! Varredura de chaves com metadados (`scan_all`) e a árvore por prefixo que
//! a coluna do meio da UI renderiza (`build_tree`). `scan_all` nunca usa
//! `KEYS`: a enumeração do banco é sempre via `SCAN` em lotes, para não
//! travar um Redis com muitas chaves numa única chamada.

use redis::aio::MultiplexedConnection;

use super::config::Host;
use super::conn::{ConnError, Pool};
use serde::{Deserialize, Serialize};

/// `COUNT` pedido a cada chamada de `SCAN`: alto o bastante para que um
/// banco com muitas chaves não precise de milhares de idas e vindas, sem
/// pedir uma passada tão grande que pese no servidor de uma vez só.
const SCAN_COUNT: usize = 5_000;

/// Tamanho do lote de chaves por pipeline de metadados (`TYPE` + `PTTL` +
/// `MEMORY USAGE`). Lotes menores que o total de chaves evitam um pipeline
/// gigante numa varredura de banco grande.
const METADATA_BATCH: usize = 200;

/// Separador usado para agrupar chaves em árvore por prefixo.
const SEPARATOR: char = ':';

/// Tipo de uma chave Redis, conforme a resposta de `TYPE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    String,
    Hash,
    List,
    #[serde(rename = "zset")]
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

    /// Mapeia o nome de tipo devolvido por `TYPE` para `KeyKind`. `pub(crate)`
    /// porque `core::value` (Task 4) também precisa desse mapeamento ao ler o
    /// conteúdo de uma chave, e duplicá-lo lá divergiria da tabela aqui com o
    /// tempo.
    pub(crate) fn from_type_name(name: &str) -> KeyKind {
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyMeta {
    pub key: String,
    pub kind: KeyKind,
    /// `None` quando a chave não expira; caso contrário, milissegundos
    /// restantes de vida (vindo de `PTTL`).
    pub ttl_ms: Option<i64>,
    /// Tamanho estimado em bytes (`MEMORY USAGE`). Fica em `0` quando o
    /// comando não está disponível no servidor (ex.: um gerenciado antigo)
    /// ou falha para aquela chave — isso nunca derruba a varredura.
    pub size_bytes: u64,
}

/// Teto de filhos distintos listados num nível.
///
/// Existe para o caso degenerado: um nível em que cada chave tem um
/// identificador próprio (`pedido:<uuid>:item`) não é uma árvore, é a lista
/// de chaves disfarçada. Listar tudo devolveria à tela exatamente o volume
/// que esta varredura existe para evitar.
pub const MAX_CHILDREN: usize = 2_000;

/// Um filho de um nível da árvore: um galho a abrir ou uma folha a ler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// O segmento, sem o prefixo do pai.
    pub label: String,
    /// O prefixo acumulado, já com o `:` no fim — é o que se passa a
    /// `scan_level` para abrir este galho.
    pub full_prefix: String,
    pub count: u64,
    /// Preenchido só quando este filho É uma chave, não um galho.
    pub leaf_key: Option<String>,
}

/// Um nível da árvore, varrido sob demanda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeLevel {
    /// O prefixo deste nível (vazio na raiz).
    pub prefix: String,
    pub children: Vec<Branch>,
    /// Quantas chaves foram varridas para montar este nível.
    pub scanned: u64,
    pub total_estimate: u64,
    /// A lista de filhos bateu em `MAX_CHILDREN` e foi cortada.
    pub capped: bool,
}

/// Escapa os curingas do `MATCH` do Redis (`*`, `?`, `[`, `]`, `\\`).
///
/// Sem isso, um prefixo contendo `*` deixaria de selecionar o que está sob
/// ele e passaria a casar com qualquer coisa — devolvendo chaves de outros
/// galhos como se fossem deste.
fn escapar_glob(texto: &str) -> String {
    let mut out = String::with_capacity(texto.len());
    for ch in texto.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Varre UM nível da árvore, contando os filhos em vez de guardar as chaves.
///
/// É esta troca que torna um banco de milhões de chaves navegável: a memória
/// passa a ser proporcional ao número de filhos DISTINTOS do nível (dezenas,
/// em geral), e não ao número de chaves. Guardar 3,7 milhões de nomes para
/// depois agrupá-los custava quase um gigabyte; contá-los enquanto passam
/// custa alguns kilobytes.
///
/// O `MATCH` do `SCAN` não torna a varredura mais curta no servidor — ele
/// filtra o que volta pela rede, que é justamente o caro. Abrir um galho de
/// mil chaves num banco de milhões transfere mil nomes, não milhões.
pub async fn scan_level(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    prefix: &str,
    progress: impl Fn(ScanTick),
) -> Result<TreeLevel, ConnError> {
    let padrao = format!("{}*", escapar_glob(prefix));
    let progress = &progress;

    let (mut children, scanned, total_estimate, capped) = pool
        .with_retry(host, db, pw, move |mut conn| {
            let padrao = padrao.clone();
            async move {
                let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

                // `BTreeMap` e não `HashMap`: a ordem sai pronta, e ordenar
                // depois custaria mais do que mantê-la.
                let mut filhos: std::collections::BTreeMap<String, (u64, Option<String>)> =
                    std::collections::BTreeMap::new();
                let mut scanned = 0u64;
                let mut capped = false;
                let mut cursor = 0u64;

                loop {
                    let (next, lote): (u64, Vec<String>) = redis::cmd("SCAN")
                        .arg(cursor)
                        .arg("MATCH")
                        .arg(&padrao)
                        .arg("COUNT")
                        .arg(SCAN_COUNT)
                        .query_async(&mut conn)
                        .await?;

                    for chave in lote {
                        // O `MATCH` é do servidor; esta checagem é nossa, e
                        // protege de um escape que tenha falhado.
                        let Some(resto) = chave.strip_prefix(prefix) else {
                            continue;
                        };
                        scanned += 1;

                        let (rotulo, folha) = match resto.split_once(SEPARATOR) {
                            Some((segmento, _)) => (segmento.to_string(), None),
                            None => (resto.to_string(), Some(chave.clone())),
                        };

                        // O `len` é lido antes do empréstimo mutável para
                        // o compilador não ver os dois ao mesmo tempo.
                        let cheio = filhos.len() >= MAX_CHILDREN;
                        if let Some(entrada) = filhos.get_mut(&rotulo) {
                            entrada.0 += 1;
                            // Duas chaves sob o mesmo rótulo: deixou de ser
                            // folha, virou galho.
                            if entrada.0 > 1 {
                                entrada.1 = None;
                            }
                        } else if cheio {
                            // Teto batido: para de listar filhos novos, mas
                            // segue contando o que foi varrido, para o
                            // número na tela continuar verdadeiro.
                            capped = true;
                        } else {
                            filhos.insert(rotulo, (1, folha));
                        }
                    }

                    progress(ScanTick {
                        scanned,
                        total_estimate,
                    });

                    if next == 0 {
                        break;
                    }
                    cursor = next;
                }

                Ok((filhos, scanned, total_estimate, capped))
            }
        })
        .await
        .map(|(filhos, scanned, total, capped)| {
            let children: Vec<Branch> = filhos
                .into_iter()
                .map(|(label, (count, leaf_key))| Branch {
                    full_prefix: format!("{prefix}{label}{SEPARATOR}"),
                    label,
                    count,
                    leaf_key,
                })
                .collect();
            (children, scanned, total, capped)
        })?;

    // Galhos antes de folhas, cada grupo em ordem de nome: a pessoa procura
    // pelo caminho, e um caminho que some no meio de uma lista de folhas é
    // um caminho perdido.
    children.sort_by(|a, b| {
        a.leaf_key
            .is_some()
            .cmp(&b.leaf_key.is_some())
            .then_with(|| a.label.cmp(&b.label))
    });

    Ok(TreeLevel {
        prefix: prefix.to_string(),
        children,
        scanned,
        total_estimate,
        capped,
    })
}

/// Andamento de uma varredura de nomes: quantas chaves já vieram e quantas
/// o servidor diz ter (`DBSIZE`).
///
/// `total_estimate` é ESTIMATIVA de verdade, não um total conhecido: chaves
/// nascem e expiram durante a varredura, e o `SCAN` pode devolver a mesma
/// chave duas vezes se o banco crescer no meio do caminho. Serve para uma
/// porcentagem na tela, não para uma conta exata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanTick {
    pub scanned: u64,
    pub total_estimate: u64,
}

/// Varre só os NOMES das chaves, sem pedir metadado nenhum.
///
/// É o que a árvore da tela precisa — e só isso: `build_tree` lê o nome e
/// descarta o resto. Pedir `TYPE`, `PTTL` e `MEMORY USAGE` de cada chave
/// para montá-la custava uma ida e volta a cada 200 chaves, trazendo da
/// outra ponta da rede um dado que ia direto para o lixo. Num Redis
/// gerenciado em outra região, com 50 mil chaves, eram 250 viagens de ~130 ms
/// só para isso.
///
/// `progress` é chamado a cada lote do cursor, com o que já veio e o
/// `DBSIZE` lido antes de começar.
pub async fn scan_names(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    progress: impl Fn(ScanTick),
) -> Result<Vec<String>, ConnError> {
    let progress = &progress;
    pool.with_retry(host, db, pw, move |mut conn| async move {
        // Uma ida e volta a mais, no começo, para a tela poder dizer
        // "45%" em vez de "aguarde". Vale a troca.
        let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

        let mut names: Vec<String> = Vec::with_capacity(total_estimate as usize);
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(SCAN_COUNT)
                .query_async(&mut conn)
                .await?;

            names.extend(batch);
            progress(ScanTick {
                scanned: names.len() as u64,
                total_estimate,
            });

            if next_cursor == 0 {
                break;
            }
            cursor = next_cursor;
        }

        Ok(names)
    })
    .await
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
pub fn build_tree(keys: &[String]) -> Vec<TreeNode> {
    let entries: Vec<Entry> = keys
        .iter()
        .map(|key| (key.split(SEPARATOR).collect(), key.as_str()))
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

    /// `build_tree` recebe nomes; este atalho poupa o `to_string` repetido.
    fn nomes(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    #[test]
    fn groups_by_colon_prefix() {
        let keys = nomes(&["session:user:1", "session:user:2", "cache:a"]);

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
        let keys = nomes(&["session:user:1", "session:user:2"]);

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
        let keys = nomes(&inputs);

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
        let keys = nomes(&["a:b:c", "a:b:d", "a:x"]);

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

    /// Cria chaves, roda o corpo e apaga tudo — mesmo se o corpo falhar
    /// numa asserção, para não deixar lixo no banco compartilhado.
    async fn com_chaves<F, Fut, T>(chaves: &[&str], corpo: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        for chave in chaves {
            let _: () = redis::cmd("SET")
                .arg(chave)
                .arg("x")
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let saida = corpo().await;

        for chave in chaves {
            let _: () = redis::cmd("DEL")
                .arg(chave)
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        saida
    }

    fn galho<'a>(nivel: &'a TreeLevel, rotulo: &str) -> &'a Branch {
        nivel
            .children
            .iter()
            .find(|b| b.label == rotulo)
            .unwrap_or_else(|| {
                panic!(
                    "esperava o galho {rotulo:?}, vieram {:?}",
                    nivel.children.iter().map(|b| &b.label).collect::<Vec<_>>()
                )
            })
    }

    #[tokio::test]
    async fn scan_level_counts_the_first_segment_without_holding_every_key() {
        let chaves = [
            "nivel_rs_test:wallet:1",
            "nivel_rs_test:wallet:2",
            "nivel_rs_test:wallet:3",
            "nivel_rs_test:cache:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let nivel = scan_level(&pool, &host, None, host.db, "nivel_rs_test:", |_| {})
                .await
                .unwrap();

            assert_eq!(galho(&nivel, "wallet").count, 3);
            assert_eq!(galho(&nivel, "cache").count, 1);
            // Galho não é folha: tem filhos abaixo dele.
            assert_eq!(galho(&nivel, "wallet").leaf_key, None);
            assert_eq!(galho(&nivel, "wallet").full_prefix, "nivel_rs_test:wallet:");
            assert!(!nivel.capped);
        })
        .await;
    }

    #[tokio::test]
    async fn scan_level_descends_only_into_the_prefix_asked_for() {
        let chaves = [
            "desce_rs_test:a:um",
            "desce_rs_test:a:dois",
            "desce_rs_test:b:tres",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let nivel = scan_level(&pool, &host, None, host.db, "desce_rs_test:a:", |_| {})
                .await
                .unwrap();

            let rotulos: Vec<&str> = nivel.children.iter().map(|b| b.label.as_str()).collect();
            assert_eq!(rotulos.len(), 2, "só os filhos de `a:`, veio {rotulos:?}");
            // Sem mais `:` adiante, cada um é uma folha com a chave inteira.
            assert_eq!(
                galho(&nivel, "um").leaf_key.as_deref(),
                Some("desce_rs_test:a:um")
            );
            assert_eq!(galho(&nivel, "dois").count, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn scan_level_escapes_glob_characters_in_the_prefix() {
        // `*`, `?` e `[` são curingas do MATCH do Redis. Sem escapá-los, um
        // prefixo que os contenha varreria o banco inteiro — e, pior,
        // devolveria chaves que não estão sob ele.
        let chaves = ["glob_rs_test:a*b:dentro", "glob_rs_test:axxb:fora"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let nivel = scan_level(&pool, &host, None, host.db, "glob_rs_test:a*b:", |_| {})
                .await
                .unwrap();

            let rotulos: Vec<&str> = nivel.children.iter().map(|b| b.label.as_str()).collect();
            assert_eq!(rotulos, vec!["dentro"], "o `*` foi tratado como curinga");
        })
        .await;
    }

    #[tokio::test]
    async fn scan_level_caps_an_absurd_number_of_children_and_says_so() {
        // Um nível com um identificador diferente por chave: listar todos
        // seria devolver a lista inteira de chaves disfarçada de árvore.
        let chaves: Vec<String> = (0..(MAX_CHILDREN + 20))
            .map(|i| format!("teto_rs_test:{i}:folha"))
            .collect();
        let refs: Vec<&str> = chaves.iter().map(|s| s.as_str()).collect();

        com_chaves(&refs, || async {
            let pool = Pool::new();
            let host = test_host();
            let nivel = scan_level(&pool, &host, None, host.db, "teto_rs_test:", |_| {})
                .await
                .unwrap();

            assert!(nivel.capped, "a tela precisa saber que a lista foi cortada");
            assert_eq!(nivel.children.len(), MAX_CHILDREN);
            // E a contagem de chaves varridas continua verdadeira, mesmo
            // com a lista cortada.
            assert_eq!(nivel.scanned, (MAX_CHILDREN + 20) as u64);
        })
        .await;
    }

    #[tokio::test]
    async fn scan_names_returns_every_key_without_asking_for_metadata() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let chaves = [
            "keys_rs_test:nomes:wallet:1",
            "keys_rs_test:nomes:cache:1",
            "keys_rs_test:nomes:provider_config",
        ];
        for chave in chaves {
            let _: () = redis::cmd("SET")
                .arg(chave)
                .arg("x")
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let nomes = scan_names(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        for chave in chaves {
            let _: () = redis::cmd("DEL")
                .arg(chave)
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        for chave in chaves {
            assert!(
                nomes.iter().any(|nome| nome == chave),
                "a varredura por nomes precisava trazer {chave}, veio {nomes:?}"
            );
        }
    }

    #[tokio::test]
    async fn scan_names_reports_progress_against_the_database_size() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let chave = "keys_rs_test:progresso:unica";
        let _: () = redis::cmd("SET")
            .arg(chave)
            .arg("x")
            .query_async(&mut conn)
            .await
            .unwrap();

        let ticks = std::sync::Mutex::new(Vec::new());
        let nomes = scan_names(&pool, &host, None, host.db, |tick: ScanTick| {
            ticks.lock().unwrap().push(tick);
        })
        .await
        .unwrap();
        let _: () = redis::cmd("DEL")
            .arg(chave)
            .query_async(&mut conn)
            .await
            .unwrap();

        let ticks = ticks.into_inner().unwrap();
        assert!(
            !ticks.is_empty(),
            "a varredura precisa avisar o andamento ao menos uma vez"
        );

        // O total é estimativa (`DBSIZE`), e o banco é compartilhado com os
        // outros testes — mas tem que ser um número de verdade, ou a tela
        // não teria como calcular porcentagem nenhuma.
        assert!(ticks.iter().all(|t| t.total_estimate > 0));
        // E o contador só anda para a frente, terminando no que foi achado.
        let scanned: Vec<u64> = ticks.iter().map(|t| t.scanned).collect();
        assert!(
            scanned.windows(2).all(|par| par[0] <= par[1]),
            "veio {scanned:?}"
        );
        assert_eq!(*scanned.last().unwrap(), nomes.len() as u64);
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
