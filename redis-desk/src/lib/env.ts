import type { Env } from "$lib/api";

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
	/** A palavra carimbada. É o único lugar do app com caixa alta. */
	label: string;
	/** A linha sob a barra — o mesmo tom, mais fundo, para dar peso a ela. */
	border: string;
}

const CHROME: Record<Env, EnvChrome> = {
	prod: { band: "#8E2B24", label: "PRODUÇÃO", border: "#6B201B" },
	homolog: { band: "#C98A20", label: "HOMOLOGAÇÃO", border: "#976818" },
	dev: { band: "#4E7A4A", label: "DESENVOLVIMENTO", border: "#3B5C38" },
};

export function envChrome(env: Env): EnvChrome {
	return CHROME[env];
}
