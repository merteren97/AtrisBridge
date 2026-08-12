import fs from "node:fs";
import path from "node:path";

const tag = process.argv[2];
if (!tag || !/^v\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$/.test(tag)) {
  throw new Error(`Unsupported AtrisBridge release tag: ${tag || "<missing>"}`);
}
const version = tag.slice(1);
const root = process.cwd();

function updateJson(relativePath, mutate) {
  const target = path.join(root, relativePath);
  const value = JSON.parse(fs.readFileSync(target, "utf8"));
  mutate(value);
  fs.writeFileSync(target, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function updateCargoPackage(relativePath, packageName) {
  const target = path.join(root, relativePath);
  const cargo = fs.readFileSync(target, "utf8");
  const updated = cargo.replace(
    /(\[package\][\s\S]*?\nname\s*=\s*"[^"]+"[\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/,
    `$1${version}$2`,
  );
  if (updated === cargo) throw new Error(`Could not update the ${packageName} Cargo package version.`);
  fs.writeFileSync(target, updated, "utf8");
}

function updateCargoLockPackage(relativePath, packageName) {
  const target = path.join(root, relativePath);
  if (!fs.existsSync(target)) return;
  const cargoLock = fs.readFileSync(target, "utf8").replace(/\r\n?/g, "\n");
  const escapedName = packageName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const updated = cargoLock.replace(
    new RegExp(`(\\[\\[package\\]\\]\\nname = "${escapedName}"\\nversion = ")[^"]+("` + ")"),
    `$1${version}$2`,
  );
  if (updated === cargoLock) throw new Error(`Could not update the ${packageName} Cargo.lock package version.`);
  fs.writeFileSync(target, updated, "utf8");
}

updateJson("package.json", (value) => { value.version = version; });
updateJson("src-tauri/tauri.conf.json", (value) => { value.version = version; });

const packageLockPath = path.join(root, "package-lock.json");
if (fs.existsSync(packageLockPath)) {
  updateJson("package-lock.json", (value) => {
    value.version = version;
    if (value.packages?.[""]) value.packages[""].version = version;
  });
}

updateCargoPackage("src-tauri/Cargo.toml", "atrisbridge");
updateCargoLockPackage("src-tauri/Cargo.lock", "atrisbridge");
updateCargoPackage("src-tauri/mcp-companion/Cargo.toml", "atrisbridge-mcp");
updateCargoLockPackage("src-tauri/mcp-companion/Cargo.lock", "atrisbridge-mcp");

console.log(`Applied AtrisBridge release version ${version} to desktop and MCP companion.`);
