# Menu de contexto no prefixo, e apagar em massa

Desenho. 2026-11-24.

---

## Intenção

Hoje o botão direito numa linha da árvore cai no menu nativo do sistema —
"Usar Siri", "Traduzir", "Procurar no Google". Nenhuma dessas coisas tem a
ver com o banco que está na tela.

Quem opera quer, naquele clique, três coisas: **limpar em massa** o que
está sob um prefixo, **achar** alguma coisa lá dentro, e **ver** como aquele
galho é usado. Esta leva entrega a primeira e o menu que hospeda as outras
duas.

O caso concreto que a pediu: um `cache:` com 11 chaves e um `wallet:` com
1.395, e a necessidade de zerar um deles sem apagar chave por chave e sem
`FLUSHDB`.

## Objetivos

1. Botão direito numa linha da árvore abre um menu do aplicativo, não do
   sistema.
2. Apagar **todas** as chaves sob um prefixo, com a contagem real conferida
   na hora.
3. Apagar sob **vários** prefixos selecionados, ou sob todos os de primeiro
   nível.
4. Perguntar antes se quer cópia de segurança. Sim faz; não apaga direto e o
   histórico registra, em letras, que foi **sem cópia**.
5. Uma entrada de histórico por operação, com volta atrás que restaura tudo
   de uma vez.
6. Tudo construído por TDD: teste escrito primeiro, falha observada,
   implementação depois.

## Não-objetivos (nesta leva)

- **Busca dentro do galho.** Vai no mesmo menu, depois. É a pergunta aberta
  nº 4 do `CONTEXTO.md` e tem desenho próprio pela frente.
- **Estatística de uso do prefixo.** Decidida (abordagem E2, abaixo), não
  construída aqui.
- **Apagar por padrão digitado à mão** (um campo onde se escreve `*:tmp:*`).
  O alvo é sempre um prefixo que está na árvore, visível, clicado.
- **Expor isso ao MCP.** Apagar em massa pela LLM é outra conversa de risco,
  e esta leva não a abre. As ferramentas MCP seguem como estão.

## Anti-objetivos

Coisas que fariam isto fracassar mesmo funcionando:

- **Apagar mais do que o diálogo mostrou.** O número no diálogo é um
  contrato.
- **Apagar menos e dizer que apagou tudo.** Sobrar chave em silêncio é pior
  que falhar alto.
- **Cópia de segurança que não restaura.** Backup não testado é backup que
  não existe.
- **Contornar a trava de produção.** A trava mora em `commands.rs`; uma
  operação nova que a implemente por conta própria na tela é um buraco.
- **Travar a janela** durante a varredura de um banco grande.

## Restrições

Herdadas do projeto, todas já escritas em `CONTEXTO.md`:

- `KEYS`, `FLUSHALL` e `FLUSHDB` continuam proibidos. Apagar "tudo" aqui é
  sempre **enumerar e apagar o que foi enumerado** — nunca o comando de
  limpar banco. A diferença não é cosmética: o caminho enumerado tem
  contagem conferida, cópia de segurança, histórico e volta atrás; o
  `FLUSHDB` não tem nenhum dos quatro.
- `SCAN MATCH` **não encurta o trabalho do servidor**. Ele filtra o que volta
  pela rede; o cursor percorre o espaço de chaves inteiro de todo jeito.
  Logo: uma varredura por operação, nunca uma por prefixo selecionado.
- Escrita em host `prod` exige confirmação decidida em `commands.rs`, não na
  tela.
- Nada fala com o Redis fora de `core/`; nada fala com o `core` a partir da
  tela sem passar por `commands.rs`.
- Identificadores em inglês no Rust; comentários e texto de tela em
  português. Os três catálogos de idioma têm a forma do `pt-BR.ts` — chave
  faltando não compila.
- O vermelho `--prod` e a caixa alta significam ambiente, e só.

## Abordagens consideradas

### Onde mora o apagar em massa

**Escolhida — operação nova no núcleo.** Um módulo `core/bulk.rs` com
`delete_prefix`, enumeração própria por `SCAN MATCH`, e um formato novo de
entrada de histórico que guarda N retratos num registro só. A tela chama uma
vez e acompanha por evento de progresso, como já faz com a varredura.

**Descartada — laço na tela sobre `write_value`.** Zero Rust novo: a tela
enumeraria e chamaria `writeValue(..., {op:'delete_key'})` por chave. Custa
1.395 idas e voltas pelo Tauri, produz 1.395 linhas de histórico, e a trava
de produção dispararia 1.395 diálogos ou seria burlada uma vez só no começo.
Não dá desfazer atômico, que foi o que se pediu.

**Descartada — laço no Rust reusando `write_with_backup`.** Barato de
escrever e sem os problemas de diálogo, mas o histórico vira N linhas do
mesmo jeito, e cada chave paga uma leitura completa mais um `INSERT`.

### O que "fazer o backup" produz

**Escolhida — uma entrada só, com teto.** Um registro de histórico
descrevendo a operação inteira, com os retratos de todas as chaves dentro
dele, e um "Voltar atrás" que restaura tudo. Passando do teto de tamanho, o
aplicativo avisa e oferece gravar **em arquivo**, com a linha do histórico
apontando para ele.

**Descartada — N entradas, uma por chave.** É o que o código já faz hoje por
escrita avulsa. Desfazer item a item não é desfazer uma limpeza de cache, e
o SQLite engorda com o valor inteiro de cada chave em linha separada.

**Descartada como padrão — arquivo em disco sempre.** Sobrevive ao
aplicativo e viaja, mas transforma todo desfazer num importar, e a limpeza
de 11 chaves não merece um arquivo. Fica como o degrau de cima do teto.

### De onde viria a estatística (decidida, construída depois)

**Escolhida — E2, varredura dirigida coletando metadado.** `TYPE`, `PTTL` e
`MEMORY USAGE` por chave do prefixo, produzindo quebra por tipo, memória
total, quantas chaves sem TTL e as maiores. Custa uma varredura mais os
pipelines de metadado — exatamente o que foi tirado da varredura de árvore
por custo (11 idas e voltas viraram 350 quando esse metadado vinha junto).
Por isso: **sob pedido explícito**, com progresso, nunca por conta própria.

**Descartada — só o que a trie já sabe.** Contagem, número de filhos,
profundidade. Instantâneo e grátis, mas não responde "uso": não sabe tipo,
tamanho nem TTL.

**Descartada — amostragem com extrapolação.** Rápida e aproximada. Número
aproximado numa tela onde se decide apagar é armadilha: a pessoa apaga com
base nele.

### Ordem de entrega

**Escolhida — apagar primeiro.** Menu, apagar em massa e seleção múltipla
fecham e vão ao ar. Busca no galho e estatística entram depois, no menu já
pronto. A parte perigosa merece atenção isolada.

---

## O desenho

### 1. O menu

Componente novo `MenuContexto.svelte`, genérico: recebe a posição e uma
lista de itens, devolve o escolhido.

- Abre no `contextmenu` da linha, com `preventDefault()` — é isso que tira o
  menu do sistema do caminho.
- Abre também pelo teclado (tecla Menu, e `Shift+F10`), ancorado na linha
  com foco. Um menu que só existe para o mouse não serve a quem opera de
  teclado.
- `Escape` fecha e devolve o foco à linha de onde saiu. Clique fora fecha.
  Setas navegam, `Enter` escolhe.
- Posiciona no cursor e **vira** quando não cabe: para cima se a borda de
  baixo está perto, para a esquerda se a direita está.

**Itens em linha de prefixo, nesta leva:**

| Item | O que faz |
|:-|:-|
| Apagar tudo sob este prefixo… | Abre o fluxo da seção 3 |
| Copiar prefixo | Põe `wallet:` na área de transferência |

As reticências são contrato: o item que abre diálogo as tem, o que age
sozinho não.

**Itens em linha de chave (folha):** `Copiar nome` e `Apagar chave…`. Esta
segunda chama o `write_value` que já existe, sem código novo de escrita.
Vale a pena: sem ela, o botão direito funciona em metade das linhas e cai no
menu do sistema na outra metade — e inconsistência dessas lê como defeito.
**Chamada minha**, não pedida.

Os itens de busca e estatística **não aparecem desabilitados**. Item cinza
que nunca acende ensina a pessoa a ignorar o menu.

### 2. Seleção múltipla

Convenção de gerenciador de arquivos, porque é a que a mão já sabe:

- `⌘+clique` (Ctrl fora do mac) junta ou tira um prefixo da seleção.
- `Shift+clique` pega a faixa desde a última linha clicada.
- `⌘+A`, com o foco na árvore, seleciona os prefixos **do primeiro nível** —
  é o "todos de uma vez".
- Botão direito **numa linha já selecionada** age sobre a seleção inteira; o
  item passa a ler "Apagar tudo sob 3 prefixos…".
- Botão direito **numa linha fora da seleção** limpa a seleção e age só
  naquela linha. É o comportamento do Finder, e quem erra o clique perde a
  seleção em vez de apagar o que não queria.
- A linha selecionada ganha marca visível que **não** usa vermelho nem caixa
  alta — esses dois significam ambiente, e só.
- A seleção guarda **só prefixos**. `⌘+clique` numa folha não junta nada:
  misturar chave avulsa e galho numa mesma seleção faria o item do menu ter
  que falar das duas coisas ao mesmo tempo, e nenhuma frase fica honesta
  assim.

Selecionar todos os prefixos de primeiro nível equivale, no efeito, a
esvaziar o banco. O diálogo diz isso com todas as letras ("todas as 1.559
chaves do banco 0"), e o caminho continua sendo o enumerado: contagem
conferida, cópia, histórico e volta atrás. O `FLUSHDB` segue proibido e não é
usado aqui.

### 3. O fluxo de apagar

```
 clique direito → menu → "Apagar tudo sob este prefixo…"
        ↓
 [1] enumerar          SCAN MATCH, com progresso e cancelar
        ↓
 [2] diálogo           host, ambiente, banco, prefixo(s),
                       CONTAGEM CONFERIDA, escolha da cópia
        ↓ (prod: digitar o nome do host)
 [3] apagar            UNLINK em lotes, com progresso e cancelar
        ↓
 [4] histórico         uma entrada, com ou sem cópia
```

**[1] Enumerar antes de perguntar.** A contagem ao lado do prefixo vem da
trie, que é cache: pode estar parcial (varredura em curso) ou cortada (teto
de 50.000 por nível). Perguntar "apagar 11 chaves?" e apagar 400 é o pior
defeito que esta funcionalidade pode ter. Então o número do diálogo sai de
uma varredura feita naquele instante.

Função nova `core/keys.rs::scan_prefix_keys`, irmã de `expand_branch`: mesmo
`SCAN MATCH` com o mesmo escape de curinga (`escapar_glob` — sem ele um
prefixo com `*` passa a casar com galhos alheios), mesmo `with_retry`, mesmo
`ScanTick` de progresso. Difere em juntar **nomes de chave**, não agregar
filhos.

Com vários prefixos selecionados: **uma** varredura só, filtrando contra
todos eles. O custo do `SCAN` é do banco inteiro por varredura — fazer uma
por prefixo multiplicaria o preço pelo número de seleções sem ganhar nada.

A mesma passagem coleta o **`TYPE`** de cada chave, por pipeline no lote que
já está voltando. Dois motivos: `set`, `stream` e RedisJSON não têm volta
atrás (`HistoryError::UnsupportedKind`), e isso precisa ser dito **antes** de
apagar, não descoberto depois; e essa quebra por tipo é o primeiro pedaço da
estatística E2, construída aqui de graça. `PTTL` e `MEMORY USAGE` ficam de
fora — são eles que levaram a varredura de 11 para 350 idas e voltas, e esta
leva não precisa deles.

**Teto de coleta.** Juntar nomes num `Vec` acompanha o tamanho do prefixo: um
galho de milhões custaria centenas de megabytes (foi por isso que
`scan_matching_names` nasceu filtrando em vez de juntar). A coleta **para** no
teto de `100_000` e marca o resultado como cortado; o diálogo então diz "mais
de 100.000" e manda para o caminho do arquivo, sem nunca ter carregado o
banco todo na memória.

**O intervalo entre contar e apagar.** Apaga-se a lista enumerada no passo
[1], não o que o prefixo tiver no passo [3]. Chave criada no meio **não** é
apagada; chave que sumiu sozinha já estava fora. É o contrato do
anti-objetivo: o número mostrado é o número apagado, e o diálogo é a foto
daquele instante.

**[2] O diálogo.** Mostra, nesta ordem: o carimbo de ambiente, o nome do
host, o banco, o prefixo (ou a lista, com "e mais N" quando passar de cinco)
e a contagem conferida. Abaixo, a escolha da cópia, com **"com cópia" já
marcado** — o padrão é o caminho reversível.

Quando a enumeração achou chaves de tipo sem volta atrás (`set`, `stream`,
RedisJSON), o diálogo diz quantas são, **mesmo com cópia marcada**: a cópia
guarda o que leu, mas o voltar atrás não vai recriar essas. Dizer isso depois
de apagar não serve para nada.

Escolher "sem cópia" troca o texto do botão e acende o aviso de
irreversível. Não pede nada além disso fora de produção: quem desmarcou leu.

**Em produção, pede o nome do host digitado**, exatamente, e o botão só
acende quando confere. Reusa a regra de exatidão que já existe em
`approval/mod.rs:143` para a escrita da LLM.

> **Tensão registrada, e por que resolvi assim.** O `ConfirmDialog.svelte`
> diz, em comentário, que a decisão do produto é "que a pessoa seja avisada,
> não que seja obrigada a datilografar". Essa decisão continua valendo para o
> que ela governa: **uma** escrita em **uma** chave, com cópia automática e
> volta atrás a um clique. Aqui o raio é de N chaves e o caminho "sem cópia"
> não tem volta. Datilografar o nome do host nessa combinação é a mesma
> régua que o projeto já aplica à escrita da LLM, pelo mesmo motivo: o custo
> de errar não cabe num clique. O `ConfirmDialog` existente **não muda** —
> ganha um irmão para este caso.

**[3] Apagar.** `UNLINK` em lotes por pipeline, não `DEL`: `DEL` libera a
memória na mesma linha de execução e um lote de valores grandes prende o
servidor; `UNLINK` entrega a liberação a uma linha de fundo. Num banco de
produção essa diferença é a diferença entre uma limpeza e um incidente.

Cancelar para depois do lote corrente. **O que já foi apagado está apagado** —
e o histórico registra o que de fato saiu, não o que foi pedido. Uma
operação cancelada aparece dita assim.

**[4] Caches.** Cada chave apagada sai da trie (`KeyTrie::remover`) e do
índice FTS5 (`index.forget_key`), exatamente como `write_value` já faz com
`DeleteKey`. Sem isso, a árvore mostraria contagem de coisa que não existe e
a busca devolveria texto que não está mais lá.

### 3b. Duas chamadas, não uma

`commands.rs` ganha **duas** portas, não uma:

- `enumerate_prefix(host, db, prefixes) -> Enumeration` — a varredura do
  passo [1]. Devolve a contagem, a quebra por tipo, o aviso de cortado e um
  identificador do conjunto enumerado.
- `delete_prefix(host, db, enumeration_id, backup, confirmed) -> WriteOutcome`
  — recebe o que foi enumerado, não o prefixo, e obedece à trava de produção
  pelo mesmo `needs_confirmation` de `write_value`.

Separar é o que torna possível mostrar a contagem conferida antes de
perguntar. O conjunto enumerado vive na memória do `AppState` até ser usado
ou descartado, como a trie já vive.

### 4. A entrada de histórico

`WriteOp` ganha uma variante:

```rust
DeletePrefix {
    prefixes: Vec<String>,
    /// Quantas chaves saíram DE VERDADE (cancelar faz isto ser
    /// menor que o enumerado).
    deleted: u64,
    /// Quantas a varredura tinha achado quando a pessoa confirmou.
    enumerated: u64,
    backup: BackupKind,
}
```

`BackupKind` é `Inline`, `File { path: String }` ou `None`. Acrescentar
variante é aditivo: as linhas de histórico já gravadas continuam
desserializando.

Na linha do histórico: `key` guarda o prefixo (ou `"wallet: e mais 2"`),
`summary` traz a frase pronta, `before_json` traz o **vetor** de retratos
(ou `None` quando foi sem cópia). A tela lista com "sem cópia" em destaque e
**não oferece** voltar atrás nessas — o botão não fica cinza, ele não existe,
e no lugar vai a frase dizendo por quê.

**Teto.** Dois, e o que vier primeiro manda: `100_000` chaves ou `64 MiB` de
retratos somados. Estourou, o aplicativo **para antes de apagar** e oferece
gravar a cópia em arquivo, em `<dados do app>/backups/`, com nome
`AAAA-MM-DD-hhmmss-<host>-db<N>-<prefixo>.json`. Recusou o arquivo, a saída
é cancelar ou seguir sem cópia — nunca apagar com uma cópia que não coube.

**Volta atrás.** Desfazer é escrita: em host `prod` passa pela mesma
confirmação, e o ambiente consultado é o da **entrada**, não o do host aberto
na tela — a regra que `rollback` já aplica hoje. Restaura chave a chave a partir dos retratos, na ordem
gravada, e é **melhor esforço com relatório**: devolve quantas voltaram e a
lista das que falharam. A entrada só é marcada `rolled_back` se **todas**
voltaram; parcial continua aberta, com o relatório à vista. Um "desfeito"
que desfez 900 de 1.395 é mentira.

### 5. Idioma

Cerca de 25 chaves novas nos três catálogos (`pt-BR.ts` é a base). Entram
aqui o nome dos itens do menu, as frases do diálogo (inclusive a de
irreversível), o progresso, e o texto do histórico. Número e contagem saem
por `Intl`, no idioma em uso, como o resto.

As mensagens de erro novas do Rust ficam em português, como as outras 37, e
pelo mesmo motivo — `ErroTecnico.svelte` põe acima delas, no idioma de quem
lê, a frase que diz QUE operação falhou.

---

## Os testes

TDD tarefa a tarefa: teste primeiro, falha observada, implementação depois,
commit por tarefa.

**Bancos:** 13 e 14. Estão livres (0–2, 1, 4, 5, 6–9, 10, 11, 12 e 15 já têm
dono) e precisam entrar na lista de `CONTEXTO.md` junto com o código.

### Rust

| O que prova | Teste |
|:-|:-|
| Enumera certo | `scan_prefix_keys` devolve exatamente as chaves do prefixo, nenhuma de fora |
| Curinga não vaza | prefixo contendo `*` não traz chave de outro galho |
| Prefixo vazio | enumera o banco inteiro (o caminho do "todos") |
| Apaga tudo | depois de `delete_prefix`, `DBSIZE` cai exatamente pela contagem; o que estava fora do prefixo continua lá |
| Vários prefixos | uma varredura, as chaves dos três somem, as demais ficam |
| Cópia restaura | entrada tem N retratos; volta atrás recria as N chaves com valor **e TTL** |
| Sem cópia | `before_json` é `None`, a entrada diz sem cópia, e voltar atrás é recusado com erro claro |
| Produção trava | host `prod` sem confirmar não apaga nada e devolve `NeedsConfirmation` |
| Teto estoura | acima do teto nada é apagado antes de a cópia ter destino |
| Cópia em arquivo | o arquivo gravado restaura o mesmo que a cópia embutida restauraria |
| Cancelar é honesto | cancelado no meio, o histórico registra o apagado de verdade, menor que o enumerado |
| Tipo sem volta é dito antes | a enumeração marca o `stream`, e o diálogo o anuncia com a cópia marcada |
| Teto de coleta | prefixo acima de 100.000 para de juntar, marca cortado, e não carrega tudo na memória |
| Criada no meio sobrevive | chave criada entre enumerar e apagar continua lá depois |
| Caches esquecem | trie e índice FTS5 não conhecem mais as chaves apagadas |
| Volta atrás parcial | uma chave que falha deixa a entrada **não** marcada como desfeita, e o relatório a nomeia |

### Interface (travados em `pt-BR`, como os demais)

| O que prova | Teste |
|:-|:-|
| Menu abre | `contextmenu` na linha abre o menu e chama `preventDefault` |
| Teclado | `Shift+F10` abre; `Escape` fecha e devolve o foco à linha |
| Clique fora | fecha sem agir |
| Seleção | `⌘+clique` junta; o item do menu passa a dizer "3 prefixos" |
| Clique fora da seleção | limpa a seleção e age só na linha clicada |
| Contagem conferida | o diálogo mostra o número da varredura, não o da árvore |
| Padrão é com cópia | a opção vem marcada |
| Sem cópia avisa | desmarcar troca o texto do botão e acende o irreversível |
| Produção datilografa | botão travado até o nome do host bater exatamente |
| Progresso e cancelar | a barra anda e o cancelar some com o diálogo |
| Histórico sem cópia | a linha não oferece voltar atrás, e diz por quê |

---

## O que fica a cargo de quem constrói

- Os números do teto (`100_000` chaves, `64 MiB`) são palpite informado.
  Ajuste contra medição real se a medição discordar.
- O tamanho do lote do `UNLINK` — comece em 500 e meça.
- O visual do menu sai dos tokens que já existem (`--panel-dark`,
  `--rule-strong`, `--text-faint`). Nada de paleta nova.
- Se `⌘+A` deve ter teto de seleção num banco com milhares de prefixos de
  primeiro nível.
