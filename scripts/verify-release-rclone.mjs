import { access } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const REQUIRED_VERSION = "1.74.4";
const root = resolve(import.meta.dirname, "..");
const executable = join(root, "src-tauri", "rclone", process.platform === "win32" ? "rclone.exe" : "rclone");
await access(executable);
const result = spawnSync(executable, ["version"], { encoding: "utf8" });
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`Staged rclone failed with exit code ${result.status}: ${result.stderr || ""}`);
const firstLine = String(result.stdout || "").split(/\r?\n/, 1)[0].trim();
if (firstLine !== `rclone v${REQUIRED_VERSION}`) {
  throw new Error(`Expected rclone v${REQUIRED_VERSION}, received ${firstLine || "no version output"}.`);
}
console.log(`Verified packaged rclone v${REQUIRED_VERSION}.`);
