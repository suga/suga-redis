/**
 * O mecanismo de idioma da interface.
 *
 * O catálogo em português é a base: é o idioma em que o produto foi escrito
 * e o único que está completo por construção. Os outros caem nele quando
 * lhes falta uma chave — meia tela traduzida é pior que uma tela inteira
 * num idioma só.
 */
import { beforeEach, describe, expect, test } from "vitest";
import { formatarPrazo, haQuantoTempo, numero } from "$lib/formato";
import { CATALOGOS, definirIdioma, idioma, iniciarIdioma, t } from "$lib/i18n.svelte";

beforeEach(() => {
	definirIdioma("pt-BR");
	localStorage.clear();
});

describe("tradução", () => {
	test("devolve o texto do idioma escolhido", () => {
		expect(t("acao.conectar")).toBe("Conectar");

		definirIdioma("en");
		expect(t("acao.conectar")).toBe("Connect");

		definirIdioma("es");
		expect(t("acao.conectar")).toBe("Conectar");
	});

	test("o idioma atual é legível", () => {
		definirIdioma("en");
		expect(idioma()).toBe("en");
	});
});

describe("de onde vem o idioma inicial", () => {
	/** Finge o idioma do navegador, que no app é o idioma do sistema. */
	function sistemaFala(tag: string) {
		Object.defineProperty(navigator, "language", { value: tag, configurable: true });
	}

	test("a escolha de quem usa fica gravada", () => {
		definirIdioma("es");
		expect(localStorage.getItem("idioma")).toBe("es");
	});

	test("a escolha guardada vence o idioma do sistema", () => {
		// Como se uma sessão anterior tivesse escolhido espanhol.
		localStorage.setItem("idioma", "es");
		sistemaFala("en-US");

		iniciarIdioma();

		expect(idioma()).toBe("es");
	});

	test("sem escolha guardada, segue o idioma do sistema", () => {
		sistemaFala("es-AR");
		iniciarIdioma();
		expect(idioma()).toBe("es");
	});

	test("idioma de sistema que não falamos cai no inglês", () => {
		sistemaFala("fr-FR");
		iniciarIdioma();
		expect(idioma()).toBe("en");
	});

	test("escolha guardada inválida não derruba o app", () => {
		localStorage.setItem("idioma", "klingon");
		sistemaFala("en-US");
		iniciarIdioma();
		expect(idioma()).toBe("en");
	});
});

describe("o que o texto carrega dentro", () => {
	test("os números da frase entram no lugar certo", () => {
		expect(t("estado.conectado", { ms: 12, banco: 3 })).toBe(
			"conectado · 12 ms · banco 3",
		);

		definirIdioma("en");
		expect(t("estado.conectado", { ms: 12, banco: 3 })).toBe(
			"connected · 12 ms · database 3",
		);
	});

	test("um valor que ninguém mandou aparece como a própria marca", () => {
		// Melhor um buraco visível que uma frase truncada que parece correta.
		expect(t("estado.conectado", { ms: 12 })).toContain("{banco}");
	});
});

describe("os catálogos", () => {
	const chavesBase = Object.keys(CATALOGOS["pt-BR"]).sort();

	for (const [nome, catalogo] of Object.entries(CATALOGOS)) {
		test(`${nome} tem exatamente as chaves do português`, () => {
			expect(Object.keys(catalogo).sort()).toEqual(chavesBase);
		});

		test(`${nome} não tem texto vazio`, () => {
			const vazias = Object.entries(catalogo)
				.filter(([, texto]) => texto.trim() === "")
				.map(([chave]) => chave);
			expect(vazias).toEqual([]);
		});
	}
});

describe("as chaves do menu de índice", () => {
	/**
	 * Nomeadas uma a uma, e não só pela igualdade de forma logo acima: a
	 * comparação de formas passa verde se as TRÊS esquecerem a mesma chave,
	 * que é exatamente o que acontece quando alguém acrescenta uma frase na
	 * tela e não a traz para cá.
	 */
	const NOVAS = [
		"menu.copiarPrefixo",
		"menu.copiarPrefixos",
		"menu.apagarPrefixo",
		"menu.apagarPrefixos",
		"menu.copiarNome",
		"menu.apagarChave",
		"erro.aoEnumerarPrefixo",
		"erro.aoApagarPrefixo",
		"erro.aoCopiar",
		"erro.aoRelerArvore",
		"apagarPrefixo.nadaAqui",
		"apagarPrefixo.copiaNaoCoube",
		"apagarPrefixo.copiaNemEmArquivo",
		"apagarPrefixo.feitoUma",
		"apagarPrefixo.feitoVarias",
		"apagarPrefixo.feitoParcial",
		"historico.semCopia",
		"historico.voltouParcial",
		"historico.naoVoltaram",
		"historico.copiaEmArquivo",
		"op.deletePrefixUma",
		"op.deletePrefixVarias",
		"op.deletePrefixVariosPrefixos",
	];

	for (const [nome, catalogo] of Object.entries(CATALOGOS)) {
		test(`${nome} tem as chaves novas`, () => {
			const faltando = NOVAS.filter((chave) => !(chave in catalogo));
			expect(faltando).toEqual([]);
		});
	}

	test("os três dizem coisas diferentes, não o português copiado", () => {
		// Copiar o português nos outros dois passaria na checagem de forma e
		// na de texto vazio, e deixaria o produto monolíngue sem ninguém ver.
		for (const chave of ["menu.copiarPrefixo", "apagarPrefixo.nadaAqui"] as const) {
			definirIdioma("pt-BR");
			const base = t(chave);
			definirIdioma("en");
			expect(t(chave)).not.toBe(base);
		}
	});
});

describe("número, tempo e data", () => {
	test("o separador de milhar segue o idioma", () => {
		expect(numero(1234567)).toBe("1.234.567");

		definirIdioma("en");
		expect(numero(1234567)).toBe("1,234,567");
	});

	test("quanto falta para expirar é texto traduzido", () => {
		expect(formatarPrazo(0)).toBe("expirada");
		expect(formatarPrazo(90_000)).toBe("1 min 30 s");

		definirIdioma("en");
		expect(formatarPrazo(0)).toBe("expired");
		expect(formatarPrazo(90_000)).toBe("1 min 30 s");
	});

	test("há quanto tempo também", () => {
		expect(haQuantoTempo(30)).toBe("agora há pouco");

		definirIdioma("es");
		expect(haQuantoTempo(30)).toBe("hace un momento");
		expect(haQuantoTempo(7200)).toBe("hace 2 h");
	});
});
