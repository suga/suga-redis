/**
 * Como uma falha vinda do Rust chega a quem está lendo.
 *
 * A mensagem técnica continua em português e literal — ela é o que se cola
 * num chamado e o que se procura no código. O que muda é que, acima dela,
 * aparece em qualquer idioma a frase que diz O QUE falhou, que é a parte
 * que a pessoa precisa para decidir o que fazer.
 */
import { render, screen } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";

import ErroTecnico from "$lib/ErroTecnico.svelte";
import { definirIdioma } from "$lib/i18n.svelte";

afterEach(() => {
	definirIdioma("pt-BR");
});

test("diz o que falhou no idioma de quem lê, e preserva o detalhe", () => {
	definirIdioma("en");

	render(ErroTecnico, {
		props: {
			assunto: "erro.aoConectar",
			detalhe: "falha ao conectar: Connection refused (os error 61)",
		},
	});

	const aviso = screen.getByRole("alert");
	expect(aviso).toHaveTextContent("Could not connect to the host.");
	// Verbatim: traduzir a mensagem técnica a esconderia de quem procura por
	// ela no código ou a cola num chamado.
	expect(aviso).toHaveTextContent("falha ao conectar: Connection refused (os error 61)");
});

test("sem detalhe nenhum, mostra só a frase", () => {
	definirIdioma("es");

	render(ErroTecnico, { props: { assunto: "erro.aoSalvar", detalhe: "" } });

	expect(screen.getByRole("alert")).toHaveTextContent("No se pudo guardar el cambio.");
});
