import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import ContextMenu from "$lib/ContextMenu.svelte";
import type { Item } from "$lib/ContextMenu.svelte";

afterEach(() => {
	cleanup();
	// Desfaz o `offsetHeight` fingido pelo teste de viragem, para não
	// vazar para os testes seguintes.
	delete (HTMLElement.prototype as { offsetHeight?: number }).offsetHeight;
});

const ITENS: Item[] = [
	{ id: "copiar", rotulo: "Copiar" },
	{ id: "renomear", rotulo: "Renomear" },
	{ id: "apagar", rotulo: "Apagar", perigoso: true },
];

beforeEach(() => {
	window.innerWidth = 1200;
	window.innerHeight = 800;
});

test("mostra um botão por item", () => {
	render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher: vi.fn(), onFechar: vi.fn() },
	});

	expect(screen.getByRole("menu")).toBeInTheDocument();
	for (const item of ITENS) {
		expect(screen.getByRole("menuitem", { name: item.rotulo })).toBeInTheDocument();
	}
});

test("Escape fecha", async () => {
	const user = userEvent.setup();
	const onFechar = vi.fn();
	render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher: vi.fn(), onFechar },
	});

	await user.keyboard("{Escape}");

	expect(onFechar).toHaveBeenCalledOnce();
});

test("clique fora fecha sem escolher", async () => {
	const user = userEvent.setup();
	const onEscolher = vi.fn();
	const onFechar = vi.fn();
	render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher, onFechar },
	});

	// Um botão fora do menu, para ter onde clicar.
	const fora = document.createElement("button");
	fora.textContent = "fora";
	document.body.appendChild(fora);

	await user.click(fora);

	expect(onFechar).toHaveBeenCalledOnce();
	expect(onEscolher).not.toHaveBeenCalled();
});

test("setas andam entre os itens e Enter escolhe", async () => {
	const user = userEvent.setup();
	const onEscolher = vi.fn();
	render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher, onFechar: vi.fn() },
	});

	// A seta para baixo, a partir de fora, cai no primeiro item.
	await user.keyboard("{ArrowDown}");
	expect(screen.getByRole("menuitem", { name: "Copiar" })).toHaveFocus();

	await user.keyboard("{ArrowDown}");
	expect(screen.getByRole("menuitem", { name: "Renomear" })).toHaveFocus();

	await user.keyboard("{ArrowUp}");
	expect(screen.getByRole("menuitem", { name: "Copiar" })).toHaveFocus();

	await user.keyboard("{Enter}");
	expect(onEscolher).toHaveBeenCalledWith("copiar");
});

test("vira para cima quando não cabe abaixo do cursor", () => {
	window.innerHeight = 200;
	// O jsdom não faz layout: `offsetHeight` fica sempre 0, o que nunca
	// estouraria a janela. Fingir uma altura real é o equivalente honesto
	// do que um navegador mediria.
	Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
		configurable: true,
		value: 150,
	});

	const { container } = render(ContextMenu, {
		props: { x: 10, y: 190, itens: ITENS, onEscolher: vi.fn(), onFechar: vi.fn() },
	});

	const menu = container.querySelector('[role="menu"]') as HTMLElement;
	// Virado para cima: a borda inferior do menu é que encosta no cursor,
	// não o topo — por isso `bottom`, não `top`.
	expect(menu.style.top).toBe("");
	expect(menu.style.bottom).not.toBe("");
});

test("não vira quando cabe abaixo do cursor", () => {
	window.innerHeight = 800;
	// Mesma altura fingida do teste de viragem — aqui sobra espaço de
	// sobra (800 - 10 - 150), então o menu deveria abrir para baixo,
	// sem virar. Uma implementação que sempre virasse passaria pelos
	// seis testes do brief sem que nenhum deles notasse.
	Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
		configurable: true,
		value: 150,
	});

	const { container } = render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher: vi.fn(), onFechar: vi.fn() },
	});

	const menu = container.querySelector('[role="menu"]') as HTMLElement;
	expect(menu.style.top).not.toBe("");
	expect(menu.style.bottom).toBe("");
});

test("devolve o foco ao elemento que o abriu", async () => {
	const abridor = document.createElement("button");
	abridor.textContent = "abrir menu";
	document.body.appendChild(abridor);
	abridor.focus();
	expect(abridor).toHaveFocus();

	const user = userEvent.setup();
	render(ContextMenu, {
		props: { x: 10, y: 10, itens: ITENS, onEscolher: vi.fn(), onFechar: vi.fn() },
	});

	await user.keyboard("{Escape}");

	expect(abridor).toHaveFocus();
});
