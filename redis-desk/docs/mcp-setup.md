# Ligar o Suga-Redis ao Claude local

O Suga-Redis é também um servidor MCP. Ligado ao Claude Code, ele deixa o
Claude olhar os seus bancos Redis — e, com a sua liberação na hora, mexer
neles.

## A linha pronta

Com o app já instalado no mac:

```bash
claude mcp add suga-redis -- /Applications/suga-redis.app/Contents/MacOS/suga-redis --mcp
```

No linux, aponte para o binário onde ele estiver:

```bash
claude mcp add suga-redis -- /caminho/para/suga-redis --mcp
```

Compilando do código, o binário sai em
`src-tauri/target/release/suga-redis`.

Confira com `claude mcp list`. Devem aparecer 10 ferramentas.

## Como funciona

O servidor MCP é um **processo separado** da janela. Ele abre o mesmo
`hosts.json` e o mesmo banco SQLite que o aplicativo, e faz as próprias
conexões com o Redis. Consequência prática: **ler funciona sem o aplicativo
aberto**.

Cadastre os hosts pela janela, uma vez. O Claude enxerga o mesmo cadastro.

## As sete ferramentas de leitura

| Ferramenta | Para quê |
| :- | :- |
| `list_hosts` | Os hosts cadastrados, com o ambiente de cada um. Sempre o primeiro passo. |
| `list_databases` | Os bancos de um host que têm alguma chave. |
| `search_keys` | Acha um pedaço de texto em qualquer lugar: nome da chave, campo de hash, item de lista, membro de sorted set, JSON guardado como string. |
| `read_key` | O conteúdo de uma chave, com tipo, tamanho e prazo. |
| `count_keys` | Quantas chaves e quantos bytes sob um prefixo. |
| `key_ttls` | Menor, maior e mediana dos prazos sob um prefixo, e quantas nunca expiram. |
| `summarize_prefixes` | A forma de um banco desconhecido, agrupada por prefixo. |

A senha de um host **nunca** sai nessas respostas: ela fica no chaveiro do
sistema e não tem representação na saída.

`search_keys` responde a partir de um índice local. Quando esse índice está
velho (mais de cinco minutos) ou não existe, a ferramenta varre o banco antes
de responder e avisa no campo `scanned_first`. O índice é um retrato, e um
modelo não tem como desconfiar de um dado parado — então ele é avisado.

## As três ferramentas de escrita, e a trava

`write_key`, `delete_key` e `set_ttl` **não conseguem agir sozinhas**.

Quando o Claude chama uma delas:

1. O servidor MCP monta um pedido e manda para o aplicativo pelo socket unix.
2. O aplicativo traz a janela para a frente e mostra o pedido: qual host,
   qual banco, qual chave e o que exatamente vai mudar.
3. Você libera **digitando o nome do host**. Exatamente o nome — sem aparar
   espaços, sem ignorar maiúsculas.
4. Só então a escrita acontece.

Enquanto o diálogo está na tela, nada foi escrito: o processo do MCP está
parado esperando.

**Sem aplicativo aberto, não há socket, e a escrita não acontece.** A
ferramenta devolve uma mensagem dizendo para abrir o aplicativo. Essa é a
propriedade central do desenho: ela vale por construção, não por disciplina.

Sem resposta em **120 segundos**, o pedido expira e nada é escrito.

A trava vale em **qualquer ambiente, inclusive desenvolvimento**. A
confirmação de produção e esta trava respondem a perguntas diferentes: aquela
é sobre *onde* a mudança cai, esta é sobre *quem* a pediu.

Toda escrita liberada entra no histórico marcada como feita pelo Claude, com
o valor anterior guardado. Você desfaz pela aba Histórico como qualquer outra
mudança.

## Se alguma coisa não funcionar

| Sintoma | Causa provável |
| :- | :- |
| `claude mcp list` não mostra o servidor | Caminho do binário errado, ou faltou o `--mcp`. |
| `list_hosts` devolve lista vazia | Nenhum host cadastrado ainda, ou o Claude está rodando com outro usuário (o cadastro é por usuário). |
| Escrita recusada com "abra o aplicativo" | A janela do Suga-Redis não está aberta. |
| Erro citando o nome de um host | O Redis não respondeu: VPN caída, host fora do ar ou senha trocada. A mensagem sempre diz qual host. |
