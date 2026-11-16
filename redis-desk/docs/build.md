# Compilar o Suga-Redis

Do código-fonte ao `.dmg`, ao `.AppImage` e ao `.deb`.

## O que precisa estar instalado

| Ferramenta | Versão | Por quê |
| :- | :- | :- |
| [Rust](https://rustup.rs) | estável | o núcleo e a camada Tauri |
| [Node.js](https://nodejs.org) | **20 ou mais** | o Vite 8 usa `node:util/styleText`, que não existe no 18 |
| [Docker](https://docs.docker.com/get-docker/) | qualquer | só para os testes e para o build do linux |

No mac, as ferramentas de linha de comando do Xcode:

```bash
xcode-select --install
```

## Preparar

```bash
git clone <este-repositorio>
cd redis-desk
npm install
```

## Rodar em modo de desenvolvimento

```bash
./scripts/test-redis.sh up    # sobe um Redis de teste em 127.0.0.1:6399
npm run tauri dev
```

A janela abre com recarregamento automático: mexeu em `src/`, a tela
atualiza; mexeu em `src-tauri/`, o Rust recompila.

O container de teste roda na porta 6399 e é o **único** lugar do repositório
onde `FLUSHALL` pode aparecer. `./scripts/test-redis.sh reset` limpa os
dados, `down` remove o container.

## Testes

```bash
npm test                      # interface (Vitest + Testing Library)
cd src-tauri && cargo test    # núcleo, comandos, MCP e a trava de aprovação
```

Os testes de Rust falam com o Redis de teste de verdade — sem ele, a maioria
falha com `Connection refused`. Suba o container antes.

Antes de abrir um PR:

```bash
npm run check                 # svelte-check: 0 erros, 0 avisos
cd src-tauri
cargo fmt --check
cargo clippy --all-targets
```

## Soltar uma versão

O caminho normal. Não compile na sua máquina para distribuir: o GitHub
Actions constrói as duas plataformas a partir da mesma tag.

```bash
npm run versao 0.2.0     # sobe a versão nos 4 lugares, roda os testes, commita, cria a tag
git push && git push origin v0.2.0
```

O `npm run versao` para antes do `push` de propósito: até ali tudo é local
e desfazível (ele mesmo diz como desfazer). Empurrar a tag é o gesto que
publica uma release pública e dispara o aviso de atualização em todo app
instalado.

O que o workflow faz, em `../.github/workflows/release.yml`:

| Máquina | Sai de lá |
|---|---|
| `macos-latest` | `.dmg` universal (Apple Silicon + Intel) |
| `ubuntu-22.04` | `.AppImage` e `.deb`, x86_64 |

Mais o `latest.json` — o manifesto que o app lê para saber que existe versão
nova — e a assinatura de cada pacote de atualização.

O Ubuntu é o 22.04, e não o mais recente, porque o AppImage exige em quem o
roda a mesma glibc contra a qual foi compilado: construir no mais antigo que
ainda tenha `webkit2gtk-4.1` é o que faz o pacote abrir em mais lugares.

### A chave que assina as atualizações

O updater não instala o que não estiver assinado com a chave do projeto. É
isso que permite servir a atualização de uma URL pública sem risco: o
endereço qualquer um alcança, mas só quem tem a chave privada produz algo
que o app aceite.

- **Pública** — está em `src-tauri/tauri.conf.json`, commitada. É o lugar
  dela: serve só para verificar.
- **Privada** — foi gerada em `~/.suga-redis/updater.key` e **não** está no
  repositório. O workflow a lê de dois Secrets do GitHub
  (*Settings → Secrets and variables → Actions*):

| Secret | Valor |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | o conteúdo do arquivo `~/.suga-redis/updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | vazio (a chave foi gerada sem senha) |

> **Guarde a chave privada em outro lugar também.** Perdê-la não quebra
> quem já tem o app, mas nenhuma atualização futura será aceita por quem
> está instalado: a chave pública que eles carregam não vai mais conferir
> com nada. O conserto seria todo mundo reinstalar na mão.

Para gerar outro par (só se a chave atual se perder ou vazar):

```bash
npx tauri signer generate -w ~/.suga-redis/updater.key
```

...e trocar `pubkey` no `tauri.conf.json` pelo conteúdo do `.pub`.

## Compilar na mão

Para testar localmente antes de soltar uma versão. Para distribuir, use
*Soltar uma versão* acima — não mande um pacote compilado na sua máquina.

### macOS

```bash
npm run dist
```

Compila e copia os artefatos para `dist/`:

- `Suga-Redis.app` — o aplicativo
- `Suga-Redis_<versão>_aarch64.dmg` — o instalador

Para instalar o que acabou de sair:

```bash
ditto dist/Suga-Redis.app /Applications/Suga-Redis.app
```

> O build é assinado em modo **ad-hoc** (`signingIdentity: "-"` em
> `src-tauri/tauri.conf.json`) e **não é notarizado**. A assinatura ad-hoc
> não diz de quem o app veio; ela só garante que o bundle está íntegro. Sem
> ela, o `.app` chega quebrado na máquina de quem baixa e o macOS diz que o
> app *"está danificado"* — sem nenhuma forma de abrir mesmo assim.
>
> Com ela, o bloqueio vira o aviso normal de desenvolvedor não identificado,
> que a pessoa libera em **Ajustes do Sistema → Privacidade e Segurança →
> Abrir Mesmo Assim**. Veja a seção *Instalar* do [`README.md`](../README.md).
>
> Para distribuir sem atrito nenhum, é preciso uma conta de desenvolvedor
> Apple: trocar o `-` por um certificado Developer ID e notarizar o pacote
> (as variáveis `APPLE_ID`, `APPLE_PASSWORD` e `APPLE_TEAM_ID` no ambiente
> fazem o `tauri build` notarizar sozinho).

### Linux, a partir do mac

O build do linux sai de um container, para não exigir as bibliotecas do GTK
instaladas no seu sistema:

```bash
docker run --rm -v "$PWD":/app -w /app \
  -e PATH="/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  rust:1-bookworm bash -c '
    set -e
    apt-get update -qq
    apt-get install -y -qq curl ca-certificates file \
      libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libayatana-appindicator3-dev > /dev/null
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash - > /dev/null 2>&1
    apt-get install -y -qq nodejs > /dev/null
    npm install --no-audit --no-fund --silent
    npm run tauri build
  '
```

Duas armadilhas nesse comando, ambas já custaram um build quebrado:

1. **`bash -c`, não `bash -lc`.** O shell de login reescreve o `PATH` e o
   `cargo` some — por isso o `-e PATH=...` explícito.
2. **Node 22 do NodeSource, não o do Debian.** O Debian bookworm traz o
   Node 18, e o Vite 8 morre com
   `does not provide an export named 'styleText'`.

Saem um `.AppImage` e um `.deb` em
`src-tauri/target/release/bundle/`.

> **Depois do build do linux, reinstale as dependências.** O container
> compartilha o volume e troca os binários nativos do rollup pelos de linux,
> o que quebra o `npm test` no mac:
>
> ```bash
> rm -rf node_modules && npm install
> ```

### Arquitetura

O build sai para a arquitetura da máquina: `aarch64` num Mac com Apple
Silicon, `x86_64` num Intel. Para gerar um binário universal do mac:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
npm run tauri build -- --target universal-apple-darwin
```

## Como o projeto é organizado

```
redis-desk/
├─ src/                      interface (Svelte 5 + TypeScript)
│  ├─ lib/api.ts             a ÚNICA porta entre a tela e o Rust
│  ├─ lib/*.svelte           componentes
│  └─ routes/                a casca da janela
├─ src-tauri/
│  └─ src/
│     ├─ core/               o núcleo, independente do Tauri
│     │  ├─ config.rs        hosts e senhas no chaveiro
│     │  ├─ conn.rs          pool de conexões
│     │  ├─ keys.rs          varredura e árvore por prefixo
│     │  ├─ value.rs         ler e escrever por tipo
│     │  ├─ index.rs         índice FTS5 de busca
│     │  ├─ search.rs        busca por substring
│     │  └─ history.rs       cópia de segurança e volta atrás
│     ├─ commands.rs         fachada: a trava de produção mora aqui
│     ├─ approval/           a trava de escrita da LLM (socket unix)
│     └─ mcp/                servidor MCP
└─ dist/                     os pacotes gerados
```

Duas regras que o código segue de ponta a ponta:

- **Identificadores em inglês; comentários e texto de tela em português.**
- **Nada fala com o Redis fora de `core/`,** e nada fala com o `core` a
  partir da tela sem passar por `commands.rs`.
