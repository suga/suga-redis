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

/// Teto de nós da trie de prefixos.
///
/// Limita a memória no caso degenerado — um identificador distinto por
/// chave (`pedido:<uuid>:item`), em que a trie viraria a lista de chaves
/// inteira. Batido o teto, nós novos param de ser criados e as contagens
/// dos que existem seguem subindo, de modo que os números na tela
/// continuam verdadeiros mesmo com a lista incompleta.
pub const MAX_NODES: usize = 200_000;

/// Um nó da trie, numa arena indexada por `usize`.
///
/// Arena e não `Box`: uma trie de duzentos mil nós com ponteiros espalha
/// alocações pela memória toda e torna a construção — que roda durante a
/// varredura, a cada chave — visivelmente mais lenta.
#[derive(Debug)]
struct No {
    label: String,
    full_prefix: String,
    count: u64,
    filhos: std::collections::BTreeMap<String, usize>,
    /// A chave que TERMINA exatamente aqui, se houver.
    terminal: Option<String>,
}

/// Índice de prefixos construído durante UMA varredura do banco.
///
/// Existe por causa de um fato do Redis que não dá para contornar: o
/// `MATCH` do `SCAN` não encurta o trabalho do servidor. Ele filtra o que
/// volta pela rede, mas o cursor percorre o espaço de chaves inteiro de
/// todo jeito. Abrir um galho de 2 chaves num banco de 3,7 milhões custava
/// a mesma varredura completa que abrir qualquer outro — e cada clique
/// pagava de novo.
///
/// Com a trie, a varredura acontece UMA vez e todo galho abre em memória.
#[derive(Debug)]
pub struct KeyTrie {
    nos: Vec<No>,
    orcamento: usize,
    estourou: bool,
}

impl Default for KeyTrie {
    fn default() -> KeyTrie {
        KeyTrie::new()
    }
}

impl KeyTrie {
    pub fn new() -> KeyTrie {
        KeyTrie::com_orcamento(MAX_NODES)
    }

    pub fn com_orcamento(orcamento: usize) -> KeyTrie {
        KeyTrie {
            nos: vec![No {
                label: String::new(),
                full_prefix: String::new(),
                count: 0,
                filhos: std::collections::BTreeMap::new(),
                terminal: None,
            }],
            orcamento,
            estourou: false,
        }
    }

    /// A lista de filhos de algum nível ficou incompleta por falta de
    /// espaço.
    pub fn budget_exhausted(&self) -> bool {
        self.estourou
    }

    /// Quantas chaves entraram na trie.
    pub fn keys(&self) -> u64 {
        self.nos[0].count
    }

    /// Acrescenta uma chave, criando os nós do caminho que faltarem.
    pub fn inserir(&mut self, chave: &str) {
        self.nos[0].count += 1;

        let mut atual = 0usize;
        let mut prefixo = String::new();
        let mut segmentos = chave.split(SEPARATOR).peekable();

        while let Some(segmento) = segmentos.next() {
            prefixo.push_str(segmento);
            prefixo.push(SEPARATOR);

            let proximo = match self.nos[atual].filhos.get(segmento) {
                Some(&indice) => indice,
                None => {
                    if self.nos.len() >= self.orcamento {
                        // Sem espaço para o caminho inteiro: a contagem dos
                        // ancestrais já subiu, que é o que a tela mostra.
                        self.estourou = true;
                        return;
                    }
                    let indice = self.nos.len();
                    self.nos.push(No {
                        label: segmento.to_string(),
                        full_prefix: prefixo.clone(),
                        count: 0,
                        filhos: std::collections::BTreeMap::new(),
                        terminal: None,
                    });
                    self.nos[atual].filhos.insert(segmento.to_string(), indice);
                    indice
                }
            };

            self.nos[proximo].count += 1;
            if segmentos.peek().is_none() {
                self.nos[proximo].terminal = Some(chave.to_string());
            }
            atual = proximo;
        }
    }

    /// Os filhos de `prefixo`, ou `None` se esse caminho não existe.
    ///
    /// Um nó que é chave E caminho (existem `a:b` e `a:b:c`) aparece como
    /// galho para o pai, e a chave dele entra entre os próprios filhos —
    /// senão ela ficaria inalcançável, escondida atrás do caminho que leva
    /// aos outros.
    pub fn level(&self, prefixo: &str) -> Option<Vec<Branch>> {
        let no = self.achar(prefixo)?;

        let mut filhos: Vec<Branch> = self.nos[no]
            .filhos
            .values()
            .map(|&indice| {
                let filho = &self.nos[indice];
                Branch {
                    label: filho.label.clone(),
                    full_prefix: filho.full_prefix.clone(),
                    count: filho.count,
                    leaf_key: if filho.filhos.is_empty() {
                        filho.terminal.clone()
                    } else {
                        None
                    },
                }
            })
            .collect();

        if !self.nos[no].filhos.is_empty()
            && let Some(chave) = &self.nos[no].terminal
        {
            filhos.push(Branch {
                label: "(a própria chave)".to_string(),
                full_prefix: chave.clone(),
                count: 1,
                leaf_key: Some(chave.clone()),
            });
        }

        // Galhos antes de folhas, cada grupo em ordem de nome: a pessoa
        // procura pelo caminho, e um caminho perdido no meio de uma lista
        // de folhas é um caminho que não se acha.
        filhos.sort_by(|a, b| {
            a.leaf_key
                .is_some()
                .cmp(&b.leaf_key.is_some())
                .then_with(|| a.label.cmp(&b.label))
        });
        Some(filhos)
    }

    fn achar(&self, prefixo: &str) -> Option<usize> {
        let mut atual = 0usize;
        for segmento in prefixo.trim_end_matches(SEPARATOR).split(SEPARATOR) {
            if segmento.is_empty() {
                continue;
            }
            atual = *self.nos[atual].filhos.get(segmento)?;
        }
        Some(atual)
    }
}

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

/// Um retrato da varredura em andamento, para a tela mostrar o que já veio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSnapshot {
    pub scanned: u64,
    pub total_estimate: u64,
    /// Os prefixos de primeiro nível achados até aqui, já contados.
    pub root: Vec<Branch>,
    pub done: bool,
}

/// De quanto em quanto tempo a varredura manda um retrato para a tela.
///
/// Mais curto do que isto e a tela passa a gastar mais com redesenho do que
/// a varredura com rede; mais longo e a árvore parece parada.
const INTERVALO_RETRATO: std::time::Duration = std::time::Duration::from_millis(250);

/// Varre o banco UMA vez e constrói a trie de prefixos, mandando retratos
/// da raiz pelo caminho.
///
/// A varredura única é o ponto. O `MATCH` do `SCAN` não deixa a varredura
/// mais curta — o servidor percorre o espaço de chaves inteiro de todo
/// jeito — então varrer por galho fazia cada clique pagar o banco inteiro
/// de novo. Aqui paga-se uma vez, com a árvore crescendo à vista, e depois
/// todo galho abre em memória.
pub async fn scan_trie(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    trie: &std::sync::Mutex<KeyTrie>,
    snapshot: impl Fn(ScanSnapshot),
) -> Result<(), ConnError> {
    let snapshot = &snapshot;

    pool.with_retry(host, db, pw, move |mut conn| async move {
        let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

        // A trie recomeça a cada tentativa: uma reconexão no meio refaz o
        // cursor do zero, e misturar o que veio antes contaria chaves duas
        // vezes.
        *trie.lock().unwrap() = KeyTrie::new();

        let mut cursor = 0u64;
        // `None` para o PRIMEIRO lote mandar retrato na hora: esperar o
        // intervalo deixaria a tela em branco no começo justamente quando a
        // pessoa está esperando ver alguma coisa.
        let mut ultimo_retrato: Option<std::time::Instant> = None;

        loop {
            let (next, lote): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(SCAN_COUNT)
                .query_async(&mut conn)
                .await?;

            // Um lote por travamento, não uma chave por travamento: quem
            // abre um galho no meio da varredura precisa conseguir ler a
            // trie entre os lotes, e travar cinco mil vezes por lote
            // deixaria a leitura esperando por conta da disputa.
            {
                let mut trie = trie.lock().unwrap();
                for chave in &lote {
                    trie.inserir(chave);
                }
            }

            let na_hora = ultimo_retrato.is_none_or(|t| t.elapsed() >= INTERVALO_RETRATO);
            if na_hora {
                snapshot(retrato(trie, total_estimate, false));
                ultimo_retrato = Some(std::time::Instant::now());
            }

            if next == 0 {
                break;
            }
            cursor = next;
        }

        snapshot(retrato(trie, total_estimate, true));
        Ok(())
    })
    .await
}

/// Um retrato da raiz, com o travamento segurado o mínimo possível.
fn retrato(trie: &std::sync::Mutex<KeyTrie>, total_estimate: u64, done: bool) -> ScanSnapshot {
    let trie = trie.lock().unwrap();
    ScanSnapshot {
        scanned: trie.keys(),
        total_estimate,
        root: trie.level("").unwrap_or_default(),
        done,
    }
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

/// Varre os nomes e guarda SÓ os que contêm `needle`, até `limit`.
///
/// Filtra enquanto os nomes passam e para assim que tem acertos
/// suficientes, de modo que a memória acompanha o número de ACERTOS e não o
/// tamanho do banco. Juntar o banco inteiro num `Vec` para filtrar depois
/// custava centenas de megabytes a cada busca.
///
/// A comparação ignora a caixa. Para uma chave em ASCII puro — a esmagadora
/// maioria — isso é feito sem alocar nada; só um nome com acento ou
/// caractere fora do ASCII paga uma cópia em caixa baixa.
pub async fn scan_matching_names(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    needle: &str,
    limit: usize,
) -> Result<Vec<String>, ConnError> {
    let agulha = needle.to_lowercase();
    let agulha = &agulha;

    pool.with_retry(host, db, pw, move |mut conn| async move {
        let mut achados: Vec<String> = Vec::new();
        let mut cursor = 0u64;

        loop {
            let (next, lote): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(SCAN_COUNT)
                .query_async(&mut conn)
                .await?;

            for chave in lote {
                if contem_ignorando_caixa(&chave, agulha) {
                    achados.push(chave);
                    if achados.len() >= limit {
                        return Ok(achados);
                    }
                }
            }

            if next == 0 {
                break;
            }
            cursor = next;
        }

        Ok(achados)
    })
    .await
}

/// `palheiro` contém `agulha` (já em caixa baixa), ignorando a caixa.
///
/// O caminho de ASCII não aloca: percorre os bytes comparando com a caixa
/// dobrada na hora. Só um nome com byte fora do ASCII cai no caminho geral,
/// que copia a string em caixa baixa — correto para todo o Unicode, e raro
/// o bastante em nome de chave para não pesar.
fn contem_ignorando_caixa(palheiro: &str, agulha: &str) -> bool {
    if agulha.is_empty() {
        return true;
    }
    if !palheiro.is_ascii() || !agulha.is_ascii() {
        return palheiro.to_lowercase().contains(agulha);
    }

    let (p, a) = (palheiro.as_bytes(), agulha.as_bytes());
    if a.len() > p.len() {
        return false;
    }
    p.windows(a.len())
        .any(|janela| janela.eq_ignore_ascii_case(a))
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

    fn trie_de(chaves: &[&str]) -> KeyTrie {
        let mut trie = KeyTrie::new();
        for chave in chaves {
            trie.inserir(chave);
        }
        trie
    }

    fn rotulos(trie: &KeyTrie, prefixo: &str) -> Vec<String> {
        trie.level(prefixo)
            .expect("nível existe")
            .into_iter()
            .map(|b| b.label)
            .collect()
    }

    #[test]
    fn a_trie_conta_cada_nivel_e_responde_na_hora() {
        let trie = trie_de(&[
            "catalogo:eventos:1",
            "catalogo:precos:1",
            "catalogo:precos:2",
            "sessao:token:x",
        ]);

        let raiz = trie.level("").unwrap();
        let api = raiz.iter().find(|b| b.label == "catalogo").unwrap();
        assert_eq!(api.count, 3);
        assert_eq!(api.full_prefix, "catalogo:");

        // Abrir um galho é consulta em memória, não varredura nova.
        assert_eq!(rotulos(&trie, "catalogo:"), vec!["events", "fixtures"]);
        assert_eq!(
            trie.level("catalogo:precos:").unwrap().len(),
            2,
            "e desce quantos níveis a chave tiver"
        );
    }

    #[test]
    fn uma_chave_sozinha_vira_folha_com_o_nome_inteiro() {
        let trie = trie_de(&["a:b:c"]);
        let folha = &trie.level("a:b:").unwrap()[0];
        assert_eq!(folha.label, "c");
        assert_eq!(folha.leaf_key.as_deref(), Some("a:b:c"));
    }

    #[test]
    fn uma_chave_que_tambem_e_prefixo_continua_alcancavel() {
        // `a:b` é chave E caminho para `a:b:c`. Nenhuma das duas pode sumir.
        let trie = trie_de(&["a:b", "a:b:c"]);

        let b = &trie.level("a:").unwrap()[0];
        assert_eq!(b.label, "b");
        assert_eq!(b.leaf_key, None, "tem filhos, então é galho");

        let dentro = trie.level("a:b:").unwrap();
        let chaves: Vec<Option<&str>> = dentro.iter().map(|x| x.leaf_key.as_deref()).collect();
        assert!(
            chaves.contains(&Some("a:b")),
            "a própria chave sumiu: {dentro:?}"
        );
        assert!(chaves.contains(&Some("a:b:c")));
    }

    #[test]
    fn o_orcamento_de_nos_para_de_crescer_e_a_contagem_continua_certa() {
        // Um identificador distinto por chave: sem teto, a trie viraria a
        // lista de chaves inteira.
        let mut trie = KeyTrie::com_orcamento(50);
        for i in 0..500 {
            trie.inserir(&format!("pedido:{i}:item"));
        }

        assert!(
            trie.budget_exhausted(),
            "a tela precisa saber que faltou espaço"
        );
        assert!(
            trie.level("").unwrap()[0].count == 500,
            "a contagem segue verdadeira"
        );
        assert!(
            trie.level("pedido:").unwrap().len() < 500,
            "mas a lista de filhos não cresceu sem limite"
        );
    }

    #[tokio::test]
    async fn scan_trie_builds_the_whole_tree_in_one_pass_and_reports_along_the_way() {
        let chaves = [
            "passo_rs_test:api:itens:1",
            "passo_rs_test:api:lotes:1",
            "passo_rs_test:api:lotes:2",
            "passo_rs_test:auth:token",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let retratos = std::sync::Mutex::new(Vec::new());
            let trie = std::sync::Mutex::new(KeyTrie::new());
            scan_trie(&pool, &host, None, host.db, &trie, |r: ScanSnapshot| {
                retratos.lock().unwrap().push(r);
            })
            .await
            .unwrap();
            let trie = trie.into_inner().unwrap();

            // Uma varredura só constrói a árvore inteira: abrir qualquer
            // galho daqui em diante é consulta em memória.
            let api = trie.level("passo_rs_test:api:").unwrap();
            let fixtures = api.iter().find(|b| b.label == "fixtures").unwrap();
            assert_eq!(fixtures.count, 2);
            assert_eq!(
                trie.level("passo_rs_test:auth:").unwrap()[0]
                    .leaf_key
                    .as_deref(),
                Some("passo_rs_test:auth:token")
            );

            // E a tela é avisada ao menos uma vez, no fim, com a raiz pronta.
            let retratos = retratos.into_inner().unwrap();
            let ultimo = retratos.last().expect("precisa haver ao menos um retrato");
            assert!(ultimo.done);
            assert!(ultimo.total_estimate > 0);
            assert!(ultimo.root.iter().any(|b| b.label == "passo_rs_test"));
        })
        .await;
    }

    #[tokio::test]
    async fn scan_matching_names_keeps_only_the_matches_and_stops_at_the_limit() {
        let chaves = [
            "casa_rs_test:ALVO:1",
            "casa_rs_test:alvo:2",
            "casa_rs_test:alvo:3",
            "casa_rs_test:outro:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            // Caixa diferente ainda é o mesmo nome.
            let achados = scan_matching_names(&pool, &host, None, host.db, "alvo", 10)
                .await
                .unwrap();
            let meus: Vec<&String> = achados
                .iter()
                .filter(|n| n.starts_with("casa_rs_test:"))
                .collect();
            assert_eq!(meus.len(), 3, "veio {meus:?}");
            assert!(meus.iter().all(|n| !n.contains("outro")));

            // O teto corta a coleta — é ele que impede um banco de milhões
            // de chaves virar um Vec de milhões de nomes.
            let poucos = scan_matching_names(&pool, &host, None, host.db, "casa_rs_test:", 2)
                .await
                .unwrap();
            assert_eq!(poucos.len(), 2);
        })
        .await;
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
