import { render, screen, within } from "@testing-library/svelte";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, logs: vi.fn() };
});

import { logs } from "$lib/api";
import LogPanel from "$lib/LogPanel.svelte";
import type { LogEntry } from "$lib/api";

const leu = vi.mocked(logs);

function entrada(partes: Partial<LogEntry> = {}): LogEntry {
	return {
		at_ms: 1_762_000_000_000,
		kind: "connection",
		host: "redis-exemplo",
		db: 0,
		operation: "connect",
		duration_ms: 42,
		detail: null,
		...partes,
	};
}

function hora(ms: number): string {
	return new Date(ms).toLocaleTimeString("pt-BR", { hour12: false });
}

function montar() {
	return render(LogPanel);
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("mostra a mais recente em cima, com hora, host, operação e duração", async () => {
	leu.mockResolvedValue([
		entrada({
			at_ms: 1_762_000_050_000,
			host: "redis-exemplo",
			operation: "connect",
			duration_ms: 42,
		}),
		entrada({
			at_ms: 1_762_000_000_000,
			host: "redis-estoque",
			operation: "ping",
			duration_ms: 8,
		}),
	]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	expect(linhas).toHaveLength(2);

	const primeira = linhas[0];
	expect(primeira).toHaveTextContent(hora(1_762_000_050_000));
	expect(primeira).toHaveTextContent("redis-exemplo");
	expect(primeira).toHaveTextContent("connect");
	expect(primeira).toHaveTextContent("42 ms");

	expect(linhas[1]).toHaveTextContent("redis-estoque");
});

test("mostra o banco quando houver, e omite quando não houver", async () => {
	leu.mockResolvedValue([
		entrada({ db: 3 }),
		entrada({ at_ms: 1_762_000_000_000, host: "redis-estoque", db: null }),
	]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	expect(within(linhas[0]).getByTestId("banco")).toHaveTextContent("banco 3");
	expect(within(linhas[1]).queryByTestId("banco")).not.toBeInTheDocument();
});

test("mostra o motivo da falha", async () => {
	leu.mockResolvedValue([
		entrada({
			kind: "failure",
			operation: "connect",
			duration_ms: null,
			detail: "ECONNREFUSED: conexão recusada pelo host",
		}),
	]);
	montar();

	const linha = (await screen.findAllByTestId("entrada"))[0];
	expect(linha).toHaveTextContent("ECONNREFUSED: conexão recusada pelo host");
	// null não deve aparecer na tela em lugar nenhum da linha.
	expect(linha.textContent).not.toMatch(/\bnull\b/);
});

test("diz quando não há nada registrado", async () => {
	leu.mockResolvedValue([]);
	montar();

	const vazio = await screen.findByTestId("vazio");
	expect(vazio).toBeInTheDocument();
	expect(screen.queryByTestId("entrada")).not.toBeInTheDocument();
});

test("mostra hosts que não são o aberto", async () => {
	leu.mockResolvedValue([
		entrada({ host: "redis-exemplo" }),
		entrada({ host: "redis-que-nao-abriu" }),
	]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	const texto = linhas.map((l) => l.textContent).join(" ");
	expect(texto).toContain("redis-exemplo");
	expect(texto).toContain("redis-que-nao-abriu");
});

test("avisa quando não conseguiu ler o log", async () => {
	leu.mockRejectedValue(new Error("falha ao ler"));
	montar();

	const erro = await screen.findByRole("alert");
	expect(erro).toHaveTextContent(/não foi possível ler o log/i);
	expect(screen.queryByTestId("vazio")).not.toBeInTheDocument();
	expect(screen.queryByTestId("entrada")).not.toBeInTheDocument();
});
