import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function createPackageBuildConfig() {
  return {
    bundle: {
      active: true,
      resources: ["rclone/**/*"],
      externalBin: ["binaries/atrisbridge-mcp"],
      icon: [
        "icons/32x32.png",
        "icons/128x128.png",
        "icons/128x128@2x.png",
        "icons/icon.ico"
      ],
      publisher: "AtrisHub",
      copyright: "Copyright © 2026 AtrisHub"
    }
  };
}

export function writePackageBuildConfig(outputPath) {
  if (!outputPath) throw new Error("A package build config output path is required.");
  const resolved = path.resolve(outputPath);
  fs.mkdirSync(path.dirname(resolved), { recursive: true });
  fs.writeFileSync(resolved, `${JSON.stringify(createPackageBuildConfig(), null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600
  });
  return resolved;
}

const currentFile = fileURLToPath(import.meta.url);
const invokedFile = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedFile === currentFile) {
  const outputPath = process.argv[2];
  console.log(`Generated package-only Tauri config at ${writePackageBuildConfig(outputPath)}`);
}
