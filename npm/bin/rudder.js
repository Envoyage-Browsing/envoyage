#!/usr/bin/env node
// Thin launcher: exec the downloaded native `rudder` binary, forwarding args,
// stdio (MCP speaks JSON-RPC over stdin/stdout), and the exit code.

const path = require("path");
const fs = require("fs");
const { spawnSync } = require("child_process");

const bin = path.join(__dirname, "rudder");
if (!fs.existsSync(bin)) {
  console.error(
    "rudder: native binary not found. The postinstall download may have failed — " +
      "reinstall, or build from source (cargo build --release -p rudder)."
  );
  process.exit(1);
}

const res = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (res.error) {
  console.error(`rudder: ${res.error.message}`);
  process.exit(1);
}
process.exit(res.status === null ? 1 : res.status);
