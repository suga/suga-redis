import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, search: vi.fn(), rescan: vi.fn() };
});

import { rescan, search } from "$lib/api";
import SearchPanel from "$lib/SearchPanel.svelte";
import type { SearchHit, SearchOutcome } from "$lib/api";

const buscou = vi.mocked(search);
const varreu = vi.mocked(rescan);

function acerto(partes: Partial<SearchHit> = {}): SearchHit {
	return {
		key: "cliente:7",
		kind: "hash",
		ttl_ms: null,
		where_found: "value",
		snippet: "email ana@empresa.com",
		match_start: 6,
		match_len: 15,
		...partes,
	};
}

function resultado(partes: Partial<SearchOutcome> = {}): SearchOutcome {
	return {
		hits: [acerto()],
		scanned: 42,
		elapsed_ms: 7,
		stale_keys_removed: 0,
		index_age_secs: 30,
		...partes,
	};
}

function montar(props: Record<string, unknown> = {}) {
	return render(SearchPanel, {
		props: { hostId: "h1", db: 0, onOpenKey: vi.fn(), ...props },
	});
}

async function buscar(user: ReturnType<typeof userEvent.setup>, agulha: string) {
	await user.type(screen.getByLabelText("Buscar no banco"), agulha);
	await user.click(screen.getByRole("button", { name: "Buscar" }));
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("buscar mostra os acertos com o trecho destacado", async () => {
	const user = userEvent.setup();
	buscou.mockResolvedValue(resultado());
	montar();

	await buscar(user, "ana@empresa.com");

	expect(buscou).toHaveBeenCalledWith("h1", 0, "ana@empresa.com");
	expect(await screen.findByText("cliente:7")).toBeInTheDocument();
	// O pedaço encontrado vem marcado, não só o trecho inteiro em texto corrido.
	expect(screen.getByTestId("trecho").querySelector("mark")).toHaveTextContent(
		"ana@empresa.com",
	);
	expect(screen.getByTestId("aviso")).toHaveTextContent("42");
	expect(screen.getByTestId("aviso")).toHaveTextContent("7 ms");
});

test("clicar num acerto seleciona a chave na árvore", async () => {
	const user = userEvent.setup();
	const onOpenKey = vi.fn();
	buscou.mockResolvedValue(resultado());
	montar({ onOpenKey });

	await buscar(user, "ana");
	await user.click(await screen.findByRole("button", { name: /cliente:7/ }));

	expect(onOpenKey).toHaveBeenCalledWith("cliente:7");
});

test("busca sem acerto mostra o que fazer, não uma tela vazia", async () => {
	const user = userEvent.setup();
	buscou.mockResolvedValue(resultado({ hits: [], scanned: 0 }));
	montar();

	await buscar(user, "nao-existe");

	const vazio = await screen.findByTestId("sem-acerto");
	expect(vazio).toHaveTextContent("Nenhuma chave com esse texto");
	// Diz o próximo passo, em vez de deixar a pessoa adivinhando.
	expect(vazio).toHaveTextContent("varra o banco de novo");
});

test("índice velho mostra aviso com botão de varrer de novo", async () => {
	const user = userEvent.setup();
	// Dois dias: o retrato que respondeu já não vale grande coisa.
	buscou.mockResolvedValue(resultado({ index_age_secs: 172_800 }));
	varreu.mockResolvedValue({
		host_id: "h1",
		db: 0,
		keys: 10,
		finished_at: Date.now(),
		duration_ms: 5,
	});
	montar();

	await buscar(user, "ana");

	expect(await screen.findByTestId("indice-velho")).toBeInTheDocument();
	await user.click(screen.getByRole("button", { name: "Varrer de novo" }));

	expect(varreu).toHaveBeenCalledWith("h1", 0);
	// E refaz a busca sozinho: varrer sem reconsultar deixaria a lista velha na tela.
	expect(buscou).toHaveBeenCalledTimes(2);
});

test("acerto cujo índice estava velho some da lista e o aviso conta quantos sumiram", async () => {
	const user = userEvent.setup();
	buscou.mockResolvedValue(resultado({ hits: [acerto()], stale_keys_removed: 3 }));
	montar();

	await buscar(user, "ana");

	expect(await screen.findByTestId("aviso")).toHaveTextContent(
		"3 chaves tinham sumido",
	);
	// Só o que existe de verdade fica na lista.
	expect(screen.getAllByTestId("acerto")).toHaveLength(1);
});
