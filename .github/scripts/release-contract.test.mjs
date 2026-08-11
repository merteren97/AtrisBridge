import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { createPackageBuildConfig } from "./create-package-build-config.mjs";
import { channelForTag, createUpdaterBuildConfig, updaterEndpoint } from "./create-updater-build-config.mjs";
import { generateUpdaterManifest } from "./generate-updater-manifest.mjs";

test("package config bundles the verified rclone resource", () => {
  const config = createPackageBuildConfig();
  assert.equal(config.bundle.active, true);
  assert.deepEqual(config.bundle.resources, ["rclone/**/*"]);
  assert.ok(config.bundle.icon.some((entry) => entry.endsWith("icon.ico")));
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

test("manifest contains signed canonical Windows and Linux updater targets", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "atrisbridge-manifest-"));
  try {
    for (const [name, contents] of [
      ["AtrisBridge_0.1.0-alpha.8_x64-setup.exe", "exe"],
      ["AtrisBridge_0.1.0-alpha.8_x64-setup.exe.sig", "windows-signature"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.AppImage", "appimage"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.AppImage.tar.gz", "appimage-updater"],
      ["AtrisBridge_0.1.0-alpha.8_amd64.AppImage.tar.gz.sig", "linux-signature"],
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
    assert.match(manifest.platforms["linux-x86_64"].url, /\.AppImage\.tar\.gz$/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
