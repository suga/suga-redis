// A regra de CSS da marca é conferida na fonte (ver o último teste).
// @ts-expect-error type error without @types/node package
import { readFileSync } from "node:fs";

import { createEvent, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, treeLevel: vi.fn() };
});

import { treeLevel } from "$lib/api";
import KeyTree from "$lib/KeyTree.svelte";
import type { Branch } from "$lib/api";

const nivel = vi.mocked(treeLevel);

function galho(label: string, prefixoPai: string, count: number): Branch {
	return { label, full_prefix: `${prefixoPai}${label}:`, count, leaf_key: null };
}

function folha(label: string, prefixoPai: string): Branch {
	return {
		label,
		full_prefix: `${prefixoPai}${label}:`,
		count: 1,
		leaf_key: `${prefixoPai}${label}`,
	};
}

/** Cada prefixo responde com os seus filhos, como a trie em memória faria. */
function arvore(mapa: Record<string, Branch[]>) {
	nivel.mockImplementation(async (_h, _db, prefix) =>
		mapa[prefix] ? { children: mapa[prefix], truncated: false } : null,
	);
}

/** Três prefixos e uma chave solta: a seleção só pode pegar os três. */
const RAIZ: Branch[] = [
	galho("catalogo", "", 24),
	galho("pedido", "", 12),
	galho("sessao", "", 7),
	folha("solta", ""),
];

function montar(onSelect = vi.fn()) {
	const onPrefixMenu = vi.fn();
	const onKeyMenu = vi.fn();
	const tela = render(KeyTree, {
		props: {
			hostId: "h1",
			db: 0,
			root: RAIZ,
			version: 0,
			selectedKey: null,
			onSelect,
			filter: "",
			onPrefixMenu,
			onKeyMenu,
		},
	});
	return { ...tela, onPrefixMenu, onKeyMenu };
}

function linha(nome: RegExp) {
	return screen.getByRole("button", { name: nome });
}

/** O clique com a tecla de juntar segurada. */
async function cliqueComando(user: ReturnType<typeof userEvent.setup>, el: HTMLElement) {
	await user.keyboard("{Meta>}");
	await user.click(el);
	await user.keyboard("{/Meta}");
}

async function cliqueShift(user: ReturnType<typeof userEvent.setup>, el: HTMLElement) {
	await user.keyboard("{Shift>}");
	await user.click(el);
	await user.keyboard("{/Shift}");
}

beforeEach(() => {
	vi.clearAllMocks();
});

afterEach(() => {
	// Desfaz a altura fingida pelo teste da janela, para não vazar.
	delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
});

test("contextmenu numa linha de prefixo chama preventDefault e onPrefixMenu", () => {
	const { onPrefixMenu } = montar();

	const evento = createEvent.contextMenu(linha(/catalogo/), {
		clientX: 120,
		clientY: 240,
	});
	fireEvent(linha(/catalogo/), evento);

	// Sem o preventDefault, o menu do sistema operacional abre por cima do nosso.
	expect(evento.defaultPrevented).toBe(true);
	expect(onPrefixMenu).toHaveBeenCalledWith(["catalogo:"], 120, 240);
});

test("Shift+F10 na linha com foco abre o menu ancorado nela", async () => {
	const user = userEvent.setup();
	const { onPrefixMenu } = montar();

	const alvo = linha(/pedido/);
	// O jsdom não faz layout: sem uma caixa fingida, toda linha fica em
	// 0,0 e o teste não distinguiria "ancorado nela" de "ancorado em nada".
	alvo.getBoundingClientRect = () =>
		({ left: 40, bottom: 130 }) as DOMRect;
	alvo.focus();

	await user.keyboard("{Shift>}{F10}{/Shift}");

	expect(onPrefixMenu).toHaveBeenCalledWith(["pedido:"], 40, 130);
});

test("contextmenu numa linha de chave chama onKeyMenu", () => {
	const { onPrefixMenu, onKeyMenu } = montar();

	fireEvent.contextMenu(linha(/solta/), { clientX: 10, clientY: 20 });

	expect(onKeyMenu).toHaveBeenCalledWith("solta", 10, 20);
	expect(onPrefixMenu).not.toHaveBeenCalled();
});

test("⌘+clique junta prefixos à seleção", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	expect(linha(/pedido/)).not.toHaveClass("marcada");
	// Juntar à seleção não é abrir: nenhum galho foi lido.
	expect(nivel).not.toHaveBeenCalled();

	// E junta nos dois sentidos: o mesmo clique tira de volta.
	await cliqueComando(user, linha(/catalogo/));
	expect(linha(/catalogo/)).not.toHaveClass("marcada");
});

test("⌘+clique numa folha não junta nada", async () => {
	const user = userEvent.setup();
	const onSelect = vi.fn();
	montar(onSelect);

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/solta/));

	// A chave solta não entra na seleção — e também não leva a seleção junto.
	expect(linha(/solta/)).not.toHaveClass("marcada");
	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(onSelect).not.toHaveBeenCalled();
});

test("Shift+clique pega a faixa", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueShift(user, linha(/sessao/));

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/pedido/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	// A chave solta está fora da faixa, e não entraria nem dentro dela.
	expect(linha(/solta/)).not.toHaveClass("marcada");
});

test("⌘+A seleciona os prefixos de primeiro nível", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	montar();

	await user.click(linha(/catalogo/));
	await screen.findByRole("button", { name: /eventos/ });

	linha(/pedido/).focus();
	await user.keyboard("{Meta>}a{/Meta}");

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/pedido/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	// Primeiro nível é primeiro nível: o que está dentro de um prefixo
	// marcado já vai junto, e marcá-lo também faria o menu contar duas vezes.
	// Reconsultado DEPOIS do ⌘+A: o nó de antes não leva a classe nova.
	expect(linha(/eventos/)).not.toHaveClass("marcada");
	expect(linha(/solta/)).not.toHaveClass("marcada");
});

test("contextmenu numa linha JÁ selecionada manda a seleção inteira", async () => {
	const user = userEvent.setup();
	const { onPrefixMenu } = montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	fireEvent.contextMenu(linha(/sessao/), { clientX: 5, clientY: 6 });

	expect(onPrefixMenu).toHaveBeenCalledWith(["catalogo:", "sessao:"], 5, 6);
	expect(linha(/catalogo/)).toHaveClass("marcada");
});

test("contextmenu numa linha fora da seleção limpa a seleção e manda só ela", async () => {
	const user = userEvent.setup();
	const { onPrefixMenu } = montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	fireEvent.contextMenu(linha(/pedido/), { clientX: 5, clientY: 6 });

	// Quem errou o alvo perde a seleção, em vez de apagar o que não queria.
	expect(onPrefixMenu).toHaveBeenCalledWith(["pedido:"], 5, 6);
	expect(linha(/catalogo/)).not.toHaveClass("marcada");
	expect(linha(/sessao/)).not.toHaveClass("marcada");
});

test("fechar um galho guarda a marca e avisa na linha que a esconde", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	montar();

	await user.click(linha(/catalogo/));
	await cliqueComando(user, await screen.findByRole("button", { name: /eventos/ }));
	expect(linha(/eventos/)).toHaveClass("marcada");

	await user.click(linha(/catalogo/));

	// Nada do que foi marcado se perde ao fechar — mas nada fica invisível:
	// o galho que esconde a marca diz isso na própria linha.
	expect(screen.queryByRole("button", { name: /eventos/ })).not.toBeInTheDocument();
	expect(linha(/catalogo/)).toHaveClass("guarda-marcados");
	expect(linha(/catalogo/)).not.toHaveClass("marcada");
	// E o aviso é só de quem esconde: `pedido` não guarda nada.
	expect(linha(/pedido/)).not.toHaveClass("guarda-marcados");

	// Reabrir devolve a marca onde ela estava, e o aviso sai.
	await user.click(linha(/catalogo/));
	expect(await screen.findByRole("button", { name: /eventos/ })).toHaveClass("marcada");
	expect(linha(/catalogo/)).not.toHaveClass("guarda-marcados");
});

test("o filtro não esconde um prefixo marcado", async () => {
	const user = userEvent.setup();
	const { rerender } = montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	await rerender({ filter: "ped" });

	// Só `pedido` casa com o filtro, mas sumir com os marcados mandaria
	// para o menu dois prefixos fora da vista — o mesmo perigo do galho
	// fechado, entrando por outra porta.
	expect(linha(/pedido/)).toBeInTheDocument();
	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	// O que não está marcado nem casa com o filtro sai, como sempre saiu.
	expect(screen.queryByRole("button", { name: /solta/ })).not.toBeInTheDocument();
});

test("o filtro mantém o caminho até uma marca lá dentro", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	const { rerender } = montar();

	await user.click(linha(/catalogo/));
	await cliqueComando(user, await screen.findByRole("button", { name: /eventos/ }));

	// Um filtro que não casa com nada: sem guardar o caminho, o galho de
	// cima sumiria e levaria a marca junto, sem aviso nenhum.
	await rerender({ filter: "zzz" });

	expect(linha(/catalogo/)).toBeInTheDocument();
	expect(linha(/eventos/)).toHaveClass("marcada");
	expect(screen.queryByRole("button", { name: /sessao/ })).not.toBeInTheDocument();
});

test("Shift+clique com a âncora fora da tela adota a linha clicada", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	montar();

	// A âncora passa a ser uma linha de dentro de `catalogo`...
	await user.click(linha(/catalogo/));
	await cliqueComando(user, await screen.findByRole("button", { name: /eventos/ }));
	// ...e some da tela quando o galho fecha.
	await user.click(linha(/catalogo/));

	await cliqueShift(user, linha(/sessao/));

	// Sem adotar a linha clicada, este Shift+clique e todos os seguintes
	// morreriam calados.
	expect(linha(/sessao/)).toHaveClass("marcada");

	// E a âncora nova funciona: a faixa seguinte sai dela.
	await cliqueShift(user, linha(/catalogo/));
	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/pedido/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
});

test("a marca de selecionado não usa a classe de produção", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));

	const marcada = linha(/catalogo/);
	expect(marcada).toHaveClass("marcada");
	expect(marcada.className).not.toMatch(/prod/i);
	// O rótulo sai como está escrito no banco: caixa alta é ambiente.
	expect(marcada).toHaveTextContent("catalogo");

	// O vitest não aplica o CSS do componente — `getComputedStyle` aqui não
	// enxerga a regra —, então ela é conferida na fonte: `--prod` e caixa
	// alta significam só o ambiente nesta tela, e emprestá-los para a
	// seleção deixaria o aviso de produção ambíguo.
	const fonte = readFileSync("src/lib/KeyTree.svelte", "utf8");
	for (const regra of [".marcada {", ".guarda-marcados {"]) {
		const inicio = fonte.indexOf(regra);
		expect(inicio).toBeGreaterThan(-1);
		const corpo = fonte.slice(inicio, fonte.indexOf("}", inicio));
		expect(corpo).not.toMatch(/--prod/);
		expect(corpo).not.toMatch(/uppercase/);
	}
});

test("a marca sobrevive à linha sair da janela", async () => {
	const user = userEvent.setup();
	// Com a janela medida, a árvore só desenha a fatia à vista. É o estado
	// real do aplicativo, e o que o jsdom nunca alcança sozinho: sem altura,
	// `viewport` fica 0 e TODA linha é desenhada — a virtualização, que é a
	// razão de a seleção morar em prefixo e não em índice de linha, ficaria
	// sem rede.
	Object.defineProperty(HTMLElement.prototype, "clientHeight", {
		configurable: true,
		value: 100,
	});

	const muitos = Array.from({ length: 60 }, (_, i) =>
		galho(`p${String(i).padStart(2, "0")}`, "", i + 1),
	);
	const { container } = render(KeyTree, {
		props: {
			hostId: "h1",
			db: 0,
			root: muitos,
			version: 0,
			selectedKey: null,
			onSelect: vi.fn(),
			filter: "",
			onPrefixMenu: vi.fn(),
			onKeyMenu: vi.fn(),
		},
	});

	await cliqueComando(user, linha(/p00/));
	expect(linha(/p00/)).toHaveClass("marcada");

	const rolagem = container.querySelector(".rolagem") as HTMLElement;
	Object.defineProperty(rolagem, "scrollTop", { configurable: true, value: 1200 });
	await fireEvent.scroll(rolagem);

	// A linha marcada deixou de existir no DOM — é exatamente aqui que uma
	// seleção guardada por índice de linha ou por nó perderia o rastro.
	expect(screen.queryByRole("button", { name: /p00/ })).not.toBeInTheDocument();
	expect(linha(/p45/)).toBeInTheDocument();

	Object.defineProperty(rolagem, "scrollTop", { configurable: true, value: 0 });
	await fireEvent.scroll(rolagem);

	expect(linha(/p00/)).toHaveClass("marcada");
});
