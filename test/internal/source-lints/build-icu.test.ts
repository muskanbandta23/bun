/** scripts/build/deps/icu.ts: which ICU a configuration compiles against and links. */
import { afterEach, beforeEach, expect, test } from "bun:test";
import { join } from "node:path";

import type { Config } from "../../../scripts/build/config.ts";
import { icu } from "../../../scripts/build/deps/icu.ts";
import { Ninja, readManifest } from "../../../scripts/build/ninja.ts";
import { profiles } from "../../../scripts/build/profiles.ts";
import { resolveDep } from "../../../scripts/build/source.ts";

const target = (os: Config["os"], abi: Config["abi"], mode: Config["icu"]) =>
  ({ os, abi, icu: mode, linux: os === "linux", darwin: os === "darwin", localDeps: {} }) as Config;

const inherited = process.env.BUN_ICU_ROOT;
beforeEach(() => {
  delete process.env.BUN_ICU_ROOT;
});
afterEach(() => {
  if (inherited === undefined) delete process.env.BUN_ICU_ROOT;
  else process.env.BUN_ICU_ROOT = inherited;
});

test("the -local profiles, and no other, ask for the system's", () => {
  expect(
    Object.entries(profiles).flatMap(([name, profile]) => ("icu" in profile ? [[name, profile.icu]] : [])),
  ).toEqual([
    ["debug-local", "system"],
    ["release-local", "system"],
  ]);
});

test.each(["gnu", "musl"] as const)("the system's on linux-%s is found by name", abi => {
  const n = new Ninja({ buildDir: "/build" });
  expect(resolveDep(n, target("linux", abi, "system"), icu, new Map())).toEqual({
    name: "icu",
    libs: [],
    linkFlags: ["-licui18n", "-licuuc", "-licudata"],
    objects: [],
    includes: [],
    defines: [],
    outputs: [],
    checks: [],
  });
  expect(readManifest(n.toString()).edges.map(edge => [edge.rule, edge.outputs])).toEqual([["phony", ["icu"]]]);
});

test.each(["gnu", "musl", "android"] as const)("BUN_ICU_ROOT names a static one for linux-%s", abi => {
  process.env.BUN_ICU_ROOT = "/icu";
  expect(icu.provides(target("linux", abi, "system"))).toEqual({
    libs: ["libicui18n.a", "libicuuc.a", "libicudata.a"].map(name => join("/icu", "lib", name)),
    includes: [join("/icu", "include")],
  });
  expect(icu.provides(target("darwin", undefined, "system")).linkFlags).toEqual(["-licucore"]);
});

test.each([
  ["linux", "android", "android"],
  ["windows", undefined, "windows"],
  ["freebsd", undefined, "freebsd"],
] as const)("%s (%s) has none", (os, abi, name) => {
  expect(() => icu.provides(target(os, abi, "system"))).toThrow(`--icu=system: ${name} has no ICU of its own to link`);
});

test("the bundled one is not there yet", () => {
  expect(() => icu.provides(target("linux", "gnu", "bundled"))).toThrow("The bundled ICU is not available yet");
});
