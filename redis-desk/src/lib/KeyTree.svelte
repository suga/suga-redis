<script lang="ts">
	/**
	 * A árvore de chaves, agrupada por prefixo e carregada por NÍVEL.
	 *
	 * Um banco de milhões de chaves não cabe numa árvore montada de uma vez:
	 * guardar os nomes todos para depois agrupá-los custava quase um
	 * gigabyte e mais de um minuto antes de aparecer qualquer coisa na tela.
	 * Aqui cada galho é varrido quando a pessoa o abre, e o servidor devolve
	 * só os filhos daquele prefixo, já contados.
	 *
	 * O que estiver aberto continua desenhado por janela: um nível com
	 * milhares de filhos ainda pesaria no DOM.
	 */
	import { untrack } from "svelte";
	import { SvelteMap, SvelteSet } from "svelte/reactivity";
	import { errorMessage, loadMoreChildren, treeLevel, type Branch } from "$lib/api";
	import { numero } from "$lib/formato";
	import { t } from "$lib/i18n.svelte";

	interface Props {
		hostId: string;
		db: number;
		/**
		 * Os prefixos de primeiro nível, vindos dos retratos da varredura.
		 * Chegam parciais e vão crescendo — a árvore não os busca.
		 */
		root: Branch[];
		/**
		 * Sobe a cada retrato da varredura. É a dependência que faz os
		 * galhos ABERTOS serem relidos: sem ela, um galho aberto cedo
		 * congelaria no que a trie tinha naquele instante — foi o que
		 * aconteceu com um `fixtures` marcando 11 chaves e mostrando 1.
		 *
		 * Um número, e não a própria `root`: depender dela exigiria lê-la
		 * sem usar o valor, e o compilador descarta uma leitura assim —
		 * exatamente o bug que isto conserta.
		 */
		version: number;
		selectedKey: string | null;
		onSelect: (key: string) => void;
		/** Filtra o que já está aberto. Vazio mostra tudo. */
		filter: string;
		/**
		 * Pede o menu de contexto sobre os prefixos marcados, na posição dada.
		 *
		 * Opcional porque a árvore não decide o que vai no menu: quem monta a
		 * lista é quem a desenha, e até lá a árvore segue inteira sem ele.
		 */
		onMenuPrefixo?: (prefixos: string[], x: number, y: number) => void;
		/** O mesmo, para uma chave solta: uma chave não é um prefixo. */
		onMenuChave?: (chave: string, x: number, y: number) => void;
	}

	let {
		hostId,
		db,
		root,
		version,
		selectedKey,
		onSelect,
		filter,
		onMenuPrefixo,
		onMenuChave,
	}: Props = $props();

	/** Altura fixa de cada linha, em pixels. É o que torna a janela calculável. */
	const ROW_HEIGHT = 26;
	/** Linhas desenhadas além da janela, para a rolagem não mostrar buraco. */
	const OVERSCAN = 8;

	/** O que se sabe de cada prefixo já pedido. */
	type Estado =
		| { situacao: "carregando" }
		| { situacao: "pronto"; filhos: Branch[]; cortado: boolean }
		| { situacao: "ainda-nao" }
		| { situacao: "erro"; mensagem: string };

	let niveis = new SvelteMap<string, Estado>();
	let abertos = new SvelteSet<string>();

	/**
	 * Os prefixos marcados.
	 *
	 * Guardados pelo NOME do prefixo, nunca pela linha: a árvore é desenhada
	 * por janela, e a linha marcada deixa de existir assim que a rolagem
	 * passa longe dela.
	 *
	 * Só prefixos. Juntar uma chave solta a um galho faria o item do menu
	 * falar de duas coisas diferentes na mesma frase, e não há redação que
	 * fique honesta assim.
	 */
	let selecionados = new SvelteSet<string>();

	/** De onde a faixa do Shift+clique parte — o prefixo, pela mesma razão. */
	let ancora: string | null = $state(null);
	let scrollTop = $state(0);
	let viewport = $state(0);

	/**
	 * Lê os filhos de um prefixo da árvore em memória.
	 *
	 * Sempre relê, mesmo que já tenha lido antes: enquanto a varredura
	 * corre, o mesmo galho ganha filhos novos, e guardar a primeira
	 * resposta congelaria a árvore no estado em que ela estava quando a
	 * pessoa abriu.
	 */
	async function carregar(prefixo: string) {
		if (!niveis.has(prefixo)) {
			niveis.set(prefixo, { situacao: "carregando" });
		}
		try {
			const nivel = await treeLevel(hostId, db, prefixo);
			niveis.set(
				prefixo,
				nivel === null
					? { situacao: "ainda-nao" }
					: { situacao: "pronto", filhos: nivel.children, cortado: nivel.truncated },
			);
		} catch (err) {
			// O erro fica NO GALHO: uma falha ao abrir um prefixo não pode
			// derrubar o que já está navegável acima dele.
			niveis.set(prefixo, { situacao: "erro", mensagem: errorMessage(err) });
		}
	}

	/** Qual (host, banco) os galhos abertos descrevem. */
	let bancoAberto = $state("");

	// Trocar de host ou de banco invalida tudo que foi aberto.
	//
	// A comparação com `bancoAberto` não é economia, é CORREÇÃO: um efeito
	// pode rodar de novo sem que a dependência tenha mudado de valor, e
	// limpar sem conferir apagava os galhos que a pessoa tinha acabado de
	// abrir — um `fixtures` marcando 22 chaves e mostrando uma.
	$effect(() => {
		const chave = `${hostId}#${db}`;
		untrack(() => {
			if (chave === bancoAberto) return;
			bancoAberto = chave;
			niveis.clear();
			abertos.clear();
			selecionados.clear();
			ancora = null;
		});
	});

	// A raiz cresce a cada retrato da varredura; os galhos abertos
	// acompanham, para o que está dentro subir junto com o que está fora.
	$effect(() => {
		// `version` é lida e USADA: é o que registra a dependência.
		if (version >= 0) {
			untrack(() => {
					for (const prefixo of abertos) void carregar(prefixo);
			});
		}
	});

	/**
	 * Quantos degraus de corredor seguir sozinho ao abrir um galho.
	 *
	 * Finito porque uma chave com dezenas de níveis existe (`a:b:c:...`), e
	 * descer todos de uma vez encheria a tela de linhas que ninguém pediu.
	 */
	const MAX_CORREDOR = 12;

	function alternar(branch: Branch) {
		if (abertos.has(branch.full_prefix)) {
			abertos.delete(branch.full_prefix);
			// O que fecha some da tela, e seleção invisível é seleção perigosa:
			// o menu diria "3 prefixos" com dois deles fora da vista.
			for (const prefixo of selecionados) {
				if (prefixo !== branch.full_prefix && prefixo.startsWith(branch.full_prefix)) {
					selecionados.delete(prefixo);
				}
			}
			return;
		}
		abertos.add(branch.full_prefix);
		descer(branch.full_prefix);
	}

	/**
	 * Abre o galho e segue descendo enquanto o caminho não se dividir.
	 *
	 * Um nível com um filho só não é uma escolha, é um corredor: obrigar um
	 * clique em cada degrau faz a pessoa clicar seis vezes para chegar à
	 * primeira decisão de verdade. Para quando houver mais de um filho,
	 * quando chegar numa folha, ou no teto de degraus.
	 */
	async function descer(prefixo: string) {
		let atual = prefixo;
		for (let degrau = 0; degrau < MAX_CORREDOR; degrau += 1) {
			await carregar(atual);
			const estado = niveis.get(atual);
			if (estado?.situacao !== "pronto" || estado.filhos.length !== 1) return;

			const unico = estado.filhos[0];
			if (unico.leaf_key !== null) return;
			abertos.add(unico.full_prefix);
			atual = unico.full_prefix;
		}
	}

	/** Prefixos cuja próxima leva está sendo buscada. */
	let buscandoMais = new SvelteSet<string>();

	/**
	 * Traz a próxima leva de filhos de um galho cortado.
	 *
	 * Nenhum dado fica inalcançável: o nível que não coube de primeira é
	 * varrido de novo, só ele, e a lista cresce até acabar.
	 */
	async function carregarMais(prefixo: string) {
		if (buscandoMais.has(prefixo)) return;
		buscandoMais.add(prefixo);
		try {
			const nivel = await loadMoreChildren(hostId, db, prefixo);
			niveis.set(prefixo, {
				situacao: "pronto",
				filhos: nivel.children,
				cortado: nivel.truncated,
			});
		} catch (err) {
			niveis.set(prefixo, { situacao: "erro", mensagem: errorMessage(err) });
		} finally {
			buscandoMais.delete(prefixo);
		}
	}

	interface Linha {
		tipo: "galho" | "aviso";
		branch?: Branch;
		depth: number;
		aberto: boolean;
		/** Para as linhas de aviso: o texto e o papel na tela. */
		texto?: string;
		papel?: "carregando" | "erro" | "ainda-nao" | "vazio" | "cortado";
		/** Para a linha de "cortado": o prefixo a pedir mais. */
		prefixo?: string;
	}

	let alvo = $derived(filter.trim().toLowerCase());

	/** Achata o que está aberto nas linhas à vista. */
	function achatar(prefixo: string, depth: number, out: Linha[]): Linha[] {
		// A raiz vem de fora; os demais níveis, da árvore em memória.
		const filhosDoNivel: Branch[] | null = (() => {
			if (prefixo === "") return root;
			const estado = niveis.get(prefixo);
			if (!estado) return null;
			if (estado.situacao === "pronto") return estado.filhos;
			if (estado.situacao === "carregando") {
				out.push({ tipo: "aviso", depth, aberto: false, texto: t("arvore.lendo"), papel: "carregando" });
			} else if (estado.situacao === "ainda-nao") {
				out.push({
					tipo: "aviso",
					depth,
					aberto: false,
					texto: "Este trecho ainda sendo lido — aparece assim que a varredura chegar nele.",
					papel: "ainda-nao",
				});
			} else {
				out.push({ tipo: "aviso", depth, aberto: false, texto: estado.mensagem, papel: "erro" });
			}
			return null;
		})();

		if (filhosDoNivel === null) return out;

		const visiveis = alvo
			? filhosDoNivel.filter((b) => b.label.toLowerCase().includes(alvo))
			: filhosDoNivel;

		for (const branch of visiveis) {
			const aberto = abertos.has(branch.full_prefix);
			out.push({ tipo: "galho", branch, depth, aberto });
			if (aberto) achatar(branch.full_prefix, depth + 1, out);
		}

		const estadoAtual = prefixo === "" ? null : niveis.get(prefixo);
		if (estadoAtual?.situacao === "pronto" && estadoAtual.cortado) {
			out.push({
				tipo: "aviso",
				depth,
				aberto: false,
				texto: `Mostrando ${numero(estadoAtual.filhos.length)} deste nível`,
				papel: "cortado",
				prefixo,
			});
		}

		if (alvo && visiveis.length === 0) {
			out.push({
				tipo: "aviso",
				depth,
				aberto: false,
				texto: "Nada com esse nome entre os galhos abertos. Use a aba Busca para procurar no banco inteiro.",
				papel: "vazio",
			});
		}

		return out;
	}

	let linhas = $derived(achatar("", 0, []));

	// ------------------------------------------------------- seleção e menu

	function alternarMarca(branch: Branch) {
		// Uma chave solta não entra — ver o comentário de `selecionados`.
		if (branch.leaf_key !== null) return;
		if (selecionados.has(branch.full_prefix)) {
			selecionados.delete(branch.full_prefix);
		} else {
			selecionados.add(branch.full_prefix);
		}
		ancora = branch.full_prefix;
	}

	/**
	 * Marca tudo que é prefixo entre a âncora e esta linha.
	 *
	 * A faixa é contada sobre `linhas` — a lista achatada inteira —, e não
	 * sobre a fatia desenhada: só um punhado de linhas existe no DOM de cada
	 * vez, e contar por ali pegaria a faixa errada assim que a lista passasse
	 * da janela.
	 */
	function marcarFaixa(branch: Branch) {
		if (branch.leaf_key !== null || ancora === null) return;
		const de = linhas.findIndex((l) => l.branch?.full_prefix === ancora);
		const ate = linhas.findIndex((l) => l.branch?.full_prefix === branch.full_prefix);
		if (de === -1 || ate === -1) return;

		selecionados.clear();
		for (let i = Math.min(de, ate); i <= Math.max(de, ate); i += 1) {
			const dentro = linhas[i].branch;
			if (dentro && dentro.leaf_key === null) selecionados.add(dentro.full_prefix);
		}
	}

	/**
	 * Marca os prefixos de primeiro nível.
	 *
	 * Primeiro nível e nada abaixo: o que está dentro de um prefixo marcado
	 * já vai junto com ele, e marcá-lo também faria o menu contar duas vezes
	 * a mesma coisa.
	 */
	function marcarPrimeiroNivel() {
		selecionados.clear();
		for (const linha of linhas) {
			const branch = linha.branch;
			if (linha.depth === 0 && branch && branch.leaf_key === null) {
				selecionados.add(branch.full_prefix);
			}
		}
	}

	function aoClicar(e: MouseEvent, branch: Branch) {
		if (e.shiftKey) {
			marcarFaixa(branch);
			return;
		}
		if (e.metaKey || e.ctrlKey) {
			alternarMarca(branch);
			return;
		}

		// O clique simples continua sendo abrir, fechar ou escolher a chave; a
		// seleção sai do caminho, e a próxima faixa parte daqui.
		selecionados.clear();
		ancora = branch.leaf_key === null ? branch.full_prefix : null;
		if (branch.leaf_key !== null) onSelect(branch.leaf_key);
		else alternar(branch);
	}

	/**
	 * Pede o menu sobre esta linha, na posição dada.
	 *
	 * Convenção de gerenciador de arquivos: numa linha que já está marcada,
	 * vale a seleção inteira; numa linha de fora, a seleção se perde e vale
	 * só ela — quem errou o alvo perde a seleção em vez de agir sobre o que
	 * não queria.
	 */
	function abrirMenu(branch: Branch, x: number, y: number) {
		if (branch.leaf_key !== null) {
			selecionados.clear();
			ancora = null;
			onMenuChave?.(branch.leaf_key, x, y);
			return;
		}

		if (!selecionados.has(branch.full_prefix)) {
			selecionados.clear();
			selecionados.add(branch.full_prefix);
			ancora = branch.full_prefix;
		}
		onMenuPrefixo?.([...selecionados], x, y);
	}

	function aoMenuContexto(e: MouseEvent, branch: Branch) {
		// Sem isto o menu do sistema abre por cima do nosso.
		e.preventDefault();
		abrirMenu(branch, e.clientX, e.clientY);
	}

	/**
	 * O teclado alcança o mesmo: Shift+F10 abre o menu ancorado na linha com
	 * foco, e ⌘/Ctrl+A marca o primeiro nível.
	 */
	function aoTeclar(e: KeyboardEvent, branch: Branch, linha: HTMLElement) {
		if (e.shiftKey && e.key === "F10") {
			e.preventDefault();
			const caixa = linha.getBoundingClientRect();
			abrirMenu(branch, caixa.left, caixa.bottom);
			return;
		}
		if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") {
			e.preventDefault();
			marcarPrimeiroNivel();
		}
	}

	/**
	 * A fatia a desenhar. Enquanto a janela não foi medida (primeira pintura,
	 * e também em teste fora de um navegador de verdade) `viewport` é zero —
	 * aí desenha tudo, que é o correto para as listas pequenas e nunca
	 * esconde linha de ninguém.
	 */
	let inicio = $derived(
		viewport === 0 ? 0 : Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN),
	);
	let fim = $derived(
		viewport === 0
			? linhas.length
			: Math.min(
					linhas.length,
					Math.ceil((scrollTop + viewport) / ROW_HEIGHT) + OVERSCAN,
				),
	);
	let fatia = $derived(linhas.slice(inicio, fim));

	function medir(el: HTMLElement) {
		viewport = el.clientHeight;
		const observer = new ResizeObserver(() => (viewport = el.clientHeight));
		observer.observe(el);
		return { destroy: () => observer.disconnect() };
	}
</script>

<div
	class="rolagem"
	use:medir
	onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
>
	{#if linhas.length === 0}
		<p class="vazio">{t("arvore.bancoVazio")}</p>
	{:else}
		<div class="pista" style="height: {linhas.length * ROW_HEIGHT}px">
			<div class="janela" style="transform: translateY({inicio * ROW_HEIGHT}px)">
				{#each fatia as linha, i (linha.branch?.full_prefix ?? `${linha.papel}-${inicio + i}`)}
					{#if linha.tipo === "galho" && linha.branch}
						{@const branch = linha.branch}
						<button
							class="linha"
							class:selecionada={branch.leaf_key !== null && branch.leaf_key === selectedKey}
							class:marcada={selecionados.has(branch.full_prefix)}
							style="padding-left: {0.5 + linha.depth * 0.85}rem"
							onclick={(e) => aoClicar(e, branch)}
							oncontextmenu={(e) => aoMenuContexto(e, branch)}
							onkeydown={(e) => aoTeclar(e, branch, e.currentTarget)}
						>
							<span class="caret" aria-hidden="true">
								{branch.leaf_key !== null ? "" : linha.aberto ? "▾" : "▸"}
							</span>
							<span class="rotulo">{branch.label}</span>
							{#if branch.leaf_key === null}
								<span class="contagem mono">{numero(branch.count)}</span>
							{/if}
						</button>
					{:else if linha.papel === "cortado" && linha.prefixo}
						{@const alvoPrefixo = linha.prefixo}
						<div
							class="linha aviso aviso--cortado"
							style="padding-left: {0.5 + linha.depth * 0.85}rem"
							data-testid="cortado-nivel"
						>
							<span class="rotulo">{linha.texto}</span>
							<button
								class="mais"
								disabled={buscandoMais.has(alvoPrefixo)}
								onclick={() => carregarMais(alvoPrefixo)}
							>
								{buscandoMais.has(alvoPrefixo) ? t("acao.carregando") : t("acao.carregarMais")}
							</button>
						</div>
					{:else}
						<p
							class="linha aviso aviso--{linha.papel}"
							style="padding-left: {0.5 + linha.depth * 0.85}rem"
							role={linha.papel === "erro" ? "alert" : undefined}
							data-testid={linha.papel === "ainda-nao" ? "ainda-nao" : undefined}
						>
							{linha.texto}
						</p>
					{/if}
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	.rolagem {
		height: 100%;
		overflow-y: auto;
	}

	.vazio {
		margin: 0;
		padding: 1rem 0.8rem;
		color: var(--text-faint);
	}

	/* A pista tem a altura de tudo que está aberto, para a barra de rolagem
	   corresponder ao conteúdo real; a janela desliza dentro dela. */
	.pista {
		position: relative;
	}

	.janela {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
	}

	.linha {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		width: 100%;
		height: 26px;
		margin: 0;
		padding-right: 0.6rem;
		border: none;
		border-radius: 0;
		text-align: left;
		white-space: nowrap;
	}

	button.linha:hover {
		background: var(--panel-dark);
	}

	.selecionada {
		background: var(--panel-dark);
		box-shadow: inset 2px 0 0 var(--action);
	}

	/* A marca da seleção é um cinza mais claro — nunca o vermelho `--prod`
	   nem caixa alta: os dois significam só o ambiente nesta tela, e
	   emprestá-los aqui deixaria o aviso de produção ambíguo. */
	.marcada {
		background: var(--rule);
	}

	.caret {
		width: 0.8rem;
		color: var(--text-faint);
		flex: none;
	}

	.rotulo {
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.contagem {
		margin-left: auto;
		color: var(--text-faint);
		font-size: 0.85em;
	}

	.aviso {
		font-size: 0.85rem;
		color: var(--text-faint);
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.aviso--erro {
		color: #e49a94;
	}

	.aviso--ainda-nao,
	.aviso--cortado {
		color: var(--action);
	}

	.mais {
		margin-left: auto;
		padding: 0 0.45rem;
		font-size: 0.82rem;
		color: var(--action);
		border-color: var(--rule-strong);
		flex: none;
	}
</style>
