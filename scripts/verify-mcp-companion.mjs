import { access, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "..");
const target = process.env.ATRISBRIDGE_BUILD_TARGET?.trim() || rustHostTarget();
const windows = target.includes("windows");
const executable = join(
  root,
  "src-tauri",
  "binaries",
  windows ? `atrisbridge-mcp-${target}.exe` : `atrisbridge-mcp-${target}`,
);
const packageJson = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
const expectedVersion = String(packageJson.version || "").trim();
if (!expectedVersion) throw new Error("AtrisBridge package version is missing.");

await access(executable);
const result = spawnSync(executable, ["--version"], { encoding: "utf8" });
if (result.error) throw result.error;
if (result.status !== 0) {
  throw new Error(`Staged AtrisBridge MCP companion failed with exit code ${result.status}: ${result.stderr || ""}`);
}
const output = String(result.stdout || "").trim();
if (output !== `atrisbridge-mcp ${expectedVersion}`) {
  throw new Error(`Expected atrisbridge-mcp ${expectedVersion}, received ${output || "no output"}.`);
}
console.log(`Verified packaged AtrisBridge MCP companion for ${target}: ${output}`);

function rustHostTarget() {
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Could not inspect Rust host target: ${result.stderr || ""}`);
  const line = String(result.stdout || "")
    .split(/\r?\n/)
    .find((entry) => entry.startsWith("host: "));
  return line?.slice("host: ".length).trim() || "";
}
