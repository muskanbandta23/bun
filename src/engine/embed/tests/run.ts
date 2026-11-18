/**
 * Builds and runs the tests of the C++ headers, once with assertions and sanitizers and once as they ship.
 *
 *   bun src/engine/embed/tests/run.ts [--only=checked|optimized] [part of a test's name]
 */

import { $, Glob } from "bun";
import { mkdirSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";

const { values, positionals } = parseArgs({ options: { only: { type: "string" } }, allowPositionals: true });

const embed = resolve(import.meta.dir, "..");
const root = resolve(embed, "../../..");
const compiler = process.env.CXX ?? "clang++";
const host = (await $`rustc -vV`.cwd(root).text()).match(/^host: (.+)$/m)![1];

const sources = ["tests/*.cpp", "cpp/Support*.cpp", "cpp/Text*.cpp"].flatMap(pattern =>
  [...new Glob(pattern).scanSync({ cwd: embed, absolute: true })].sort(),
);
const flags = [
  ..."-std=gnu++23 -g -fno-exceptions -fno-rtti -fno-omit-frame-pointer -fno-c++-static-destructors".split(" "),
  ..."-Wall -Wextra -Werror -Wno-unused-parameter -Wthread-safety".split(" "),
  ...(process.arch === "x64" ? ["-march=nehalem"] : []),
  ...[join(embed, "include"), join(root, "vendor/mimalloc/include"), join(root, "vendor/simdutf/include")].map(
    directory => `-I${directory}`,
  ),
];
const libraries =
  process.platform === "darwin" ? ["-licucore"] : ["-fuse-ld=lld", "-licui18n", "-licuuc", "-licudata", "-ldl"];

const configurations = {
  checked: { cxx: ["-O1", "-fsanitize=address,undefined", "-fno-sanitize-recover=all"], rust: "--cfg=bun_asan" },
  optimized: { cxx: ["-O2", "-DNDEBUG"], rust: "" },
};

for (const [name, { cxx, rust }] of Object.entries(configurations)) {
  if (values.only && values.only !== name) continue;
  const directory = join(root, "target", `embed-tests-${name}`);
  mkdirSync(directory, { recursive: true });

  const library = $`cargo build -q -p bun_engine_embed --example native_tests --target ${host} --target-dir ${directory}`
    .cwd(root)
    .env({ ...process.env, RUSTFLAGS: rust });
  const objects = sources.map(source => join(directory, `${basename(dirname(source))}-${basename(source)}.o`));
  await Promise.all([library, ...sources.map((source, i) => $`${compiler} ${flags} ${cxx} -c ${source} -o ${objects[i]}`)]);

  const program = join(directory, "tests");
  const archive = join(directory, host, "debug/examples/libnative_tests.a");
  const deadStrip = process.platform === "darwin" ? "-Wl,-dead_strip" : "-Wl,--gc-sections";
  await $`${compiler} ${cxx} ${objects} ${archive} ${libraries} ${deadStrip} -o ${program}`;

  console.log(`${name}:`);
  const { exitCode } = await $`${program} ${positionals}`.nothrow();
  if (exitCode) process.exit(exitCode);
}
