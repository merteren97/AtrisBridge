import { chmod, copyFile, mkdir, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const projectRoot = resolve(import.meta.dirname, "..");
const prepareScript = join(projectRoot, "scripts", "prepare-rclone.mjs");
const result = spawnSync(process.execPath, [prepareScript], {
  cwd: projectRoot,
  stdio: "inherit",
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`Pinned rclone preparation failed with exit code ${result.status}.`);

const executable = process.platform === "win32" ? "rclone.exe" : "rclone";
const source = join(projectRoot, "src-tauri", "binaries", executable);
const resourceDir = join(projectRoot, "src-tauri", "rclone");
const destination = join(resourceDir, executable);

await mkdir(resourceDir, { recursive: true });
await Promise.all([
  rm(join(resourceDir, "rclone"), { force: true }),
  rm(join(resourceDir, "rclone.exe"), { force: true }),
]);
await copyFile(source, destination);
if (process.platform !== "win32") await chmod(destination, 0o755);
console.log(`Staged verified AtrisBridge rclone release resource: ${destination}`);
