import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";
import ts from "typescript";

const file = resolve("src/lib/remote-mcp-grants.ts");
const transpiled = ts.transpileModule(readFileSync(file, "utf8"), {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
});
const { combineAuthorizedRemoteMcpClients } = await import(
  "data:text/javascript;base64," + Buffer.from(transpiled.outputText).toString("base64")
);

const idA = "mcp.remote." + "a".repeat(32);
const idB = "mcp.remote." + "b".repeat(32);
const idRevoked = "mcp.remote." + "c".repeat(32);
const grant = (principal, displayName, ready = false) => ({
  principal,
  displayName,
  activeOnThisDevice: ready,
  relayReadyOnThisDevice: ready,
  authorizedAt: "2026-11-12T12:00:00Z",
  updatedAt: "2026-11-12T12:01:00Z",
});
const seen = (principal) => ({
  principal,
  displayName: "Local cached label",
  firstSeenAt: "2026-11-12T12:01:00Z",
  lastSeenAt: "2026-11-12T12:02:00Z",
});

test("revoked process-local observation never resurrects a missing OAuth grant", () => {
  const results = combineAuthorizedRemoteMcpClients(
    [grant(idA, "Google")], [seen(idA), seen(idRevoked)],
  );
  assert.deepEqual(results.map((x) => x.principal), [idA]);
  assert.equal(results[0].displayName, "Google");
  assert.equal(results[0].observed, true);
});

test("all revoked grants produce an empty list even if cached tool traffic remains", () => {
  assert.deepEqual(combineAuthorizedRemoteMcpClients([], [seen(idRevoked)]), []);
});

test("authorized but not observed grants remain visible with correct routing metadata", () => {
  const results = combineAuthorizedRemoteMcpClients(
    [grant(idB, "ChatGPT Actions"), grant(idA, "Google", true)], [seen(idRevoked)],
  );
  assert.deepEqual(results.map((x) => x.principal), [idA, idB]);
  assert.equal(results[0].relayReadyOnThisDevice, true);
  assert.equal(results[0].observed, false);
  assert.equal(results[1].activeOnThisDevice, false);
  assert.equal(results[1].observed, false);
});

test("same-client authorizations retain their distinct principals and server routing", () => {
  const results = combineAuthorizedRemoteMcpClients(
    [grant(idA, "Google", true), grant(idB, "Google")],
    [seen(idA), seen(idB)],
  );
  assert.deepEqual(results.map((x) => x.principal), [idA, idB]);
  assert(results.every((x) => x.observed));
});
