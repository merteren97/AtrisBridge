import fs from "node:fs";

const [configPath] = process.argv.slice(2);
const thumbprint = (process.env.WINDOWS_CERTIFICATE_THUMBPRINT || "").trim();
if (!configPath) throw new Error("Tauri release config path is required.");
if (!thumbprint) throw new Error("WINDOWS_CERTIFICATE_THUMBPRINT is required.");
const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
config.bundle ||= {};
config.bundle.windows ||= {};
config.bundle.windows.certificateThumbprint = thumbprint;
config.bundle.windows.digestAlgorithm = "sha256";
config.bundle.windows.timestampUrl = process.env.WINDOWS_TIMESTAMP_URL || "http://timestamp.digicert.com";
fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, "utf8");
console.log("Configured Windows Authenticode signing for this release runner.");
