import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, listHistory: vi.fn(), rollback: vi.fn() };
});

import { listHistory, rollback } from "$lib/api";
import HistoryPanel from "$lib/HistoryPanel.svelte";
import type { HistoryEntry, WriteOp } from "$lib/api";

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

// ------------------------------------------------- a exclusão em massa

/** A operação que o Rust grava numa exclusão em massa. */
function apagouPrefixo(
	backup: { backup: "inline" } | { backup: "file"; path: string } | { backup: "none" },
): WriteOp {
	return {
		op: "delete_prefix",
		prefixes: ["sessao:"],
		deleted: 1_200,
		enumerated: 1_200,
		backup,
	};
}

test("a entrada sem cópia não oferece voltar atrás e diz por quê", async () => {
	listou.mockResolvedValue([
		entrada({
			id: 7,
			key: "sessao:",
			op: apagouPrefixo({ backup: "none" }),
			before_json: null,
			after_json: null,
		}),
	]);
	montar();

	const linha = (await screen.findAllByTestId("entrada"))[0];
	// O botão não fica cinza esperando: ele não está lá. Um botão
	// desabilitado promete que um dia acende, e este nunca acende.
	expect(within(linha).queryByRole("button", { name: "Voltar atrás" })).toBeNull();
	expect(within(linha).getByTestId("sem-volta")).toHaveTextContent(/sem cópia/i);
});

test("a entrada COM cópia continua oferecendo voltar atrás", async () => {
	listou.mockResolvedValue([
		entrada({ id: 8, key: "sessao:", op: apagouPrefixo({ backup: "inline" }) }),
	]);
	montar();

	const linha = (await screen.findAllByTestId("entrada"))[0];
	expect(within(linha).getByRole("button", { name: "Voltar atrás" })).toBeEnabled();
	expect(within(linha).queryByTestId("sem-volta")).toBeNull();
});

test("uma volta atrás parcial mostra quais chaves não voltaram", async () => {
	const user = userEvent.setup();
	listou.mockResolvedValue([
		entrada({ id: 9, key: "sessao:", op: apagouPrefixo({ backup: "inline" }) }),
	]);
	desfez.mockResolvedValue({
		outcome: "restored",
		restored: 1_198,
		failed: [
			{ key: "sessao:alpha", reason: "a chave mudou de tipo desde a cópia" },
			{ key: "sessao:beta", reason: "a chave já existia de novo" },
		],
	});
	montar();

	await user.click(await screen.findByRole("button", { name: "Voltar atrás" }));

	// "Pronto" esconderia o que ficou para trás. Os números E os nomes: sem
	// os nomes não há como ir conferir as duas que faltaram.
	const parcial = await screen.findByTestId("volta-parcial");
	expect(parcial).toHaveTextContent("1.198");
	expect(parcial).toHaveTextContent("sessao:alpha");
	expect(parcial).toHaveTextContent("sessao:beta");
	expect(parcial).toHaveTextContent("a chave mudou de tipo desde a cópia");
});

test("uma volta atrás inteira não inventa aviso de parcial", async () => {
	const user = userEvent.setup();
	listou.mockResolvedValue([
		entrada({ id: 10, key: "sessao:", op: apagouPrefixo({ backup: "inline" }) }),
	]);
	desfez.mockResolvedValue({ outcome: "restored", restored: 1_200, failed: [] });
	montar();

	await user.click(await screen.findByRole("button", { name: "Voltar atrás" }));

	await waitFor(() => expect(desfez).toHaveBeenCalled());
	expect(screen.queryByTestId("volta-parcial")).toBeNull();
});
