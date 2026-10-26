// Bundles src/ and index.html into the single-file template the Rust crate embeds.
import { build } from "esbuild";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const out = resolve(root, "../crates/heapscope-report/assets/report-template.html");

const bundle = async (entry) => {
  const r = await build({
    entryPoints: [resolve(root, entry)],
    bundle: true,
    minify: true,
    write: false,
    target: "es2020",
    legalComments: "none",
    charset: "utf8",
    logLevel: "warning",
    ...(entry.endsWith(".ts") ? { format: "iife" } : {}),
  });
  return r.outputFiles[0].text;
};

// A "</script" or "</style" in the output would end the element early.
const js = (await bundle("src/main.ts")).replace(/<\/(script)/gi, "<\/$1");
const css = (await bundle("src/styles/index.css")).replace(/<\/(style)/gi, "<\/$1");

// Replacer functions: the bundle may contain "$&" and the like.
let html = readFileSync(resolve(root, "index.html"), "utf8").replace(/\r\n/g, "\n");
// The CSP admits exactly this script (by hash): nothing else may run, and nothing may use the network.
const jsHash = createHash("sha256").update(js.trim()).digest("base64");
html = html
  .replace("/*HS_JS_HASH*/", () => jsHash)
  .replace("/*HS_CSS*/", () => css.trim())
  .replace("/*HS_JS*/", () => js.trim());

if ((html.match(/\{\{HS_DATA\}\}/g) ?? []).length !== 1) {
  throw new Error("the template must contain the {{HS_DATA}} placeholder exactly once (does the bundle mention it?)");
}
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, html);
console.log(`wrote ${out} (${(html.length / 1024).toFixed(1)} KiB)`);
