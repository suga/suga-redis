<div align="center">

<img src="static/suga-redis-logo.png" alt="Suga-Redis" width="420" />

**Conecte · Gerencie · Acelere**

Um cliente de mesa para Redis feito para quem opera banco de produção —
e precisa saber *onde está pisando* antes de apertar qualquer coisa.

</div>

---

## O problema

Você abre uma ferramenta de Redis com cinco abas iguais. Uma é produção. Qual?

Você precisa achar o cliente com o e-mail `ana@empresa.com`, mas ele está
dentro de um JSON, dentro de uma chave, entre cinquenta mil.

Você salva. Era a chave errada. E agora?

O Suga-Redis responde essas três perguntas por construção.

## O que ele faz

### 🚦 O ambiente grita antes de você errar

A janela inteira muda de cor conforme o host aberto, com a palavra carimbada
no topo: `PRODUÇÃO`, `HOMOLOGAÇÃO`, `DESENVOLVIMENTO`.

Caixa alta tem **um significado só** no app inteiro — o ambiente. Nenhum
outro rótulo grita junto, então o carimbo nunca se dilui.

Na coluna de hosts, cada linha carrega a marca colorida do seu ambiente: dá
para ver quantos hosts de produção existem sem ler um nome sequer.

### 🔎 Busca que entra dentro do valor

Procure `arco@emp` e ele acha `marco@empresa.com` — dentro de campo de hash,
item de lista, membro de sorted set ou JSON guardado como string.

Substring de verdade, em qualquer posição, não só palavra inteira.

**Medido: 3 ms com 5.000 chaves indexadas.**

O nome das chaves é lido do servidor na hora, então a busca por nome funciona
mesmo num banco recém-aberto, sem varredura nenhuma.

### 🌳 Árvore que aguenta um banco de verdade

As chaves vêm agrupadas por prefixo (`wallet:`, `cache:`, `session:`), com a
contagem de cada galho. A lista desenha só o que cabe na tela — dezenas de
milhares de chaves não travam a janela.

E a leitura é enxuta de propósito: **onze idas e voltas** para ler 50 mil
chaves, não trezentas e cinquenta. Num Redis gerenciado em outra região, isso
é a diferença entre 1,5 segundo e 45.

### ✏️ Editor por tipo, com JSON colorido e validado

Cada tipo tem sua tela: `string` com área de texto, `hash` com tabela
campo/valor, `list` com índice, `zset` com membro e pontuação.

Valor em JSON abre **colorido e indentado**, pronto para conferir. Ao editar,
um validador aponta **linha e coluna** do erro e segura o Salvar enquanto o
JSON estiver quebrado.

Valor binário recusa edição por texto: o que voltasse do campo já não seriam
os mesmos bytes.

### ⏱️ Prazo de expiração na mão

Veja quanto falta em contagem regressiva, defina um prazo novo em segundos,
minutos, horas ou dias, ou tire o prazo de vez.

### ↩️ Volta atrás de verdade

Toda escrita guarda o valor anterior **antes** de acontecer. O histórico
mostra o que mudou, quem mudou e quando, com antes e depois lado a lado — e
um botão para desfazer.

### 🔒 Produção pede confirmação

Salvar ou apagar em host de produção abre um diálogo com o resumo exato da
mudança. Um clique. Nunca digitar frase.

A trava mora no backend, não na tela: um botão pode esquecer de perguntar,
essa função não pode ser contornada.

### 🤖 O Claude lê seus bancos — e pede licença para escrever

O mesmo binário é um servidor MCP. Ligado ao Claude Code, ele responde coisas
como *"quantas chaves de sessão existem e quando expiram?"* direto do seu
Redis.

Ler funciona sem o app aberto. **Escrever, não.** Quando o Claude pede uma
escrita, a janela vem para a frente e você libera digitando o nome do host.
Sem app aberto, não existe socket, e a escrita não acontece.

Vale em **qualquer ambiente**, inclusive desenvolvimento: a confirmação de
produção pergunta *onde* a mudança cai; esta trava pergunta *quem* pediu.

---

## Instalar

### macOS

Gere os pacotes com `npm run dist` (veja [`docs/build.md`](docs/build.md)) e
abra o `.dmg` que aparece em `dist/`, arrastando o app para `Aplicativos`.

Na primeira vez o macOS vai reclamar que o app não é assinado. Clique nele
com o botão direito → **Abrir** → **Abrir**.

### Linux

Pegue o `.AppImage` gerado em `dist/`:

```bash
chmod +x Suga-Redis_*.AppImage
./Suga-Redis_*.AppImage
```

Ou instale o `.deb`:

```bash
sudo dpkg -i suga-redis_*.deb
```

### Compilar você mesmo

Veja [`docs/build.md`](docs/build.md).

### Mexer no código

Comece por [`docs/CONTEXTO.md`](docs/CONTEXTO.md): estado atual, arquitetura,
invariantes e as armadilhas que já quebraram este projeto.

---

## Começar a usar

1. Abra o app e clique em **Novo host**.
2. Preencha nome, endereço e porta. **Escolha o ambiente com atenção** — é
   ele que decide a cor da janela e se a escrita vai pedir confirmação.
3. Clique em **Conectar**.

A senha vai para o chaveiro do sistema, nunca para um arquivo.

Para procurar dentro dos valores, clique em **Varrer de novo** uma vez: é o
que monta o índice de busca local. Procurar por nome de chave já funciona
sem isso.

### Ligar ao Claude Code

```bash
claude mcp add suga-redis -- /Applications/Suga-Redis.app/Contents/MacOS/suga-redis --mcp
```

Detalhes e solução de problemas em [`docs/mcp-setup.md`](docs/mcp-setup.md).

---

## Onde ficam os dados

| O quê | Onde |
| :- | :- |
| Cadastro de hosts | `<dados>/hosts.json` |
| Histórico e índice de busca | `<dados>/suga-redis.sqlite` |
| Socket de aprovação | `<dados>/suga-redis.sock` (permissão 0600) |
| **Senhas** | **Chaveiro do sistema, nunca em arquivo** |

`<dados>` é `~/Library/Application Support/com.sugamele.sugaredis` no mac e
`~/.local/share/com.sugamele.sugaredis` no linux. A variável
`SUGA_REDIS_DATA_DIR` reposiciona tudo, se quiser uma segunda instalação lado
a lado.

O índice de busca guarda os valores **em claro**, sem cifrar: é uma cópia
local do que já está no Redis, e cifrá-la só mudaria o problema de lugar.
Quem tem acesso à sua conta de usuário tem acesso a ele.

---

## O que ele não faz, de propósito

- **Nunca roda `KEYS`, `FLUSHALL` ou `FLUSHDB`.** Varredura só com `SCAN` e
  família, que não bloqueia o servidor.
- **Não edita `set`, `stream` nem RedisJSON.** Abrem em modo leitura:
  editá-los exigiria operações que este produto não sabe fazer de forma
  atômica.
- **Não reordena itens de lista.** Não existe troca atômica de posições, e
  fazer em duas escritas deixaria a lista errada se a segunda falhasse.
  Ausente é melhor que silenciosamente corrompido.
- **Não mascara valor nenhum.** Nem na tela, nem para o Claude. Decisão
  registrada do dono.

---

## Linha de comando

```
suga-redis              abre a janela
suga-redis --mcp        roda como servidor MCP em stdio
suga-redis --version    mostra a versão
suga-redis --help       mostra a ajuda
```

## Licença

MIT.
