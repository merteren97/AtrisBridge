import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createPackageBuildConfig } from "./create-package-build-config.mjs";

export function channelForTag(tag) {
  if (/^v\d+\.\d+\.\d+$/.test(tag)) return "stable";
  if (/^v\d+\.\d+\.\d+-(?:alpha|beta|rc)\.\d+$/.test(tag)) return "preview";
  throw new Error(`Unsupported AtrisBridge release tag: ${tag}`);
}

export function updaterEndpoint(channel) {
  if (!['preview', 'stable'].includes(channel)) throw new Error(`Unsupported updater channel: ${channel}`);
  return `https://atrishub.com/api/desktop/v1/releases/atrisbridge/{{target}}/{{arch}}/{{current_version}}?channel=${channel}`;
}

export function createUpdaterBuildConfig(publicKey, tag) {
  const normalizedPublicKey = typeof publicKey === "string" ? publicKey.trim() : "";
  if (!normalizedPublicKey) {
    throw new Error("TAURI_UPDATER_PUBLIC_KEY is required to generate the release updater configuration.");
  }
  const channel = channelForTag(tag);
  const base = createPackageBuildConfig();
  return {
    ...base,
    bundle: {
      ...base.bundle,
      createUpdaterArtifacts: true
    },
    plugins: {
      updater: {
        pubkey: normalizedPublicKey,
        endpoints: [updaterEndpoint(channel)],
        windows: { installMode: "passive" }
      }
    }
  };
}

export function writeUpdaterBuildConfig(outputPath, tag, publicKey = process.env.TAURI_UPDATER_PUBLIC_KEY) {
  if (!outputPath) throw new Error("An updater build config output path is required.");
  if (!tag) throw new Error("A release tag is required to generate the updater config.");
  const resolved = path.resolve(outputPath);
  fs.mkdirSync(path.dirname(resolved), { recursive: true });
  fs.writeFileSync(resolved, `${JSON.stringify(createUpdaterBuildConfig(publicKey, tag), null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600
  });
  return resolved;
}

const currentFile = fileURLToPath(import.meta.url);
const invokedFile = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedFile === currentFile) {
  try {
    const [outputPath, tag] = process.argv.slice(2);
    const written = writeUpdaterBuildConfig(outputPath, tag);
    console.log(`Generated release-only Tauri updater configuration at ${written}`);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
