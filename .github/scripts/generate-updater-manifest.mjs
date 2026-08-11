import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function walk(directory) {
  const entries = [];
  for (const item of fs.readdirSync(directory, { withFileTypes: true })) {
    const absolute = path.join(directory, item.name);
    if (item.isDirectory()) entries.push(...walk(absolute));
    else entries.push(absolute);
  }
  return entries;
}

function requireSingle(files, predicate, label) {
  const matches = files.filter((file) => predicate(path.basename(file)));
  if (matches.length !== 1) {
    throw new Error(`Expected exactly one ${label}; found ${matches.length}: ${matches.map((item) => path.basename(item)).join(", ")}`);
  }
  return matches[0];
}

function releaseUrl(repository, tag, filename) {
  return `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(filename)}`;
}

function platformEntry(files, repository, tag, artifactPath) {
  const signaturePath = `${artifactPath}.sig`;
  if (!files.includes(signaturePath)) throw new Error(`Missing updater signature for ${path.basename(artifactPath)}`);
  const signature = fs.readFileSync(signaturePath, "utf8").trim();
  if (!signature) throw new Error(`Updater signature is empty for ${path.basename(artifactPath)}`);
  const filename = path.basename(artifactPath);
  return { signature, url: releaseUrl(repository, tag, filename) };
}

export function generateUpdaterManifest({ directory, repository, tag, publishedAt = new Date().toISOString() }) {
  if (!/^v\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$/.test(tag)) {
    throw new Error(`Invalid AtrisBridge SemVer release tag: ${tag}`);
  }
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error(`Invalid GitHub repository name: ${repository}`);
  }
  const files = walk(directory);
  const nsis = requireSingle(files, (name) => /-setup\.exe$/i.test(name) && !name.endsWith(".sig"), "Windows NSIS installer");
  const appImage = requireSingle(files, (name) => /\.AppImage$/i.test(name) && !name.endsWith(".sig"), "Linux AppImage");
  return {
    version: tag.slice(1),
    notes: `AtrisBridge ${tag} is available. Open the GitHub release for the full changelog.`,
    pub_date: publishedAt,
    platforms: {
      "windows-x86_64": platformEntry(files, repository, tag, nsis),
      "linux-x86_64": platformEntry(files, repository, tag, appImage)
    }
  };
}

const currentFile = fileURLToPath(import.meta.url);
const invokedFile = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedFile === currentFile) {
  const [directory, repository, tag] = process.argv.slice(2);
  if (!directory || !repository || !tag) {
    console.error("Usage: node generate-updater-manifest.mjs <release-assets-dir> <owner/repo> <vX.Y.Z[-alpha.N]>");
    process.exit(2);
  }
  const outputPath = path.join(directory, "latest.json");
  fs.writeFileSync(outputPath, `${JSON.stringify(generateUpdaterManifest({ directory, repository, tag }), null, 2)}\n`, "utf8");
  console.log(`Generated ${outputPath}`);
}
