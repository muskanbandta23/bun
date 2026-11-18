/**
 * simdutf — Unicode validation, transcoding and base64 with runtime dispatch.
 *
 * `src/simdutf.cpp` is its unity TU: it includes every kernel and selects the
 * ISA of each with target attributes, so no per-file `-m` flags are needed.
 */

import type { Dependency } from "../source.ts";

const SIMDUTF_COMMIT = "ca7acbcea967b5dcbab490066e99e3a6e6925539"; // v9.0.0

export const simdutf: Dependency = {
  name: "simdutf",

  source: () => ({
    kind: "github-archive",
    repo: "simdutf/simdutf",
    commit: SIMDUTF_COMMIT,
  }),

  build: cfg => ({
    kind: "direct",
    lang: "cxx",
    pic: true,
    sources: ["src/simdutf.cpp"],
    includes: ["include", "src"],
    cflags: [cfg.windows ? "/clang:-std=c++20" : "-std=c++20"],
  }),

  provides: () => ({
    libs: [],
    includes: ["include"],
  }),
};
