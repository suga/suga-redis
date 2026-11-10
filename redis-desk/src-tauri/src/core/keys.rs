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

/// Teto de filhos DISTINTOS listados num nível.
///
/// Alto de propósito: os níveis de uso real — um milhar de tokens, algumas
/// centenas de sessões — precisam sair INTEIROS, porque uma lista cortada
/// ao lado de uma contagem grande parece defeito mesmo quando não é.
///
/// O teto existe para o caso em que um nível tem milhões de identificadores
/// distintos: guardá-los todos seria a lista de chaves disfarçada de
/// árvore. Nesse caso o nível diz quantos está mostrando de quantos — a
/// contagem conta CHAVES e continua verdadeira.
pub const MAX_CHILDREN: usize = 50_000;

/// Um nível da árvore: os filhos e se a lista saiu inteira.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level {
    pub children: Vec<Branch>,
    /// A lista bateu em `MAX_CHILDREN`: há mais filhos distintos aqui.
    pub truncated: bool,
}

/// Teto de nós da trie de prefixos.
///
/// Limita a memória no caso degenerado — um identificador distinto por
/// chave (`pedido:<uuid>:item`), em que a trie viraria a lista de chaves
/// inteira. Batido o teto, nós novos param de ser criados e as contagens
/// dos que existem seguem subindo, de modo que os números na tela
/// continuam verdadeiros mesmo com a lista incompleta.
///
/// Com o teto por nível acima, este aqui é só rede de segurança: os níveis
/// patológicos param em `MAX_CHILDREN` muito antes de o total chegar perto
/// disto.
pub const MAX_NODES: usize = 400_000;

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
    /// A lista de filhos deste nó bateu no teto.
    cortado: bool,
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
    /// Teto de filhos por nível. Campo, e não constante, porque um galho
    /// pode pedir mais — ver `expand_branch`.
    teto_filhos: usize,
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

    /// Uma trie com um teto de filhos por nível diferente do padrão.
    pub fn com_teto_de_filhos(teto_filhos: usize) -> KeyTrie {
        KeyTrie {
            teto_filhos,
            ..KeyTrie::new()
        }
    }

    /// Esvazia a trie MANTENDO os tetos configurados.
    ///
    /// Usada quando a varredura recomeça (uma reconexão refaz o cursor do
    /// zero). Trocá-la por uma trie nova perderia o teto que quem a criou
    /// escolheu — foi assim que a paginação de um galho deixou de valer.
    pub fn limpar(&mut self) {
        let KeyTrie {
            orcamento,
            teto_filhos,
            ..
        } = *self;
        *self = KeyTrie {
            orcamento,
            teto_filhos,
            ..KeyTrie::new()
        };
    }

    pub fn com_orcamento(orcamento: usize) -> KeyTrie {
        KeyTrie {
            nos: vec![No {
                label: String::new(),
                full_prefix: String::new(),
                count: 0,
                filhos: std::collections::BTreeMap::new(),
                cortado: false,
                terminal: None,
            }],
            orcamento,
            teto_filhos: MAX_CHILDREN,
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
                    // Teto deste nível: para de listar filhos novos, mas a
                    // contagem dos ancestrais segue subindo — e o nó fica
                    // marcado, para a tela poder dizer que a lista está
                    // incompleta em vez de deixar o número parecer errado.
                    if self.nos[atual].filhos.len() >= self.teto_filhos {
                        self.nos[atual].cortado = true;
                        return;
                    }
                    if self.nos.len() >= self.orcamento {
                        // Sem espaço para o caminho inteiro: a contagem dos
                        // ancestrais já subiu, que é o que a tela mostra.
                        self.estourou = true;
                        self.nos[atual].cortado = true;
                        return;
                    }
                    let indice = self.nos.len();
                    self.nos.push(No {
                        label: segmento.to_string(),
                        full_prefix: prefixo.clone(),
                        count: 0,
                        filhos: std::collections::BTreeMap::new(),
                        cortado: false,
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

    /// Tira UMA chave da trie, devolvendo se ela estava lá.
    ///
    /// Existe porque a árvore na tela é desenhada a partir daqui, não do
    /// servidor: sem isto, uma chave apagada (ou expirada) continuava
    /// visível até alguém desconectar, que é o que joga a trie fora.
    ///
    /// Só mexe nas contagens se achar a chave EXATA registrada como
    /// terminal. Um caminho que existe sem a chave no fim — ou uma chave
    /// que a varredura nunca chegou a inserir, por teto de filhos ou de
    /// nós — não desce contagem nenhuma: descer seria corromper números
    /// que ninguém subiu.
    pub fn remover(&mut self, chave: &str) -> bool {
        // Caminho de nós, da raiz até o terminal, antes de mexer em nada.
        let mut caminho = vec![0usize];
        let mut atual = 0usize;
        for segmento in chave.split(SEPARATOR) {
            match self.nos[atual].filhos.get(segmento) {
                Some(&indice) => {
                    caminho.push(indice);
                    atual = indice;
                }
                None => return false,
            }
        }

        if self.nos[atual].terminal.as_deref() != Some(chave) {
            return false;
        }

        self.nos[atual].terminal = None;
        for &indice in &caminho {
            self.nos[indice].count = self.nos[indice].count.saturating_sub(1);
        }

        // Da folha para a raiz: um nó que ficou sem chave e sem filhos não
        // tem mais o que mostrar. Desvincula do pai em vez de remover da
        // arena — os índices dos outros nós apontam para posições fixas.
        for janela in caminho.windows(2).rev() {
            let (pai, filho) = (janela[0], janela[1]);
            if self.nos[filho].count == 0 && self.nos[filho].filhos.is_empty() {
                let rotulo = self.nos[filho].label.clone();
                self.nos[pai].filhos.remove(&rotulo);
            }
        }

        true
    }

    /// Os filhos de `prefixo`, ou `None` se esse caminho não existe.
    ///
    /// Um nó que é chave E caminho (existem `a:b` e `a:b:c`) aparece como
    /// galho para o pai, e a chave dele entra entre os próprios filhos —
    /// senão ela ficaria inalcançável, escondida atrás do caminho que leva
    /// aos outros.
    pub fn level(&self, prefixo: &str) -> Option<Level> {
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
        Some(Level {
            children: filhos,
            truncated: self.nos[no].cortado,
        })
    }

    /// Troca os filhos de `prefixo` pelos `filhos` informados.
    ///
    /// Usado pela paginação de um galho (`expand_branch`): o galho é
    /// varrido de novo, sozinho, e a lista dele cresce. Os netos que já
    /// estivessem abertos são descartados junto — eles serão relidos quando
    /// a pessoa abrir de novo, e mantê-los exigiria casar duas árvores
    /// construídas em momentos diferentes.
    pub fn substituir_filhos(
        &mut self,
        prefixo: &str,
        filhos: Vec<(String, u64, bool)>,
        cortado: bool,
    ) {
        let Some(no) = self.achar(prefixo) else {
            return;
        };

        self.nos[no].filhos.clear();
        self.nos[no].cortado = cortado;
        for (label, count, terminal) in filhos {
            let indice = self.nos.len();
            let full_prefix = format!("{prefixo}{label}{SEPARATOR}");
            let chave = format!("{prefixo}{label}");
            self.nos.push(No {
                label: label.clone(),
                full_prefix,
                count,
                filhos: std::collections::BTreeMap::new(),
                cortado: false,
                terminal: terminal.then_some(chave),
            });
            self.nos[no].filhos.insert(label, indice);
        }
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

/// Onde varrer: o host, a senha dele e o banco.
///
/// Os três andam sempre juntos e nunca fazem sentido separados; passá-los
/// como um só deixa a assinatura legível.
#[derive(Clone, Copy)]
pub struct Alvo<'a> {
    pub host: &'a Host,
    pub pw: Option<&'a str>,
    pub db: u8,
}

/// Escapa os curingas do `MATCH` do Redis (`*`, `?`, `[`, `]`, `\\`).
///
/// Sem isso, um prefixo que contenha `*` deixa de selecionar o que está sob
/// ele e passa a casar com qualquer coisa — devolvendo chaves de outros
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

/// Varre de novo UM galho e acrescenta filhos a ele, até `limite`.
///
/// Existe porque nenhum dado pode ficar inalcançável. Um nível com milhões
/// de nomes distintos não cabe na trie de uma vez, mas quem precisa ver
/// além do teto pede mais, e esta varredura — só do prefixo, com progresso
/// à vista — traz a próxima parte com as contagens certas.
///
/// É cara: o `MATCH` do `SCAN` filtra o que volta pela rede, mas o servidor
/// percorre o espaço de chaves inteiro de todo jeito. Por isso ela acontece
/// só quando a pessoa pede, e nunca por conta própria.
pub async fn expand_branch(
    pool: &Pool,
    alvo: Alvo<'_>,
    trie: &std::sync::Mutex<KeyTrie>,
    prefix: &str,
    limite: usize,
    progress: impl Fn(ScanTick),
) -> Result<(), ConnError> {
    let Alvo { host, pw, db } = alvo;
    let padrao = format!("{}*", escapar_glob(prefix));
    let progress = &progress;

    let (filhos, cortado) = pool
        .with_retry(host, db, pw, move |mut conn| {
            let padrao = padrao.clone();
            async move {
                let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

                // (contagem, é a própria chave) por nome de filho.
                let mut filhos: std::collections::BTreeMap<String, (u64, bool)> =
                    std::collections::BTreeMap::new();
                let mut cortado = false;
                let mut vistas = 0u64;
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
                        let Some(resto) = chave.strip_prefix(prefix) else {
                            continue;
                        };
                        vistas += 1;

                        let (rotulo, terminal) = match resto.split_once(SEPARATOR) {
                            Some((segmento, _)) => (segmento.to_string(), false),
                            None => (resto.to_string(), true),
                        };

                        // `len` lido antes do empréstimo mutável, para o
                        // compilador não ver os dois ao mesmo tempo.
                        let cheio = filhos.len() >= limite;
                        if let Some(entrada) = filhos.get_mut(&rotulo) {
                            entrada.0 += 1;
                            // Dois sob o mesmo nome: virou caminho.
                            entrada.1 = false;
                        } else if cheio {
                            cortado = true;
                        } else {
                            filhos.insert(rotulo, (1, terminal));
                        }
                    }

                    progress(ScanTick {
                        scanned: vistas,
                        total_estimate,
                    });

                    if next == 0 {
                        break;
                    }
                    cursor = next;
                }

                Ok((filhos, cortado))
            }
        })
        .await?;

    trie.lock().unwrap().substituir_filhos(
        prefix,
        filhos
            .into_iter()
            .map(|(label, (count, terminal))| (label, count, terminal))
            .collect(),
        cortado,
    );
    Ok(())
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
        // vezes. `limpar` e não uma trie nova, para os tetos configurados
        // sobreviverem ao recomeço.
        trie.lock().unwrap().limpar();

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
        root: trie.level("").map(|n| n.children).unwrap_or_default(),
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

    fn alvo(host: &Host) -> Alvo<'_> {
        Alvo {
            host,
            pw: None,
            db: host.db,
        }
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
            .children
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

        let raiz = trie.level("").unwrap().children;
        let api = raiz.iter().find(|b| b.label == "catalogo").unwrap();
        assert_eq!(api.count, 3);
        assert_eq!(api.full_prefix, "catalogo:");

        // Abrir um galho é consulta em memória, não varredura nova.
        assert_eq!(rotulos(&trie, "catalogo:"), vec!["events", "fixtures"]);
        assert_eq!(
            trie.level("catalogo:precos:").unwrap().children.len(),
            2,
            "e desce quantos níveis a chave tiver"
        );
    }

    #[test]
    fn uma_chave_sozinha_vira_folha_com_o_nome_inteiro() {
        let trie = trie_de(&["a:b:c"]);
        let folha = &trie.level("a:b:").unwrap().children[0];
        assert_eq!(folha.label, "c");
        assert_eq!(folha.leaf_key.as_deref(), Some("a:b:c"));
    }

    #[test]
    fn uma_chave_que_tambem_e_prefixo_continua_alcancavel() {
        // `a:b` é chave E caminho para `a:b:c`. Nenhuma das duas pode sumir.
        let trie = trie_de(&["a:b", "a:b:c"]);

        let b = &trie.level("a:").unwrap().children[0];
        assert_eq!(b.label, "b");
        assert_eq!(b.leaf_key, None, "tem filhos, então é galho");

        let dentro = trie.level("a:b:").unwrap().children;
        let chaves: Vec<Option<&str>> = dentro.iter().map(|x| x.leaf_key.as_deref()).collect();
        assert!(
            chaves.contains(&Some("a:b")),
            "a própria chave sumiu: {dentro:?}"
        );
        assert!(chaves.contains(&Some("a:b:c")));
    }

    #[test]
    fn um_nivel_cheio_demais_e_cortado_e_diz_que_foi() {
        // O caso real: `pedido:tokens:` com mil UUIDs.
        // Mostrar vinte e marcar 1.078 ao lado, calado, faz o número
        // parecer errado quando na verdade a LISTA é que está incompleta.
        let mut trie = KeyTrie::new();
        for i in 0..(MAX_CHILDREN + 500) {
            trie.inserir(&format!("tokens:{i}"));
        }

        let nivel = trie.level("tokens:").unwrap();
        assert!(
            nivel.truncated,
            "a tela precisa saber que a lista foi cortada"
        );
        assert_eq!(nivel.children.len(), MAX_CHILDREN);

        // E a contagem continua verdadeira: ela conta CHAVES, não a lista.
        let raiz = trie.level("").unwrap().children;
        assert_eq!(raiz[0].count, (MAX_CHILDREN + 500) as u64);
    }

    #[test]
    fn um_nivel_dentro_do_teto_lista_tudo() {
        // Mil e setenta e oito cabem: nada de corte onde não precisa.
        let mut trie = KeyTrie::new();
        for i in 0..1_200 {
            trie.inserir(&format!("tokens:{i}"));
        }

        let nivel = trie.level("tokens:").unwrap();
        assert!(!nivel.truncated);
        assert_eq!(nivel.children.len(), 1_200);
    }

    #[tokio::test]
    async fn a_varredura_de_um_galho_escapa_curingas_do_nome() {
        // `*`, `?` e `[` são curingas do MATCH. Sem escapá-los, o galho
        // `a*b:` traria chaves de `axxb:` como se fossem dele.
        let chaves = ["glob_rs_test:a*b:dentro", "glob_rs_test:axxb:fora"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let trie = std::sync::Mutex::new(KeyTrie::new());
            scan_trie(&pool, &host, None, host.db, &trie, |_| {})
                .await
                .unwrap();

            expand_branch(&pool, alvo(&host), &trie, "glob_rs_test:a*b:", 50, |_| {})
                .await
                .unwrap();

            let nivel = trie.lock().unwrap().level("glob_rs_test:a*b:").unwrap();
            let rotulos: Vec<&str> = nivel.children.iter().map(|b| b.label.as_str()).collect();
            assert_eq!(rotulos, vec!["dentro"], "o `*` foi tratado como curinga");
        })
        .await;
    }

    #[tokio::test]
    async fn um_galho_cortado_pode_ser_carregado_em_partes() {
        // Nenhum dado pode ficar inalcançável: um nível que não coube de
        // primeira é varrido de novo, só ele, e ganha os filhos que faltam.
        let chaves: Vec<String> = (0..12).map(|i| format!("pag_rs_test:t{i:02}")).collect();
        let refs: Vec<&str> = chaves.iter().map(|s| s.as_str()).collect();

        com_chaves(&refs, || async {
            let pool = Pool::new();
            let host = test_host();
            let trie = std::sync::Mutex::new(KeyTrie::com_teto_de_filhos(4));
            scan_trie(&pool, &host, None, host.db, &trie, |_| {})
                .await
                .unwrap();

            let primeiro = trie.lock().unwrap().level("pag_rs_test:").unwrap();
            assert_eq!(primeiro.children.len(), 4);
            assert!(primeiro.truncated);

            // Pede mais: o galho é varrido de novo e a lista cresce.
            expand_branch(&pool, alvo(&host), &trie, "pag_rs_test:", 8, |_| {})
                .await
                .unwrap();
            let segundo = trie.lock().unwrap().level("pag_rs_test:").unwrap();
            assert_eq!(segundo.children.len(), 8);
            assert!(segundo.truncated, "ainda faltam quatro");

            // Até acabar — e aí o aviso some.
            expand_branch(&pool, alvo(&host), &trie, "pag_rs_test:", 50, |_| {})
                .await
                .unwrap();
            let terceiro = trie.lock().unwrap().level("pag_rs_test:").unwrap();
            assert_eq!(terceiro.children.len(), 12);
            assert!(!terceiro.truncated, "tudo à vista, nada a avisar");
            assert!(
                terceiro.children.iter().all(|b| b.leaf_key.is_some()),
                "e cada um continua sendo a chave que é"
            );
        })
        .await;
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
            let api = trie.level("passo_rs_test:api:").unwrap().children;
            let fixtures = api.iter().find(|b| b.label == "fixtures").unwrap();
            assert_eq!(fixtures.count, 2);
            assert_eq!(
                trie.level("passo_rs_test:auth:").unwrap().children[0]
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

    /// Uma chave apagada — ou expirada — precisa sair da trie.
    ///
    /// A árvore na tela é desenhada a partir daqui, não do servidor. Sem
    /// remoção, apagar uma chave deixava ela visível até alguém
    /// desconectar e conectar de novo, que é o que joga a trie fora.
    #[test]
    fn removing_a_key_takes_it_off_the_tree() {
        let mut trie = KeyTrie::new();
        trie.inserir("app:cache:a");
        trie.inserir("app:cache:b");
        trie.inserir("app:fila:x");

        assert!(trie.remover("app:cache:a"));

        assert_eq!(trie.keys(), 2, "a contagem da raiz desce junto");
        let cache = trie.level("app:cache:").unwrap();
        let rotulos: Vec<&str> = cache.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(rotulos, vec!["b"], "só o irmão sobra");

        let app = trie.level("app:").unwrap();
        let contagem = app
            .children
            .iter()
            .find(|c| c.label == "cache")
            .map(|c| c.count);
        assert_eq!(contagem, Some(1), "o galho pai também desce");
    }

    #[test]
    fn removing_the_last_key_of_a_branch_prunes_the_branch() {
        let mut trie = KeyTrie::new();
        trie.inserir("app:cache:a");
        trie.inserir("app:fila:x");

        assert!(trie.remover("app:cache:a"));

        let app = trie.level("app:").unwrap();
        let rotulos: Vec<&str> = app.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(rotulos, vec!["fila"], "o galho vazio não fica para trás");
        assert!(trie.level("app:cache:").is_none());
    }

    #[test]
    fn removing_a_key_that_is_not_there_changes_nothing() {
        let mut trie = KeyTrie::new();
        trie.inserir("app:cache:a");

        assert!(!trie.remover("app:cache:z"));
        assert!(!trie.remover("outro:caminho"));
        assert_eq!(trie.keys(), 1);
        assert_eq!(trie.level("app:cache:").unwrap().children.len(), 1);
    }

    /// Um nó que é chave E caminho ao mesmo tempo: apagar a chave não pode
    /// levar embora o galho que leva aos filhos dela.
    #[test]
    fn removing_a_key_that_is_also_a_path_keeps_the_path() {
        let mut trie = KeyTrie::new();
        trie.inserir("sessao:ativa");
        trie.inserir("sessao:ativa:detalhe");

        assert!(trie.remover("sessao:ativa"));

        let sessao = trie.level("sessao:").unwrap();
        let ativa = sessao.children.iter().find(|c| c.label == "ativa");
        assert!(ativa.is_some(), "o caminho para o filho continua de pé");
        assert_eq!(ativa.unwrap().count, 1, "só a chave de cima saiu");
        assert!(trie.level("sessao:ativa:").is_some());
    }
}
