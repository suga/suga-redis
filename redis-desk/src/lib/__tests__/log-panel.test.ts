import { render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

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

afterEach(() => {
	vi.useRealTimers();
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

test("sobrevive a duas entradas do mesmo instante, host e operação", async () => {
	// Uma falha de "Conectar" empurra DUAS linhas separadas por nanossegundos:
	// o pool registra a tentativa de socket (`connection`) e a fachada
	// registra a falha da operação (`failure`). Elas caem no mesmo
	// milissegundo, com o mesmo host e a mesma operação — só o `kind` muda.
	// Uma chave de `{#each}` montada com o conteúdo da entrada trata as duas
	// como a mesma, e o Svelte 5 derruba a aba inteira. É o caminho exato que
	// esta aba existe para servir: clicar em Logs depois de uma conexão que
	// falhou.
	leu.mockResolvedValue([
		entrada({
			kind: "failure",
			duration_ms: 5,
			detail: "não foi possível alcançar o host",
		}),
		entrada({
			kind: "connection",
			duration_ms: 5,
			detail: "não foi possível alcançar o host",
		}),
	]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	expect(linhas).toHaveLength(2);
});

test("avisa quando não conseguiu ler o log", async () => {
	leu.mockRejectedValue(new Error("falha ao ler"));
	montar();

	const erro = await screen.findByRole("alert");
	expect(erro).toHaveTextContent(/não foi possível ler o log/i);
	expect(screen.queryByTestId("vazio")).not.toBeInTheDocument();
	expect(screen.queryByTestId("entrada")).not.toBeInTheDocument();
});

test("uma entrada de outro dia mostra a data, não só a hora", async () => {
	const ontem = Date.now() - 36 * 60 * 60 * 1000;
	const agora = Date.now();
	leu.mockResolvedValue([entrada({ at_ms: agora }), entrada({ at_ms: ontem })]);
	montar();

	const linhas = await screen.findAllByTestId("entrada");
	const dia = (ms: number) =>
		new Date(ms).toLocaleDateString("pt-BR", { day: "2-digit", month: "2-digit" });

	// Num aplicativo de mesa que fica dias aberto, só a hora faz a entrada
	// de segunda-feira parecer de agora há pouco.
	expect(linhas[1]).toHaveTextContent(dia(ontem));
	expect(linhas[0]).not.toHaveTextContent(dia(agora));
});

test("o botão Atualizar relê o log", async () => {
	const user = userEvent.setup();
	leu.mockResolvedValue([]);
	montar();
	await screen.findByTestId("vazio");

	await user.click(screen.getByRole("button", { name: "Atualizar" }));

	expect(leu).toHaveBeenCalledTimes(2);
});

test("relê o log sozinho enquanto a aba fica aberta", async () => {
	vi.useFakeTimers();
	leu.mockResolvedValue([]);
	montar();
	await vi.advanceTimersByTimeAsync(0);
	expect(leu).toHaveBeenCalledTimes(1);

	// Uma lista congelada, sem nada na tela dizendo que está congelada, faz
	// quem assiste a um host de produção concluir que nada está sendo
	// registrado.
	await vi.advanceTimersByTimeAsync(3000);
	expect(leu).toHaveBeenCalledTimes(2);
});

test("uma falha na releitura não apaga o que já estava na tela", async () => {
	vi.useFakeTimers();
	leu.mockResolvedValueOnce([entrada({ detail: "conexão recusada" })]);
	montar();
	await vi.advanceTimersByTimeAsync(0);
	expect(await screen.findByTestId("entrada")).toBeInTheDocument();

	leu.mockRejectedValueOnce(new Error("falha ao ler"));
	await vi.advanceTimersByTimeAsync(3000);

	expect(screen.getByRole("alert")).toHaveTextContent(/não foi possível ler o log/i);
	expect(screen.getByTestId("entrada")).toBeInTheDocument();
});
