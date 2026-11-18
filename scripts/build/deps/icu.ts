/**
 * ICU — Unicode and locale data: Intl, collation, normalization, IDNA.
 *
 * `system`: on macOS the SDK's headers and libicucore; on Linux the host's headers and shared libraries, or the
 * static ICU whose include/ and lib/ are under $BUN_ICU_ROOT.
 */

import { join } from "node:path";
import { BuildError } from "../error.ts";
import type { Dependency } from "../source.ts";

/** In link order. */
const libraries = ["icui18n", "icuuc", "icudata"];

export const icu: Dependency = {
  name: "icu",

  source: () => ({ kind: "system" }),

  build: () => ({ kind: "none" }),

  provides: cfg => {
    if (cfg.icu === "bundled") {
      throw new BuildError("The bundled ICU is not available yet", {
        hint: "On Linux, --icu=system (the debug-local and release-local profiles) links the host's",
      });
    }
    if (cfg.darwin) return { libs: [], includes: [], linkFlags: ["-licucore"] };
    const root = process.env.BUN_ICU_ROOT;
    if (cfg.linux && root) {
      return { libs: libraries.map(name => join(root, "lib", `lib${name}.a`)), includes: [join(root, "include")] };
    }
    if (cfg.linux && cfg.abi !== "android") {
      return { libs: [], includes: [], linkFlags: libraries.map(name => `-l${name}`) };
    }
    throw new BuildError(`--icu=system: ${cfg.abi ?? cfg.os} has no ICU of its own to link`, {
      ...(cfg.linux && { hint: "Name a static ICU built for the target (include/, lib/) in BUN_ICU_ROOT" }),
    });
  },
};
