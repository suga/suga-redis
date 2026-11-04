import { render, screen, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, writeValue: vi.fn() };
});

import { writeValue } from "$lib/api";
import KeyEditor from "$lib/KeyEditor.svelte";
import type { Bytes, ValueView } from "$lib/api";

const escrito = vi.mocked(writeValue);

function texto(valor: string): Bytes {
	return { text: valor, base64: null, truncated: false };
}

function view(partes: Partial<ValueView> = {}): ValueView {
	return {
		key: "cliente:7",
		kind: "string",
		ttl_ms: null,
		size_bytes: 5,
		value: { kind: "string", data: texto("antes") },
		total_elements: 1,
		truncated: false,
		...partes,
	};
}

function montar(v: ValueView = view()) {
	return render(KeyEditor, {
		props: { hostId: "h1", db: 0, view: v, onWritten: vi.fn() },
	});
}

beforeEach(() => {
	vi.clearAllMocks();
	escrito.mockResolvedValue({ outcome: "applied", entry_id: 1 });
});

// --------------------------------------------------------------- TTL

test("dá para definir um prazo novo, em minutos, numa chave sem prazo", async () => {
	const user = userEvent.setup();
	montar(view({ ttl_ms: null }));

	await user.type(screen.getByLabelText("Novo prazo"), "90");
	await user.selectOptions(screen.getByLabelText("Unidade do prazo"), "min");
	await user.click(screen.getByRole("button", { name: "Definir prazo" }));

	expect(escrito).toHaveBeenCalledWith(
		"h1",
		0,
		"cliente:7",
		{ op: "set_ttl", ttl_ms: 90 * 60 * 1000 },
		false,
	);
});

test("prazo vazio ou zero não vira escrita", async () => {
	const user = userEvent.setup();
	montar();

	const definir = screen.getByRole("button", { name: "Definir prazo" });
	expect(definir).toBeDisabled();

	await user.type(screen.getByLabelText("Novo prazo"), "0");
	expect(definir).toBeDisabled();
	expect(escrito).not.toHaveBeenCalled();
});

test("chave com prazo mostra o prazo atual e deixa tirá-lo", async () => {
	const user = userEvent.setup();
	montar(view({ ttl_ms: 3_600_000 }));

	expect(screen.getByTestId("prazo-atual")).toHaveTextContent("1 h 0 min");

	await user.click(screen.getByRole("button", { name: "Tirar o prazo" }));
	expect(escrito).toHaveBeenCalledWith(
		"h1",
		0,
		"cliente:7",
		{ op: "set_ttl", ttl_ms: null },
		false,
	);
});

// -------------------------------------------------------------- JSON

const JSON_VALIDO = '{"email":"ana@empresa.com","plano":"ouro","ativo":true,"saldo":12.5}';

test("valor JSON abre colorido, com cada tipo em sua cor", async () => {
	montar(view({ value: { kind: "string", data: texto(JSON_VALIDO) } }));

	const realce = await screen.findByTestId("json-realce");
	// Chave, texto, número e booleano precisam ser distinguíveis entre si.
	// `classList` e não `className`: o Svelte acrescenta a classe de escopo
	// dele a cada elemento, e comparar a string inteira casaria com ela.
	const papeis = [...realce.querySelectorAll("span")].flatMap((s) => [...s.classList]);
	for (const esperado of ["chave", "texto", "numero", "booleano"]) {
		expect(papeis).toContain(esperado);
	}
});

test("dá para editar com as cores ligadas, no mesmo campo", async () => {
	const user = userEvent.setup();
	montar(view({ value: { kind: "string", data: texto(JSON_VALIDO) } }));

	// Um campo só: o texto é editável E colorido ao mesmo tempo.
	const campo = screen.getByLabelText("Valor");
	const realce = screen.getByTestId("json-realce");
	expect(campo.tagName).toBe("TEXTAREA");

	await user.clear(campo);
	await user.type(campo, '{{"ativo": false}}');

	// A camada de cor acompanha o que foi digitado agora.
	expect(realce).toHaveTextContent('"ativo"');
	const papeis = [...realce.querySelectorAll("span")].flatMap((s) => [...s.classList]);
	expect(papeis).toContain("chave");
	expect(papeis).toContain("booleano");
});

test("a camada de cor não é lida duas vezes por leitor de tela", () => {
	montar(view({ value: { kind: "string", data: texto(JSON_VALIDO) } }));
	// O texto já está no textarea; a camada é só pintura.
	expect(screen.getByTestId("json-realce")).toHaveAttribute("aria-hidden", "true");
});

test("JSON quebrado mostra o erro e trava o salvar", async () => {
	const user = userEvent.setup();
	montar(view({ value: { kind: "string", data: texto(JSON_VALIDO) } }));

	const campo = screen.getByLabelText("Valor");
	await user.clear(campo);
	await user.type(campo, '{{"email": "ana",}}');

	const aviso = await screen.findByTestId("json-erro");
	expect(aviso).toHaveTextContent(/posição|linha/i);
	expect(screen.getByRole("button", { name: "Salvar" })).toBeDisabled();
	expect(escrito).not.toHaveBeenCalled();
});

test("texto que não é JSON salva normalmente, sem validação atrapalhar", async () => {
	const user = userEvent.setup();
	montar(view({ value: { kind: "string", data: texto("so um texto") } }));

	const campo = screen.getByLabelText("Valor");
	await user.clear(campo);
	await user.type(campo, "outro texto qualquer");
	await user.click(screen.getByRole("button", { name: "Salvar" }));

	expect(screen.queryByTestId("json-erro")).not.toBeInTheDocument();
	expect(escrito).toHaveBeenCalledWith(
		"h1",
		0,
		"cliente:7",
		{ op: "set_string", value: texto("outro texto qualquer") },
		false,
	);
});

// ------------------------------------------------ o módulo de JSON, direto

import { realcarJson, validarJson } from "$lib/json";

test("o erro de JSON aponta a linha e a coluna, não a posição corrida", () => {
	const erro = validarJson('{\n  "a": 1,\n  "b": ,\n}');
	expect(erro).toMatch(/linha 3/);
});

test("literal dentro de um texto não é colorido como literal", () => {
	// `true` e `42` aqui são conteúdo da string, não valores do JSON.
	const tokens = realcarJson('{"obs":"true 42"}');
	const valor = tokens.find((t) => t.classe === "texto");
	expect(valor?.texto).toBe('"true 42"');
	expect(tokens.some((t) => t.classe === "booleano")).toBe(false);
	expect(tokens.some((t) => t.classe === "numero")).toBe(false);
});

test("o validador acha a posição em vários jeitos de quebrar", () => {
	// vírgula sobrando, valor faltando, string aberta, chave sem aspas
	expect(validarJson('{"a": 1,}')).toMatch(/linha 1, coluna 9/);
	expect(validarJson('{"a": }')).toMatch(/linha 1, coluna 7/);
	expect(validarJson('{"a": "sem fim}')).toMatch(/linha 1/);
	expect(validarJson("{a: 1}")).toMatch(/linha 1, coluna 2/);
	expect(validarJson('[1, 2, ]')).toMatch(/linha 1/);
	// e não reclama do que está certo
	expect(validarJson('{"a": [1, 2.5e3, true, null], "b": {"c": "d"}}')).toBeNull();
	expect(validarJson("nao e json")).toBeNull();
});

// ------------------------------ valor cortado: o risco de perder dado

test("valor cortado não deixa salvar, e explica por quê", async () => {
	const user = userEvent.setup();
	// 112 kB no servidor, 64 kB na tela: foi o caso real que revelou isto.
	montar(
		view({
			value: { kind: "string", data: texto("so os primeiros bytes") },
			total_elements: 114_800,
			truncated: true,
		}),
	);

	const campo = screen.getByLabelText("Valor");
	await user.type(campo, " mexido");

	// Salvar aqui gravaria o pedaço por cima do valor inteiro.
	expect(screen.getByRole("button", { name: "Salvar" })).toBeDisabled();
	expect(screen.getByTestId("cortado")).toHaveTextContent(/carregar o valor inteiro/i);
	expect(escrito).not.toHaveBeenCalled();
});

test("valor cortado não é acusado de JSON inválido", () => {
	// O começo de um JSON grande nunca fecha; acusar sintaxe aqui seria
	// culpar o conteúdo por um corte que foi nosso.
	montar(
		view({
			value: { kind: "string", data: texto('{"a": 1, "b": "come') },
			total_elements: 114_800,
			truncated: true,
		}),
	);

	expect(screen.queryByTestId("json-erro")).not.toBeInTheDocument();
});
