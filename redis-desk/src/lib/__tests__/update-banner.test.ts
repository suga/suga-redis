import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

// `vi.hoisted` porque a fábrica do `vi.mock` sobe para o topo do arquivo:
// um `const` comum ainda não existe quando ela roda.
const { instalar } = vi.hoisted(() => ({ instalar: vi.fn() }));

vi.mock("$lib/atualizacao", async (original) => ({
	...(await original<typeof import("$lib/atualizacao")>()),
	instalar,
}));

import UpdateBanner from "$lib/UpdateBanner.svelte";
import type { Achado, Progresso } from "$lib/atualizacao";

const achado: Achado = {
	versao: "0.2.0",
	notas: "a árvore some quando a chave some",
	pacote: {
		version: "0.2.0",
		downloadAndInstall: vi.fn(),
	},
};

function montar(props: Partial<{ onDispensar: () => void }> = {}) {
	return render(UpdateBanner, {
		props: { achado, atual: "0.1.0", onDispensar: vi.fn(), ...props },
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("diz de qual versão para qual, e o que mudou", () => {
	montar();

	expect(screen.getByRole("status")).toHaveTextContent("Versão 0.2.0 disponível");
	expect(screen.getByRole("status")).toHaveTextContent("Você está na 0.1.0");
	expect(screen.getByRole("status")).toHaveTextContent("a árvore some quando a chave some");
});

test("'Agora não' é uma resposta aceita", async () => {
	// Atualizar não pode ser obrigatório: quem está no meio de uma edição
	// em produção precisa poder adiar.
	const onDispensar = vi.fn();
	montar({ onDispensar });

	await userEvent.click(screen.getByRole("button", { name: "Agora não" }));

	expect(onDispensar).toHaveBeenCalledOnce();
	expect(instalar).not.toHaveBeenCalled();
});

test("o aviso não rouba o foco de quem está trabalhando", () => {
	// `role=status` é anunciado sem interromper; um `alertdialog` pararia a
	// pessoa no meio do que estava fazendo.
	montar();

	expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
	expect(screen.queryByRole("dialog")).toBeNull();
});

test("mostra o progresso enquanto baixa", async () => {
	const capturado: { reportar?: (p: Progresso) => void } = {};
	instalar.mockImplementation((_achado, aoProgresso) => {
		capturado.reportar = aoProgresso;
		return new Promise(() => {});
	});
	montar();

	await userEvent.click(screen.getByRole("button", { name: "Atualizar" }));
	capturado.reportar?.({ baixado: 500, total: 1000, fracao: 0.5 });

	expect(await screen.findByText("50%")).toBeInTheDocument();
});

test("sem tamanho anunciado, mostra o quanto veio em vez de uma barra falsa", async () => {
	const capturado: { reportar?: (p: Progresso) => void } = {};
	instalar.mockImplementation((_achado, aoProgresso) => {
		capturado.reportar = aoProgresso;
		return new Promise(() => {});
	});
	montar();

	await userEvent.click(screen.getByRole("button", { name: "Atualizar" }));
	capturado.reportar?.({ baixado: 2_097_152, total: null, fracao: null });

	expect(await screen.findByText("2.0 MB baixados")).toBeInTheDocument();
});

test("falhar ao atualizar diz que a versão instalada continua de pé", async () => {
	// A frase existe porque a pergunta imediata de quem vê um erro de
	// atualização é "quebrou o que eu já tinha?". A resposta é não: o
	// plugin só troca o app depois de conferir a assinatura.
	instalar.mockRejectedValue(new Error("error sending request"));
	montar();

	await userEvent.click(screen.getByRole("button", { name: "Atualizar" }));

	expect(await screen.findByText(/continua instalada e funcionando/)).toBeInTheDocument();
	expect(screen.getByRole("status")).toHaveTextContent("error sending request");
	expect(screen.getByRole("button", { name: "Tentar de novo" })).toBeInTheDocument();
});
