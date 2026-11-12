// Copies what the server needs from the playground build (scripts/build-web.sh) into assets/.
import fs from "node:fs";

const web = new URL("../../web/", import.meta.url);
const assets = new URL("../assets/", import.meta.url);
fs.mkdirSync(assets, { recursive: true });
for (const name of ["cartuccia.wasm", "mattone.wasm", "llms.txt", "export.js"]) {
  const from = new URL(name, web);
  if (!fs.existsSync(from)) {
    console.error(`missing web/${name}: run scripts/build-web.sh first`);
    process.exit(1);
  }
  fs.copyFileSync(from, new URL(name, assets));
  console.log(`assets/${name}: ${fs.statSync(new URL(name, assets)).size} bytes`);
}
