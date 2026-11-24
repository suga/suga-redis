//! Enumeração de chaves sob um ou mais prefixos — o primeiro passo da
//! exclusão em massa pedida pelo menu de contexto da árvore. Varre, nomeia e
//! classifica; apagar é responsabilidade de outra função, que decide o que
//! fazer com a lista que sai daqui.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use super::conn::{ConnError, Pool};
use super::keys::{Alvo, KeyKind, SCAN_COUNT, ScanTick, escapar_glob};

/// Teto de chaves que uma enumeração devolve.
///
/// Existe pela mesma razão do teto de filhos da trie (`keys::MAX_CHILDREN`):
/// um prefixo com milhões de chaves por baixo não pode virar um `Vec` do
/// tamanho do banco inteiro só porque alguém clicou em "excluir". Batido o
/// teto, a enumeração para e avisa — quem chamou decide se mostra o aviso ou
/// recusa a exclusão.
pub const MAX_ENUMERATED: usize = 100_000;

/// O resultado de enumerar um ou mais prefixos: os nomes encontrados, quantos
/// de cada tipo, e por que a lista parou — no teto, ou numa desistência.
#[derive(Debug, Clone, PartialEq)]
pub struct Enumeration {
    pub keys: Vec<String>,
    /// Quantas de cada tipo, para o diálogo avisar o que não volta atrás.
    pub kinds: Vec<(KeyKind, u64)>,
    /// Parou no teto: há mais do que isto sob o prefixo.
    pub truncated: bool,
    /// Parou porque alguém desistiu. A lista que sai daqui não descreve o
    /// prefixo e NÃO pode virar a contagem de um diálogo: quem recebe isto
    /// joga fora. Separado de `truncated` porque as duas paradas pedem
    /// respostas opostas — o teto é um aviso para repetir a operação, a
    /// desistência é um pedido para esquecê-la.
    pub cancelled: bool,
}

/// Como a varredura decide se uma chave pertence ao pedido.
///
/// Existe porque o `MATCH` do `SCAN` não encurta o trabalho do servidor —
/// ele só filtra o que volta pela rede, e o cursor percorre o espaço de
/// chaves inteiro de todo jeito (ver `keys::expand_branch` e o fato
/// registrado em `docs/CONTEXTO.md`). Por isso, com VÁRIOS prefixos, pedir
/// `MATCH` de cada um custaria uma varredura por prefixo; filtrar em Rust
/// depois de UMA varredura sem `MATCH` custa o mesmo que uma varredura só,
/// filtrada no servidor.
#[derive(Clone)]
enum Modo {
    /// Um único prefixo: o próprio servidor filtra via `MATCH`, então toda
    /// chave que volta já serve.
    Match(String),
    /// Nenhum prefixo: pedido explícito do banco inteiro, sem filtro.
    SemFiltro,
    /// Vários prefixos: sem `MATCH`, filtrando por `starts_with` aqui.
    Filtra(Vec<String>),
}

fn combina(modo: &Modo, chave: &str) -> bool {
    match modo {
        Modo::Match(_) | Modo::SemFiltro => true,
        Modo::Filtra(prefixos) => prefixos
            .iter()
            .any(|prefixo| chave.starts_with(prefixo.as_str())),
    }
}

/// Filtra um lote do `SCAN`, decidindo o que é candidato novo e se o teto
/// foi batido. Pura e síncrona — não fala com o Redis — para o teste de
/// deduplicação poder alimentar um lote com repetição à mão, sem depender de
/// conseguir fazer um servidor de verdade devolver a mesma chave duas vezes.
///
/// `vistas` é o conjunto de chaves JÁ aceitas, atravessando lotes: o `SCAN`
/// garante "pelo menos uma vez", nunca "no máximo uma vez" — a mesma chave
/// pode voltar no mesmo lote ou num lote seguinte depois de um rehash do
/// servidor. Sem isto, ela entraria duas vezes na enumeração, inflando a
/// contagem que o diálogo de exclusão promete ser exata (uma chave apagada
/// duas vezes também duplicaria a cópia de segurança de uma chave só).
///
/// O teto conta chaves DISTINTAS: uma repetição não consome vaga, e o aviso
/// de corte só liga quando uma chave realmente nova não coube mais.
fn filtra_lote(
    modo: &Modo,
    lote: Vec<String>,
    vistas: &mut std::collections::HashSet<String>,
    teto: usize,
) -> (Vec<String>, bool) {
    let mut candidatos = Vec::new();
    let mut truncated = false;

    for chave in lote {
        if vistas.len() >= teto {
            truncated = true;
            break;
        }
        if !combina(modo, &chave) {
            continue;
        }
        if !vistas.insert(chave.clone()) {
            // Já vista — neste lote ou num anterior. Não conta de novo nem
            // consome vaga do teto.
            continue;
        }
        candidatos.push(chave);
    }

    (candidatos, truncated)
}

/// Enumera toda chave sob um ou mais prefixos, com o `TYPE` de cada uma.
///
/// Prefixo é casamento por `starts_with`, nada mais: uma chave que também é
/// prefixo de outra (`wallet` ao lado de `wallet:1`) aparece nas duas
/// posições — como galho e como folha, a árvore já faz isso, e apagar em
/// massa precisa do mesmo tratamento, ou a própria chave escaparia da
/// exclusão.
pub async fn enumerate(
    pool: &Pool,
    alvo: Alvo<'_>,
    prefixes: &[String],
    cancel: &AtomicBool,
    progress: impl Fn(ScanTick),
) -> Result<Enumeration, ConnError> {
    enumerate_com_teto(pool, alvo, prefixes, cancel, progress, MAX_ENUMERATED).await
}

/// `enumerate` com o teto como parâmetro, para o teste do caso de corte não
/// precisar criar `MAX_ENUMERATED` chaves de verdade.
async fn enumerate_com_teto(
    pool: &Pool,
    alvo: Alvo<'_>,
    prefixes: &[String],
    cancel: &AtomicBool,
    progress: impl Fn(ScanTick),
    teto: usize,
) -> Result<Enumeration, ConnError> {
    let Alvo { host, pw, db } = alvo;
    let progress = &progress;

    let modo = match prefixes {
        [] => Modo::SemFiltro,
        [unico] => Modo::Match(format!("{}*", escapar_glob(unico))),
        varios => Modo::Filtra(varios.to_vec()),
    };

    pool.with_retry(host, db, pw, move |mut conn| {
        let modo = modo.clone();
        async move {
            let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

            let mut keys: Vec<String> = Vec::new();
            let mut kinds: Vec<(KeyKind, u64)> = Vec::new();
            let mut vistas: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut truncated = false;
            let mut cancelled = false;
            let mut cursor = 0u64;

            loop {
                // Conferido no TOPO, antes da primeira volta do cursor: o
                // `MATCH` do `SCAN` não encurta o trabalho do servidor (ver
                // `Modo`), então num banco grande cujo prefixo tem poucas
                // chaves o teto nunca dispara e o cursor percorre o espaço de
                // chaves INTEIRO. Quem clicou no prefixo errado precisa
                // conseguir parar isso, e precisa conseguir parar antes da
                // primeira ida também — a desistência pode chegar enquanto a
                // tarefa ainda nem foi escalonada.
                if cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }

                let mut comando = redis::cmd("SCAN");
                comando.arg(cursor).arg("COUNT").arg(SCAN_COUNT);
                if let Modo::Match(padrao) = &modo {
                    comando.arg("MATCH").arg(padrao);
                }
                let (next, lote): (u64, Vec<String>) = comando.query_async(&mut conn).await?;

                let (candidatos, cortou) = filtra_lote(&modo, lote, &mut vistas, teto);
                if cortou {
                    truncated = true;
                }

                if !candidatos.is_empty() {
                    // `TYPE` dos candidatos do lote, num pipeline só — não
                    // uma ida e volta por chave.
                    let mut pipe = redis::pipe();
                    pipe.ignore_errors();
                    for nome in &candidatos {
                        pipe.cmd("TYPE").arg(nome);
                    }
                    let respostas: Vec<redis::RedisResult<redis::Value>> =
                        pipe.query_async(&mut conn).await?;

                    for (nome, resposta) in candidatos.into_iter().zip(respostas) {
                        let tipo = resposta
                            .as_ref()
                            .ok()
                            .and_then(|valor| redis::from_redis_value_ref::<String>(valor).ok())
                            .unwrap_or_else(|| "none".to_string());

                        if tipo == "none" {
                            // Sumiu entre o SCAN e o TYPE (expirou ou foi
                            // apagada por outra conexão nesse meio-tempo) —
                            // não entra na lista, mesma regra de `scan_all`.
                            continue;
                        }

                        let kind = KeyKind::from_type_name(&tipo);
                        if let Some((_, contagem)) = kinds.iter_mut().find(|(k, _)| *k == kind) {
                            *contagem += 1;
                        } else {
                            kinds.push((kind, 1));
                        }
                        keys.push(nome);
                    }
                }

                progress(ScanTick {
                    scanned: keys.len() as u64,
                    total_estimate,
                });

                if truncated || next == 0 {
                    break;
                }
                cursor = next;
            }

            Ok(Enumeration {
                keys,
                kinds,
                truncated,
                cancelled,
            })
        }
    })
    .await
}

/// Tamanho do lote de `UNLINK` — grande o bastante para poucas idas e
/// voltas num prefixo com dezenas de milhares de chaves, pequeno o
/// bastante para o `cancel` (ver `unlink_em_lotes`) ter chance real de
/// interromper antes do fim da lista.
pub const UNLINK_BATCH: usize = 500;

/// Erro que CARREGA o parcial: o que já saiu, saiu, e quem grava o
/// histórico (tarefa seguinte) precisa do número de verdade mesmo quando a
/// conexão morre no meio de uma exclusão em lotes — um `ConnError` sozinho
/// perderia essa contagem.
#[derive(Debug)]
pub struct BulkError {
    pub deleted: u64,
    /// Quantas chaves foram TENTADAS — ver `BulkDeleted::attempted`. Inclui
    /// o lote que falhou: ele foi enviado, e um `UNLINK` que o servidor
    /// aplicou antes de a resposta se perder apagou essas chaves de
    /// qualquer jeito. A cópia de segurança precisa continuar descrevendo-as.
    pub attempted: u64,
    pub source: ConnError,
}

/// O que uma exclusão em lotes fez: quantas o servidor disse ter apagado e
/// quantas foram TENTADAS.
///
/// Os dois números existem porque nenhum dos dois serve sozinho:
///
/// - `deleted` é um **limite inferior**. `Pool::with_retry` reenvia o mesmo
///   `UNLINK` quando a conexão morre, e `UNLINK` é idempotente no estado
///   final mas não no valor de retorno: se o servidor aplicou o primeiro
///   envio e a resposta se perdeu, o reenvio devolve 0 para chaves que já
///   saíram. Ele também é menor que o pedido quando uma chave expirou
///   sozinha entre a contagem e a exclusão.
/// - `attempted` é um **limite superior**, e não depende de resposta
///   nenhuma: é a soma dos tamanhos dos lotes enviados. Como os lotes correm
///   na ORDEM da lista e `cancel` só é conferido entre eles, `keys[..attempted]`
///   descreve exatamente o conjunto tentado — nem mais, nem menos. É por isso
///   que um `u64` basta e não é preciso devolver nomes.
///
/// Quem recorta a cópia de segurança e os caches da tela usa `attempted`:
/// errar para o lado de guardar o retrato de uma chave que ficou é
/// recuperável; oferecer a volta atrás de uma chave que nunca foi tocada,
/// não.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkDeleted {
    pub deleted: u64,
    pub attempted: u64,
}

/// O laço de lotes, sem Redis: `apagar` é a costura que de fato conversa
/// com o servidor. `unlink_keys` passa um fechamento que manda `UNLINK`;
/// os testes passam um fechamento que devolve resultados forjados — é o
/// que torna o tamanho do lote e a falha no meio testáveis sem contêiner e
/// sem precisar criar 1200 chaves de verdade.
///
/// `cancel` só é conferido ENTRE lotes, nunca dentro de um: o lote corrente
/// sempre termina antes de parar, para não deixar uma exclusão pela
/// metade dentro do próprio lote.
///
/// Conferido no TOPO do laço além do fim dele, e não é redundância. Quem
/// chama este laço (`History::delete_prefix_with_backup`) passa antes pela
/// leitura dos retratos para a cópia de segurança — uma ida e volta por
/// chave, que num prefixo grande demora. Um clique em "desistir" cai
/// naturalmente aí dentro, e só com a conferência no fim o primeiro lote
/// saía mesmo assim: "para depois do lote corrente" era obedecido ao pé da
/// letra num momento em que não havia lote corrente nenhum.
pub(crate) async fn unlink_em_lotes<F, Fut>(
    keys: &[String],
    lote: usize,
    cancel: &AtomicBool,
    progress: impl Fn(u64),
    apagar: F,
) -> Result<BulkDeleted, BulkError>
where
    F: Fn(&[String]) -> Fut,
    Fut: Future<Output = Result<u64, ConnError>>,
{
    let mut total: u64 = 0;
    let mut attempted: u64 = 0;
    let tamanho_lote = lote.max(1);

    for pedaco in keys.chunks(tamanho_lote) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        // Somado ANTES de pedir, e não depois de a resposta chegar: um lote
        // enviado foi tentado mesmo que a resposta se perca, e é justamente
        // nesse caso que a cópia de segurança precisa descrevê-lo.
        attempted += pedaco.len() as u64;

        match apagar(pedaco).await {
            Ok(apagadas) => {
                total += apagadas;
                progress(total);
            }
            Err(source) => {
                return Err(BulkError {
                    deleted: total,
                    attempted,
                    source,
                });
            }
        }

        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }

    Ok(BulkDeleted {
        deleted: total,
        attempted,
    })
}

/// Apaga as chaves enumeradas, em lotes de `UNLINK_BATCH`, via `UNLINK` —
/// nunca `DEL`: `DEL` libera a memória do valor na mesma linha de execução
/// do servidor, e um lote com valores grandes prenderia o Redis inteiro
/// enquanto libera; `UNLINK` manda essa liberação para uma thread de
/// segundo plano.
///
/// Devolve `BulkDeleted`: quantas o servidor disse ter apagado e quantas
/// foram tentadas. O primeiro número pode ser menor que `keys.len()` por
/// dois motivos diferentes — uma chave que sumiu sozinha (expirou, ou outra
/// conexão a apagou) entre a enumeração e a exclusão, e um lote que
/// `Pool::with_retry` reenviou depois de perder a resposta de um `UNLINK`
/// que o servidor JÁ tinha aplicado. O segundo caso faz `deleted` ser um
/// **limite inferior**, não um número exato: ver `BulkDeleted`.
pub async fn unlink_keys(
    pool: &Pool,
    alvo: Alvo<'_>,
    keys: &[String],
    cancel: &AtomicBool,
    progress: impl Fn(u64),
) -> Result<BulkDeleted, BulkError> {
    let Alvo { host, pw, db } = alvo;

    unlink_em_lotes(keys, UNLINK_BATCH, cancel, progress, |lote: &[String]| {
        let lote = lote.to_vec();
        async move {
            pool.with_retry(host, db, pw, move |mut conn| {
                let lote = lote.clone();
                async move {
                    let apagadas: u64 = redis::cmd("UNLINK")
                        .arg(lote)
                        .query_async(&mut conn)
                        .await?;
                    Ok(apagadas)
                }
            })
            .await
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Env, Host};

    /// `filtra_lote` é pura e síncrona — não precisa do Redis de teste para
    /// provar a deduplicação. Forçar um servidor de verdade a devolver a
    /// mesma chave duas vezes (o caso real do `SCAN` durante um rehash) não
    /// é algo que dá para reproduzir de forma confiável num teste; alimentar
    /// a repetição à mão aqui é a mesma ideia do teste do teto, que também
    /// controla o parâmetro em vez de recriar a condição real.
    #[test]
    fn filtra_lote_dedups_a_key_seen_twice_in_the_same_batch() {
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (candidatos, truncated) = filtra_lote(
            &modo,
            vec!["a".to_string(), "a".to_string(), "b".to_string()],
            &mut vistas,
            10,
        );

        assert_eq!(candidatos, vec!["a".to_string(), "b".to_string()]);
        assert!(!truncated);
    }

    #[test]
    fn filtra_lote_dedups_a_key_seen_again_in_a_later_batch() {
        // O caso real: o cursor do `SCAN` percorre de novo um bucket já
        // visitado depois de um rehash do servidor, e a mesma chave volta
        // num lote seguinte — não dentro do mesmo lote.
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (primeiro, _) = filtra_lote(&modo, vec!["a".to_string()], &mut vistas, 10);
        assert_eq!(primeiro, vec!["a".to_string()]);

        let (segundo, truncated) = filtra_lote(
            &modo,
            vec!["a".to_string(), "c".to_string()],
            &mut vistas,
            10,
        );
        assert_eq!(
            segundo,
            vec!["c".to_string()],
            "\"a\" já tinha sido aceita no lote anterior"
        );
        assert!(!truncated);
    }

    #[test]
    fn filtra_lote_repeats_do_not_spend_a_slot_of_the_ceiling() {
        // O teto conta chaves DISTINTAS: uma repetição não pode empurrar uma
        // chave nova para fora da lista.
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (candidatos, truncated) = filtra_lote(
            &modo,
            vec![
                "a".to_string(),
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
            ],
            &mut vistas,
            2,
        );

        assert_eq!(candidatos, vec!["a".to_string(), "b".to_string()]);
        assert!(truncated, "\"c\" não coube no teto de 2 chaves distintas");
    }

    /// Banco 13: nenhum outro teste do projeto usa este número (ver
    /// `docs/CONTEXTO.md`), então os testes deste módulo podem rodar em
    /// paralelo sem disputar banco com o resto da suíte.
    fn test_host() -> Host {
        Host {
            id: "bulk-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            db: 13,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    fn alvo(host: &Host) -> Alvo<'_> {
        Alvo {
            host,
            pw: None,
            db: host.db,
        }
    }

    /// Cria chaves, roda o corpo e apaga tudo — mesmo se o corpo falhar numa
    /// asserção, para não deixar lixo no banco compartilhado. Cópia do
    /// auxiliar de `core::keys::tests`, contra o banco 13 em vez do 1.
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

    fn prefixos(lista: &[&str]) -> Vec<String> {
        lista.iter().map(|p| p.to_string()).collect()
    }

    fn ordenadas(mut nomes: Vec<String>) -> Vec<String> {
        nomes.sort();
        nomes
    }

    #[tokio::test]
    async fn enumerates_only_the_prefix() {
        let chaves = ["only_rs_test:a:1", "only_rs_test:a:2", "only_rs_test:b:1"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["only_rs_test:a:"]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "only_rs_test:a:1".to_string(),
                    "only_rs_test:a:2".to_string()
                ]
            );
            assert!(!enumeration.truncated);
        })
        .await;
    }

    #[tokio::test]
    async fn the_prefix_itself_is_included() {
        // Review Focus 1: `wallet` é prefixo de `wallet:1` E uma chave por
        // si só. As duas precisam sair enumeradas, ou a própria chave
        // escaparia da exclusão em massa.
        let chaves = [
            "prefix_itself_rs_test:wallet",
            "prefix_itself_rs_test:wallet:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["prefix_itself_rs_test:wallet"]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "prefix_itself_rs_test:wallet".to_string(),
                    "prefix_itself_rs_test:wallet:1".to_string(),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn wildcard_in_prefix_does_not_leak() {
        // `*` é curinga do MATCH. Sem escapá-lo, o prefixo `lit*` passaria a
        // casar com qualquer coisa que comece com `lit`, trazendo chaves de
        // outro galho como se fossem dele.
        let chaves = ["wildcard_rs_test:lit*:1", "wildcard_rs_test:outro:1"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["wildcard_rs_test:lit*"]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                enumeration.keys,
                vec!["wildcard_rs_test:lit*:1".to_string()]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn degenerate_names_survive() {
        // Review Focus 2: nomes degenerados sob o prefixo vazio — inclusive
        // uma chave de nome vazio — não podem se perder nem se fundir uns
        // com os outros.
        let chaves = ["", ":", "a:", "::b"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&[""]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();

            for chave in chaves {
                assert!(
                    enumeration.keys.iter().any(|k| k == chave),
                    "chave degenerada \"{chave}\" sumiu da enumeração: {:?}",
                    enumeration.keys
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn empty_prefix_takes_the_whole_db() {
        // Sem prefixo nenhum (lista vazia) ou com um prefixo vazio — as duas
        // formas de pedir o banco inteiro. O banco 13 é compartilhado com
        // outros testes deste arquivo rodando em paralelo, então a asserção
        // é "as chaves criadas aparecem", não "só elas existem".
        let chaves = ["whole_db_rs_test:x", "whole_db_rs_test:y"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let sem_prefixo_nenhum =
                enumerate(&pool, alvo(&host), &[], &AtomicBool::new(false), |_| {})
                    .await
                    .unwrap();
            for chave in chaves {
                assert!(sem_prefixo_nenhum.keys.iter().any(|k| k == chave));
            }

            let com_prefixo_vazio = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&[""]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
            for chave in chaves {
                assert!(com_prefixo_vazio.keys.iter().any(|k| k == chave));
            }
        })
        .await;
    }

    #[tokio::test]
    async fn several_prefixes_scan_once() {
        let chaves = [
            "several_rs_test:a:1",
            "several_rs_test:b:1",
            "several_rs_test:c:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["several_rs_test:a:", "several_rs_test:c:"]),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "several_rs_test:a:1".to_string(),
                    "several_rs_test:c:1".to_string()
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn kinds_are_counted() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "kinds_rs_test:";
        let string1 = format!("{prefix}string:1");
        let string2 = format!("{prefix}string:2");
        let hash1 = format!("{prefix}hash:1");

        redis::cmd("SET")
            .arg(&string1)
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("SET")
            .arg(&string2)
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("HSET")
            .arg(&hash1)
            .arg("campo")
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();

        let resultado = enumerate(
            &pool,
            alvo(&host),
            &prefixos(&[prefix]),
            &AtomicBool::new(false),
            |_| {},
        )
        .await;

        for chave in [&string1, &string2, &hash1] {
            let _: () = redis::cmd("DEL")
                .arg(chave)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let enumeration = resultado.unwrap();
        assert_eq!(enumeration.keys.len(), 3);

        let contagem = |alvo_kind: KeyKind| -> u64 {
            enumeration
                .kinds
                .iter()
                .find(|(k, _)| *k == alvo_kind)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(contagem(KeyKind::String), 2);
        assert_eq!(contagem(KeyKind::Hash), 1);
    }

    #[tokio::test]
    async fn stops_at_the_ceiling() {
        let chaves: Vec<String> = (0..7).map(|i| format!("ceiling_rs_test:{i:02}")).collect();
        let refs: Vec<&str> = chaves.iter().map(|s| s.as_str()).collect();

        com_chaves(&refs, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate_com_teto(
                &pool,
                alvo(&host),
                &prefixos(&["ceiling_rs_test:"]),
                &AtomicBool::new(false),
                |_| {},
                5,
            )
            .await
            .unwrap();

            assert!(enumeration.truncated, "devia avisar que parou no teto");
            assert_eq!(enumeration.keys.len(), 5);
        })
        .await;
    }

    /// Converte uma lista de nomes literais num `Vec<String>` — mesma ideia
    /// de `prefixos`, mas usada para nomes de chave de verdade, não
    /// prefixos de busca.
    fn nomes(lista: &[&str]) -> Vec<String> {
        lista.iter().map(|n| n.to_string()).collect()
    }

    #[tokio::test]
    async fn deletes_what_it_was_given() {
        let chaves = ["unlink_rs_test:a", "unlink_rs_test:b", "unlink_rs_test:c"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let cancel = AtomicBool::new(false);

            let apagadas = unlink_keys(&pool, alvo(&host), &nomes(&chaves), &cancel, |_| {})
                .await
                .unwrap();

            assert_eq!(apagadas.deleted, 3);
            assert_eq!(apagadas.attempted, 3);

            let mut conn = pool.get(&host, host.db, None).await.unwrap();
            for chave in chaves {
                let existe: bool = redis::cmd("EXISTS")
                    .arg(chave)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                assert!(!existe, "\"{chave}\" devia ter sido apagada");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn leaves_the_rest_alone() {
        let chaves = ["leaves_rs_test:a:1", "leaves_rs_test:b:1"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let cancel = AtomicBool::new(false);

            let apagadas = unlink_keys(
                &pool,
                alvo(&host),
                &nomes(&["leaves_rs_test:a:1"]),
                &cancel,
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(apagadas.deleted, 1);

            let mut conn = pool.get(&host, host.db, None).await.unwrap();
            let existe: bool = redis::cmd("EXISTS")
                .arg("leaves_rs_test:b:1")
                .query_async(&mut conn)
                .await
                .unwrap();
            assert!(existe, "\"leaves_rs_test:b:1\" não devia ter sido apagada");
        })
        .await;
    }

    #[tokio::test]
    async fn counts_what_actually_went() {
        // Review Focus 3: uma chave da lista já não existe de verdade (uma
        // outra conexão a apagou, ou ela expirou, entre a enumeração e a
        // exclusão) — o servidor conta só o que de fato removeu, não o
        // tamanho da lista pedida.
        let chaves = ["counts_rs_test:a", "counts_rs_test:b"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();
            let mut conn = pool.get(&host, host.db, None).await.unwrap();
            let _: () = redis::cmd("DEL")
                .arg("counts_rs_test:b")
                .query_async(&mut conn)
                .await
                .unwrap();

            let cancel = AtomicBool::new(false);
            let apagadas = unlink_keys(&pool, alvo(&host), &nomes(&chaves), &cancel, |_| {})
                .await
                .unwrap();

            assert_eq!(
                apagadas.deleted, 1,
                "só \"counts_rs_test:a\" existia de verdade"
            );
            assert_eq!(
                apagadas.attempted, 2,
                "as duas foram TENTADAS: a que já tinha sumido também entrou no \
                 `UNLINK`"
            );
        })
        .await;
    }

    /// Desistir ANTES do primeiro lote não gasta um lote.
    ///
    /// Quem chama o laço lê os retratos da cópia de segurança antes dele, uma
    /// ida e volta por chave, e o clique cai naturalmente aí dentro. Com a
    /// bandeira conferida só no fim do laço, as 500 primeiras iam embora de
    /// qualquer jeito.
    #[tokio::test]
    async fn cancel_before_the_first_batch_deletes_nothing() {
        let keys: Vec<String> = (0..6)
            .map(|i| format!("cancel_rs_test:antes:{i}"))
            .collect();
        let cancel = AtomicBool::new(true);
        // `apagar` é `Fn`, então o contador precisa de mutabilidade interna.
        let lotes = std::sync::atomic::AtomicUsize::new(0);

        let total = unlink_em_lotes(
            &keys,
            2,
            &cancel,
            |_| {},
            |lote: &[String]| {
                lotes.fetch_add(1, Ordering::Relaxed);
                let quantas = lote.len() as u64;
                async move { Ok(quantas) }
            },
        )
        .await
        .unwrap();

        assert_eq!(total.deleted, 0, "nada podia ter saído");
        assert_eq!(
            total.attempted, 0,
            "nenhuma chave podia ter sido TENTADA — é este zero que apaga a \
             linha do histórico em vez de deixá-la oferecendo uma volta atrás"
        );
        assert_eq!(
            lotes.load(Ordering::Relaxed),
            0,
            "nenhum lote podia sequer ter sido tentado"
        );
    }

    #[tokio::test]
    async fn cancel_stops_and_reports() {
        // Sem Redis: o fechamento marca `cancel` ao processar o 1º lote, e
        // o laço precisa parar ANTES do 2º — nunca no meio de um lote.
        let keys: Vec<String> = (0..6).map(|i| format!("cancel_rs_test:{i}")).collect();
        let cancel = AtomicBool::new(false);
        let lotes_vistos = std::sync::Mutex::new(Vec::<usize>::new());

        let total = unlink_em_lotes(
            &keys,
            2,
            &cancel,
            |_| {},
            |lote: &[String]| {
                lotes_vistos.lock().unwrap().push(lote.len());
                cancel.store(true, Ordering::Relaxed);
                let n = lote.len() as u64;
                async move { Ok(n) }
            },
        )
        .await
        .unwrap();

        assert_eq!(total.deleted, 2, "só o 1º lote devia ter rodado");
        assert_eq!(
            total.attempted, 2,
            "o conjunto tentado é o 1º lote inteiro, e é ele que a cópia \
             precisa descrever"
        );
        assert_eq!(*lotes_vistos.lock().unwrap(), vec![2]);
    }

    #[tokio::test]
    async fn batches_are_bounded() {
        let keys: Vec<String> = (0..1200).map(|i| format!("batch_rs_test:{i:04}")).collect();
        let cancel = AtomicBool::new(false);
        let tamanhos = std::sync::Mutex::new(Vec::<usize>::new());

        let total = unlink_em_lotes(
            &keys,
            UNLINK_BATCH,
            &cancel,
            |_| {},
            |lote: &[String]| {
                tamanhos.lock().unwrap().push(lote.len());
                let n = lote.len() as u64;
                async move { Ok(n) }
            },
        )
        .await
        .unwrap();

        assert_eq!(*tamanhos.lock().unwrap(), vec![500, 500, 200]);
        assert_eq!(total.deleted, 1200);
        assert_eq!(total.attempted, 1200);
    }

    #[tokio::test]
    async fn a_failed_batch_carries_the_partial() {
        // Review Focus 5: uma falha de conexão no meio de uma exclusão não
        // pode ser injetada contra um contêiner de verdade — por isso o
        // fechamento forjado, que acerta o 1º lote e falha o 2º.
        let keys: Vec<String> = (0..1000).map(|i| format!("fail_rs_test:{i:04}")).collect();
        let cancel = AtomicBool::new(false);
        let chamada = std::sync::atomic::AtomicUsize::new(0);

        let erro = unlink_em_lotes(
            &keys,
            500,
            &cancel,
            |_| {},
            |lote: &[String]| {
                let indice = chamada.fetch_add(1, Ordering::Relaxed);
                let n = lote.len() as u64;
                async move {
                    if indice == 0 {
                        Ok(n)
                    } else {
                        Err(ConnError::Protocol(
                            "conexão caiu no meio do lote".to_string(),
                        ))
                    }
                }
            },
        )
        .await
        .unwrap_err();

        assert_eq!(erro.deleted, 500);
        assert_eq!(
            erro.attempted, 1000,
            "o lote que falhou FOI tentado: suas chaves podem ter saído com a \
             resposta perdida, e a cópia precisa continuar descrevendo-as"
        );
    }

    /// Falha no PRIMEIRO lote (`indice == 0`) — a fronteira que
    /// `delete_prefix_com_teto` passou a usar como corte.
    ///
    /// Zero apagadas com um lote tentado é diferente de zero apagadas sem
    /// lote nenhum: o primeiro pode ter deixado chaves no chão com a
    /// resposta perdida, o segundo não tocou em nada.
    #[tokio::test]
    async fn a_failed_first_batch_reports_zero_deleted_and_one_batch_attempted() {
        let keys: Vec<String> = (0..1000).map(|i| format!("fail0_rs_test:{i:04}")).collect();
        let cancel = AtomicBool::new(false);

        let erro = unlink_em_lotes(
            &keys,
            500,
            &cancel,
            |_| {},
            |_: &[String]| async {
                Err(ConnError::Protocol(
                    "a conexão caiu no primeiro lote".to_string(),
                ))
            },
        )
        .await
        .unwrap_err();

        assert_eq!(erro.deleted, 0, "nada chegou a ser contado");
        assert_eq!(
            erro.attempted, 500,
            "o primeiro lote foi tentado, e isso não é o mesmo que não ter \
             começado"
        );
    }

    /// `deleted` é um LIMITE INFERIOR: `Pool::with_retry` reenvia o mesmo
    /// `UNLINK` quando a conexão morre, e um reenvio de comando já aplicado
    /// devolve 0 para chaves que saíram. `attempted` é o número que não
    /// depende da resposta do servidor.
    #[tokio::test]
    async fn attempted_counts_a_batch_the_server_reported_as_zero() {
        let keys: Vec<String> = (0..600).map(|i| format!("lower_rs_test:{i:04}")).collect();
        let cancel = AtomicBool::new(false);

        let total = unlink_em_lotes(&keys, 500, &cancel, |_| {}, |_: &[String]| async { Ok(0) })
            .await
            .unwrap();

        assert_eq!(total.deleted, 0, "o servidor disse que não apagou nada");
        assert_eq!(
            total.attempted, 600,
            "tentadas foram todas, e é o que a cópia e os caches precisam saber"
        );
    }

    /// O `progress` do laço é o único sinal que a pessoa tem durante a
    /// exclusão, e nenhum teste conferia os VALORES que ele recebe.
    #[tokio::test]
    async fn progress_reports_the_running_total_after_each_batch() {
        let keys: Vec<String> = (0..1200).map(|i| format!("prog_rs_test:{i:04}")).collect();
        let cancel = AtomicBool::new(false);
        let vistos = std::sync::Mutex::new(Vec::<u64>::new());

        unlink_em_lotes(
            &keys,
            500,
            &cancel,
            |quantas| vistos.lock().unwrap().push(quantas),
            |lote: &[String]| {
                let n = lote.len() as u64;
                async move { Ok(n) }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *vistos.lock().unwrap(),
            vec![500, 1000, 1200],
            "o andamento é o ACUMULADO, um retrato por lote — nunca o tamanho \
             do lote sozinho"
        );
    }

    /// Desistir durante a enumeração para a varredura em vez de deixá-la
    /// percorrer o espaço de chaves inteiro.
    #[tokio::test]
    async fn a_cancelled_enumeration_stops_and_says_so() {
        let pool = Pool::new();
        let host = test_host();
        let cancel = AtomicBool::new(true);

        let enumeration = enumerate(
            &pool,
            alvo(&host),
            &prefixos(&["cancel_scan_rs_test:"]),
            &cancel,
            |_| {},
        )
        .await
        .unwrap();

        assert!(
            enumeration.cancelled,
            "a enumeração precisa DIZER que parou por desistência — a tela não \
             pode receber uma lista cortada como se fosse o prefixo inteiro"
        );
        assert!(
            enumeration.keys.is_empty(),
            "nenhuma volta do cursor aconteceu"
        );
        assert!(
            !enumeration.truncated,
            "parar por desistência não é parar no teto"
        );
    }
}
