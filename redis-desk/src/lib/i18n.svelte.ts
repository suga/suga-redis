/**
 * O idioma da interface.
 *
 * O catálogo em português é a base, e os outros têm a forma dele: um idioma
 * que esqueceu uma chave não compila. Meia tela traduzida é pior que uma
 * tela inteira num idioma só.
 */
import { en } from "./idiomas/en";
import { es } from "./idiomas/es";
import { ptBR } from "./idiomas/pt-BR";

export const IDIOMAS = ["pt-BR", "en", "es"] as const;
export type Idioma = (typeof IDIOMAS)[number];

export const CATALOGOS = { "pt-BR": ptBR, en, es } as const;

/** Onde a escolha de quem usa fica guardada entre uma sessão e outra. */
const CHAVE = "idioma";

/**
 * Para quem o app cai quando não reconhece o idioma do sistema.
 *
 * Inglês, e não português: quem roda o sistema em francês tem muito mais
 * chance de ler inglês do que português.
 */
const PADRAO: Idioma = "en";

let atual = $state<Idioma>("pt-BR");

function ehIdioma(valor: unknown): valor is Idioma {
	return IDIOMAS.includes(valor as Idioma);
}

/**
 * Decide o idioma de abertura: a escolha guardada vence; depois o idioma do
 * sistema; depois o padrão.
 *
 * O idioma do sistema vem como etiqueta BCP 47 (`es-AR`, `pt-PT`), então a
 * comparação é pela parte antes do hífen — menos `pt`, que tem catálogo
 * brasileiro e nenhum português europeu para confundir com ele.
 */
export function iniciarIdioma() {
	const guardado = localStorage.getItem(CHAVE);
	if (ehIdioma(guardado)) {
		atual = guardado;
		return;
	}

	const sistema = navigator.language ?? "";
	const base = sistema.split("-")[0];
	atual = base === "pt" ? "pt-BR" : ehIdioma(base) ? base : PADRAO;
}

/** O idioma em uso agora. */
export function idioma(): Idioma {
	return atual;
}

/** Troca o idioma da interface. */
export function definirIdioma(novo: Idioma) {
	atual = novo;
	localStorage.setItem(CHAVE, novo);
}

/**
 * O texto da chave, no idioma em uso, com os valores no lugar das marcas.
 *
 * Marca sem valor correspondente fica como está, à vista: um buraco que se
 * enxerga na tela é melhor que uma frase truncada que parece correta.
 */
export function t(
	chave: keyof typeof ptBR,
	valores?: Record<string, string | number>,
): string {
	const texto = CATALOGOS[atual][chave];
	if (!valores) return texto;
	return texto.replace(/\{(\w+)\}/g, (marca, nome) =>
		nome in valores ? String(valores[nome]) : marca,
	);
}
