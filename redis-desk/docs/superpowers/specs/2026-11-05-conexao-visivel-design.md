# Tornar a conexão visível

Desenho de 2026-11-05. Três mudanças que resolvem o mesmo problema: hoje o
aplicativo não diz se está conectado, e por isso uma falha e um banco vazio
são o mesmo pixel na tela.

---

## O que aconteceu

O host **host-serverless** (Valkey serverless, produção, TLS) foi aberto na
janela. A árvore mostrou *"Nenhuma chave neste banco"*. O cache **tem dado em
produção**, confirmado pelo dono.

O que a investigação achou, e o que ela não conseguiu achar:

- `PING` respondeu em 5679 ms — conexão, TLS e DNS de pé.
- `SCAN` volta cursor `0` com lote vazio na primeira ida, **sem erro**.
- O seletor "Banco" aparece pelado (`0`, sem contagem). Em
  `Workspace.svelte:206` isso só acontece quando a lista de bancos voltou
  vazia — e `Workspace.svelte:130` tem `.catch(() => (bancos = []))`, que
  transforma **erro** em **lista vazia**, calado.

Duas hipóteses sobrevivem e **nenhuma pôde ser descartada**: ou `db_sizes`
estourou e o `catch` engoliu, ou servidor respondeu vazio de verdade a
`DBSIZE`/`SCAN` (proxy do serverless, ACL, shard). Decidir entre elas exige
ver a resposta crua do servidor, e não existe lugar nenhum onde ela apareça:
o projeto inteiro tem **dois `eprintln!`** e nenhum registro.

**A causa raiz do Hub segue aberta.** Este desenho não a conserta — constrói
o instrumento que permite achá-la, e fecha o buraco que a escondeu.

Durante a investigação foi encontrado e **já corrigido** (commit `c0ee59f`)
um defeito separado: `list_hosts`, `list_databases` e `summarize_prefixes`
do MCP respondiam com array na raiz, que o cliente recusa na validação de
esquema. As três estavam inutilizáveis — justamente as que serviriam para
diagnosticar isto de fora.

## Intenção

Que a pessoa olhe para a janela e saiba, sem adivinhar, **se está conectada,
desde quando, e por que não está**. E que, quando algo falhar, exista um
lugar onde ela possa ler o motivo depois.

## Objetivos

1. **Estado da conexão à vista**, por host, com quatro estados distinguíveis.
2. **Conectar e desconectar de verdade**, com a árvore refletindo isso.
3. **Aba de Logs**: conexões, falhas e lentidão, legíveis depois do susto.
4. **Colunas redimensionáveis** com o mouse, com a largura guardada.
5. **Nenhum erro engolido** no caminho entre o Redis e a tela.

## Não-objetivos

- Não resolve o host-serverless. Dá a ferramenta para resolver.
- Não registra todo comando que sai — decisão do dono, ver Restrições.
- Não guarda log em disco. Fechou o aplicativo, perdeu o log.
- Não mexe na trie, na varredura nem na busca.
- Não é tela de métrica nem gráfico de latência.

## Anti-objetivos

Coisas que, se acontecerem, tornam esta entrega um fracasso mesmo que tudo
funcione:

- **Vermelho ou caixa alta significando outra coisa que não produção.** Hoje
  os dois têm UM significado cada. Estado de conexão que use vermelho destrói
  o dispositivo de segurança central do produto.
- **Valor de chave ou senha no log.** O log vai ser lido em tela
  compartilhada e colado em conversa. Dado de pessoa não pode estar nele.
- **Mais um lugar que engole erro.** Se o log nascer com um `catch` mudo
  dentro, ele reproduz o defeito que veio corrigir.
- **Log que vira enxurrada.** Num banco de 3,7 milhões de chaves, registro
  por comando sepulta a linha que importa.

## Restrições

- **Pilha existente**: Tauri 2, Rust, Svelte 5. Nada novo entra sem
  necessidade.
- **Identificadores em inglês; comentário e texto de tela em português.**
- **Nada fala com o Redis fora de `core/`**; a tela só chega ao `core` por
  `commands.rs`, e só chega a `commands.rs` por `src/lib/api.ts`.
- **TDD**: teste escrito primeiro, falha observada, implementação depois,
  um commit por tarefa.
- O contêiner `suga-redis-test` (127.0.0.1:6399) precisa estar de pé para os
  testes Rust. Bancos 0–12 e 15 já estão tomados por testes existentes.
- Svelte 5, armadilhas já pagas caro: `void x` não registra dependência;
  efeito pode re-disparar, então precisa ser idempotente; `$state` vindo de
  prop captura só o valor inicial.

---

## O que foi decidido, como foi decidido

### 1. Estado da conexão

Uma **linha de texto abaixo dos botões** do host — no lugar onde hoje
aparece *"Respondeu em 5679 ms"*. Escolhida contra duas alternativas (ponto
antes do nome; pastilha ao lado do selo de ambiente) porque é a única que
tem espaço para dizer **por que** falhou, não só que falhou.

**Atenção, porque o nome engana.** Aquela linha de hoje é resultado do botão
**Testar** (`HostList.svelte:28`, guardado em `testes[host.id]`), não de
conexão nenhuma. E **"Conectar" hoje não conecta**: só faz
`onSelect(host)` (`+page.svelte:86`), que monta o `Workspace`. A conexão
nasce depois, preguiçosa, dentro da varredura. Quem constrói herda esse
lugar na tela, não essa semântica.

Como o pool é preguiçoso e multiplexado por `(host, banco)`, **não existe
"sessão aberta" para consultar**. O estado é derivado do que as operações
dizem: em voo → `conectando`; respondeu → `conectado · <ms>`; estourou →
`falhou · <motivo>`; sem host aberto ou depois de desconectar → `desligado`.
Dizer "conectado" sem ter perguntado nada ao servidor é mentira na tela.

Quatro estados, não dois:

| Estado | Linha | Cor do ponto |
|:-|:-|:-|
| desligado | `nunca conectado` / `desconectado` | cinza |
| conectando | `conectando…` | âmbar |
| conectado | `conectado · 5679 ms · banco 0` | verde |
| falhou | `falhou · TLS: certificado não confere` | âmbar escuro |

O quarto estado é a razão de existir desta tela. O Hub **conectou**; um
indicador de dois estados teria dito "conectado" e deixado a dúvida de pé.

O motivo da falha reusa a classificação que `ConnError` já faz (TLS, DNS,
recusado, cluster com banco único). Sem vocabulário novo.

**Cor**: verde, âmbar e cinza. **Nunca vermelho, nunca caixa alta** — ver
Anti-objetivos.

### 2. Conectar ↔ Desconectar

O botão **Conectar** vira **Desconectar** quando há conexão. Desconectar não
é rótulo: derruba a conexão no pool, zera a árvore e volta o estado para
desligado, para a tela não mostrar dado de um host do qual já se saiu.

`Pool::drop_host` (`core/conn.rs:153`) já faz a parte do pool — derruba as
entradas de todos os bancos daquele host. Falta a trilha até a tela e o
descarte da trie.

### 3. Aba de Logs

Aba nova ao lado de Valor / Busca / Histórico. Registra **três coisas**, por
decisão do dono — comando a comando seria enxurrada:

- **conexão**: abriu, fechou, reconectou, falhou (com o motivo)
- **falha**: qualquer erro vindo do `core`, inclusive os que a tela trata
- **lentidão**: qualquer operação acima de **1 s**

Cada linha tem hora, host, banco, o que foi feito, duração, e o erro quando
houver. **Nunca** valor de chave, nunca senha.

**Anel em memória de 1000 linhas.** Fechou o aplicativo, perdeu o log —
decisão do dono. Mais recente em cima.

Mostra **todos os hosts**, não só o aberto: a falha que interessa costuma ser
a de um host que não abriu. *(Decisão do autor deste desenho, não do dono.)*

**Limite conhecido, e aceito:** o `Workspace` só existe com um host
selecionado, então sem nenhum host aberto não há aba de Logs. Na prática
"Conectar" monta o `Workspace` mesmo quando a conexão falha, então o caminho
que interessa fica coberto. Tirar o log para fora da aba resolveria o resto —
e é mais tela do que este problema pede agora. *(Decisão do autor.)*

### 4. Larguras arrastáveis

Os **dois** divisores viram alça de arrastar:

- Hosts | trabalho (`src/routes/+page.svelte:140`, hoje `minmax(14rem, 20rem)`)
- Chaves | valor (`src/lib/Workspace.svelte:344`, hoje `minmax(16rem, 24rem)`)

O teto fixo de hoje é o que aperta: nome de chave longo e endereço de host
serverless vivem cortados.

- **Piso** de ~12rem por coluna e **teto** de metade da janela. Sem piso, dá
  para arrastar até sumir e não ter como trazer de volta.
- **A largura fica guardada** entre sessões. Sem isso, arrasta-se de novo a
  cada abertura.
- A largura é **do aplicativo, não por host**: arrasta uma vez, vale para
  todos.
- O empilhamento em janela estreita (`@media (max-width: 52rem)`) continua
  valendo e ignora a largura guardada.

### 5. Fechar os erros engolidos

`Workspace.svelte:130` (`listDbs`) e `Workspace.svelte:111` (`scanStatus`)
transformam erro em estado vazio, sem dizer nada. Os dois passam a registrar
no log e a mostrar o estado de falha. Foi esse silêncio que escondeu o Hub.

---

## O que fica a cargo de quem constrói

- Onde mora o estado da conexão (`AppState`, store do Svelte, ou os dois) e
  como ele chega à tela sem violar a regra de `api.ts` como única porta.
- Se o anel de log vive no Rust (e a tela lê por comando) ou no TypeScript
  (e o Rust emite evento). O log precisa pegar falha do `core`, o que puxa
  para o Rust.
- Como a largura é persistida — o aplicativo já tem `hosts.json` e SQLite;
  escolher um e dizer por quê.
- O formato exato de cada linha de log.
- Se "conectado" é verificado por `PING` periódico ou só pelo que o pool
  sabe. Pesar custo contra mentira na tela.
- A ordem das tarefas e o recorte dos commits.

## Como se sabe que funcionou

Abrir o host-serverless e, em vez de uma árvore muda, ler na tela se ele
conectou — e no log, o que o servidor respondeu a `DBSIZE` e `SCAN`.

Essa resposta decide entre as duas hipóteses abertas, e aí a causa raiz do
Hub vira um bug comum: com evidência.
