#!/usr/bin/env node
// Postinstall: download the prebuilt `rudder` binary for this platform from
// GitHub Releases and drop it next to bin/rudder.js. Mirrors immorterm-memory's
// wrapper: a plain releases/download CDN fetch, no GitHub API, no auth.

const fs = require("fs");
const path = require("path");
const https = require("https");
const { execFileSync } = require("child_process");

// Override for forks/testing (e.g. a private mirror).
const REPO = process.env.RUDDER_GITHUB_REPO || "ImmorTerm/rudder";
// Pinned to this package's version → the `rudder-<version>` release tag.
const VERSION = require("./package.json").version;
const TAG = process.env.RUDDER_RELEASE_TAG || `rudder-${VERSION}`;

const BIN_DIR = path.join(__dirname, "bin");
const BIN_PATH = path.join(BIN_DIR, "rudder");

// Map Node platform/arch → the release asset name the CI builds.
function assetName() {
  const os = { darwin: "macos", linux: "linux" }[process.platform];
  const arch = { arm64: "aarch64", x64: "x86_64" }[process.arch];
  if (!os || !arch) return null;
  // CI skips darwin-x64 (matches memory's matrix).
  if (os === "macos" && arch === "x86_64") return null;
  return `rudder-${os}-${arch}`;
}

// Follow redirects (releases/download 302s to the CDN).
function fetch(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("too many redirects"));
    https
      .get(url, { headers: { "User-Agent": "rudder-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return resolve(fetch(res.headers.location, dest, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const file = fs.createWriteStream(dest);
        res.pipe(file);
        file.on("finish", () => file.close(resolve));
        file.on("error", reject);
      })
      .on("error", reject);
  });
}

async function main() {
  const asset = assetName();
  if (!asset) {
    console.error(
      `rudder: no prebuilt binary for ${process.platform}/${process.arch}. ` +
        `Build from source: cargo build --release -p rudder.`
    );
    // Non-fatal: let install succeed; `rudder` will fail clearly if run.
    return;
  }

  fs.mkdirSync(BIN_DIR, { recursive: true });
  const url = `https://github.com/${REPO}/releases/download/${TAG}/${asset}.tar.gz`;
  const tgz = path.join(BIN_DIR, `${asset}.tar.gz`);

  console.error(`rudder: downloading ${asset} from ${url}`);
  await fetch(url, tgz);
  // The tarball contains a single `rudder` executable.
  execFileSync("tar", ["xzf", tgz, "-C", BIN_DIR], { stdio: "inherit" });
  fs.rmSync(tgz, { force: true });
  fs.chmodSync(BIN_PATH, 0o755);
  console.error(`rudder: installed ${BIN_PATH}`);
}

main().catch((err) => {
  console.error(`rudder: install failed: ${err.message}`);
  process.exit(1);
});
