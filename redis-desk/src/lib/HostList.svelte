<script lang="ts">
	/**
	 * A coluna de hosts.
	 *
	 * Cada linha carrega uma marca na cor do seu ambiente, então a coluna
	 * inteira é lida de relance como um índice colorido: dá para ver quantos
	 * hosts de produção existem sem ler nome nenhum.
	 */
	import { envChrome } from "$lib/env";
	import { connect, disconnect, errorMessage, removeHost, testHost, type Host } from "$lib/api";

	/**
	 * O que se sabe sobre a conexão com um host, até a próxima ação.
	 *
	 * Não é o resultado do botão Testar — aquele responde "dá para
	 * alcançar?". Este responde "está conectado agora?", a pergunta que
	 * faltava quando um host de produção abriu mostrando "nenhuma chave" e
	 * não havia como dizer se era banco vazio ou conexão que nunca aconteceu.
	 */
	type EstadoConexao =
		| { tipo: "desligado" }
		| { tipo: "conectando" }
		| { tipo: "conectado"; ms: number }
		| { tipo: "falhou"; motivo: string };

	interface Props {
		hosts: Host[];
		selectedId: string | null;
		/**
		 * Uma falha de conexão observada FORA desta lista — na prática, pela
		 * área de trabalho, quando uma operação no host aberto não alcançou
		 * o servidor.
		 *
		 * Existe porque o estado desta coluna era escrito uma vez, no
		 * clique, e nunca mais: o pool derrubava um socket morto, a nova
		 * tentativa falhava, a área de trabalho dizia "não foi possível
		 * conectar" e a linha do host continuava afirmando "conectado · 12
		 * ms" a duas colunas de distância. Uma tela, duas afirmações
		 * contrárias sobre o mesmo banco de produção.
		 *
		 * Escolhido contra um `PING` periódico: um relógio batendo em todo
		 * host cadastrado paga rede o tempo todo para descobrir o que a
		 * próxima operação descobriria de graça, e num host serverless que
		 * leva 5679 ms para responder um `PING` o custo não é teórico. A
		 * mentira que importa é a que fica na tela DEPOIS de uma falha
		 * visível, e essa o caminho de volta apaga sem perguntar nada a
		 * ninguém. Enquanto nenhuma operação falhar, "conectado" continua
		 * sendo o que o pool sabe — e é o que esta coluna promete dizer.
		 */
		falhaExterna: { hostId: string; motivo: string } | null;
		/**
		 * `falhaConexao` carrega o motivo quando quem chamou já sabe, por ter
		 * acabado de tentar `connect`, que a conexão falhou — para a casca
		 * repassar ao `Workspace` e ele nem tentar varrer. Ausente (clique no
		 * nome, sem passar por `connect`) ou `undefined` no sucesso.
		 */
		onSelect: (host: Host, falhaConexao?: string) => void;
		onNew: () => void;
		onEdit: (host: Host) => void;
		/** Chamado depois de remover, para a tela recarregar a lista. */
		onChanged: () => void;
		/** Chamado depois de desconectar — a casca fecha o workspace se era este o host aberto. */
		onDisconnected: (host: Host) => void;
	}

	let {
		hosts,
		selectedId,
		falhaExterna,
		onSelect,
		onNew,
		onEdit,
		onChanged,
		onDisconnected,
	}: Props = $props();

	/** Resultado do último teste por host: a latência, ou a falha. */
	let testes = $state<Record<string, { ms: number } | { erro: string }>>({});
	let testando = $state<string | null>(null);
	/**
	 * Falha ao REMOVER, por host. Slot próprio, e não o de `testes`: a
	 * remoção escrevia o erro dela no resultado do botão Testar, que ficava
	 * escondido justamente no estado "conectado" — uma remoção que falhava
	 * não deixava rastro nenhum na tela.
	 */
	let errosRemocao = $state<Record<string, string>>({});

	/** Estado de conexão por host. Ausente = nunca mexeram neste host ainda. */
	let estados = $state<Record<string, EstadoConexao>>({});
	/** Qual host está no meio de um `disconnect()` agora — evita que um
	 *  clique duplo dispare duas chamadas concorrentes, do mesmo jeito que
	 *  `conectando` já protege o lado de conectar. */
	let desconectando = $state<string | null>(null);

	function estadoDe(hostId: string): EstadoConexao {
		return estados[hostId] ?? { tipo: "desligado" };
	}

	/**
	 * Uma falha vinda de fora derruba o "conectado" do host a que ela se
	 * refere — e só dele.
	 *
	 * Idempotente de propósito: um efeito pode re-disparar (ver Svelte 5 em
	 * `docs/CONTEXTO.md`), e escrever o mesmo estado duas vezes não muda
	 * nada. `falhaExterna` é o único valor lido aqui, então escrever
	 * `estados` não realimenta o efeito.
	 */
	$effect(() => {
		const falha = falhaExterna;
		if (!falha) return;
		estados[falha.hostId] = { tipo: "falhou", motivo: falha.motivo };
	});

	/**
	 * Conecta de verdade e só então abre o workspace — nos dois desfechos.
	 * Falhando, o workspace abre do mesmo jeito, em estado de falha: é lá
	 * que vive a aba Logs, e é justamente quando a conexão falha que ela
	 * precisa estar alcançável. O motivo (se houver) viaja com `onSelect`
	 * para a casca repassar ao `Workspace`, que não tenta varrer um host já
	 * sabido inalcançável.
	 */
	async function conectar(host: Host) {
		estados[host.id] = { tipo: "conectando" };
		try {
			const ms = await connect(host.id);
			estados[host.id] = { tipo: "conectado", ms };
			onSelect(host);
		} catch (err) {
			const motivo = errorMessage(err);
			estados[host.id] = { tipo: "falhou", motivo };
			onSelect(host, motivo);
		}
	}

	/**
	 * `disconnect` também pode falhar (host desconhecido, por exemplo). Uma
	 * funcionalidade cuja razão de existir é dizer a verdade sobre a conexão
	 * não pode ter um caminho de falha mudo: se não desconectou de fato, a
	 * linha mostra o motivo — do mesmo jeito que uma falha de `connect` — em
	 * vez de ficar presa em "conectado" sem avisar ninguém.
	 */
	async function desconectar(host: Host) {
		desconectando = host.id;
		try {
			await disconnect(host.id);
			estados[host.id] = { tipo: "desligado" };
			onDisconnected(host);
		} catch (err) {
			estados[host.id] = { tipo: "falhou", motivo: errorMessage(err) };
		} finally {
			desconectando = null;
		}
	}

	async function testar(host: Host) {
		testando = host.id;
		try {
			testes[host.id] = { ms: await testHost(host.id) };
		} catch (err) {
			testes[host.id] = { erro: errorMessage(err) };
		} finally {
			testando = null;
		}
	}

	async function remover(host: Host) {
		errosRemocao[host.id] = "";
		try {
			await removeHost(host.id);
			onChanged();
		} catch (err) {
			errosRemocao[host.id] = errorMessage(err);
		}
	}
</script>

<nav>
	<div class="topo">
		<h2>Hosts</h2>
		<button onclick={onNew}>Novo host</button>
	</div>

	{#if hosts.length === 0}
		<p class="vazio">Cadastre um host para começar.</p>
	{:else}
		<ul>
			{#each hosts as host (host.id)}
				{@const resultado = testes[host.id]}
				{@const erroRemocao = errosRemocao[host.id]}
				{@const chrome = envChrome(host.env)}
				{@const estado = estadoDe(host.id)}
				<li
					class="linha"
					class:selecionada={host.id === selectedId}
					style="--marca: {chrome.band}"
					data-env={host.env}
				>
					<div class="cabeca">
						<!-- Clicar no nome conecta, igual ao botão: montar a área de
						     trabalho sem conectar deixava a árvore aparecer (o
						     `scan_tree` abre o socket por conta própria, lá no Rust)
						     com a linha dizendo "nunca conectado" ao lado. Num banco
						     vazio isso vira "Nenhuma chave neste banco" debaixo de
						     "nunca conectado" — a conclusão errada que originou este
						     trabalho. É barato: com o socket já no pool, `connect` é
						     um PING. -->
						<button class="abrir" onclick={() => conectar(host)}>
							{host.name}
						</button>
						<span class="flag" data-testid="flag-ambiente">{chrome.label}</span>
					</div>

					<!-- Endereço de cluster gerenciado passa de 70 caracteres. Cortado
					     para caber na coluna; o texto inteiro fica no `title`, porque é
					     ele que distingue dois clusters do mesmo ambiente. -->
					<p
						class="endereco mono"
						data-testid="endereco"
						title="{host.addr}:{host.port}/{host.db}"
					>
						{host.addr}:{host.port}/{host.db}
					</p>

					<div class="acoes">
						{#if estado.tipo === "conectado"}
							<button
								class="conectar"
								onclick={() => desconectar(host)}
								disabled={desconectando === host.id}
							>
								{desconectando === host.id ? "Desconectando…" : "Desconectar"}
							</button>
						{:else}
							<button
								class="conectar"
								onclick={() => conectar(host)}
								disabled={estado.tipo === "conectando"}
							>
								{estado.tipo === "conectando" ? "Conectando…" : "Conectar"}
							</button>
						{/if}
						<button onclick={() => testar(host)} disabled={testando === host.id}>
							{testando === host.id ? "Testando" : "Testar"}
						</button>
						<button onclick={() => onEdit(host)}>Editar</button>
						<button class="danger" onclick={() => remover(host)}>Remover</button>
					</div>

					<p class="estado" data-testid="estado-conexao" data-tipo={estado.tipo}>
						{#if estado.tipo === "desligado"}
							nunca conectado
						{:else if estado.tipo === "conectando"}
							conectando…
						{:else if estado.tipo === "conectado"}
							conectado · {estado.ms} ms · banco {host.db}
						{:else}
							{estado.motivo}
						{/if}
					</p>

					<!-- Sem condição de estado: escondido fora de "desligado", o
					     botão Testar virava um rótulo que pisca e não responde
					     nada — e o pior caso era o estado "falhou", justo quando
					     se pergunta "mas dá para alcançar?". Testar responde outra
					     pergunta que a linha de estado, e as duas cabem juntas. -->
					{#if resultado && "ms" in resultado}
						<p class="resultado mono">Respondeu em {resultado.ms} ms</p>
					{:else if resultado}
						<p class="resultado falha" role="alert">{resultado.erro}</p>
					{/if}

					{#if erroRemocao}
						<p class="resultado falha" role="alert" data-testid="erro-remocao">
							{erroRemocao}
						</p>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-width: 0;
		background: var(--panel);
		border-right: 1px solid var(--rule);
	}

	.topo {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		padding: 0.7rem 0.8rem;
		border-bottom: 1px solid var(--rule);
	}

	h2 {
		margin: 0;
		font-size: 1.05rem;
		font-weight: 600;
	}

	.vazio {
		margin: 0;
		padding: 1rem 0.8rem;
		color: var(--text-faint);
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		min-width: 0;
		overflow-y: auto;
	}

	/* A marca do ambiente: a informação que precisa sobreviver a uma olhada
	   rápida na coluna inteira. */
	.linha {
		min-width: 0;
		border-left: 4px solid var(--marca);
		border-bottom: 1px solid var(--rule);
		padding: 0.5rem 0.8rem 0.6rem;
	}

	.linha.selecionada {
		background: var(--panel-dark);
	}

	/* `min-width: 0` em toda a cadeia: sem isso um item de flex se recusa a
	   encolher abaixo do conteúdo, e um endereço de 70 caracteres sem espaço
	   nenhum empurra a coluna inteira, criando a barra de rolagem horizontal. */
	.cabeca {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.5rem;
		min-width: 0;
	}

	.abrir {
		min-width: 0;
		padding: 0;
		border: none;
		text-align: left;
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.abrir:hover {
		color: var(--action);
	}

	/* A flag diz em palavras o que a marca colorida da borda diz em cor.
	   Quem não decorou o código de cores ainda assim lê onde está. */
	.flag {
		flex: none;
		padding: 0.05rem 0.35rem;
		border-radius: var(--radius);
		background: var(--marca);
		color: #f6f8fa;
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.08em;
	}

	/* O âmbar da homologação é claro demais para texto branco. */
	.linha[data-env="homolog"] .flag {
		color: #1c2024;
	}

	.endereco {
		margin: 0.1rem 0 0;
		color: var(--text-dim);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.acoes {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		margin-top: 0.45rem;
	}

	/* Ação principal da linha, sem usar o âmbar: uma coluna com dez hosts
	   teria dez botões gritando, e o âmbar deixaria de significar "a ação
	   desta tela". */
	.acoes .conectar {
		color: var(--text);
		border-color: var(--text-faint);
	}

	.acoes button {
		padding: 0.15rem 0.45rem;
		font-size: 0.88rem;
		color: var(--text-dim);
	}

	.resultado {
		margin: 0.45rem 0 0;
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.falha {
		color: #e49a94;
	}

	/* Verde, âmbar e cinza — nunca vermelho, que neste app só quer dizer
	   produção, e nunca caixa alta, que só a flag do ambiente usa. Mesmos
	   tons de LogPanel.svelte, para a mesma ideia ter a mesma cor na tela
	   inteira. */
	.estado {
		margin: 0.45rem 0 0;
		color: var(--text-faint);
		font-size: 0.88rem;
	}

	.estado[data-tipo="conectado"] {
		color: #87a18e;
	}

	.estado[data-tipo="falhou"] {
		color: #bbac81;
	}
</style>
