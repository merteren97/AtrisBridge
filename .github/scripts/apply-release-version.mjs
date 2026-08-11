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

updateJson("package.json", (value) => { value.version = version; });
updateJson("src-tauri/tauri.conf.json", (value) => { value.version = version; });

const packageLockPath = path.join(root, "package-lock.json");
if (fs.existsSync(packageLockPath)) {
  updateJson("package-lock.json", (value) => {
    value.version = version;
    if (value.packages?.[""]) value.packages[""].version = version;
  });
}

const cargoPath = path.join(root, "src-tauri", "Cargo.toml");
const cargo = fs.readFileSync(cargoPath, "utf8");
const updatedCargo = cargo.replace(/(\[package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/, `$1${version}$2`);
if (updatedCargo === cargo) throw new Error("Could not update the AtrisBridge Cargo package version.");
fs.writeFileSync(cargoPath, updatedCargo, "utf8");

const cargoLockPath = path.join(root, "src-tauri", "Cargo.lock");
if (fs.existsSync(cargoLockPath)) {
  const cargoLock = fs.readFileSync(cargoLockPath, "utf8");
  const updatedLock = cargoLock.replace(
    /(\[\[package\]\]\nname = "atrisbridge"\nversion = ")[^"]+("\n)/,
    `$1${version}$2`,
  );
  if (updatedLock === cargoLock) throw new Error("Could not update the AtrisBridge Cargo.lock package version.");
  fs.writeFileSync(cargoLockPath, updatedLock, "utf8");
}

console.log(`Applied AtrisBridge release version ${version}.`);
