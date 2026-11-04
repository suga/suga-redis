import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

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
	nivel.mockImplementation(async (_h, _db, prefix) => mapa[prefix] ?? null);
}

const RAIZ: Branch[] = [galho("catalogo", "", 24), galho("scheduler", "", 3_479_578)];

function montar(raiz: Branch[] = RAIZ) {
	return render(KeyTree, {
		props: {
			hostId: "h1",
			db: 0,
			root: raiz,
			selectedKey: null,
			onSelect: vi.fn(),
			filter: "",
		},
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("a raiz vem pronta de fora, sem a árvore pedir nada", () => {
	montar();

	expect(screen.getByRole("button", { name: /scheduler/ })).toHaveTextContent(
		"3.479.578",
	);
	// A raiz chega pelos retratos da varredura; abrir um galho é que consulta.
	expect(nivel).not.toHaveBeenCalled();
});

test("abrir um galho lê a árvore em memória, sem varredura nova", async () => {
	const user = userEvent.setup();
	arvore({
		"catalogo:": [galho("events", "catalogo:", 2), folha("total", "catalogo:")],
	});
	montar();

	await user.click(screen.getByRole("button", { name: /catalogo/ }));

	await waitFor(() => expect(nivel).toHaveBeenCalledWith("h1", 0, "catalogo:"));
	expect(await screen.findByRole("button", { name: /events/ })).toBeInTheDocument();
});

test("clicar numa folha entrega a chave inteira", async () => {
	const user = userEvent.setup();
	const onSelect = vi.fn();
	render(KeyTree, {
		props: {
			hostId: "h1",
			db: 0,
			root: [folha("solta", "")],
			selectedKey: null,
			onSelect,
			filter: "",
		},
	});

	await user.click(screen.getByRole("button", { name: /solta/ }));
	expect(onSelect).toHaveBeenCalledWith("solta");
});

test("galho ainda não varrido avisa, em vez de parecer vazio", async () => {
	const user = userEvent.setup();
	// A varredura ainda não chegou neste prefixo: a trie responde `null`.
	arvore({});
	montar();

	await user.click(screen.getByRole("button", { name: /catalogo/ }));

	expect(await screen.findByTestId("ainda-nao")).toHaveTextContent(/ainda sendo lid/i);
});

test("erro ao abrir um galho fica no galho, sem derrubar a árvore", async () => {
	const user = userEvent.setup();
	nivel.mockRejectedValue({ kind: "unreachable", message: 'host "Valkey" não responde' });
	montar();

	await user.click(screen.getByRole("button", { name: /catalogo/ }));

	expect(await screen.findByRole("alert")).toHaveTextContent("Valkey");
	expect(screen.getByRole("button", { name: /scheduler/ })).toBeInTheDocument();
});
