/**
 * O diálogo que fica entre uma pessoa e apagar tudo que há sob um prefixo.
 *
 * A trava de produção não mora aqui — ela mora em `commands.rs`, onde um
 * botão não pode esquecer de perguntar. O que mora aqui é a outra metade do
 * consentimento informado: é desta tela que a pessoa aprende o que está
 * prestes a fazer. Por isso os testes cobram texto, e não só retorno de
 * função — um diálogo que engana não falha alto, devolve uma decisão errada
 * com toda a confiança.
 */
import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";

import DeletePrefixDialog from "$lib/DeletePrefixDialog.svelte";
import type { BackupRequest, EnumerationHandle, Host } from "$lib/api";

function hostDe(partes: Partial<Host> = {}): Host {
	return {
		id: "h1",
		name: "Redis pedidos",
		addr: "10.0.0.5",
		port: 6379,
		db: 0,
		username: null,
		tls: true,
		env: "prod",
		...partes,
	};
}

function enumeracaoDe(partes: Partial<EnumerationHandle> = {}): EnumerationHandle {
	return {
		id: "enum-1",
		count: 12_345,
		kinds: [
			["string", 12_000],
			["set", 345],
		],
		truncated: false,
		unrestorable: 0,
		...partes,
	};
}

interface Partes {
	host?: Host;
	db?: number;
	prefixos?: string[];
	enumeracao?: EnumerationHandle | null;
	progresso?: { scanned: number; total: number } | null;
	apagados?: number | null;
	onConfirmar?: (backup: BackupRequest, nomeDigitado: string | null) => void;
	onCancelar?: () => void;
}

function montar(partes: Partes = {}) {
	const onConfirmar = partes.onConfirmar ?? vi.fn();
	const onCancelar = partes.onCancelar ?? vi.fn();
	render(DeletePrefixDialog, {
		props: {
			host: partes.host ?? hostDe(),
			db: partes.db ?? 0,
			prefixos: partes.prefixos ?? ["sessao:"],
			enumeracao: partes.enumeracao === undefined ? enumeracaoDe() : partes.enumeracao,
			progresso: partes.progresso ?? null,
			apagados: partes.apagados ?? null,
			onConfirmar,
			onCancelar,
		},
	});
	return { onConfirmar, onCancelar };
}

/** O host fora de produção, para os testes que não são sobre a trava. */
const DEV = hostDe({ env: "dev" });

test("mostra a contagem da enumeração, não a da árvore", () => {
	montar({ host: DEV, enumeracao: enumeracaoDe({ count: 12_345 }) });

	const dialogo = screen.getByRole("dialog");
	expect(dialogo).toHaveTextContent("12.345");
	// E diz de onde esse número veio: a varredura de agora, não o número
	// guardado na árvore — é essa diferença que faz a enumeração existir.
	expect(dialogo).toHaveTextContent("conferidas agora");
});

test("a opção de cópia vem marcada", async () => {
	const user = userEvent.setup();
	const { onConfirmar } = montar({ host: DEV });

	expect(screen.getByLabelText("Guardar uma cópia no histórico antes de apagar")).toBeChecked();

	await user.click(screen.getByRole("button", { name: "Apagar 12.345 chaves" }));
	expect(onConfirmar).toHaveBeenCalledWith("inline", null);
});

test("desmarcar a cópia troca o texto do botão e acende o irreversível", async () => {
	const user = userEvent.setup();
	const { onConfirmar } = montar({ host: DEV });

	expect(screen.queryByText(/não há volta atrás/)).toBeNull();

	await user.click(screen.getByLabelText("Guardar uma cópia no histórico antes de apagar"));

	expect(screen.getByText(/não há volta atrás/)).toBeInTheDocument();
	expect(screen.queryByRole("button", { name: "Apagar 12.345 chaves" })).toBeNull();

	await user.click(
		screen.getByRole("button", { name: "Apagar 12.345 chaves sem volta atrás" }),
	);
	expect(onConfirmar).toHaveBeenCalledWith("none", null);
});

test("avisa quantas são de tipo sem volta atrás, mesmo com a cópia marcada", () => {
	montar({ host: DEV, enumeracao: enumeracaoDe({ unrestorable: 345 }) });

	// Com a cópia marcada — é justamente o caso em que a pessoa se acha
	// coberta e não está.
	expect(screen.getByLabelText("Guardar uma cópia no histórico antes de apagar")).toBeChecked();

	const aviso = screen.getByText(/não recria/);
	expect(aviso).toHaveTextContent("345");
	expect(aviso).toHaveTextContent("nem com a cópia");
});

test("em prod o botão fica travado até o nome do host bater exatamente", async () => {
	const user = userEvent.setup();
	const { onConfirmar } = montar({ host: hostDe({ env: "prod", name: "Redis pedidos" }) });

	const apagar = screen.getByRole("button", { name: "Apagar 12.345 chaves" });
	expect(apagar).toBeDisabled();

	const campo = screen.getByLabelText("Digite o nome do host para apagar");

	await user.type(campo, "redis pedidos");
	expect(apagar).toBeDisabled();

	// Nem um espaço a mais: a comparação é byte a byte, e aparar o que foi
	// digitado seria decidir pela pessoa que ela quis dizer outra coisa.
	await user.clear(campo);
	await user.type(campo, "Redis pedidos ");
	expect(apagar).toBeDisabled();

	await user.clear(campo);
	await user.type(campo, "Redis pedidos");
	expect(apagar).toBeEnabled();

	await user.click(apagar);
	expect(onConfirmar).toHaveBeenCalledWith("inline", "Redis pedidos");
});

test("fora de prod não pede nome nenhum", async () => {
	const user = userEvent.setup();
	const { onConfirmar } = montar({ host: hostDe({ env: "homolog" }) });

	expect(screen.queryByLabelText("Digite o nome do host para apagar")).toBeNull();

	const apagar = screen.getByRole("button", { name: "Apagar 12.345 chaves" });
	expect(apagar).toBeEnabled();

	await user.click(apagar);
	expect(onConfirmar).toHaveBeenCalledWith("inline", null);
});

test("acima do teto oferece gravar em arquivo e não oferece a cópia embutida", async () => {
	const user = userEvent.setup();
	const { onConfirmar } = montar({
		host: DEV,
		enumeracao: enumeracaoDe({ count: 100_000, truncated: true }),
	});

	expect(screen.getByLabelText("Gravar uma cópia em arquivo antes de apagar")).toBeChecked();
	expect(screen.queryByLabelText("Guardar uma cópia no histórico antes de apagar")).toBeNull();
	// E diz por quê: a varredura parou no teto, então são mais que isso.
	expect(screen.getByRole("dialog")).toHaveTextContent("100.000");

	await user.click(screen.getByRole("button", { name: "Apagar 100.000 chaves" }));
	expect(onConfirmar).toHaveBeenCalledWith("file", null);
});

test("mostra o carimbo de ambiente e o banco", () => {
	montar({ db: 7, prefixos: ["sessao:"] });

	const dialogo = screen.getByRole("dialog");
	expect(dialogo).toHaveTextContent("PRODUÇÃO");
	expect(dialogo).toHaveTextContent("Redis pedidos");
	expect(screen.getByText("7")).toBeInTheDocument();
	expect(screen.getByText("sessao:")).toBeInTheDocument();
});

test("lista até cinco prefixos e diz quantos ficaram de fora", () => {
	montar({
		host: DEV,
		prefixos: ["a:", "b:", "c:", "d:", "e:", "f:", "g:"],
	});

	const dialogo = screen.getByRole("dialog");
	for (const prefixo of ["a:", "b:", "c:", "d:", "e:"]) {
		expect(screen.getByText(prefixo)).toBeInTheDocument();
	}
	expect(screen.queryByText("f:")).toBeNull();
	expect(dialogo).toHaveTextContent("e mais 2");
});

test("enquanto varre, mostra o progresso e nenhum botão de confirmar", () => {
	montar({
		host: DEV,
		enumeracao: null,
		progresso: { scanned: 4_000, total: 50_000 },
	});

	const andamento = screen.getByRole("status");
	expect(andamento).toHaveTextContent("4.000");
	expect(andamento).toHaveTextContent("50.000");

	expect(screen.queryByRole("button", { name: /^Apagar/ })).toBeNull();
	expect(screen.getByRole("button", { name: "Cancelar" })).toBeInTheDocument();
});

test("cancelar durante o apagar chama onCancelar", async () => {
	const user = userEvent.setup();
	const { onCancelar } = montar({ host: DEV, apagados: 500 });

	expect(screen.getByRole("status")).toHaveTextContent("500");
	// A decisão já foi tomada: não dá para tomá-la de novo no meio.
	expect(screen.getByRole("button", { name: "Apagar 12.345 chaves" })).toBeDisabled();

	await user.click(screen.getByRole("button", { name: "Cancelar" }));
	expect(onCancelar).toHaveBeenCalledOnce();
});

test("o foco começa em Cancelar e Escape cancela", async () => {
	const user = userEvent.setup();
	const { onCancelar } = montar({ host: DEV });

	// A saída segura é a que já está sob o dedo.
	expect(screen.getByRole("button", { name: "Cancelar" })).toHaveFocus();

	await user.keyboard("{Escape}");
	expect(onCancelar).toHaveBeenCalledOnce();
});
