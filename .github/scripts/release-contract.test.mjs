import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createPackageBuildConfig } from "./create-package-build-config.mjs";
import { channelForTag, createUpdaterBuildConfig, updaterEndpoint, writeUpdaterBuildConfig } from "./create-updater-build-config.mjs";
import { generateUpdaterManifest } from "./generate-updater-manifest.mjs";

const repositoryRoot = path.resolve(import.meta.dirname, "..", "..");

test("package config bundles verified runtime resources and the MCP companion", () => {
  const config = createPackageBuildConfig();
  assert.equal(config.bundle.active, true);
  assert.deepEqual(config.bundle.resources, ["rclone/**/*"]);
  assert.deepEqual(config.bundle.externalBin, ["binaries/atrisbridge-mcp"]);
  assert.ok(config.bundle.icon.some((entry) => entry.endsWith("icon.ico")));
});

test("MCP companion version tracks the desktop package and pins the reviewed SDK", () => {
  const packageJson = JSON.parse(fs.readFileSync(path.join(repositoryRoot, "package.json"), "utf8"));
  const companionCargo = fs.readFileSync(path.join(repositoryRoot, "src-tauri", "mcp-companion", "Cargo.toml"), "utf8");
  assert.match(companionCargo, new RegExp(`version = "${packageJson.version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"`));
  assert.match(companionCargo, /rmcp = \{ version = "=3\.0\.1"/);

  const syntax = spawnSync(process.execPath, ["--check", path.join(repositoryRoot, ".github", "scripts", "apply-release-version.mjs")], { encoding: "utf8" });
  assert.equal(syntax.status, 0, syntax.stderr || syntax.stdout);
});

test("release version application updates desktop and MCP companion atomically", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "atrisbridge-release-version-"));
  try {
    fs.mkdirSync(path.join(root, "src-tauri", "mcp-companion"), { recursive: true });
    fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ version: "0.1.0-alpha.9" }));
    fs.writeFileSync(path.join(root, "package-lock.json"), JSON.stringify({
      version: "0.1.0-alpha.9",
      packages: { "": { version: "0.1.0-alpha.9" } },
    }));
    fs.writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ version: "0.1.0-alpha.9" }));
    fs.writeFileSync(
      path.join(root, "src-tauri", "Cargo.toml"),
      '[package]\nname = "atrisbridge"\nversion = "0.1.0-alpha.9"\nedition = "2021"\n',
    );
    fs.writeFileSync(
      path.join(root, "src-tauri", "Cargo.lock"),
      'version = 4\n\n[[package]]\nname = "atrisbridge"\nversion = "0.1.0-alpha.9"\n',
    );
    fs.writeFileSync(
      path.join(root, "src-tauri", "mcp-companion", "Cargo.toml"),
      '[package]\nname = "atrisbridge-mcp"\nversion = "0.1.0-alpha.9"\nedition = "2021"\n',
    );
    fs.writeFileSync(
      path.join(root, "src-tauri", "mcp-companion", "Cargo.lock"),
      'version = 4\n\n[[package]]\nname = "atrisbridge-mcp"\nversion = "0.1.0-alpha.9"\n',
    );

    const result = spawnSync(
      process.execPath,
      [path.join(repositoryRoot, ".github", "scripts", "apply-release-version.mjs"), "v1.2.3-rc.4"],
      { cwd: root, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr || result.stdout);

    assert.equal(JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version, "1.2.3-rc.4");
    assert.equal(JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8")).version, "1.2.3-rc.4");
    assert.equal(JSON.parse(fs.readFileSync(path.join(root, "package-lock.json"), "utf8")).packages[""].version, "1.2.3-rc.4");
    assert.equal(JSON.parse(fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8")).version, "1.2.3-rc.4");
    assert.match(fs.readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8"), /version = "1\.2\.3-rc\.4"/);
    assert.match(fs.readFileSync(path.join(root, "src-tauri", "Cargo.lock"), "utf8"), /name = "atrisbridge"\nversion = "1\.2\.3-rc\.4"/);
    assert.match(fs.readFileSync(path.join(root, "src-tauri", "mcp-companion", "Cargo.toml"), "utf8"), /version = "1\.2\.3-rc\.4"/);
    assert.match(fs.readFileSync(path.join(root, "src-tauri", "mcp-companion", "Cargo.lock"), "utf8"), /name = "atrisbridge-mcp"\nversion = "1\.2\.3-rc\.4"/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("release channels are derived from SemVer and use HTTPS AtrisHub endpoints", () => {
  assert.equal(channelForTag("v0.1.0-alpha.8"), "preview");
  assert.equal(channelForTag("v1.2.3"), "stable");
  assert.match(updaterEndpoint("preview"), /^https:\/\/atrishub\.com\//);
  assert.match(updaterEndpoint("preview"), /\{\{target\}\}/);
  assert.match(updaterEndpoint("preview"), /\{\{arch\}\}/);
  assert.match(updaterEndpoint("preview"), /\{\{current_version\}\}/);
});

test("release updater config fails closed without a public key", () => {
  assert.throws(() => createUpdaterBuildConfig("", "v0.1.0-alpha.8"));
  const config = createUpdaterBuildConfig("PUBLIC_KEY", "v0.1.0-alpha.8");
  assert.equal(config.bundle.createUpdaterArtifacts, true);
  assert.equal(config.plugins.updater.windows.installMode, "passive");
});

test("updater config falls back to RELEASE_TAG when a shell expands the CLI tag to empty", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "atrisbridge-updater-config-"));
  const previousReleaseTag = process.env.RELEASE_TAG;
  try {
    process.env.RELEASE_TAG = "v1.2.3";
    const outputPath = path.join(root, "tauri.release.conf.json");
    writeUpdaterBuildConfig(outputPath, "", "PUBLIC_KEY");
    const config = JSON.parse(fs.readFileSync(outputPath, "utf8"));
    assert.match(config.plugins.updater.endpoints[0], /channel=stable$/);
    assert.equal(config.plugins.updater.pubkey, "PUBLIC_KEY");
  } finally {
    if (previousReleaseTag === undefined) delete process.env.RELEASE_TAG;
    else process.env.RELEASE_TAG = previousReleaseTag;
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("manifest contains signed canonical Windows and Tauri v2 Linux updater targets", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "atrisbridge-manifest-"));
  try {
    for (const [name, contents] of [
      ["AtrisBridge_0.1.0-alpha.8_x64-setup.exe", "exe"],
      ["AtrisBridge_0.1.0-alpha.8_x64-setup.exe.sig", "windows-signature"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.AppImage", "appimage"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.AppImage.sig", "linux-signature"],
      ["AtrisBridge_0.1.0-alpha.8_x64_en-US.msi", "msi"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.deb", "deb"],
    ]) fs.writeFileSync(path.join(root, name), contents);
    const manifest = generateUpdaterManifest({
      directory: root,
      repository: "merteren97/AtrisBridge",
      tag: "v0.1.0-alpha.8",
      publishedAt: "2026-08-11T00:00:00Z",
    });
    assert.equal(manifest.version, "0.1.0-alpha.8");
    assert.equal(manifest.platforms["windows-x86_64"].signature, "windows-signature");
    assert.equal(manifest.platforms["linux-x86_64"].signature, "linux-signature");
    assert.match(manifest.platforms["linux-x86_64"].url, /\.AppImage$/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
