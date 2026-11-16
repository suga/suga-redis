import { render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, listHistory: vi.fn(), rollback: vi.fn() };
});

import { listHistory, rollback } from "$lib/api";
import HistoryPanel from "$lib/HistoryPanel.svelte";
import type { HistoryEntry } from "$lib/api";

const listou = vi.mocked(listHistory);
const desfez = vi.mocked(rollback);

/** O JSON que o backend guarda em `before_json`/`after_json`. */
function instantaneo(texto: string): string {
	return JSON.stringify({
		kind: "string",
		expires_at_ms: null,
		elements: [{ field: null, value: { text: texto, base64: null }, score: null }],
	});
}

function entrada(partes: Partial<HistoryEntry> = {}): HistoryEntry {
	return {
		id: 1,
		host_id: "h1",
		host_name: "Redis pagamentos",
		env: "prod",
		db: 0,
		key: "sessao:alpha",
		summary: "Definir o valor de sessao:alpha",
		op: null,
		before_json: instantaneo("antes"),
		after_json: instantaneo("depois"),
		actor: "human",
		at: 1_762_000_000_000,
		rolled_back: false,
		...partes,
	};
}

function montar() {
	return render(HistoryPanel, {
		props: { hostId: "h1", onRolledBack: vi.fn() },
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("mostra quem fez cada mudança, você ou o Claude", async () => {
	listou.mockResolvedValue([
		entrada({ id: 1, actor: "human" }),
		entrada({ id: 2, actor: "llm", key: "sessao:beta" }),
	]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	expect(within(linhas[0]).getByTestId("autor")).toHaveTextContent("Você");
	expect(within(linhas[1]).getByTestId("autor")).toHaveTextContent("Claude");
});

test("mostra antes e depois lado a lado", async () => {
	listou.mockResolvedValue([entrada()]);
	montar();

	const linha = (await screen.findAllByTestId("entrada"))[0];
	expect(within(linha).getByTestId("antes")).toHaveTextContent("antes");
	expect(within(linha).getByTestId("depois")).toHaveTextContent("depois");
});

test("voltar atrás em prod abre o confirmar", async () => {
	const user = userEvent.setup();
	listou.mockResolvedValue([entrada()]);
	desfez.mockResolvedValueOnce({
		outcome: "needs_confirmation",
		summary: "Desfazer em sessao:alpha: Definir o valor de sessao:alpha",
	});
	desfez.mockResolvedValueOnce({ outcome: "applied", entry_id: 1 });
	montar();

	await user.click(await screen.findByRole("button", { name: "Voltar atrás" }));

	const dialogo = await screen.findByRole("dialog");
	expect(dialogo).toHaveTextContent("Desfazer em sessao:alpha");
	expect(desfez).toHaveBeenCalledWith(1, false);

	await user.click(within(dialogo).getByRole("button", { name: "Confirmar" }));
	expect(desfez).toHaveBeenLastCalledWith(1, true);
});

test("linha já revertida não oferece voltar atrás de novo", async () => {
	listou.mockResolvedValue([entrada({ rolled_back: true })]);
	montar();

	const linha = (await screen.findAllByTestId("entrada"))[0];
	expect(linha).toHaveTextContent("Já revertida");
	expect(
		within(linha).queryByRole("button", { name: "Voltar atrás" }),
	).not.toBeInTheDocument();
});

test("histórico vazio convida a fazer a primeira mudança", async () => {
	listou.mockResolvedValue([]);
	montar();

	expect(await screen.findByTestId("vazio")).toHaveTextContent(
		"Nenhuma mudança ainda",
	);
});
