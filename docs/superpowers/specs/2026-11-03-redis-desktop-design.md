# Cliente de mesa para Redis — design

Data: 2026-11-03
Autor: Marco Sugamele (intenção) + Claude (redação)

## Intenção e o porquê

Hoje, mexer no Redis é trabalho de linha de comando e de memória. Três coisas doem,
nas palavras do dono do produto:

1. **Adicionar, editar e remover chaves.** Operação banal que exige lembrar a sintaxe
   certa para cada tipo e torcer para não errar o host.
2. **Descobrir se alguma coisa está no Redis** — não só pelo nome da chave, mas pelo
   conteúdo dentro dela. Hoje isso não tem resposta rápida.
3. **Fazer isso em produção sem medo.** Os mesmos comandos servem prod, homologação e
   dev, e nada na tela avisa em qual caixa você está.

O app existe para resolver esses três. Tudo o mais é secundário.

Um quarto motivo veio junto: a LLM que roda na máquina (Claude) deve enxergar os mesmos
hosts e responder perguntas sobre os dados — "essa chave está com o valor certo?",
"quantos registros tem?", "qual o TTL?" — sem o dono ter que exportar nada na mão.

## Público

Um engenheiro (o dono) e, depois, o time dele. Gente que entende de Redis, roda VPN,
e trabalha em prod. Não é ferramenta para leigo: é ferramenta para quem sabe o que
está fazendo e quer parar de digitar.

## Metas

- Buscar um texto e achar onde ele está — no nome da chave **e** dentro do valor —
  em milhares de chaves, com resposta que pareça instantânea.
- Criar, editar e apagar chave pela tela, em string, hash, lista e sorted set.
- Cadastrar vários hosts, cada um marcado como produção, homologação ou dev, e a
  marcação mudar a cara da janela inteira.
- Toda edição e exclusão guarda o valor anterior em disco, com histórico consultável
  e volta atrás.
- O Claude local lê os mesmos hosts e responde sobre chave, valor, contagem e TTL.
- Roda em mac e linux a partir do mesmo código. Mac primeiro.

## Não-metas

- Não é ferramenta de monitoramento. Sem gráfico de latência, sem alerta, sem painel
  de saúde do cluster.
- Não administra o servidor: nada de CONFIG SET, failover, replicação, gerência de
  usuário.
- Não edita set, stream nem RedisJSON na versão um — mostra o conteúdo, só não deixa
  alterar. *(decisão do Claude, confirmada em conversa)*
- Não tem conta, nuvem, nem sincronização entre máquinas. Tudo mora no computador do
  dono.
- Não importa nem exporta dump do Redis.

## Anti-metas — o que faz isso fracassar mesmo funcionando

Ditas pelo dono, e valem como critério de aceite:

1. **Busca lenta.** Se procurar um valor demora a ponto de ele voltar ao terminal, o
   app falhou. O alvo é resposta em menos de um segundo para milhares de chaves.
2. **Busca que não acha.** Se o texto está dentro de um campo de hash, de um item de
   lista ou de um JSON guardado como string e a busca não encontra, o app falhou.
3. **Não conseguir editar, adicionar ou remover.** Se qualquer uma dessas três travar
   — por tipo não suportado, por erro de conexão mal tratado, por trava de segurança
   exagerada — o app falhou.

Uma quarta, implícita no motivo do produto: **apagar coisa em produção sem querer.**
A proteção não pode ser tão chata que vire a anti-meta 3.

## Restrições

- Mac primeiro, linux logo depois, do mesmo código.
- Os Redis de produção ficam atrás de VPN. A máquina já está na VPN quando o app roda;
  o app só conecta normalmente. Não precisa abrir túnel SSH sozinho na versão um, mas
  precisa aceitar senha e TLS.
- Escala real: milhares de chaves por host. Não é milhões. Isso permite varrer tudo e
  guardar índice local — e é a razão de a busca por valor ser viável.
- Dado de produção trafega e repousa em claro: no índice em disco e no que vai para a
  LLM. Foi decidido assim de propósito. Quem construir não deve adicionar cifragem ou
  mascaramento por conta própria — mas também não deve espalhar o valor para mais
  lugares do que os dois já listados.
- O dono não vai escrever o código, mas vai abrir o fonte depois para analisar e
  sugerir. O código precisa ser legível e comentado onde a intenção não é óbvia.

## O que já está decidido

### Como é feito

**Tauri** — núcleo em Rust, interface em HTML/CSS/TypeScript sobre o webview do
sistema. *(escolha do Claude, aceita pelo dono)* Motivo: app de ~15 MB contra ~150 MB
do Electron e ~200 MB de um JavaFX empacotado; ~100 MB de RAM; mesmo código nos dois
sistemas; e liberdade total de CSS, que o visual escolhido exige. Java com JavaFX foi
considerado a pedido do dono e descartado pelo peso e pela limitação do CSS do JavaFX.

O servidor MCP sai do mesmo binário, não é processo separado a instalar.

### A tela principal

Árvore clássica, três colunas:

- **Esquerda — hosts.** Lista dos hosts cadastrados, cada um com a marca do ambiente.
  Abaixo do host ativo, os bancos (db0, db1…) com a contagem de chaves.
- **Meio — chaves.** Agrupadas por prefixo, usando `:` como separador, em árvore que
  abre e fecha: `session:` → `user:` → `4471`. Cada galho mostra quantas chaves tem
  dentro. Campo de filtro rápido no topo da coluna.
- **Direita — o valor.** Chave, tipo, tamanho, TTL e o conteúdo completo, editável. Os
  botões de salvar, mudar expiração e excluir ficam embaixo.

### O visual

Direção escolhida: **sala de operação**. Cabine industrial, fundo ardósia escuro
(#22262B), âmbar como cor de ação (#F2A33C), tipografia Barlow Semi Condensed para a
interface e Azeret Mono para os dados — chave e valor são texto literal e merecem
monoespaçada; rótulo de interface não.

O ambiente do host não é um selinho: ele pinta a janela. Em produção, a barra do topo
é vermelha (#8E2B24) com `PRODUÇÃO` carimbado grande, e a borda superior da janela
acompanha. Homologação e dev têm sua própria cor, mais calma. A regra é: olhar de
relance e saber em que caixa você está.

O mockup aprovado está em `.superpowers/brainstorm/20393-*/content/visual-style.html`,
bloco número 3.

### A busca

É o coração. Duas camadas:

- **Filtro de nome**, no topo da coluna de chaves, instantâneo, só sobre o nome.
- **Busca global**, sobre nome e valor ao mesmo tempo. Acha texto dentro de campo de
  hash, item de lista, membro de sorted set e dentro de JSON guardado como string.
  Mostra o trecho com o pedaço encontrado destacado, com o tipo ao lado.
  **Busca um host por vez**, o que estiver aberto. Não procura nos três ambientes ao
  mesmo tempo.

Para isso o app mantém um **índice local** do conteúdo de cada host, alimentado por
varredura incremental com `SCAN`. *(decisão do Claude)* **O índice guarda o valor em
claro, sem cifrar** — decisão do dono, tomada depois de avisado de que isso deixa dado
de produção parado em disco na máquina dele. O critério foi simplicidade e velocidade.
O índice é o que torna a busca instantânea; a varredura é o que o mantém honesto. A tela precisa dizer quando foi a
última varredura, e dar um jeito de varrer de novo na hora. Resultado vindo do índice
é confirmado contra o servidor antes de ser mostrado como achado, para nunca mostrar
chave que já morreu.

`KEYS` e qualquer comando que trave o servidor estão proibidos. *(decisão do Claude)*

### Escrever, e voltar atrás

- Criar, editar e apagar em string, hash, lista e sorted set, com editor próprio para
  cada tipo.
- **Em produção, toda escrita pede um confirmar** — um clique, mostrando chave, host e
  o que vai mudar. Nada de digitar frase aqui: o dono foi explícito, na tela é só
  confirmar.
- **Antes de toda edição e exclusão, o valor anterior é copiado para o disco da
  máquina.**
- Existe uma aba de **histórico**: o que mudou, em qual host, quando, valor antes e
  depois, e um botão de voltar atrás que regrava o valor antigo. Voltar atrás em
  produção passa pelo mesmo confirmar.

### A LLM

O app sobe um servidor MCP que o Claude da máquina consome. Ele usa os hosts já
cadastrados no app — o dono não configura senha duas vezes.

**O que a LLM pode fazer sozinha:** listar hosts e bancos, buscar chave por nome e por
valor, ler valor completo, contar registros, ver e agregar TTL, resumir por prefixo.

Valor de produção vai para a LLM **em claro, sem máscara** — decisão do dono, tomada
depois de avisado de que isso manda dado de cliente para fora da máquina. Não há
mascaramento de campo nem restrição de ambiente na leitura.

**O que ela não pode fazer sozinha:** escrever, apagar, mudar TTL. Quando ela pede,
cai um pedido na tela do app mostrando o host, a chave e o valor novo; **o dono digita
uma frase para destravar.** A frase é o nome do host, para que o ato de digitar seja o
ato de conferir em qual caixa você está. *(a frase ser o nome do host é decisão do
Claude; a exigência de digitar uma frase é do dono)*

Essa regra vale igual em prod, homologação e dev. *(decisão do Claude — o dono não
pediu exceção em dev; se quiser afrouxar depois, é um botão de configuração)*

Pedido da LLM que fica sem resposta por um tempo expira sozinho em vez de ficar
pendurado.

### Onde as coisas moram

- Senha e credencial de host: no chaveiro do sistema operacional, nunca em arquivo de
  texto. *(decisão do Claude)*
- Índice de busca, histórico e cópias de segurança: banco local do app, em SQLite.
  *(decisão do Claude)*
- Histórico guarda 30 dias ou 500 entradas por host, o que vier primeiro, e isso é
  configurável. *(decisão do Claude — número escolhido para caber no disco sem o dono
  precisar pensar nisso)*

## O que fica a cargo de quem constrói

- Como a varredura incremental decide quando revarrer, e como ela se comporta com o
  app aberto o dia inteiro.
- Forma do índice e da busca por trecho dentro do valor (FTS do SQLite ou busca
  própria) — o critério é o número da anti-meta 1: menos de um segundo.
- Se a varredura cobre só o host aberto ou também os outros em segundo plano.
- Desenho do editor de cada tipo: hash, lista e sorted set pedem telas diferentes.
- Quanto de valor muito grande mostrar antes de cortar, e como oferecer o resto.
- Se os bancos (db0…db15) aparecem como nível da árvore ou como seletor no host.
- Nome dos campos e das ferramentas do MCP.
- Estratégia de teste. O mínimo: busca por valor nos quatro tipos, cópia de segurança
  e volta atrás, e a trava da LLM.
- Empacotamento e assinatura para mac e linux.
