# redis-desk

Cliente de mesa para Redis, feito para quem mexe em banco de produção e não
quer descobrir o ambiente errado tarde demais.

Um binário só, dois modos: sem argumento sobe a janela; com `--mcp` roda como
servidor MCP para o Claude local.

## O que ele faz

- **Cadastra hosts por ambiente.** Cada host é marcado como produção,
  homologação ou desenvolvimento. A barra do topo pinta a janela inteira com
  a cor do ambiente e carimba a palavra. É o único lugar do app com letras
  maiúsculas, para o carimbo `PRODUÇÃO` nunca se diluir no meio de outros
  rótulos gritando junto.
- **Mostra as chaves em árvore**, agrupadas por prefixo, desenhando só as
  linhas visíveis — um banco com dezenas de milhares de chaves não trava a
  janela.
- **Busca texto dentro dos valores**, não só no nome da chave: dentro de
  campo de hash, item de lista, membro de sorted set e JSON guardado como
  string. Procurar `arco@emp` acha `marco@empresa.com`. Medido em 3 ms com
  5.000 chaves indexadas.
- **Edita com volta atrás.** Toda escrita guarda o valor anterior antes de
  acontecer. O histórico lista o que mudou, quem mudou e quando, com antes e
  depois lado a lado.
- **Protege produção.** Salvar ou apagar em host de produção abre um diálogo
  de confirmação com o resumo exato da mudança. Um clique — nunca digitar
  frase.
- **Expõe tudo ao Claude local** por MCP, em modo leitura. Escrever exige
  liberação na janela. Ver [docs/mcp-setup.md](docs/mcp-setup.md).

## Instalar

Baixe o `.dmg` (mac) ou o `.AppImage` (linux) da última versão, ou compile:

```bash
npm install
npm run tauri build
```

Os artefatos saem em `src-tauri/target/release/bundle/`.

## Onde ficam os dados

| O quê | Onde |
| :- | :- |
| Cadastro de hosts | `<dados>/hosts.json` |
| Histórico e índice de busca | `<dados>/redis-desk.sqlite` |
| Socket de aprovação | `<dados>/redis-desk.sock` (permissão 0600) |
| **Senhas** | **Chaveiro do sistema, nunca em arquivo** |

`<dados>` é `~/Library/Application Support/com.sugamele.redisdesk` no mac e
`~/.local/share/com.sugamele.redisdesk` no linux. A variável de ambiente
`REDIS_DESK_DATA_DIR` reposiciona tudo isso, se você quiser uma segunda
instalação lado a lado.

O índice de busca guarda os valores **em claro**, sem cifrar — é uma cópia
local do que já está no Redis, e cifrá-la só moveria o problema de lugar.
Quem tem acesso à sua conta de usuário tem acesso a ele.

## O que ele não faz, de propósito

- **Nunca roda `KEYS`, `FLUSHALL` ou `FLUSHDB`.** Varredura só com `SCAN` e
  família, que não bloqueia o servidor.
- **Não edita `set`, `stream` nem RedisJSON.** Esses tipos abrem em modo
  leitura. Editá-los pediria operações que este produto não sabe fazer de
  forma atômica.
- **Não reordena itens de lista.** Não existe troca atômica de posições no
  conjunto de operações suportado, e fazer isso em duas escritas deixaria a
  lista errada se a segunda falhasse.
- **Não mascara valor nenhum.** Nem na tela, nem para o Claude.

## Desenvolver

```bash
npm install
./scripts/test-redis.sh up     # sobe um Redis de teste em 127.0.0.1:6399
npm run tauri dev
```

Testes:

```bash
npm test                       # interface (Vitest)
cd src-tauri && cargo test     # núcleo e comandos (precisa do Redis de teste)
```

O Redis de teste roda em docker na porta 6399 e é o **único** lugar do
repositório onde `FLUSHALL` pode aparecer. `./scripts/test-redis.sh reset`
limpa, `down` remove.

## Licença

MIT.
