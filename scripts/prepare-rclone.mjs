import { createHash } from "node:crypto";
import { copyFile, chmod, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const VERSION = "1.74.4";
const TARGETS = {
  "win32-x64": {
    platform: "windows-amd64",
    sha256: "ef097ef9de37a57feb7d9f9c7afb34148ad3c65be8025f1d8f7f521554a701ea",
    executable: "rclone.exe",
  },
  "win32-arm64": {
    platform: "windows-arm64",
    sha256: "72194ad0aaf210d7a55808801191fecc7e175444dab7be7491b7a63074521f3a",
    executable: "rclone.exe",
  },
  "linux-x64": {
    platform: "linux-amd64",
    sha256: "fe435e0c36228e7c2f116a8701f01127bb1f694005fc11d1f27186c8bca4115d",
    executable: "rclone",
  },
  "linux-arm64": {
    platform: "linux-arm64",
    sha256: "97685285c9ad6a0cf17d5844115d2a67245af6444db672187074bd9c358de419",
    executable: "rclone",
  },
  "darwin-x64": {
    platform: "osx-amd64",
    sha256: "4188aa84043d7a6240912923f47639a9d2da21f3b40a521c065c8d92e66563f6",
    executable: "rclone",
  },
  "darwin-arm64": {
    platform: "osx-arm64",
    sha256: "c2100e2d4a4b3be04c55cd45380cafe7647e1ad772bb055f52f00876ed701167",
    executable: "rclone",
  },
};

const key = `${process.platform}-${process.arch}`;
const target = TARGETS[key];
if (!target) {
  throw new Error(`Unsupported rclone development target: ${key}`);
}

const archiveName = `rclone-v${VERSION}-${target.platform}.zip`;
const url = `https://downloads.rclone.org/v${VERSION}/${archiveName}`;
const projectRoot = resolve(import.meta.dirname, "..");
const binariesDir = join(projectRoot, "src-tauri", "binaries");
const destination = join(binariesDir, target.executable);
const workDir = await mkdtemp(join(tmpdir(), "atrisbridge-rclone-"));
const archivePath = join(workDir, archiveName);
const extractDir = join(workDir, "extract");

try {
  console.log(`Downloading pinned rclone v${VERSION} for ${target.platform}...`);
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`Download failed with HTTP ${response.status}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  await writeFile(archivePath, bytes);

  const actualHash = createHash("sha256").update(bytes).digest("hex");
  if (actualHash !== target.sha256) {
    throw new Error(`SHA-256 mismatch for ${archiveName}. Expected ${target.sha256}, got ${actualHash}.`);
  }

  await mkdir(extractDir, { recursive: true });
  extractArchive(archivePath, extractDir);

  const source = join(extractDir, `rclone-v${VERSION}-${target.platform}`, target.executable);
  await readFile(source);
  await mkdir(binariesDir, { recursive: true });
  await copyFile(source, destination);
  if (process.platform !== "win32") {
    await chmod(destination, 0o755);
  }

  console.log(`Prepared verified rclone sidecar: ${destination}`);
} finally {
  await rm(workDir, { recursive: true, force: true });
}

function extractArchive(archivePath, destinationPath) {
  let result;
  if (process.platform === "win32") {
    const quote = (value) => value.replaceAll("'", "''");
    result = spawnSync(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        `Expand-Archive -LiteralPath '${quote(archivePath)}' -DestinationPath '${quote(destinationPath)}' -Force`,
      ],
      { stdio: "inherit" },
    );
  } else if (process.platform === "darwin") {
    result = spawnSync("ditto", ["-x", "-k", archivePath, destinationPath], { stdio: "inherit" });
  } else {
    result = spawnSync("unzip", ["-q", "-o", archivePath, "-d", destinationPath], { stdio: "inherit" });
  }

  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Could not extract ${archivePath}; extractor exited with ${result.status}.`);
  }
}
