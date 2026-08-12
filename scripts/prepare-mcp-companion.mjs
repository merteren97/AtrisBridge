import { chmod, copyFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "..");
const manifest = join(root, "src-tauri", "mcp-companion", "Cargo.toml");
const targetDir = join(root, "src-tauri", "mcp-companion", "target");
const binariesDir = join(root, "src-tauri", "binaries");
const target = process.env.ATRISBRIDGE_BUILD_TARGET?.trim() || rustHostTarget();
if (!target) throw new Error("Could not resolve the Rust target triple for AtrisBridge MCP companion.");

const cargo = spawnSync("cargo", [
  "build",
  "--locked",
  "--release",
  "--manifest-path",
  manifest,
  "--target-dir",
  targetDir,
  "--target",
  target,
], {
  cwd: root,
  stdio: "inherit",
});
if (cargo.error) throw cargo.error;
if (cargo.status !== 0) {
  throw new Error(`AtrisBridge MCP companion build failed with exit code ${cargo.status}.`);
}

const windows = target.includes("windows");
const executable = windows ? "atrisbridge-mcp.exe" : "atrisbridge-mcp";
const source = join(targetDir, target, "release", executable);
const destination = join(
  binariesDir,
  windows ? `atrisbridge-mcp-${target}.exe` : `atrisbridge-mcp-${target}`,
);
await mkdir(binariesDir, { recursive: true });
await copyFile(source, destination);
if (!windows) await chmod(destination, 0o755);
console.log(`Staged AtrisBridge MCP companion for ${target}: ${destination}`);

function rustHostTarget() {
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Could not inspect Rust host target: ${result.stderr || "unknown rustc error"}`);
  }
  const line = String(result.stdout || "")
    .split(/\r?\n/)
    .find((entry) => entry.startsWith("host: "));
  return line?.slice("host: ".length).trim() || "";
}
