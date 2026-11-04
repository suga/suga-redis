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

## Gerar os pacotes

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

> O build **não é assinado nem notarizado**. Na primeira abertura o macOS
> bloqueia: botão direito no app → **Abrir** → **Abrir**. Para distribuir a
> outras pessoas sem esse atrito, é preciso uma conta de desenvolvedor Apple
> e configurar `signingIdentity` em `src-tauri/tauri.conf.json`.

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
