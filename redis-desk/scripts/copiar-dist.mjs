// Junta os pacotes recém-gerados numa pasta só, `dist/`, para quem for
// instalar não ter que caçar dentro de `src-tauri/target/release/bundle`.
import { cp, mkdir, readdir, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";

const ORIGEM = "src-tauri/target/release/bundle";
const DESTINO = "dist";

// Cada formato mora numa subpasta do Tauri; aqui todos caem no mesmo lugar.
const PASTAS = ["macos", "dmg", "appimage", "deb"];
/** O `dmg/` do Tauri guarda o script e o ícone que ele usa para montar a
 *  imagem — artefatos de build, não coisas para alguém baixar. */
const NAO_SAO_PACOTES = new Set(["bundle_dmg.sh", "icon.icns"]);

if (!existsSync(ORIGEM)) {
	console.error(`Nada em ${ORIGEM}. Rode o build antes: npm run tauri build`);
	process.exit(1);
}

await rm(DESTINO, { recursive: true, force: true });
await mkdir(DESTINO, { recursive: true });

let copiados = 0;
for (const pasta of PASTAS) {
	const caminho = join(ORIGEM, pasta);
	if (!existsSync(caminho)) continue;

	for (const item of await readdir(caminho)) {
		if (NAO_SAO_PACOTES.has(item)) continue;
		await cp(join(caminho, item), join(DESTINO, item), { recursive: true });
		console.log(`  ${item}`);
		copiados += 1;
	}
}

console.log(copiados > 0 ? `\n${copiados} pacote(s) em ${DESTINO}/` : "\nNenhum pacote encontrado.");
