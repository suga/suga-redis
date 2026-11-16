// Sobe a versão nos quatro lugares onde ela mora, roda os testes, commita e
// cria a tag. O `git push` fica de fora de propósito — veja o fim do arquivo.
//
// Os quatro lugares existem porque três ferramentas diferentes leem a versão
// (npm, cargo, tauri) e a quarta é o `Cargo.lock`, que registra o que o
// cargo resolveu. Deixá-los divergir dá um app que se anuncia 0.2.0 para o
// updater e 0.1.0 na tela de ajuda — e ninguém percebe até alguém reclamar.
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const SEMVER = /^\d+\.\d+\.\d+$/;

const nova = process.argv[2];
if (!nova || !SEMVER.test(nova)) {
	console.error("Uso: npm run versao 0.2.0");
	process.exit(1);
}

function git(...args) {
	return execFileSync("git", args, { encoding: "utf8" }).trim();
}

// Uma árvore suja viraria um commit de versão carregando mudanças que
// ninguém revisou junto.
if (git("status", "--porcelain")) {
	console.error("Há mudanças não commitadas. Commite ou guarde antes de soltar uma versão.");
	process.exit(1);
}

if (git("tag", "--list", `v${nova}`)) {
	console.error(`A tag v${nova} já existe.`);
	process.exit(1);
}

/** Troca um trecho exatamente uma vez, ou falha dizendo onde. */
function trocar(caminho, procurar, substituir) {
	const texto = readFileSync(caminho, "utf8");
	// Com `g`, e só com `g`: sem a flag, `match` devolve os grupos de
	// captura da primeira ocorrência, não a lista de ocorrências — e a
	// contagem abaixo daria sempre 1, deixando passar justamente o caso
	// que ela existe para pegar.
	const todas = texto.match(new RegExp(procurar.source, `${procurar.flags}g`));
	if (todas?.length !== 1) {
		console.error(
			`Esperava uma ocorrência da versão em ${caminho}, achei ${todas?.length ?? 0}.`,
		);
		process.exit(1);
	}
	writeFileSync(caminho, texto.replace(procurar, substituir));
}

trocar("package.json", /(?<=^\s{2}"version": ")[\d.]+(?=",$)/m, nova);
trocar("src-tauri/tauri.conf.json", /(?<=^\s{2}"version": ")[\d.]+(?=",$)/m, nova);
trocar("src-tauri/Cargo.toml", /(?<=^version = ")[\d.]+(?=")/m, nova);
// No lock, a versão certa é a do pacote `suga-redis` — e só a dele.
trocar(
	"src-tauri/Cargo.lock",
	/(?<=\nname = "suga-redis"\nversion = ")[\d.]+(?=")/,
	nova,
);

console.log(`Versão ${nova} escrita. Rodando os testes...`);
execFileSync("npm", ["test"], { stdio: "inherit" });

git("add", "package.json", "src-tauri/tauri.conf.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock");
git("commit", "-m", `chore: version ${nova}`);
git("tag", "-a", `v${nova}`, "-m", `Suga-Redis ${nova}`);

// O push não acontece aqui. Empurrar a tag dispara o workflow, que publica
// uma release pública e avisa todo mundo que já tem o app instalado — não é
// coisa que deva sair de um script que você rodou para "ver se funciona".
// Até este ponto tudo é local e desfazível.
console.log(`
Pronto, localmente. Para soltar de verdade:

  git push && git push origin v${nova}

Isso dispara o build das duas plataformas. Em alguns minutos a release
aparece no GitHub, e quem já tem o app recebe o aviso na próxima abertura.

Mudou de ideia antes de empurrar?

  git tag -d v${nova} && git reset --hard HEAD~1
`);
