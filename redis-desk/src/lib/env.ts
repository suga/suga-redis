import type { Env } from "$lib/api";
import { t as traduzir } from "$lib/i18n.svelte";

/**
 * As três coisas que o ambiente decide na tela: a cor que pinta a barra do
 * topo, a linha que a assenta, e a palavra carimbada nela.
 *
 * A COR DO TEXTO não está aqui de propósito. Ela não é uma decisão de
 * domínio, é uma consequência do contraste: o âmbar da homologação é claro
 * demais para texto branco, o vermelho e o verde são escuros o bastante
 * para pedi-lo. Quem resolve isso é o CSS, por `[data-env]`.
 */
export interface EnvChrome {
	/** Fundo da barra do topo. */
	band: string;
	/**
	 * A palavra carimbada. É o único lugar do app com caixa alta.
	 *
	 * Vem do catálogo de idiomas: num app que fala três línguas, o carimbo
	 * que não acompanha é justamente o aviso que mais importa perdido.
	 */
	label: string;
	/** A linha sob a barra — o mesmo tom, mais fundo, para dar peso a ela. */
	border: string;
}

const CORES: Record<Env, Omit<EnvChrome, "label">> = {
	prod: { band: "#8E2B24", border: "#6B201B" },
	homolog: { band: "#C98A20", border: "#976818" },
	dev: { band: "#4E7A4A", border: "#3B5C38" },
};

const CHAVE_DO_AMBIENTE = {
	prod: "ambiente.prod",
	homolog: "ambiente.homolog",
	dev: "ambiente.dev",
} as const;

export function envChrome(env: Env): EnvChrome {
	return { ...CORES[env], label: traduzir(CHAVE_DO_AMBIENTE[env]) };
}
